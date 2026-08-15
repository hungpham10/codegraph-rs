//! Search — substring (LIKE) search trên Radix (node-based storage).
//!
//! Thay thế `search_index`:
//! - `insert(index, key, metadata)` — caller tự cấp record index (không còn
//!   entry_id/name); metadata + key length nằm trong Storage.
//! - `search(pattern, depth)` — tìm record có key **chứa** `pattern` (substring,
//!   không chỉ prefix), trả `(record, meta)`.
//!
//! `radix::search_prefix` chỉ khớp từ đầu key nên chưa đủ — `Search` dùng
//! **shortcuts** (nằm trong Storage) để tìm candidate node chứa element đầu của
//! pattern, rồi gọi `Radix::search_dfs` với **matcher callback** do `Search`
//! cung cấp (KMP nằm ở đây; radix chỉ lái DFS theo `OnMatchCallback` mà matcher trả
//! về — đổi thuật toán khác không cần sửa radix). Port từ
//! `search_index::search_like`.
//!
//! Không có cache in-memory (LRU) — mọi truy vấn (shortcut, node, children,
//! meta, key length) đi thẳng xuống Storage. `Search` chỉ giữ một buffer tạm cho
//! split events: callback `on_split` của radix là **sync** (chạy TRƯỚC commit
//! bên trong `trie.insert` — trả Err thì transaction bị hủy) nên không await
//! được để ghi storage — nó ghi vào buffer, `insert` flush xuống storage ngay
//! sau khi tree commit; buffer rỗng giữa các insert.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::RwLock;

use crate::radix::{
    self, DfsCheckpoint, EMPTY, Element, OnMatchCallback, OnNodeAccessCallback, Radix,
    SearchMatcher,
};
use crate::storage::{InMemoryStorage, Storage};

// ==================== Constants ====================

/// Giới hạn cứng số kết quả trả về (khớp `codegraph-graph::HARD_LIMIT`).
const MAX_RESULTS: usize = 5000;

// ==================== Resumable search ====================

/// Trạng thái resume của một lần [`Search::search_resumable`] bị ngắt bởi
/// deadline — caller gọi lại với cùng pattern + `resume` này để tiếp tục từ
/// đúng vị trí dừng (candidates recompute từ storage — deterministic trong một
/// snapshot, nên chỉ cần lưu vị trí + trạng thái DFS).
#[derive(Debug, Clone, Default)]
pub struct SearchResume {
    /// Candidate tiếp theo cần xử lý (index vào shortcut candidates).
    pub cand_idx: usize,
    /// Trạng thái DFS của candidate hiện tại (`None` = giữa các candidate —
    /// chưa xử lý candidate nào dở).
    pub dfs: Option<DfsCheckpoint>,
    /// Records đã collect (dedup chéo candidates) tính tới lúc ngắt.
    pub record_ids: Vec<usize>,
    /// Vị trí trong phase resolve (filter `depth`) nếu bị ngắt ở đó.
    pub resolve_idx: usize,
}

/// Kết quả của [`Search::search_resumable`].
#[derive(Debug, Clone, Default)]
pub struct SearchPage {
    /// Records khớp (record idx). Khi `timed_out` — records đã collect tới lúc
    /// ngắt (cũng nằm trong `resume.record_ids`).
    pub record_ids: Vec<usize>,
    /// `Some` = bị ngắt giữa chừng — caller phải gọi lại với `resume` này.
    pub resume: Option<SearchResume>,
    /// `true` khi `resume` có nghĩa (deadline đã hết hạn giữa chừng).
    pub timed_out: bool,
}

// ==================== Error ====================

#[derive(Debug)]
pub enum Error {
    NotFound,
    Duplicated,
    Storage(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::Duplicated => write!(f, "duplicated"),
            Error::Storage(msg) => write!(f, "storage error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<radix::Error> for Error {
    fn from(error: radix::Error) -> Self {
        match error {
            radix::Error::NotFound => Error::NotFound,
            _ => Error::Storage(error.to_string()),
        }
    }
}

impl From<crate::storage::StorageError> for Error {
    fn from(error: crate::storage::StorageError) -> Self {
        Error::Storage(error.to_string())
    }
}

impl From<Error> for codegraph_core::Error {
    fn from(e: Error) -> Self {
        codegraph_core::Error::Search(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// ==================== KMP matcher ====================

/// Build Longest Proper Prefix which is also Suffix (LPS) array — cho KMP.
#[inline]
fn lps<T: Element>(pattern: &[T]) -> Vec<usize> {
    let n = pattern.len();
    let mut lps = vec![0; n];
    let mut j = 0;
    for i in 1..n {
        while j > 0 && pattern[i] != pattern[j] {
            j = lps[j - 1];
        }
        if pattern[i] == pattern[j] {
            j += 1;
            lps[i] = j;
        }
    }
    lps
}

/// Chạy KMP trên một `data` slice (prefix của node — `Vec<T>`).
///
/// Trả về `(found, keep, data_pos, pattern_pos)`:
/// - `found`: tìm thấy pattern hoàn chỉnh trong data
/// - `keep`: có tiến triển (partial match) — chỉ có ý nghĩa khi `!found && do_recursive`
/// - `data_pos` / `pattern_pos`: trạng thái mới sau khi match
#[inline]
fn kmp_match<T: Element>(
    pattern: &[T],
    data: &[T],
    lps: &[usize],
    mut pattern_pos: usize,
    mut data_pos: usize,
    do_recursive: bool,
) -> (bool, bool, usize, usize) {
    let mut keep = false;

    while data_pos < data.len() {
        if data[data_pos] == pattern[pattern_pos] {
            keep = true;
            data_pos += 1;
            pattern_pos += 1;
        }

        if pattern_pos == pattern.len() {
            return (true, false, data_pos, pattern_pos);
        }

        if data_pos < data.len() && pattern[pattern_pos] != data[data_pos] {
            if !do_recursive {
                return (false, false, data_pos, pattern_pos);
            }

            if pattern_pos != 0 {
                pattern_pos = lps[pattern_pos - 1];
            } else {
                data_pos += 1;
                keep = false;
            }
        }
    }

    (false, keep, data_pos, pattern_pos)
}

/// Build matcher KMP cho `pattern` — biến `Radix::search_dfs` thành DFS + KMP.
///
/// `data_pos` luôn là 0 tại node entry, nên matcher nhận `(node_prefix,
/// pattern, pattern_pos)` và trả:
/// - `found`: pattern khớp trọn trong prefix (radix collect subtree)
/// - `continuations`: các `pattern_pos` mới để radix đệ quy xuống children
///   (thứ tự: scan-block restarts trước, main continuation sau — khớp
///   `dfs_search` cũ của radix)
#[inline]
fn kmp_matcher<T: Element>(pattern: &[T]) -> SearchMatcher<T> {
    let lps = lps(pattern);
    Arc::new(move |prefix: &[T], pat: &[T], pattern_pos: usize| {
        // Nếu phần còn lại của prefix ngắn hơn phần còn lại của pattern → cần
        // đệ quy xuống children (data_pos = 0 tại node entry).
        let remaining = pat.len().saturating_sub(pattern_pos);
        let do_recursive = prefix.len() < remaining;

        let (found, keep, _, new_pattern_pos) =
            kmp_match(pat, prefix, &lps, pattern_pos, 0, do_recursive);

        if found {
            return OnMatchCallback {
                found: true,
                continuations: Vec::new(),
            };
        }

        let mut continuations = Vec::new();

        // Match thất bại và ta đang bắt đầu fresh (pattern_pos == 0) → thử tất
        // cả vị trí còn lại của pattern[0] trong cùng prefix (scan-block).
        if pattern_pos == 0 && 1 < prefix.len() {
            let mut scan_pos = 1;
            while scan_pos < prefix.len() {
                if prefix[scan_pos] == pat[0] {
                    let do_rec = (prefix.len() - scan_pos) < pat.len();
                    let (f2, k2, _, pp2) = kmp_match(pat, prefix, &lps, 0, scan_pos, do_rec);
                    if f2 {
                        return OnMatchCallback {
                            found: true,
                            continuations: Vec::new(),
                        };
                    }
                    // Partial match → DFS xuống children.
                    if do_rec && k2 && pp2 < pat.len() {
                        continuations.push(pp2);
                    }
                }
                scan_pos += 1;
            }
        }

        // Còn có thể match tiếp và prefix đã hết → DFS xuống children.
        if do_recursive && keep && new_pattern_pos < pat.len() {
            continuations.push(new_pattern_pos);
        }

        OnMatchCallback {
            found: false,
            continuations,
        }
    })
}

// ==================== Search ====================

/// Search — cho phép insert chain + substring search trên Radix.
///
/// Generic `T` là kiểu element trong key (u8, u64, …). Mỗi key insert được gắn
/// record idx do caller cấp (1-indexed, `EMPTY` = 0).
///
/// Split events chưa flush xuống storage: `(leg_id, elem_bytes)`.
type PendingSplitElems = Vec<(usize, Vec<u8>)>;

/// `Search` là lớp mỏng trên Storage: metadata, key length và shortcuts (index
/// phụ cho LIKE search) đều nằm trong Storage — không có cache in-memory nào.
pub struct Search<T: Element = u8> {
    sharding: usize,
    trie: Radix<T>,
    storage: Arc<RwLock<dyn Storage>>,

    /// Split events chưa flush xuống storage.
    ///
    /// Callback `on_split` của radix là sync (chạy TRƯỚC commit bên trong
    /// `trie.insert`) nên không await được để ghi storage. Thay vào đó callback
    /// chỉ ghi vào buffer này; `insert` (của Search) flush xuống storage ngay
    /// sau khi `trie.insert` trả về — buffer rỗng giữa các insert.
    pending_split_elems: Arc<Mutex<PendingSplitElems>>,
}

impl<T: Element> Search<T> {
    pub fn new(sharding: usize, storage: Arc<RwLock<dyn Storage>>) -> Self {
        let sharding = sharding.max(1);
        let pending_split_elems = Arc::new(Mutex::new(Vec::new()));

        let mut trie = Radix::new(sharding, storage.clone());

        // Mặc định: mọi element có meta khi insert_chain được lưu vào node
        // stream keyed theo chính element id (chain model: element id = node
        // stream key). Tầng trên (lib.rs) override bằng callback riêng khi cần
        // filter (VD chỉ lưu Node JSON, bỏ qua marker payload).
        trie.with_node_access(Arc::new(|elem: T, _meta| Ok(elem.to_usize())));

        // Register split callback — ghi leg's elements vào pending buffer.
        //
        // KHÔNG xoá parent khỏi shortcut sets: shortcut set là over-approximation
        // (chỉ thêm, không bớt) — node bị stale trong set chỉ tốn thêm DFS trên
        // candidate sai (KMP verify prefix thật), không gây sai kết quả. Sets
        // được làm sạch khi `reload`/rebuild.
        let cb_pending = pending_split_elems.clone();
        trie.with_split(Arc::new(move |_, leg_id, old_prefix: &[T], breakpoint| {
            let mut pending = match cb_pending.lock() {
                Ok(p) => p,
                Err(_) => return Err(radix::Error::Callback),
            };

            for elem in old_prefix.iter().skip(breakpoint) {
                pending.push((leg_id, elem.encode()));
            }
            Ok(())
        }));

        Self {
            sharding,
            trie,
            storage,
            pending_split_elems,
        }
    }

    /// Tạo instance in-memory (dùng cho test / dev).
    #[allow(dead_code)] // API giữ nguyên (protected) — GraphIndex dùng new() + storage riêng.
    pub fn in_memory(sharding: usize) -> Self {
        Search::new(sharding, Arc::new(RwLock::new(InMemoryStorage::default())))
    }

    /// Tạo instance persistent trên SQLite (feature `sqlite`).
    ///
    /// Dữ liệu (tree, metadata, key length, shortcuts) sống trên đĩa — reopen
    /// cùng path giữ nguyên toàn bộ index. Mỗi `Search` dùng 1 file riêng.
    #[cfg(feature = "sqlite")]
    #[allow(dead_code)] // API giữ nguyên (protected) — GraphIndex dùng SqliteStorage trực tiếp.
    pub async fn sqlite(sharding: usize, path: &str) -> Result<Self> {
        let storage = crate::storage::sqlite::SqliteStorage::open(path).await?;
        Ok(Search::new(sharding, Arc::new(RwLock::new(storage))))
    }

    /// Xoá toàn bộ index (giữ nguyên storage — dùng khi rebuild).
    ///
    /// Clear shortcuts + edge stream + node stream + chains + set root của mọi
    /// shard về EMPTY — cây trở thành rỗng với reader. Node/metadata cũ thành
    /// garbage vô hại (không reachable từ root); shortcut phải xoá kẻo candidate
    /// stale dò vào subtree cũ.
    pub async fn clear(&mut self) -> Result<()> {
        let mut storage = self.storage.write().await;
        storage.clear_shortcuts().await?;
        storage.clear_edges().await?;
        storage.clear_node_meta().await?;
        storage.clear_chains().await?;
        for si in 0..self.sharding {
            storage.set_root(si, EMPTY).await?;
        }
        Ok(())
    }

    /// Đăng ký callback node access — forward thẳng xuống radix trie.
    ///
    /// Callback fire khi `insert_chain` chạm tới element có metadata (điểm flow
    /// đi tới) — trả về id node để lưu metadata vào node stream.
    #[allow(dead_code)] // API giữ nguyên (protected) — GraphIndex dùng metas=None.
    pub fn with_node_access(&mut self, cb: OnNodeAccessCallback<T>) {
        self.trie.with_node_access(cb);
    }

    /// Thêm một chain vào index với record index do caller cấp (kèm metadata
    /// song song cho từng element).
    ///
    /// Key trùng (đã tồn tại) → `Err(Duplicated)`, không ghi đè record/meta cũ.
    /// Chain được lưu vào chain stream (keyed theo record) — callees đọc trực
    /// tiếp từ đây.
    pub async fn insert_chain(
        &mut self,
        index: usize,
        key: &[T],
        node_metas: &[Option<&[u8]>],
    ) -> Result<()> {
        if key.is_empty() {
            return Err(Error::NotFound);
        }

        let (node_id, tail) = self.trie.insert(key, index, node_metas).await?;

        // Tree trả EMPTY → key đã tồn tại, không thay đổi gì (duplicate).
        // Duplicate không gây split nên buffer rỗng — clear đề phòng.
        if node_id == EMPTY {
            if let Ok(mut pending) = self.pending_split_elems.lock() {
                pending.clear();
            }
            return Err(Error::Duplicated);
        }

        // Tree đã commit → giờ mới an toàn update storage:
        // 1. Flush split legs' shortcut updates (callback sync chỉ ghi buffer).
        // 2. Key length (filter `depth`).
        // 3. Chain (per-record) vào chain stream.
        let pending: Vec<(usize, Vec<u8>)> = {
            let mut p = self
                .pending_split_elems
                .lock()
                .map_err(|error| Error::Storage(error.to_string()))?;
            std::mem::take(&mut *p)
        };
        {
            let mut storage = self.storage.write().await;
            for (leg_id, elem_bytes) in pending {
                let elem = T::decode(&elem_bytes);
                let si = radix::shard_of(elem, self.sharding);
                storage.add_shortcut_node(si, &elem_bytes, leg_id).await?;
            }
            storage.set_key_len(index, key.len()).await?;
            // Chain stream lưu mỗi element dưới dạng u64 theo đúng encoding (BE)
            // của element — `get_chain` decode bằng `T::decode(&u.to_be_bytes())`
            // nên roundtrip chính xác cho mọi T (element bytes nằm ở đầu buffer
            // 8 byte; u64 → identity).
            let chain: Vec<u64> = key
                .iter()
                .map(|e| {
                    let bytes = e.encode();
                    debug_assert!(bytes.len() <= 8);
                    let mut buf = [0u8; 8];
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    u64::from_be_bytes(buf)
                })
                .collect();
            storage.set_chain(index, &chain).await?;
        }

        // Shortcuts cho node mới (elements từ `tail` — phần trước đã được phủ
        // bởi node cha).
        self.update_shortcuts(key, tail, node_id).await?;

        Ok(())
    }

    /// Thêm node mới vào shortcut sets (từ `breakpoint`) — ghi thẳng xuống storage.
    async fn update_shortcuts(&self, key: &[T], breakpoint: usize, node_id: usize) -> Result<()> {
        let mut storage = self.storage.write().await;

        for elem in key.iter().skip(breakpoint) {
            let si = radix::shard_of(*elem, self.sharding);
            storage
                .add_shortcut_node(si, &elem.encode(), node_id)
                .await?;
        }
        Ok(())
    }

    /// Tìm các record có key **chứa** `pattern` (substring/LIKE).
    ///
    /// Dùng shortcuts (trong Storage) để lấy candidate node chứa element đầu
    /// của pattern, rồi gọi `Radix::search_dfs` với matcher KMP (`kmp_matcher`)
    /// dò xuống các nhánh của trie — khớp pattern ở vị trí bất kỳ trong key.
    ///
    /// - `depth` — số hop tối đa: chỉ trả key dài ≤ `depth + 1` phần tử
    ///   (VD: `depth = 1` → chỉ edge `[A, B]`, không trả path dài hơn).
    ///   `None` = không giới hạn độ dài key.
    /// - Trả về `(record_idx, metadata)` — metadata `None` nếu key insert
    ///   không kèm meta. Dedup theo record.
    ///
    /// Không tìm thấy → `Err(NotFound)` (giống `search_like` của `search_index`).
    pub async fn search(
        &self,
        pattern: &[T],
        depth: Option<usize>,
    ) -> Result<Vec<(usize, Option<Vec<u8>>)>> {
        let page = self.search_resumable(pattern, depth, None, None).await?;
        if page.record_ids.is_empty() {
            return Err(Error::NotFound);
        }
        // Resolve meta (API cũ giữ nguyên) — record_ids đã được filter `depth`
        // ở search_resumable nên chỉ cần đọc meta.
        let storage = self.storage.read().await;
        let mut results = Vec::new();
        for &rid in &page.record_ids {
            if rid == EMPTY {
                continue;
            }
            let meta = storage.get_meta(rid).await?;
            results.push((rid, meta));
            if results.len() >= MAX_RESULTS {
                break;
            }
        }
        if results.is_empty() {
            Err(Error::NotFound)
        } else {
            Ok(results)
        }
    }

    /// Như [`search`](Self::search) nhưng **resumable + deadline-aware** — dùng
    /// khi index lớn làm query chạy lâu. Khi `deadline` hết hạn giữa chừng: trả
    /// `SearchPage { timed_out: true, resume: Some(...) }` — caller gọi lại với
    /// `resume` để tiếp tục từ đúng vị trí dừng (không lặp phần đã duyệt, không
    /// mất records đã collect). Hoàn tất: `timed_out: false`, `resume: None`.
    ///
    /// Khác `search`: trả `record_ids` (`Vec<usize>`) không kèm meta — callers
    /// hiện tại (name engine, chain engine) không dùng meta; `search` giữ API cũ.
    ///
    /// `resume: None` = search mới. `deadline: None` = chạy tới cùng.
    pub async fn search_resumable(
        &self,
        pattern: &[T],
        depth: Option<usize>,
        resume: Option<SearchResume>,
        deadline: Option<Instant>,
    ) -> Result<SearchPage> {
        if pattern.is_empty() {
            return Err(Error::NotFound);
        }

        let first_elem = pattern[0];
        let si = radix::shard_of(first_elem, self.sharding);

        // Query candidates trực tiếp từ storage (deterministic per snapshot —
        // resume chỉ cần cand_idx, không cần lưu candidates).
        let candidates = self
            .storage
            .read()
            .await
            .get_shortcut_nodes(si, &first_elem.encode())
            .await?;

        // depth = max hop → max key length (số element) = depth + 1.
        let max_len = depth.map(|d| d + 1);

        let (mut cand_idx, mut dfs, mut record_ids, resolve_idx) = match resume {
            Some(r) => (r.cand_idx, r.dfs, r.record_ids, r.resolve_idx),
            None => (0, None, Vec::new(), 0),
        };
        // `seen` = tập record_ids đã collect (dedup chéo candidates — subtree
        // của candidate này có thể chứa subtree của candidate khác).
        let mut seen: HashSet<usize> = record_ids.iter().copied().collect();
        let matcher = kmp_matcher(pattern);

        // ── Candidate loop ──
        while cand_idx < candidates.len() {
            if let Some(dl) = deadline
                && Instant::now() >= dl
            {
                return Ok(SearchPage {
                    record_ids: record_ids.clone(),
                    resume: Some(SearchResume {
                        cand_idx,
                        dfs,
                        record_ids: record_ids.clone(),
                        resolve_idx: 0,
                    }),
                    timed_out: true,
                });
            }

            let node_id = candidates[cand_idx];
            let (records, ckpt) = self
                .trie
                .search_dfs(node_id, pattern, matcher.clone(), dfs.take(), deadline)
                .await?;
            match ckpt {
                // Timeout giữa candidate — lưu trạng thái DFS, tiếp tục lần sau.
                Some(cp) => {
                    dfs = Some(cp);
                }
                // Candidate xong — dedup records (records của candidate này) vào
                // kết quả chung.
                None => {
                    for rid in records {
                        if seen.insert(rid) {
                            record_ids.push(rid);
                            if record_ids.len() >= MAX_RESULTS {
                                break;
                            }
                        }
                    }
                    if record_ids.len() >= MAX_RESULTS {
                        break;
                    }
                    cand_idx += 1;
                    dfs = None;
                }
            }
        }

        // ── Resolve: filter `depth` (key length trong storage) — deadline-aware.
        // Bỏ qua nếu không giới hạn depth (name engine — chiếm đa số query).
        if let Some(m) = max_len {
            let mut out = Vec::new();
            let mut ridx = resolve_idx;
            let storage = self.storage.read().await;
            loop {
                if let Some(dl) = deadline
                    && Instant::now() >= dl
                {
                    return Ok(SearchPage {
                        record_ids: out.clone(),
                        resume: Some(SearchResume {
                            cand_idx: candidates.len(),
                            dfs: None,
                            record_ids: out.clone(),
                            resolve_idx: ridx,
                        }),
                        timed_out: true,
                    });
                }
                if ridx >= record_ids.len() {
                    break;
                }
                let rid = record_ids[ridx];
                ridx += 1;
                if rid == EMPTY {
                    continue;
                }
                if storage.get_key_len(rid).await?.unwrap_or(usize::MAX) > m {
                    continue;
                }
                out.push(rid);
            }
            record_ids = out;
        }

        Ok(SearchPage {
            record_ids,
            resume: None,
            timed_out: false,
        })
    }

    /// Tìm tất cả `(full_key, record)` có key bắt đầu bằng `prefix` (prefix match).
    ///
    /// Passthrough xuống `Radix::search_prefix` từ root của shard `prefix[0]`.
    /// Prefix rỗng → `Ok(vec![])` (không lỗi như `search`).
    ///
    /// Chain model không còn dùng prefix match (callers = substring search, callees
    /// = đọc chain stream) — giữ làm API search đầy đủ cho tầng trên dùng sau.
    #[allow(dead_code)]
    pub async fn search_prefix(&self, prefix: &[T]) -> Result<Vec<(Vec<T>, usize)>> {
        if prefix.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.trie.search_prefix(EMPTY, prefix).await?)
    }

    /// Đọc metadata gắn với một record index (`None` nếu insert không kèm meta).
    ///
    /// Quản lý metadata nằm ở tầng `Search` (Radix không biết tới meta) — dùng
    /// cho roundtrip test / quản lý index.
    #[allow(dead_code)]
    pub async fn get_meta(&self, index: usize) -> Result<Option<Vec<u8>>> {
        Ok(self.storage.read().await.get_meta(index).await?)
    }

    /// Đăng ký metadata cho một element (node stream) — passthrough xuống radix.
    ///
    /// Dùng khi rebuild: mọi node trong canonical kind được register một lần,
    /// độc lập với chain insert. Trả về id đã lưu.
    #[allow(dead_code)] // API giữ nguyên (protected) — GraphIndex dùng metas=None.
    pub async fn register_node(&self, elem: T, meta: &[u8]) -> Result<usize> {
        Ok(self.trie.register_node(elem, meta).await?)
    }

    /// Đọc metadata của một element (node stream) — `None` nếu chưa có.
    #[allow(dead_code)] // API giữ nguyên (protected) — GraphIndex dùng metas=None.
    pub async fn get_node_meta(&self, elem: usize) -> Result<Option<Vec<u8>>> {
        Ok(self.storage.read().await.get_node_meta(elem).await?)
    }

    /// Đọc chain của một record (chain stream) — `None` nếu record chưa có chain.
    ///
    /// Chain model: callees = đọc chain từ stream này. Không dùng trong lib build
    /// mặc định (chỉ test/sqlite builds) — giữ làm API chain đầy đủ (Phase B).
    #[allow(dead_code)]
    pub async fn get_chain(&self, record: usize) -> Result<Option<Vec<T>>> {
        let stored = self.storage.read().await.get_chain(record).await?;
        // Chain lưu theo encoding (BE) của element — decode ngược chính xác
        // (xem insert_chain). KHÔNG dùng to_le_bytes: sẽ swap byte cho element
        // > 1 byte (bug cũ — giá trị bị dịch 56 bit).
        Ok(stored.map(|chain| chain.iter().map(|&u| T::decode(&u.to_be_bytes())).collect()))
    }

    /// Lưu dữ liệu edge (opaque bytes, VD CallEdgeMeta JSON) keyed theo edge id.
    #[allow(dead_code)] // API giữ nguyên (protected) — edges suy từ chain trong GraphIndex.
    pub async fn set_edge_data(&self, edge: usize, data: &[u8]) -> Result<()> {
        Ok(self.storage.write().await.set_edge_data(edge, data).await?)
    }

    /// Đọc dữ liệu edge — `None` nếu edge chưa có.
    #[allow(dead_code)] // API giữ nguyên (protected).
    pub async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        Ok(self.storage.read().await.get_edge_data(edge).await?)
    }

    /// Duyệt toàn bộ edge data `(edge_id, meta)` — rebuild edge registry khi
    /// reopen (edge id ↔ (from,to) không persist riêng; CallEdgeMeta chứa đủ
    /// thông tin nên registry tái dựng được từ stream này).
    ///
    /// Chỉ dùng trong sqlite builds (reload_edges) — lib build mặc định không có.
    #[allow(dead_code)]
    pub async fn for_each_edge_data(
        &self,
        f: &mut (dyn for<'a> FnMut(usize, &'a [u8]) -> Result<()> + Send),
    ) -> Result<()> {
        self.storage
            .read()
            .await
            .for_each_edge_data(&mut |id, data| {
                f(id, data).map_err(|e| crate::storage::StorageError::Internal(e.to_string()))
            })
            .await?;
        Ok(())
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// node_metas toàn None (không chạm element nào) — song song với key.
    fn no_metas(len: usize) -> Vec<Option<&'static [u8]>> {
        vec![None; len]
    }

    #[tokio::test]
    async fn test_insert_and_search_like_substring() {
        let mut idx = Search::in_memory(4);
        idx.insert_chain(1, b"hello", &[Some(b"ma"), None, None, None, None])
            .await
            .unwrap();
        idx.insert_chain(2, b"world", &no_metas(5)).await.unwrap();
        idx.insert_chain(3, b"help", &no_metas(4)).await.unwrap();

        // Prefix "hel" khớp cả hello + help.
        let hits = idx.search(b"hel", None).await.unwrap();
        assert_eq!(hits.len(), 2);
        // Substring "llo" NẰM GIỮA key "hello" — radix::search_prefix không
        // khớp được, KMP + DFS phải dò xuống nhánh.
        let hits = idx.search(b"llo", None).await.unwrap();
        assert_eq!(hits.len(), 1, "substring 'llo' chỉ có trong 'hello'");
        assert_eq!(hits[0].0, 1);

        // Metadata nằm ở node stream (per element), không phải record-level.
        assert_eq!(
            idx.get_node_meta(b'h' as usize).await.unwrap().as_deref(),
            Some(b"ma".as_slice())
        );
        assert_eq!(idx.get_node_meta(b'w' as usize).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_search_like_partial_match_through_split() {
        // "hello" → split khi insert "help"/"held" — shortcuts phải chuyển
        // parent → leg đúng để vẫn tìm được "llo" trong "hello".
        let mut idx = Search::in_memory(4);
        idx.insert_chain(1, b"hello", &no_metas(5)).await.unwrap();
        idx.insert_chain(2, b"help", &no_metas(4)).await.unwrap();
        idx.insert_chain(3, b"held", &no_metas(4)).await.unwrap();

        let hits = idx.search(b"llo", None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 1, "record 'hello' phải được tìm thấy sau split");
    }

    #[tokio::test]
    async fn test_search_depth_filter() {
        // Chain 1 → 2 → 3 (u64 keys như CallIndex).
        let mut idx = Search::<u64>::in_memory(4);
        idx.insert_chain(1, &[1, 2], &no_metas(2)).await.unwrap();
        idx.insert_chain(2, &[1, 2, 3], &no_metas(3)).await.unwrap();

        // depth = 1 hop → chỉ key dài ≤ 2 ([1,2]).
        let d1 = idx.search(&[1], Some(1)).await.unwrap();
        assert_eq!(d1.len(), 1);
        assert_eq!(d1[0].0, 1);
        // depth = 2 hop → cả [1,2] và [1,2,3].
        let d2 = idx.search(&[1], Some(2)).await.unwrap();
        assert_eq!(d2.len(), 2);
        // Không giới hạn depth → cả 2.
        let all = idx.search(&[1], None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_insert_duplicate_key_idempotent() {
        let mut idx = Search::in_memory(4);
        idx.insert_chain(1, b"abc", &no_metas(3)).await.unwrap();
        let err = idx.insert_chain(2, b"abc", &no_metas(3)).await;
        assert!(
            matches!(err, Err(Error::Duplicated)),
            "duplicate phải báo lỗi"
        );

        let hits = idx.search(b"abc", None).await.unwrap();
        assert_eq!(hits.len(), 1, "duplicate key không tạo record mới");
        assert_eq!(hits[0].0, 1, "record giữ bản đầu tiên");
    }

    #[tokio::test]
    async fn test_search_not_found() {
        // Index rỗng.
        let idx = Search::in_memory(4);
        assert!(idx.search(b"nope", None).await.is_err());
        // Có dữ liệu nhưng pattern không tồn tại.
        let mut idx = Search::in_memory(4);
        idx.insert_chain(1, b"hello", &no_metas(5)).await.unwrap();
        assert!(idx.search(b"xyz", None).await.is_err());
        // Pattern rỗng.
        assert!(idx.search(b"", None).await.is_err());
    }

    #[tokio::test]
    async fn test_chain_and_node_meta_roundtrip() {
        let mut idx = Search::<u64>::in_memory(4);
        // Chain lưu vào chain stream — callees đọc trực tiếp.
        idx.insert_chain(1, &[100, 101], &[None, Some(b"meta-101")])
            .await
            .unwrap();
        assert_eq!(idx.get_chain(1).await.unwrap(), Some(vec![100, 101]));
        assert_eq!(idx.get_chain(2).await.unwrap(), None);
        // Node meta lưu per element.
        assert_eq!(
            idx.get_node_meta(101).await.unwrap().as_deref(),
            Some(b"meta-101".as_slice())
        );
        assert_eq!(idx.get_node_meta(100).await.unwrap(), None);

        // register_node ghi độc lập, không cần insert chain.
        idx.register_node(99, b"meta-99").await.unwrap();
        assert_eq!(
            idx.get_node_meta(99).await.unwrap().as_deref(),
            Some(b"meta-99".as_slice())
        );
    }

    #[tokio::test]
    async fn test_clear_resets_index() {
        let mut idx = Search::in_memory(4);
        idx.insert_chain(1, b"hello", &no_metas(5)).await.unwrap();
        idx.register_node(104, b"node-json").await.unwrap();
        assert!(idx.get_chain(1).await.unwrap().is_some());

        idx.clear().await.unwrap();
        assert!(idx.search(b"hello", None).await.is_err());
        assert_eq!(idx.get_node_meta(104).await.unwrap(), None);
        assert_eq!(idx.get_chain(1).await.unwrap(), None);
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn test_sqlite_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.sqlite");
        let path = path.to_str().unwrap();
        {
            let mut idx = Search::sqlite(4, path).await.unwrap();
            idx.insert_chain(1, b"hello", &[Some(b"ma"), None, None, None, None])
                .await
                .unwrap();
            idx.insert_chain(2, b"world", &no_metas(5)).await.unwrap();
            let hits = idx.search(b"llo", None).await.unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].0, 1);
        }
        // Reopen: dữ liệu + node meta + chains sống trên đĩa.
        let mut idx = Search::sqlite(4, path).await.unwrap();
        let hits = idx.search(b"wor", None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            idx.get_node_meta(b'h' as usize).await.unwrap().as_deref(),
            Some(b"ma".as_slice())
        );
        assert_eq!(idx.get_chain(1).await.unwrap(), Some(b"hello".to_vec()));
        // Depth filter dùng key_len persist: "world" dài 5 > 2 → bị loại.
        assert!(idx.search(b"wor", Some(1)).await.is_err());
        // Clear → index rỗng, search báo NotFound.
        idx.clear().await.unwrap();
        assert!(idx.search(b"wor", None).await.is_err());
    }
}
