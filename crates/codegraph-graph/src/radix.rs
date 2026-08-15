//! Radix trie trên storage (radix-node + transaction).
//!
//! Thay thế `radixtree.rs` cũ:
//! - Mọi node mutation đi qua transaction (`Storage::new_tx`) → split/extend
//!   áp dụng atomic, không lộ trạng thái trung gian cho reader.
//! - Shard root được đọc trực tiếp từ storage (`get_root`) thay vì cache
//!   `endpoints` in-memory — nhất quán giữa các instance.
//! - `OnSplitCallback` được gọi TRƯỚC khi commit — callback có thể từ chối
//!   (trả Err) thì transaction bị hủy, hoặc cập nhật shortcuts/cache rồi để
//!   radix commit.

use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::storage::{self, Storage};

#[cfg(feature = "bloom-search")]
use crate::bloom::BloomFilter;

pub const EMPTY: usize = 0;

/// Cấu hình bloom filter prune nhánh trong `search_dfs` (feature `bloom-search`).
#[cfg(feature = "bloom-search")]
pub mod bloom_cfg {
    /// Số bit của bloom filter mỗi node (làm tròn lên power of 2 trong `new`).
    pub const SIZE: usize = 4096;
    /// Số hash functions.
    pub const K: usize = 10;
    /// Chỉ prune khi substring còn lại của pattern ≤ cap này — bloom chỉ lưu
    /// substring ngắn, nên pattern dài hơn cap sẽ không bị prune (không sai).
    pub const MATCH_CAP: usize = 16;
}

/// Phần tử trong key của radix tree.
pub trait Element: Eq + Hash + Clone + Copy + Debug + Send + Sync + 'static {
    fn encode(&self) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Self;
    fn byte_size() -> usize;
    fn to_usize(&self) -> usize;
}

macro_rules! impl_element {
    ($ty:ty, $size:expr) => {
        impl Element for $ty {
            fn encode(&self) -> Vec<u8> {
                self.to_be_bytes().to_vec()
            }
            fn decode(bytes: &[u8]) -> Self {
                <$ty>::from_be_bytes(bytes[..$size].try_into().unwrap())
            }
            fn byte_size() -> usize {
                $size
            }
            fn to_usize(&self) -> usize {
                *self as usize
            }
        }
    };
}

impl_element!(u8, 1);
impl_element!(u16, 2);
impl_element!(u32, 4);
impl_element!(u64, 8);
impl_element!(u128, 16);
impl_element!(i8, 1);
impl_element!(i16, 2);
impl_element!(i32, 4);
impl_element!(i64, 8);
impl_element!(i128, 16);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("index must not be zero or negative")]
    InvalidIndex,

    #[error("prefix not found")]
    NotFound,

    #[error("storage error: {0}")]
    Storage(String),

    #[error("callback error")]
    Callback,
}

impl From<storage::StorageError> for Error {
    fn from(error: storage::StorageError) -> Self {
        Error::Storage(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Kết quả matcher trả về cho một node: pattern khớp hoàn toàn trong prefix
/// của node này (`found`), và các `pattern_pos` để tiếp tục dò xuống children
/// khi prefix đã hết mà pattern chưa khớp hết.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnMatchCallback {
    /// Pattern khớp hoàn toàn trong prefix node này → collect subtree.
    pub found: bool,
    /// Các `pattern_pos` (0 < pp < pattern.len()) để dò tiếp ở children.
    pub continuations: Vec<usize>,
}

/// Matcher hướng dẫn `search_dfs` khớp pattern với prefix từng node:
/// `(node_prefix, pattern, pattern_pos)` → `OnMatchCallback`.
///
/// Radix không biết thuật toán match cụ thể (KMP, naive, automaton, …) —
/// caller cung cấp qua callback; đổi thuật toán không cần sửa radix.
pub type SearchMatcher<T> = Arc<dyn Fn(&[T], &[T], usize) -> OnMatchCallback + Send + Sync>;

/// Callback khi split: `(parent_id, leg_id, old_prefix, breakpoint)`.
/// Được gọi TRƯỚC khi commit — trả Err để hủy transaction, hoặc cập nhật
/// shortcuts/cache dựa trên `old_prefix` + `breakpoint` rồi để radix commit.
pub type OnSplitCallback<T> = Arc<dyn Fn(usize, usize, &[T], usize) -> Result<()> + Send + Sync>;

// ==================== Resumable DFS ====================

/// Frame trên work-stack của `Radix::search_dfs`.
///
/// Chỉ lưu 4 số — `prefix`/`continuations`/`children` được recompute từ
/// `node_id` khi xử lý (matcher deterministic theo `(prefix, pattern,
/// pattern_pos)`), nên checkpoint nhỏ và resume chính xác.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DfsFrame {
    pub node_id: usize,
    pub pattern_pos: usize,
    pub cont_idx: usize,
    pub child_idx: usize,
}

/// Trạng thái duyệt hiện tại của `Radix::search_dfs` khi bị deadline
/// ngắt giữa chừng.
#[derive(Debug, Clone)]
pub enum DfsState {
    /// Đang dò xuống children (chưa tìm thấy match hoàn chỉnh).
    Search(Vec<DfsFrame>),
    /// Đang collect toàn bộ records trong subtree của `root` (sau khi matcher
    /// báo `found`). Stack = `(node_id, child_idx)` — duyệt pre-order.
    Collect {
        root: usize,
        stack: Vec<(usize, usize)>,
    },
}

/// Checkpoint của một lần duyệt bị ngắt — resume từ đây.
#[derive(Debug, Clone, Default)]
pub struct DfsCheckpoint {
    /// `None` = đã duyệt xong (caller advance sang candidate khác).
    pub state: Option<DfsState>,
    /// Records đã collect được tính tới lúc ngắt.
    pub records: Vec<usize>,
}

/// Callback khi chạm tới một node cụ thể, chứa thông tin đầy đủ về node
/// đó dưới dạng metadata, có cấu trúc dạng node, metadata và trả về id của
/// node, lưu ý vì đây là callback access nên nó có thể bị trùng hoặc gọi lại
/// nhiều lần nhưng phải trả về cùng 1 id nếu trùng
pub type OnNodeAccessCallback<T> = Arc<dyn Fn(T, &[u8]) -> Result<usize> + Send + Sync>;

/// Shard index của một element: `elem.to_usize() % sharding`.
pub fn shard_of<T: Element>(elem: T, sharding: usize) -> usize {
    elem.to_usize() % sharding
}

pub struct Radix<T: Element = u8> {
    sharding: usize,
    storage: Arc<RwLock<dyn Storage>>,
    on_node: Option<OnNodeAccessCallback<T>>,
    on_split: Option<OnSplitCallback<T>>,
}

impl<T: Element> Radix<T> {
    pub fn new(sharding: usize, storage: Arc<RwLock<dyn Storage>>) -> Self {
        Self {
            sharding: sharding.max(1),
            storage,
            on_node: None,
            on_split: None,
        }
    }

    #[cfg(test)]
    pub fn in_memory(sharding: usize) -> Self {
        Radix::new(
            sharding,
            Arc::new(RwLock::new(storage::InMemoryStorage::default())),
        )
    }

    pub fn with_split(&mut self, cb: OnSplitCallback<T>) {
        self.on_split = Some(cb);
    }

    pub fn with_node_access(&mut self, cb: OnNodeAccessCallback<T>) {
        self.on_node = Some(cb);
    }

    #[inline]
    fn from_vec(prefix: &[T]) -> Vec<u8> {
        prefix.iter().flat_map(|e| e.encode()).collect()
    }

    #[inline]
    fn to_vec(bytes: &[u8]) -> Vec<T> {
        bytes.chunks(T::byte_size()).map(T::decode).collect()
    }

    /// Chèn key với record index. Trả về `(node_id, tail)`:
    /// - node_id khác `EMPTY`: node chứa record (mới hoặc vừa cập nhật)
    /// - node_id = `EMPTY`: key đã tồn tại, không thay đổi gì (duplicate)
    ///
    /// `node_metas` song song với `prefix` (cùng độ dài): metadata của từng
    /// element trong key. Mỗi element có meta sẽ fire `on_node` — chạm tới node
    /// đó (điểm flow đi tới) → lưu metadata vào node stream. Fire ngay từ đầu
    /// insert, **độc lập với kết quả structural** (duplicate/split/extend đều
    /// fire) — callback access có thể gọi lại nhiều lần nhưng phải trả cùng id.
    pub async fn insert(
        &mut self,
        prefix: &[T],
        index: usize,
        node_metas: &[Option<&[u8]>],
    ) -> Result<(usize, usize)> {
        if index == EMPTY {
            return Err(Error::InvalidIndex);
        }
        if prefix.is_empty() {
            return Err(Error::NotFound);
        }

        // Chạm từng element có metadata — idempotent, không phụ thuộc kết quả insert.
        if node_metas.len() == prefix.len() {
            for (elem, meta) in prefix.iter().zip(node_metas.iter()) {
                if let Some(meta) = meta {
                    self.fire_node(*elem, meta).await?;
                }
            }
        }

        let mut tail = 0;
        let mut node_id = self
            .storage
            .read()
            .await
            .get_root(shard_of(prefix[0], self.sharding))
            .await?;

        while node_id != EMPTY {
            let mut found = false;

            let (prefix_bytes, node_record) =
                { self.storage.read().await.get_node(node_id).await? };
            let node_prefix = Self::to_vec(&prefix_bytes);

            // So node_prefix với đoạn còn lại của query key (bắt đầu từ `tail`).
            let common = node_prefix
                .iter()
                .zip(prefix[tail..].iter())
                .take_while(|(a, b)| a == b)
                .count();

            // Tới đoạn rẽ nhánh giữa chừng → chẻ node_prefix làm đôi.
            // `tail + common` là điểm split trong query key.
            if common < node_prefix.len() {
                let split_off = tail + common;
                let id = self
                    .split(node_id, common, &prefix[split_off..], index)
                    .await?;
                self.maintain_bloom(prefix).await?;
                return Ok((id, tail));
            }

            tail += common;

            // Match hoàn toàn key → ghi record vào node này (nếu chưa có).
            if tail == prefix.len() {
                if node_record == EMPTY {
                    self.storage
                        .write()
                        .await
                        .update_node(node_id, None, Some(index))
                        .await?;
                    self.maintain_bloom(prefix).await?;
                    return Ok((node_id, tail));
                }
                return Ok((EMPTY, tail));
            }

            // tail < prefix.len(): dò xem có thể đi tiếp nhánh nào không.
            let next_elem = prefix[tail];
            let children = self.storage.read().await.get_children(node_id).await?;

            for &child in &children {
                let (cp_bytes, _) = self.storage.read().await.get_node(child).await?;
                let cp = Self::to_vec(&cp_bytes);
                if !cp.is_empty() && cp[0] == next_elem {
                    node_id = child;
                    found = true;
                    break;
                }
            }
            if !found {
                let id = self.extend(node_id, &prefix[tail..], index).await?;
                self.maintain_bloom(prefix).await?;
                return Ok((id, tail));
            }
        }

        // Không có root cho shard này → tạo node gốc mới.
        if prefix.len() >= 2 {
            // Root giữ element đầu (không record), leaf giữ phần còn lại +
            // record → record-node len ≥ 2 LUÔN có link parent để gắn edge
            // (nếu tạo root nguyên key thì không có link nào vào node có record).
            let root = self
                .storage
                .write()
                .await
                .new_node(Self::from_vec(&prefix[..1]), EMPTY)
                .await?;
            let si = shard_of(prefix[0], self.sharding);
            self.storage.write().await.set_root(si, root).await?;
            let leaf = self.extend(root, &prefix[1..], index).await?;
            // Root mới chưa có shortcut cho element đầu (Search::update_shortcuts
            // chỉ phủ elements từ `tail = 1`) — bổ sung để LIKE search có
            // candidate khi pattern bắt đầu từ element đầu.
            self.storage
                .write()
                .await
                .add_shortcut_node(si, &prefix[0].encode(), root)
                .await?;
            self.maintain_bloom(prefix).await?;
            return Ok((leaf, 1));
        }
        let id = self
            .storage
            .write()
            .await
            .new_node(Self::from_vec(prefix), index)
            .await?;
        let si = shard_of(prefix[0], self.sharding);
        self.storage.write().await.set_root(si, id).await?;
        self.maintain_bloom(prefix).await?;
        Ok((id, 0))
    }

    /// Chạm vào một element trong flow: fire `on_node` callback với metadata
    /// của element → id node → lưu metadata vào node stream.
    ///
    /// Bỏ qua khi: không có callback, `elem == EMPTY`, hoặc callback trả `EMPTY`.
    #[inline]
    async fn fire_node(&self, elem: T, meta: &[u8]) -> Result<()> {
        let Some(cb) = &self.on_node else {
            return Ok(());
        };
        if elem.to_usize() == EMPTY {
            return Ok(());
        }
        let node = cb(elem, meta)?;
        if node == EMPTY {
            return Ok(());
        }
        self.storage.write().await.set_node_meta(node, meta).await?;
        Ok(())
    }

    /// Đăng ký metadata cho một element (node stream), không cần insert key.
    ///
    /// Dùng khi rebuild index: mọi node trong canonical kind được register
    /// một lần, độc lập với chain insert. Không có callback thì dùng chính
    /// `elem.to_usize()` làm id. Trả về id đã lưu (hoặc `EMPTY` nếu bỏ qua).
    #[allow(dead_code)] // API node-stream — GraphIndex mới dùng metas=None, giữ cho tương lai.
    pub async fn register_node(&self, elem: T, meta: &[u8]) -> Result<usize> {
        if elem.to_usize() == EMPTY {
            return Ok(EMPTY);
        }
        let node = match &self.on_node {
            Some(cb) => cb(elem, meta)?,
            None => elem.to_usize(),
        };
        if node == EMPTY {
            return Ok(EMPTY);
        }
        self.storage.write().await.set_node_meta(node, meta).await?;
        Ok(node)
    }

    /// Match chính xác key → record index.
    /// `begin == EMPTY` thì bắt đầu từ root của shard tương ứng element đầu;
    /// `begin != EMPTY` thì bắt đầu từ node cụ thể (đã biết trước).
    #[cfg(test)]
    pub async fn r#match(&self, begin: usize, prefix: &[T]) -> Result<usize> {
        if prefix.is_empty() {
            return Err(Error::NotFound);
        }

        let mut tail = 0;
        let mut node_id = if begin == EMPTY {
            self.storage
                .read()
                .await
                .get_root(shard_of(prefix[0], self.sharding))
                .await?
        } else {
            begin
        };

        if node_id == EMPTY {
            return Err(Error::NotFound);
        }

        while node_id != EMPTY {
            let (prefix_bytes, node_record) = self.storage.read().await.get_node(node_id).await?;
            let node_prefix = Self::to_vec(&prefix_bytes);

            // So node_prefix với query key (từ `tail`).
            let common = node_prefix
                .iter()
                .zip(prefix[tail..].iter())
                .take_while(|(a, b)| a == b)
                .count();

            // Không khớp trọn node_prefix → key không tồn tại.
            if common < node_prefix.len() {
                return Err(Error::NotFound);
            }

            tail += common;

            // Khớp hết key → trả record nếu node thực sự chứa record.
            if tail == prefix.len() {
                if node_record != EMPTY {
                    return Ok(node_record);
                }
                return Err(Error::NotFound);
            }

            // Tìm child khớp ký tự tiếp theo.
            let next_elem = prefix[tail];
            let children = self.storage.read().await.get_children(node_id).await?;

            let mut next_node_id = EMPTY;
            for &child in &children {
                let (cp_bytes, _) = self.storage.read().await.get_node(child).await?;
                let cp = Self::to_vec(&cp_bytes);
                if !cp.is_empty() && cp[0] == next_elem {
                    next_node_id = child;
                    break;
                }
            }

            node_id = next_node_id;
        }

        Err(Error::NotFound)
    }

    /// Theo dõi `key` từ root → trả `Vec<usize>` node id dọc theo đường đi
    /// (node đầu là root của shard). Chỉ dùng trong test để biết node con
    /// trên đường đi khi muốn `search_dfs` bắt đầu từ một node giữa.
    ///
    /// Ngoài test, chỉ được gọi từ `maintain_bloom` — khi feature
    /// `bloom-search` tắt hàm thành dead code, nên ghi `allow(dead_code)`.
    #[allow(dead_code)]
    async fn follow_path(&self, key: &[T]) -> Result<Vec<usize>> {
        #[cfg(feature = "bloom-search")]
        #[allow(unreachable_code)]
        return self.follow_path_with_bloom(key).await;

        #[allow(unreachable_code)]
        return self.follow_path_default(key).await;
    }

    #[allow(dead_code)]
    async fn follow_path_default(&self, key: &[T]) -> Result<Vec<usize>> {
        if key.is_empty() {
            return Ok(Vec::new());
        }

        let mut node_id = self
            .storage
            .read()
            .await
            .get_root(shard_of(key[0], self.sharding))
            .await?;
        if node_id == EMPTY {
            return Ok(Vec::new());
        }

        let mut path = vec![node_id];
        let mut pos = 0;

        loop {
            let (prefix_bytes, _) = self.storage.read().await.get_node(node_id).await?;
            let node_prefix = Self::to_vec(&prefix_bytes);
            let common = node_prefix
                .iter()
                .zip(key[pos..].iter())
                .take_while(|(a, b)| a == b)
                .count();

            pos += common;
            if pos == key.len() || common < node_prefix.len() {
                return Ok(path);
            }

            let next_elem = key[pos];
            let children = self.storage.read().await.get_children(node_id).await?;
            let mut found = false;
            for &child in &children {
                let (cp_bytes, _) = self.storage.read().await.get_node(child).await?;
                let cp = Self::to_vec(&cp_bytes);
                if !cp.is_empty() && cp[0] == next_elem {
                    node_id = child;
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok(path);
            }
            path.push(node_id);
        }
    }

    /// Tìm tất cả `(full_key, record)` có key bắt đầu bằng `prefix`.
    pub async fn search_prefix(&self, begin: usize, prefix: &[T]) -> Result<Vec<(Vec<T>, usize)>> {
        if prefix.is_empty() {
            return Ok(Vec::new());
        }

        // Node khởi đầu: `begin` hoặc root của shard.
        let mut node_id = if begin == EMPTY {
            self.storage
                .read()
                .await
                .get_root(shard_of(prefix[0], self.sharding))
                .await?
        } else {
            begin
        };

        if node_id == EMPTY {
            return Ok(Vec::new());
        }

        let mut tail = 0;
        let mut matched_path: Vec<T> = Vec::new();

        while node_id != EMPTY {
            let (prefix_bytes, _) = self.storage.read().await.get_node(node_id).await?;
            let node_prefix = Self::to_vec(&prefix_bytes);

            let remaining_prefix = &prefix[tail..];
            let common = node_prefix
                .iter()
                .zip(remaining_prefix.iter())
                .take_while(|(a, b)| a == b)
                .count();

            matched_path.extend_from_slice(&node_prefix);

            // Prefix tìm kiếm ngắn hơn node_prefix và khớp trọn đoạn đầu
            // (VD: prefix="te", node_prefix="test") → thu thập từ node này.
            if common == remaining_prefix.len() {
                let mut results = Vec::new();
                self.collect_all(node_id, matched_path, &mut results)
                    .await?;
                return Ok(results);
            }

            // Sai lệch giữa chừng → prefix không tồn tại.
            if common < node_prefix.len() {
                return Ok(Vec::new());
            }

            tail += common;

            let next_elem = prefix[tail];
            let children = self.storage.read().await.get_children(node_id).await?;

            let mut next_node_id = EMPTY;
            for &child in &children {
                let (cp_bytes, _) = self.storage.read().await.get_node(child).await?;
                let cp = Self::to_vec(&cp_bytes);
                if !cp.is_empty() && cp[0] == next_elem {
                    next_node_id = child;
                    break;
                }
            }

            node_id = next_node_id;
        }

        Ok(Vec::new())
    }

    /// Thu thập toàn bộ `(full_key, record)` trong subtree của `root`.
    ///
    /// `root_path` ĐÃ gồm prefix của `root` (search_prefix nối dần qua từng cấp),
    /// nên node nào cũng dùng thẳng path của chính nó — không append lại.
    /// Duyệt iterative bằng explicit stack (tránh async recursion).
    async fn collect_all(
        &self,
        root: usize,
        root_path: Vec<T>,
        results: &mut Vec<(Vec<T>, usize)>,
    ) -> Result<()> {
        let mut stack = vec![(root, root_path)];
        while let Some((curr_node, current_path)) = stack.pop() {
            let (_prefix_bytes, record) = self.storage.read().await.get_node(curr_node).await?;

            // Node chứa record hợp lệ → thêm vào kết quả.
            if record != EMPTY {
                results.push((current_path.clone(), record));
            }

            let children = self.storage.read().await.get_children(curr_node).await?;

            for child in children {
                let (cp_bytes, _) = self.storage.read().await.get_node(child).await?;
                let child_prefix = Self::to_vec(&cp_bytes);

                let mut next_path = current_path.clone();
                next_path.extend_from_slice(&child_prefix);

                stack.push((child, next_path));
            }
        }

        Ok(())
    }

    // ── DFS SEARCH (LIKE / substring) ──

    /// Tìm record có key **chứa** `pattern` (substring — LIKE search, không chỉ
    /// khớp từ đầu key như `search_prefix`).
    ///
    /// Dò bắt đầu từ node `begin` (thường là candidate tìm qua shortcut index
    /// của `Search`); `begin == EMPTY` thì bắt đầu từ root của shard tương ứng
    /// `pattern[0]`.
    ///
    /// Mỗi node: đọc prefix, hỏi `matcher` xem pattern khớp tới đâu; khớp hoàn
    /// toàn → thu thập toàn bộ record trong subtree (dừng); prefix hết mà còn
    /// partial match → đệ quy xuống children có element khớp element tiếp theo.
    ///
    /// Trả về record IDs của match đầu tiên theo DFS trong mỗi subtree (khớp
    /// hành vi `search_index::search_like`). Không kèm meta/key length — đó là
    /// concern của caller (`Search` lưu chúng trong Storage).
    ///
    /// **Resumable + deadline-aware**: duyệt bằng explicit work-stack (không
    /// async recursion) nên ngắt được giữa chừng khi `deadline` hết hạn. Khi
    /// ngắt: trả `(records, Some(checkpoint))` — caller gọi lại với `resume =
    /// Some(checkpoint)` để tiếp tục chính xác từ vị trí dừng; hoàn tất không
    /// timeout: `None` ở vị trí checkpoint. Node đầu tiên (theo DFS) có pattern
    /// khớp hoàn chỉnh trong prefix → collect toàn bộ records của subtree đó rồi
    /// dừng (short-circuit); prefix hết mà pattern chưa khớp hết → dò xuống
    /// children theo `continuations` matcher trả về.
    pub async fn search_dfs(
        &self,
        begin: usize,
        pattern: &[T],
        matcher: SearchMatcher<T>,
        resume: Option<DfsCheckpoint>,
        deadline: Option<std::time::Instant>,
    ) -> Result<(Vec<usize>, Option<DfsCheckpoint>)> {
        if pattern.is_empty() {
            return Err(Error::NotFound);
        }

        // Trạng thái: từ checkpoint (resume) hoặc khởi tạo từ `begin`.
        let (mut state, mut records) = if let Some(cp) = resume {
            (cp.state, cp.records)
        } else {
            let node_id = if begin == EMPTY {
                self.storage
                    .read()
                    .await
                    .get_root(shard_of(pattern[0], self.sharding))
                    .await?
            } else {
                begin
            };
            if node_id == EMPTY {
                return Ok((Vec::new(), None));
            }
            (
                Some(DfsState::Search(vec![DfsFrame {
                    node_id,
                    pattern_pos: 0,
                    cont_idx: 0,
                    child_idx: 0,
                }])),
                Vec::new(),
            )
        };

        // Mỗi vòng lặp xử lý đúng 1 bước duyệt; giữa các bước check deadline.
        // `state = None` → duyệt xong (Search không match / Collect xong).
        while let Some(cur) = state.take() {
            if let Some(dl) = deadline
                && std::time::Instant::now() >= dl
            {
                return Ok((
                    Vec::new(),
                    Some(DfsCheckpoint {
                        state: Some(cur),
                        records,
                    }),
                ));
            }

            state = match cur {
                DfsState::Search(mut stack) => {
                    // Bước tới: pop frame, đọc prefix, hỏi matcher. Found → chuyển
                    // sang Collect; ngược lại tìm child khớp element tiếp theo.
                    let mut next: Option<DfsState> = None;
                    while next.is_none() {
                        let Some(mut frame) = stack.pop() else {
                            break; // stack rỗng — không có match trong subtree này.
                        };

                        let (prefix_bytes, _record) =
                            { self.storage.read().await.get_node(frame.node_id).await? };
                        let prefix = Self::to_vec(&prefix_bytes);
                        let result = matcher(&prefix, pattern, frame.pattern_pos);

                        // Match hoàn chỉnh → collect toàn bộ subtree rồi dừng.
                        if result.found {
                            next = Some(DfsState::Collect {
                                root: frame.node_id,
                                stack: vec![(frame.node_id, 0)],
                            });
                            break;
                        }

                        let children = {
                            self.storage
                                .read()
                                .await
                                .get_children(frame.node_id)
                                .await?
                        };
                        let mut descended = false;
                        while frame.cont_idx < result.continuations.len() {
                            let pp = result.continuations[frame.cont_idx];
                            if pp == 0 || pp >= pattern.len() {
                                frame.cont_idx += 1;
                                frame.child_idx = 0;
                                continue;
                            }

                            let next_elem = pattern[pp];
                            while frame.child_idx < children.len() {
                                let child = children[frame.child_idx];
                                frame.child_idx += 1;
                                let (cp_bytes, _) =
                                    { self.storage.read().await.get_node(child).await? };
                                let cp = Self::to_vec(&cp_bytes);
                                if cp.is_empty() || cp[0] != next_elem {
                                    continue;
                                }

                                // Prune nhánh: bloom của child không chứa
                                // `pattern[pp..]` (substring) → subtree chắc chắn
                                // không có match tiếp tục, bỏ nhánh. Bloom có 0
                                // false negative nên không bao giờ bỏ nhánh có
                                // match thật. Chỉ prune khi substring đủ ngắn và
                                // child có bloom (không có → fallback traversal).
                                #[cfg(feature = "bloom-search")]
                                {
                                    let remaining_len = pattern.len() - pp;
                                    if remaining_len <= bloom_cfg::MATCH_CAP {
                                        let bloom_bytes = {
                                            self.storage.read().await.get_node_bloom(child).await?
                                        };
                                        if let Some(bloom_bytes) = bloom_bytes
                                            && let Some(bf) = BloomFilter::deserialize(&bloom_bytes)
                                            && !bf.contains(&Self::from_vec(&pattern[pp..]))
                                        {
                                            continue;
                                        }
                                    }
                                }

                                // Đi xuống child — đẩy frame hiện tại lại (với vị
                                // trí đã tiến) + frame con mới.
                                stack.push(frame);
                                stack.push(DfsFrame {
                                    node_id: child,
                                    pattern_pos: pp,
                                    cont_idx: 0,
                                    child_idx: 0,
                                });
                                descended = true;
                                break;
                            }
                            if descended {
                                break;
                            }
                            frame.cont_idx += 1;
                            frame.child_idx = 0;
                        }
                        if descended {
                            next = Some(DfsState::Search(stack));
                            break;
                        }
                        // Frame này đã dò hết continuations — pop frame tiếp theo.
                    }
                    // `None` = stack rỗng không có match → candidate xong.
                    next
                }
                DfsState::Collect { root, mut stack } => {
                    // Collect subtree theo pre-order (record của node trước, sau
                    // đó mới children — giống bản đệ quy cũ).
                    if let Some((node_id, child_idx)) = stack.pop() {
                        let (_prefix_bytes, record) =
                            { self.storage.read().await.get_node(node_id).await? };
                        if record != EMPTY {
                            records.push(record);
                        }
                        let children = { self.storage.read().await.get_children(node_id).await? };
                        if child_idx < children.len() {
                            stack.push((node_id, child_idx + 1));
                            stack.push((children[child_idx], 0));
                        }
                        Some(DfsState::Collect { root, stack })
                    } else {
                        None // Collect xong — candidate đã có records, dừng.
                    }
                }
            };
        }

        Ok((records, None))
    }

    /// Chẻ `parent` tại `breakpoint`:
    /// - `parent` giữ đoạn đầu (root_prefix)
    /// - leg mới giữ đoạn sau + toàn bộ children cũ
    /// - node mới (nếu `suffix` không rỗng) chứa phần query key còn lại
    ///
    /// Toàn bộ thao tác nằm trong 1 transaction → commit atomic.
    /// `on_split` callback chạy TRƯỚC commit (trả Err → hủy transaction).
    #[inline]
    async fn split(
        &mut self,
        parent: usize,
        breakpoint: usize,
        suffix: &[T],
        value: usize,
    ) -> Result<usize> {
        let (old_bytes, old_record) = { self.storage.read().await.get_node(parent).await? };
        let existing_children = { self.storage.read().await.get_children(parent).await? };

        let old_prefix = Self::to_vec(&old_bytes);
        let root_prefix = old_prefix[..breakpoint].to_vec();
        let leg_prefix = old_prefix[breakpoint..].to_vec();

        // suffix rỗng → key mới là prefix của key cũ: parent chính là node đích.
        let inserting_at_parent = suffix.is_empty();

        let mut tx = self.storage.read().await.new_tx();

        let new_id = if inserting_at_parent {
            parent
        } else {
            tx.new_node(Self::from_vec(suffix), value).await?
        };

        // Leg chứa các children cũ + record cũ của parent.
        let leg_id = tx.new_node(Self::from_vec(&leg_prefix), old_record).await?;

        // Migrate toàn bộ children cũ sang leg.
        for &child in &existing_children {
            tx.move_child(parent, leg_id, child).await?;
        }

        tx.add_child(parent, leg_id).await?;
        if !inserting_at_parent {
            tx.add_child(parent, new_id).await?;
        }

        tx.update_node(
            parent,
            Some(Self::from_vec(&root_prefix)),
            Some(if inserting_at_parent { value } else { EMPTY }),
        )
        .await?;

        // Callback về việc cây đã thay đổi thật sự
        if let Some(callback) = &self.on_split {
            callback(parent, leg_id, &old_prefix, breakpoint)?;
        }

        // Nếu callback báo ok thì commit luôn
        tx.commit().await?;
        Ok(new_id)
    }

    /// Thêm child mới (suffix) vào `parent` — transaction 2 ops (new_node + add_child).
    #[inline]
    async fn extend(&self, parent: usize, suffix: &[T], value: usize) -> Result<usize> {
        let mut tx = self.storage.read().await.new_tx();
        let id = tx.new_node(Self::from_vec(suffix), value).await?;
        tx.add_child(parent, id).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Follow key từ root → leaf, trả về toàn bộ node ids trên đường đi.
    /// Dùng để tìm ancestors khi cập nhật bloom filters sau insert.
    #[cfg(feature = "bloom-search")]
    async fn follow_path_with_bloom(&self, key: &[T]) -> Result<Vec<usize>> {
        if key.is_empty() {
            return Ok(Vec::new());
        }

        let mut node_id = self
            .storage
            .read()
            .await
            .get_root(shard_of(key[0], self.sharding))
            .await?;
        if node_id == EMPTY {
            return Ok(Vec::new());
        }

        let mut path = vec![node_id];
        let mut pos = 0;

        loop {
            let (prefix_bytes, _) = self.storage.read().await.get_node(node_id).await?;
            let node_prefix = Self::to_vec(&prefix_bytes);
            let common = node_prefix
                .iter()
                .zip(key[pos..].iter())
                .take_while(|(a, b)| a == b)
                .count();

            pos += common;
            if pos == key.len() || common < node_prefix.len() {
                return Ok(path);
            }

            let next_elem = key[pos];
            let children = self.storage.read().await.get_children(node_id).await?;
            let mut found = false;
            for &child in &children {
                let (cp_bytes, _) = self.storage.read().await.get_node(child).await?;
                let cp = Self::to_vec(&cp_bytes);
                if !cp.is_empty() && cp[0] == next_elem {
                    node_id = child;
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok(path);
            }
            path.push(node_id);
        }
    }

    /// Duy trì bloom filter sau mỗi mutation (insert/update record): no-op khi
    /// feature `bloom-search` tắt. Mỗi node trên path của `key` nhận mọi
    /// substring của `key` (giới hạn `MATCH_CAP`) — đây chính là điều kiện để
    /// `search_dfs` prune nhánh con không chứa `pattern[pp..]`.
    async fn maintain_bloom(&self, key: &[T]) -> Result<()> {
        #[cfg(feature = "bloom-search")]
        {
            if key.is_empty() {
                return Ok(());
            }
            let enc = Self::from_vec(key);
            let bs = T::byte_size();
            let elem_len = enc.len() / bs;
            if elem_len == 0 {
                return Ok(());
            }

            // Mọi substring aligned theo element, dài 1..=cap element.
            let cap = bloom_cfg::MATCH_CAP.min(elem_len);
            let mut subs: Vec<Vec<u8>> = Vec::new();
            for start in 0..elem_len {
                for end in (start + 1)..=(start + cap) {
                    if end > elem_len {
                        break;
                    }
                    subs.push(enc[start * bs..end * bs].to_vec());
                }
            }

            let path = self.follow_path(key).await?;
            for node_id in path {
                let mut bf = self
                    .storage
                    .read()
                    .await
                    .get_node_bloom(node_id)
                    .await?
                    .and_then(|b| BloomFilter::deserialize(&b))
                    .unwrap_or_else(|| BloomFilter::new(bloom_cfg::SIZE, bloom_cfg::K));
                for s in &subs {
                    bf.insert(s);
                }
                self.storage
                    .write()
                    .await
                    .set_node_bloom(node_id, &bf.serialize())
                    .await?;
            }
        }
        #[cfg(not(feature = "bloom-search"))]
        let _ = key;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn k(s: &str) -> Vec<u8> {
        s.bytes().collect()
    }

    /// `node_metas` toàn `None` (độ dài khớp key) — test structural insert
    /// không cần node access.
    fn no_meta(n: usize) -> Vec<Option<&'static [u8]>> {
        vec![None; n]
    }

    /// Matcher naive (test-only): substring search thuần — quét mọi vị trí của
    /// `pattern[pattern_pos..]` trong prefix, trả `found` nếu khớp trọn; nếu
    /// prefix hết mà còn partial thì push pattern_pos mới vào `continuations`
    /// (radix sẽ đệ quy xuống children theo các vị trí này).
    fn naive_matcher() -> SearchMatcher<u8> {
        Arc::new(move |prefix: &[u8], pat: &[u8], pattern_pos: usize| {
            let n = pat.len();
            if pattern_pos >= n {
                return OnMatchCallback {
                    found: false,
                    continuations: Vec::new(),
                };
            }
            let mut continuations = Vec::new();
            for start in 0..prefix.len() {
                if pat[pattern_pos] != prefix[start] {
                    continue;
                }
                let mut j = pattern_pos;
                let mut i = start;
                while j < n && i < prefix.len() && pat[j] == prefix[i] {
                    j += 1;
                    i += 1;
                }
                if j == n {
                    return OnMatchCallback {
                        found: true,
                        continuations: Vec::new(),
                    };
                }
                // Prefix hết, còn partial → có thể nối tiếp xuống children.
                if i == prefix.len() && j > pattern_pos {
                    continuations.push(j);
                }
            }
            OnMatchCallback {
                found: false,
                continuations,
            }
        })
    }

    #[tokio::test]
    async fn test_insert_and_match() {
        let mut tree = Radix::in_memory(4);
        assert!(tree.insert(&k("hello"), 1, &no_meta(5)).await.is_ok());
        assert!(tree.insert(&k("world"), 2, &no_meta(5)).await.is_ok());
        assert!(tree.insert(&k("help"), 3, &no_meta(4)).await.is_ok());

        assert_eq!(tree.r#match(EMPTY, &k("hello")).await.unwrap(), 1);
        assert_eq!(tree.r#match(EMPTY, &k("world")).await.unwrap(), 2);
        assert_eq!(tree.r#match(EMPTY, &k("help")).await.unwrap(), 3);
        assert!(tree.r#match(EMPTY, &k("notfound")).await.is_err());
    }

    #[tokio::test]
    async fn test_insert_empty_key() {
        let mut tree: Radix<u8> = Radix::in_memory(1);
        assert!(tree.insert(&[], 1, &[]).await.is_err());
    }

    #[tokio::test]
    async fn test_insert_zero_index() {
        let mut tree = Radix::in_memory(1);
        assert!(tree.insert(&k("key"), 0, &[]).await.is_err());
    }

    #[tokio::test]
    async fn test_match_empty_tree() {
        let tree = Radix::in_memory(2);
        assert!(tree.r#match(EMPTY, &k("anything")).await.is_err());
    }

    #[tokio::test]
    async fn test_insert_prefix_of_existing_key() {
        let mut tree = Radix::in_memory(4);

        tree.insert(&k("hello"), 1, &no_meta(5)).await.unwrap();
        tree.insert(&k("hel"), 2, &no_meta(3)).await.unwrap();

        assert_eq!(tree.r#match(EMPTY, &k("hel")).await.unwrap(), 2);
        assert_eq!(tree.r#match(EMPTY, &k("hello")).await.unwrap(), 1);
        assert!(tree.r#match(EMPTY, &k("help")).await.is_err());
    }

    #[tokio::test]
    async fn test_insert_nested_prefixes() {
        let mut tree = Radix::in_memory(1);

        tree.insert(&k("abc"), 3, &no_meta(3)).await.unwrap();
        tree.insert(&k("ab"), 2, &no_meta(2)).await.unwrap();
        tree.insert(&k("a"), 1, &no_meta(1)).await.unwrap();

        assert_eq!(tree.r#match(EMPTY, &k("a")).await.unwrap(), 1);
        assert_eq!(tree.r#match(EMPTY, &k("ab")).await.unwrap(), 2);
        assert_eq!(tree.r#match(EMPTY, &k("abc")).await.unwrap(), 3);

        let results = tree.search_prefix(EMPTY, &k("a")).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_duplicate_prefix_insert() {
        let mut tree = Radix::in_memory(4);

        tree.insert(&k("hello"), 1, &no_meta(5)).await.unwrap();
        let (id1, _) = tree.insert(&k("hel"), 2, &no_meta(3)).await.unwrap();
        assert_ne!(id1, 0);

        let (id2, _) = tree.insert(&k("hel"), 2, &no_meta(3)).await.unwrap();
        assert_eq!(id2, 0, "duplicate prefix insert trả về EMPTY");

        assert_eq!(tree.r#match(EMPTY, &k("hel")).await.unwrap(), 2);
        assert_eq!(tree.r#match(EMPTY, &k("hello")).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_search_prefix() {
        let mut tree = Radix::in_memory(4);
        tree.insert(&k("hello"), 1, &no_meta(5)).await.unwrap();
        tree.insert(&k("help"), 2, &no_meta(4)).await.unwrap();
        tree.insert(&k("held"), 3, &no_meta(4)).await.unwrap();
        tree.insert(&k("world"), 4, &no_meta(5)).await.unwrap();

        let results = tree.search_prefix(EMPTY, &k("he")).await.unwrap();
        assert_eq!(results.len(), 3);
        assert!(results.contains(&(k("hello"), 1)));
        assert!(results.contains(&(k("help"), 2)));
        assert!(results.contains(&(k("held"), 3)));

        let results = tree.search_prefix(EMPTY, &k("hel")).await.unwrap();
        assert_eq!(results.len(), 3);

        let results = tree.search_prefix(EMPTY, &k("hello")).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], (k("hello"), 1));

        // Không match → Ok(vec![]) (khác Err ở radixtree cũ)
        let results = tree.search_prefix(EMPTY, &k("xyz")).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_split_migrates_children() {
        let mut tree = Radix::in_memory(4);

        for i in 0..10u8 {
            let key = format!("aaaaaa{i}");
            tree.insert(&k(&key), i as usize + 1, &no_meta(key.len()))
                .await
                .unwrap();
        }
        tree.insert(&k("aaaab"), 20, &no_meta(5)).await.unwrap();

        for i in 0..10u8 {
            let key = format!("aaaaaa{i}");
            assert!(
                tree.r#match(EMPTY, &k(&key)).await.is_ok(),
                "'{key}' phải match sau split — children đã migrate sang leg"
            );
        }
        assert_eq!(tree.r#match(EMPTY, &k("aaaab")).await.unwrap(), 20);

        let results = tree.search_prefix(EMPTY, &k("aaaaaa")).await.unwrap();
        assert_eq!(results.len(), 10);
    }

    #[tokio::test]
    async fn test_on_split_callback_after_commit() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mut tree = Radix::in_memory(4);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        tree.with_split(Arc::new(move |_parent, leg_id, old_prefix, breakpoint| {
            assert_ne!(leg_id, EMPTY);
            // R1: root giữ "h", leaf "ello" — split khi insert "help" chẻ "ello"
            // tại breakpoint 2 ("el" + "lo").
            assert_eq!(old_prefix, b"ello".to_vec());
            assert_eq!(breakpoint, 2);
            calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));

        tree.insert(&k("hello"), 1, &no_meta(5)).await.unwrap();
        tree.insert(&k("help"), 2, &no_meta(4)).await.unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "callback chạy đúng 1 lần (sau split commit)"
        );
    }

    #[tokio::test]
    async fn test_follow_path() {
        let mut tree = Radix::in_memory(4);
        tree.insert(&k("hello"), 1, &no_meta(5)).await.unwrap();
        tree.insert(&k("helloworld"), 2, &no_meta(10))
            .await
            .unwrap();

        let path = tree.follow_path(&k("helloworld")).await.unwrap();
        assert!(!path.is_empty(), "path không rỗng");
    }

    #[tokio::test]
    async fn test_search_dfs_substring() {
        let mut tree = Radix::in_memory(4);
        tree.insert(&k("hello"), 1, &no_meta(5)).await.unwrap();
        tree.insert(&k("help"), 2, &no_meta(4)).await.unwrap();
        tree.insert(&k("held"), 3, &no_meta(4)).await.unwrap();

        // R1: root chỉ giữ element đầu ("h"), phần còn lại nằm ở depth sâu
        // ("hello" = "h" + "el" + "lo") — substring "llo" phải bắt đầu từ
        // candidate node chứa element 'l' (production lấy qua shortcut index;
        // ở đây dùng follow_path để mô phỏng).
        let path = tree.follow_path(&k("hello")).await.unwrap();
        let (hits, _) = tree
            .search_dfs(path[1], &k("llo"), naive_matcher(), None, None)
            .await
            .unwrap();
        assert_eq!(hits, vec![1]);

        // Prefix khớp từ root → collect toàn bộ records trong subtree.
        let (hits, _) = tree
            .search_dfs(EMPTY, &k("hel"), naive_matcher(), None, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 3);
        assert!(hits.contains(&1));
        assert!(hits.contains(&2));
        assert!(hits.contains(&3));
    }

    #[tokio::test]
    async fn test_search_dfs_from_node() {
        let mut tree = Radix::in_memory(4);
        tree.insert(&k("hello"), 1, &no_meta(5)).await.unwrap();
        tree.insert(&k("help"), 2, &no_meta(4)).await.unwrap();

        // begin = node "el" (parent sau split) — match 'l' ở cuối prefix rồi
        // nối tiếp xuống child "lo".
        let path = tree.follow_path(&k("hello")).await.unwrap();
        let parent = path[1];
        let (hits, _) = tree
            .search_dfs(parent, &k("llo"), naive_matcher(), None, None)
            .await
            .unwrap();
        assert_eq!(hits, vec![1]);
    }

    #[tokio::test]
    async fn test_search_dfs_not_found() {
        let mut tree = Radix::in_memory(4);
        tree.insert(&k("hello"), 1, &no_meta(5)).await.unwrap();

        // Pattern rỗng → Err.
        assert!(
            tree.search_dfs(EMPTY, &[], naive_matcher(), None, None)
                .await
                .is_err()
        );
        // Pattern không tồn tại → Ok(vec![]).
        let (hits, _) = tree
            .search_dfs(EMPTY, &k("xyz"), naive_matcher(), None, None)
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    // ── Node access stream (OnNodeAccessCallback) ──

    /// Các lần on_node được ghi nhận: (elem, metadata).
    type NodeCalls = Vec<(u8, Vec<u8>)>;

    /// Callback node test: ghi nhận (elem, meta) + trả elem as usize (identity —
    /// chain model: element id chính là node stream key).
    fn node_cb(calls: Arc<Mutex<NodeCalls>>) -> OnNodeAccessCallback<u8> {
        Arc::new(move |elem, meta| {
            calls.lock().unwrap().push((elem, meta.to_vec()));
            Ok(elem as usize)
        })
    }

    #[tokio::test]
    async fn test_node_fired_per_element_with_meta() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut tree = Radix::in_memory(4);
        tree.with_node_access(node_cb(calls.clone()));

        // Mỗi element có meta → fire on_node, độc lập với kết quả structural.
        // "ab" + "ac" cùng root 'a' → 'a' fire 2 lần (access callback được phép
        // gọi lại, phải trả cùng id).
        tree.insert(&k("ab"), 1, &[Some(b"ma"), Some(b"mb")])
            .await
            .unwrap();
        tree.insert(&k("ac"), 2, &[Some(b"ma"), None])
            .await
            .unwrap();
        tree.insert(&k("d"), 3, &[Some(b"md")]).await.unwrap();

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[
                (b'a', b"ma".to_vec()),
                (b'b', b"mb".to_vec()),
                (b'a', b"ma".to_vec()),
                (b'd', b"md".to_vec())
            ],
            "fire đúng mỗi element có meta (None = marker → skip)"
        );

        // Metadata lưu vào node stream, keyed theo id callback trả về (= elem).
        let storage = tree.storage.read().await;
        assert_eq!(
            storage
                .get_node_meta(b'a' as usize)
                .await
                .unwrap()
                .as_deref(),
            Some(b"ma".as_slice())
        );
        assert_eq!(
            storage
                .get_node_meta(b'b' as usize)
                .await
                .unwrap()
                .as_deref(),
            Some(b"mb".as_slice())
        );
        assert_eq!(storage.get_node_meta(b'c' as usize).await.unwrap(), None);
        assert_eq!(
            storage
                .get_node_meta(b'd' as usize)
                .await
                .unwrap()
                .as_deref(),
            Some(b"md".as_slice())
        );
        drop(storage);
    }

    #[tokio::test]
    async fn test_node_skipped_for_empty_element() {
        // elem.to_usize() == EMPTY → không fire (0 không phải node hợp lệ).
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut tree = Radix::in_memory(4);
        tree.with_node_access(node_cb(calls.clone()));

        tree.insert(&[0u8, 1], 1, &[Some(b"m0"), Some(b"m1")])
            .await
            .unwrap();

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[(1u8, b"m1".to_vec())],
            "element 0 (EMPTY) bị skip"
        );
    }

    #[tokio::test]
    async fn test_node_not_fired_without_callback() {
        // Không đăng ký callback → insert có metas vẫn ok, không lưu node stream.
        let mut tree = Radix::in_memory(4);
        tree.insert(&k("ab"), 1, &[Some(b"ma"), Some(b"mb")])
            .await
            .unwrap();
        let storage = tree.storage.read().await;
        assert_eq!(storage.get_node_meta(b'a' as usize).await.unwrap(), None);
        drop(storage);
    }

    #[tokio::test]
    async fn test_register_node_writes_meta_and_returns_id() {
        let tree = Radix::in_memory(4);
        // Không có callback → dùng elem làm id.
        let id = tree.register_node(b'x', b"mx").await.unwrap();
        assert_eq!(id, b'x' as usize);
        let storage = tree.storage.read().await;
        assert_eq!(
            storage
                .get_node_meta(b'x' as usize)
                .await
                .unwrap()
                .as_deref(),
            Some(b"mx".as_slice())
        );
        drop(storage);

        // Ghi đè (last-wins) — cùng id.
        tree.register_node(b'x', b"mx2").await.unwrap();
        let storage = tree.storage.read().await;
        assert_eq!(
            storage
                .get_node_meta(b'x' as usize)
                .await
                .unwrap()
                .as_deref(),
            Some(b"mx2".as_slice())
        );
        drop(storage);
    }

    #[tokio::test]
    async fn test_register_node_skips_empty() {
        let tree = Radix::in_memory(4);
        assert_eq!(tree.register_node(0, b"m0").await.unwrap(), EMPTY);
        let storage = tree.storage.read().await;
        assert_eq!(storage.get_node_meta(0).await.unwrap(), None);
        drop(storage);
    }
}
