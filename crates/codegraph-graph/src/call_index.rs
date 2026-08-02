//! CallIndex — chỉ mục call-graph trên SearchIndex (PoC/benchmark).
//!
//! ## Ý tưởng
//!
//! Mỗi symbol/function là một `u64` id. Edge A→B được biểu diễn bằng key trong
//! SearchIndex:
//!
//! - **Edge mode** — key `[A, B]` (2 phần tử). `callees`/`callers` đa-hop duyệt
//!   lặp theo depth bằng `search_prefix`, mirror BFS của `codegraph-graph` nhưng
//!   thay vì query SQLite `edges_from`/`edges_to` thì dùng radix-tree lookup.
//! - **Path mode** — mỗi path `[A, B, C, …]` (≤ `limit` hop, cycle-broken) là một
//!   key. `callees`/`callers` với `depth ≤ limit` = **1 prefix lookup** + filter
//!   độ dài key (không cần duyệt lặp). `depth > limit` trả về lỗi.
//!
//! Luôn duy trì 2 index đối xứng: `forward` (chiều xuôi) + `reverse` (chiều ngược,
//! cho `callers`). Meta call-site gắn với record của edge (Edge mode) — record idx
//! là ID edge tự nhiên để enrich (xem `docs/BENCH.md` phần review radixtree).
//!
//! Module này **độc lập, không nối vào pipeline** codegraph-graph/resolve — chỉ là
//! PoC để benchmark trước khi quyết định có refactor hay không.

use std::collections::{HashMap, HashSet};

use crate::search_index::{SearchError, SearchIndex};

#[cfg(feature = "sqlite")]
use crate::search_index::SqliteStorage;
#[cfg(feature = "sqlite")]
use std::path::PathBuf;
#[cfg(feature = "sqlite")]
use std::path::PathBuf;

/// Giới hạn cứng số node trả về (khớp `codegraph-graph::HARD_LIMIT`).
pub const DEFAULT_HARD_LIMIT: usize = 5000;

/// Hình dạng key lưu trong index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyShape {
    /// Key 2 phần tử `[A, B]` — mỗi edge là 1 entry.
    Edge,
    /// Mỗi path (≤ limit hop) là 1 key — đa-hop = 1 prefix lookup.
    Path { limit: usize },
}

/// Lỗi của `CallIndex`.
#[derive(Debug)]
pub enum CallError {
    /// Lỗi tầng SearchIndex/Storage.
    Search(SearchError),
    /// Lỗi backend (vd: mở SQLite file).
    Backend(String),
    /// `depth` vượt quá path limit (chỉ Path mode).
    DepthExceedsLimit { depth: usize, limit: usize },
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Search(e) => write!(f, "search error: {e}"),
            CallError::Backend(m) => write!(f, "backend error: {m}"),
            CallError::DepthExceedsLimit { depth, limit } => {
                write!(f, "depth {depth} exceeds path limit {limit}")
            }
        }
    }
}

impl std::error::Error for CallError {}

impl From<SearchError> for CallError {
    fn from(e: SearchError) -> Self {
        CallError::Search(e)
    }
}

pub type Result<T> = std::result::Result<T, CallError>;

/// Chỉ định index xuôi hay ngược khi dựng backend.
#[derive(Debug, Clone, Copy)]
enum Which {
    Forward,
    Reverse,
}

/// Cấu hình backend — đủ để `clear`/`rebuild` tạo lại storage mới.
///
/// Lưu ý: forward/reverse **không dùng chung một SQLite file** — hai index chia
/// sẻ `rt_nodes`/`rt_roots` sẽ ghi đè root lẫn nhau và hỏng khi reload.
#[derive(Clone)]
enum Backend {
    /// In-memory (test, không persist).
    Mem,
    /// SQLite file. Forward: `path`, Reverse: `path` + `.rev`.
    #[cfg(feature = "sqlite")]
    File { fwd: PathBuf, rev: PathBuf },
}

impl Backend {
    /// Dựng SearchIndex mới với storage mới từ backend config.
    ///
    /// `wipe == true` (clear/rebuild): xoá toàn bộ dữ liệu cũ trước khi dùng.
    /// `wipe == false` (open mới): giữ dữ liệu có sẵn — dùng `reload()` để phục hồi.
    fn build(&self, which: Which, sharding: usize, wipe: bool) -> Result<SearchIndex<u64>> {
        match self {
            Backend::Mem => {
                let _ = (which, wipe);
                Ok(SearchIndex::in_memory(sharding))
            }
            #[cfg(feature = "sqlite")]
            Backend::File { fwd, rev } => {
                let path = match which {
                    Which::Forward => fwd,
                    Which::Reverse => rev,
                };
                let path_str = path.to_string_lossy().into_owned();
                let mut storage = SqliteStorage::open(&path_str)
                    .map_err(|e| CallError::Backend(e.to_string()))?;
                if wipe {
                    storage
                        .clear()
                        .map_err(|e| CallError::Backend(e.to_string()))?;
                }
                Ok(SearchIndex::in_storage(sharding, storage))
            }
        }
    }
}

/// Call-graph index trên SearchIndex.
///
/// `shape = Edge`: mỗi edge `[A, B]` là 1 key. `shape = Path{limit}`: mỗi path
/// (simple, ≤ limit hop) là 1 key.
pub struct CallIndex {
    shape: KeyShape,
    sharding: usize,
    hard_limit: usize,
    backend: Backend,
    /// Key theo `shape` — chiều xuôi (edge/path forward).
    forward: SearchIndex<u64>,
    /// Chiều ngược (cho `callers`).
    reverse: SearchIndex<u64>,
    /// Tên symbol (best-effort hiển thị; không quan trọng cho traversal).
    names: HashMap<u64, String>,
}

impl CallIndex {
    /// Index in-memory (dùng cho test hiệu chỉnh — đúng trước khi đo).
    pub fn in_memory(shape: KeyShape) -> Self {
        Self::new(shape, Backend::Mem, 64).expect("in-memory backend is infallible")
    }

    /// Index in-memory với sharding tuỳ chỉnh.
    pub fn in_memory_sharded(shape: KeyShape, sharding: usize) -> Self {
        Self::new(shape, Backend::Mem, sharding).expect("in-memory backend is infallible")
    }

    /// Index trên SQLite file (chỉ khi feature `sqlite`).
    ///
    /// Mở file có sẵn (giữ dữ liệu) — gọi `reload()` để phục hồi sau restart.
    /// File mới: dùng `rebuild()` hoặc `insert_edge` để build.
    #[cfg(feature = "sqlite")]
    pub fn open(shape: KeyShape, path: &str) -> Result<Self> {
        Self::open_sharded(shape, path, 64)
    }

    /// `open` với sharding tuỳ chỉnh.
    #[cfg(feature = "sqlite")]
    pub fn open_sharded(shape: KeyShape, path: &str, sharding: usize) -> Result<Self> {
        let backend = Backend::File {
            fwd: PathBuf::from(path),
            rev: PathBuf::from(format!("{path}.rev")),
        };
        Self::new(shape, backend, sharding)
    }

    fn new(shape: KeyShape, backend: Backend, sharding: usize) -> Result<Self> {
        // Path limit 0 là vô nghĩa (không có path nào) — clamp lên 1.
        let shape = match shape {
            KeyShape::Path { limit: 0 } => KeyShape::Path { limit: 1 },
            other => other,
        };
        let forward = backend.build(Which::Forward, sharding, false)?;
        let reverse = backend.build(Which::Reverse, sharding, false)?;
        Ok(Self {
            shape,
            sharding,
            hard_limit: DEFAULT_HARD_LIMIT,
            backend,
            forward,
            reverse,
            names: HashMap::new(),
        })
    }

    // ── Config ──

    pub fn shape(&self) -> KeyShape {
        self.shape
    }

    /// Đặt giới hạn cứng số node trả về (mặc định 5000 — khớp codegraph).
    pub fn set_hard_limit(&mut self, limit: usize) {
        self.hard_limit = limit;
    }

    /// Đặt tên hiển thị cho một symbol (cosmetic — không ảnh hưởng traversal).
    pub fn set_name(&mut self, id: u64, name: &str) {
        self.names.insert(id, name.to_string());
    }

    fn name_of(&self, id: u64) -> String {
        self.names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("n{id}"))
    }

    // ── Build / lifecycle ──

    /// Xoá toàn bộ dữ liệu và dựng lại index rỗng (cùng backend).
    pub async fn clear(&mut self) -> Result<()> {
        self.forward = self.backend.build(Which::Forward, self.sharding, true)?;
        self.reverse = self.backend.build(Which::Reverse, self.sharding, true)?;
        self.names.clear();
        Ok(())
    }

    /// Reload toàn bộ state từ storage (crash recovery / restart).
    pub async fn reload(&mut self) -> Result<()> {
        self.forward.reload().await?;
        self.reverse.reload().await?;
        Ok(())
    }

    /// Rebuild toàn bộ index từ danh sách edge `(from, to, meta)`.
    ///
    /// - Edge mode: clear + insert từng edge.
    /// - Path mode: clear + sinh toàn bộ simple path (≤ limit hop) theo batch DFS.
    ///
    /// Toàn bộ insert được bọc trong `begin_bulk`/`end_bulk` (transaction) — cắt
    /// chi phí autocommit per-write. SAVEPOINT bên trong (commit_split, counter)
    /// vẫn an toàn khi lồng nhau. Luôn COMMIT kể cả khi loop lỗi giữa chừng
    /// (dữ liệu partial vẫn nhất quán ở mức từng insert).
    pub async fn rebuild<I>(&mut self, edges: I) -> Result<usize>
    where
        I: IntoIterator<Item = (u64, u64, Vec<u8>)>,
    {
        let edges: Vec<(u64, u64, Vec<u8>)> = edges.into_iter().collect();
        self.clear().await?;

        self.forward.begin_bulk().await?;
        self.reverse.begin_bulk().await?;

        let result = async {
            let mut n = 0usize;
            match self.shape {
                KeyShape::Edge => {
                    for (from, to, meta) in &edges {
                        self.insert_edge(*from, *to, meta).await?;
                        n += 1;
                    }
                }
                KeyShape::Path { limit } => {
                    // Adjacency (deterministic thứ tự) cho path generation.
                    let mut adj: HashMap<u64, Vec<u64>> = HashMap::new();
                    for (from, to, _) in &edges {
                        adj.entry(*from).or_default().push(*to);
                    }
                    for v in adj.values_mut() {
                        v.sort_unstable();
                        v.dedup();
                    }

                    let mut sources: Vec<u64> = adj.keys().copied().collect();
                    sources.sort_unstable();

                    for source in sources {
                        let mut path = vec![source];
                        let mut visited = HashSet::new();
                        visited.insert(source);
                        let mut paths = Vec::new();
                        Self::collect_paths(
                            &adj,
                            source,
                            limit,
                            &mut path,
                            &mut visited,
                            &mut paths,
                        );
                        for p in &paths {
                            self.insert_path_key(p).await?;
                            n += 1;
                        }
                    }
                }
            }
            Ok(n)
        }
        .await;

        // Luôn commit (ignore lỗi end_bulk nếu loop đã lỗi).
        let _ = async {
            self.forward.end_bulk().await?;
            self.reverse.end_bulk().await
        }
        .await;

        result
    }

    /// DFS (backtracking) sinh toàn bộ simple path bắt đầu từ `cur` có độ dài
    /// 2..=limit+1 phần tử (= 1..=limit hop). `visited` theo backtracking để
    /// không tạo path lặp đỉnh (cycle-broken).
    fn collect_paths(
        adj: &HashMap<u64, Vec<u64>>,
        cur: u64,
        limit: usize,
        path: &mut Vec<u64>,
        visited: &mut HashSet<u64>,
        out: &mut Vec<Vec<u64>>,
    ) {
        let children = match adj.get(&cur) {
            Some(c) => c.as_slice(),
            None => return,
        };
        for &nxt in children {
            // Self-loop (edge cur→cur): path 1-hop hợp lệ, không mở rộng tiếp
            // (mọi path chứa cur lặp lại đều không phải simple path).
            if nxt == cur {
                out.push(vec![cur, nxt]);
                continue;
            }
            if visited.contains(&nxt) {
                continue;
            }
            path.push(nxt);
            visited.insert(nxt);
            if path.len() >= 2 {
                out.push(path.clone());
            }
            if path.len() <= limit {
                Self::collect_paths(adj, nxt, limit, path, visited, out);
            }
            path.pop();
            visited.remove(&nxt);
        }
    }

    /// Thêm edge `from → to` (kèm meta call-site). Idempotent với edge trùng.
    ///
    /// - Edge mode: insert trực tiếp key `[from, to]` (+ reverse).
    /// - Path mode: insert key 1-hop + mở rộng incremental các path đang có đi
    ///   qua `from`/`to` (cycle-broken, ≤ limit) — index luôn chứa đủ mọi path.
    pub async fn insert_edge(&mut self, from: u64, to: u64, meta: &[u8]) -> Result<()> {
        debug_assert!(from < i32::MAX as u64 && to < i32::MAX as u64);
        match self.shape {
            KeyShape::Edge => {
                self.forward
                    .insert(&[from, to], to as i32, &self.name_of(to), Some(meta))
                    .await?;
                self.reverse
                    .insert(&[to, from], from as i32, &self.name_of(from), Some(meta))
                    .await?;
            }
            KeyShape::Path { limit } => {
                self.insert_path_key(&[from, to]).await?;
                self.extend_paths_through(from, to, limit).await?;
            }
        }
        Ok(())
    }

    /// Insert một path key vào cả forward + reverse. Entry = đỉnh cuối (cho
    /// forward) / đỉnh đầu (cho reverse) — dùng để hiển thị tên.
    async fn insert_path_key(&mut self, path: &[u64]) -> Result<()> {
        let last = *path.last().unwrap();
        self.forward
            .insert(path, last as i32, &self.name_of(last), None)
            .await?;
        let mut rev = path.to_vec();
        rev.reverse();
        let first = *rev.last().unwrap();
        self.reverse
            .insert(&rev, first as i32, &self.name_of(first), None)
            .await?;
        Ok(())
    }

    /// Mở rộng incremental qua edge mới `(from, to)`: mọi path mới chứa edge này
    /// đều có dạng `P + [to] + Q`, trong đó:
    /// - `P` = path đang có kết thúc tại `from` (hoặc rỗng → path bắt đầu ở `from`)
    /// - `Q` = path đang có bắt đầu tại `to` (hoặc rỗng → path kết thúc ở `to`)
    ///
    /// Nested loop qua (P, Q) để sinh đủ mọi path mới, kiểm tra cycle + limit.
    async fn extend_paths_through(&mut self, from: u64, to: u64, limit: usize) -> Result<()> {
        // Paths kết thúc tại `from` = reversed paths trong `reverse` bắt đầu ở `from`.
        let tails = self.prefix_keys(&self.reverse, &[from]).await?;
        // Paths bắt đầu tại `to` = forward keys bắt đầu ở `to`.
        let heads = self.prefix_keys(&self.forward, &[to]).await?;

        // Prefix candidates: [rỗng] + mỗi tail (reverse → path gốc kết thúc ở from).
        let mut prefixes: Vec<Vec<u64>> = vec![Vec::new()];
        for tail in &tails {
            let mut p = tail.clone();
            p.reverse();
            prefixes.push(p);
        }

        // Suffix candidates: [rỗng] + mỗi head (path bắt đầu ở to).
        let mut suffixes: Vec<Vec<u64>> = vec![Vec::new()];
        suffixes.extend(heads);

        for p in &prefixes {
            for q in &suffixes {
                // Base = path kết thúc tại `from` (rỗng → chỉ có `from`).
                let mut combined: Vec<u64> = if p.is_empty() { vec![from] } else { p.clone() };
                combined.push(to);
                // `q` bắt đầu tại `to` (head = forward key với prefix `[to]`), mà
                // `to` đã được push ở trên → bỏ phần tử đầu của q để khỏi lặp.
                combined.extend_from_slice(q.get(1..).unwrap_or(&[]));

                if combined.len() > limit + 1 {
                    continue;
                }
                // Cycle-broken: mọi đỉnh trong path phải khác nhau.
                let distinct: HashSet<u64> = combined.iter().copied().collect();
                if distinct.len() != combined.len() {
                    continue;
                }
                self.insert_path_key(&combined).await?;
            }
        }
        Ok(())
    }

    // ── Queries ──

    /// Có edge `from → to` hay không.
    pub async fn has_edge(&self, from: u64, to: u64) -> Result<bool> {
        Ok(!self
            .prefix_keys(&self.forward, &[from, to])
            .await?
            .is_empty())
    }

    /// Danh sách callee trực tiếp (1 hop) — dedup, sorted.
    ///
    /// Không gồm `from` (self-loop bị loại — khớp semantics BFS của codegraph).
    pub async fn direct_callees(&self, from: u64) -> Result<Vec<u64>> {
        let mut out: Vec<u64> = Vec::new();
        for key in self.prefix_keys(&self.forward, &[from]).await? {
            if key.len() == 2 && key[1] != from {
                out.push(key[1]);
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// Danh sách caller trực tiếp (1 hop) — dedup, sorted.
    pub async fn direct_callers(&self, to: u64) -> Result<Vec<u64>> {
        let mut out: Vec<u64> = Vec::new();
        for key in self.prefix_keys(&self.reverse, &[to]).await? {
            if key.len() == 2 && key[1] != to {
                out.push(key[1]);
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// Tất cả callee trong `depth` hop (dedup, không gồm `from`).
    ///
    /// - Edge mode: BFS lặp theo depth (mirror `codegraph-graph::traverse`).
    /// - Path mode: **1 prefix lookup** (filter độ dài key), `depth ≤ limit`.
    pub async fn callees(&self, from: u64, depth: usize) -> Result<Vec<u64>> {
        match self.shape {
            KeyShape::Edge => self.callees_bfs(from, depth).await,
            KeyShape::Path { limit } => {
                if depth > limit {
                    return Err(CallError::DepthExceedsLimit { depth, limit });
                }
                self.callees_path(from, depth).await
            }
        }
    }

    /// Tất cả caller trong `depth` hop (dedup, không gồm `to`).
    pub async fn callers(&self, to: u64, depth: usize) -> Result<Vec<u64>> {
        match self.shape {
            KeyShape::Edge => self.callers_bfs(to, depth).await,
            KeyShape::Path { limit } => {
                if depth > limit {
                    return Err(CallError::DepthExceedsLimit { depth, limit });
                }
                self.callers_path(to, depth).await
            }
        }
    }

    /// BFS lặp theo depth bằng `search_prefix` trên edge key (chiều xuôi).
    async fn callees_bfs(&self, from: u64, depth: usize) -> Result<Vec<u64>> {
        let mut visited: HashSet<u64> = HashSet::new();
        visited.insert(from);
        let mut frontier: Vec<u64> = vec![from];
        let mut out: Vec<u64> = Vec::new();

        for _ in 0..depth {
            if visited.len() > self.hard_limit {
                break;
            }
            let mut next: Vec<u64> = Vec::new();
            for &cur in &frontier {
                for key in self.prefix_keys(&self.forward, &[cur]).await? {
                    if key.len() != 2 {
                        continue;
                    }
                    let node = key[1];
                    if visited.insert(node) {
                        out.push(node);
                        next.push(node);
                        if out.len() >= self.hard_limit {
                            return Ok(out);
                        }
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    /// BFS lặp trên reverse index (chiều ngược).
    async fn callers_bfs(&self, to: u64, depth: usize) -> Result<Vec<u64>> {
        let mut visited: HashSet<u64> = HashSet::new();
        visited.insert(to);
        let mut frontier: Vec<u64> = vec![to];
        let mut out: Vec<u64> = Vec::new();

        for _ in 0..depth {
            if visited.len() > self.hard_limit {
                break;
            }
            let mut next: Vec<u64> = Vec::new();
            for &cur in &frontier {
                for key in self.prefix_keys(&self.reverse, &[cur]).await? {
                    if key.len() != 2 {
                        continue;
                    }
                    let node = key[1];
                    if visited.insert(node) {
                        out.push(node);
                        next.push(node);
                        if out.len() >= self.hard_limit {
                            return Ok(out);
                        }
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    /// Path mode — 1 prefix lookup trên path index; filter `2 ≤ len ≤ depth+1`.
    async fn callees_path(&self, from: u64, depth: usize) -> Result<Vec<u64>> {
        let mut seen: HashSet<u64> = HashSet::new();
        let mut out: Vec<u64> = Vec::new();
        for key in self.prefix_keys(&self.forward, &[from]).await? {
            if key.len() >= 2 && key.len() <= depth + 1 {
                let node = key[key.len() - 1];
                // Loại `from` (self-loop/cycle về đích) — khớp BFS visited.
                if node != from && seen.insert(node) {
                    out.push(node);
                    if out.len() >= self.hard_limit {
                        break;
                    }
                }
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Path mode — 1 prefix lookup trên reverse index; filter độ dài key.
    async fn callers_path(&self, to: u64, depth: usize) -> Result<Vec<u64>> {
        let mut seen: HashSet<u64> = HashSet::new();
        let mut out: Vec<u64> = Vec::new();
        for key in self.prefix_keys(&self.reverse, &[to]).await? {
            if key.len() >= 2 && key.len() <= depth + 1 {
                // Reverse key [to, ..., start] → node gốc = key cuối (start của path).
                let node = key[key.len() - 1];
                if node != to && seen.insert(node) {
                    out.push(node);
                    if out.len() >= self.hard_limit {
                        break;
                    }
                }
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// `search_prefix` nhưng trả về `[]` khi không có key (NotFound).
    ///
    /// Dùng variant raw (không load entry_id/name/meta) — traversal chỉ cần key
    /// để tái dựng chain, record idx (1-indexed) là ID edge ổn định. `search_prefix_full`
    /// (có meta) chỉ dùng khi caller thực sự cần enrich.
    async fn prefix_keys(&self, idx: &SearchIndex<u64>, prefix: &[u64]) -> Result<Vec<Vec<u64>>> {
        match idx.search_prefix(prefix).await {
            Ok(hits) => Ok(hits.into_iter().map(|(key, _)| key).collect()),
            Err(SearchError::NotFound) => Ok(Vec::new()),
            Err(e) => Err(CallError::Search(e)),
        }
    }
}

// ==================== Tests ====================
//
// Correctness: so sánh CallIndex (cả Edge + Path mode) với BFS tham chiếu trên
// đồ thị nhỏ — **đúng trước khi đo** (Phase 3 của PoC plan).

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet, VecDeque};

    fn to_edges(list: &[(u64, u64)]) -> Vec<(u64, u64, Vec<u8>)> {
        list.iter().map(|&(f, t)| (f, t, Vec::new())).collect()
    }

    /// BFS tham chiếu — mirror `codegraph-graph::Traversal::traverse`:
    /// visited bắt đầu với `start`, BFS theo depth, kết quả = node phát hiện ở
    /// depth 1..=max_depth, dedup (node chỉ vào queue 1 lần).
    fn bfs_ref(adj: &HashMap<u64, Vec<u64>>, start: u64, depth: usize, reverse: bool) -> Vec<u64> {
        let mut visited: HashSet<u64> = HashSet::new();
        visited.insert(start);
        let mut queue: VecDeque<(u64, u32)> = VecDeque::new();
        queue.push_back((start, 0));
        let mut out = Vec::new();

        while let Some((cur, d)) = queue.pop_front() {
            if d >= depth as u32 {
                continue;
            }
            let neighbors: Vec<u64> = if reverse {
                // đảo adjacency: to → các from
                adj.iter()
                    .filter_map(|(f, ts)| if ts.contains(&cur) { Some(*f) } else { None })
                    .collect()
            } else {
                adj.get(&cur).cloned().unwrap_or_default()
            };
            for nxt in neighbors {
                if visited.insert(nxt) {
                    out.push(nxt);
                    queue.push_back((nxt, d + 1));
                }
            }
        }
        out
    }

    fn adjacency(edges: &[(u64, u64)]) -> HashMap<u64, Vec<u64>> {
        let mut adj: HashMap<u64, Vec<u64>> = HashMap::new();
        for &(f, t) in edges {
            adj.entry(f).or_default().push(t);
        }
        adj
    }

    /// So sánh CallIndex (1 shape) với BFS tham chiếu trên một graph.
    async fn check_shape(shape: KeyShape, edges: &[(u64, u64)], all_nodes: &[u64]) {
        let mut idx = CallIndex::in_memory(shape);
        idx.rebuild(to_edges(edges)).await.unwrap();

        let adj = adjacency(edges);
        let max_depth = match shape {
            KeyShape::Edge => 3,
            KeyShape::Path { limit } => limit.min(3),
        };

        // has_edge
        for &(f, t) in edges {
            assert!(idx.has_edge(f, t).await.unwrap(), "edge {f}->{t} missing");
        }
        assert!(!idx.has_edge(999, 998).await.unwrap());

        // direct_callees / direct_callers
        for &n in all_nodes {
            let mut ref_out = bfs_ref(&adj, n, 1, false);
            ref_out.sort_unstable();
            let got = idx.direct_callees(n).await.unwrap();
            assert_eq!(got, ref_out, "direct_callees({n}) shape={shape:?}");

            let mut ref_in = bfs_ref(&adj, n, 1, true);
            ref_in.sort_unstable();
            let got_in = idx.direct_callers(n).await.unwrap();
            assert_eq!(got_in, ref_in, "direct_callers({n}) shape={shape:?}");
        }

        // callees / callers theo depth
        for &n in all_nodes {
            for d in 1..=max_depth {
                let mut ref_out = bfs_ref(&adj, n, d, false);
                ref_out.sort_unstable();
                let mut got = idx.callees(n, d).await.unwrap();
                got.sort_unstable();
                assert_eq!(got, ref_out, "callees({n}, {d}) shape={shape:?}");

                let mut ref_in = bfs_ref(&adj, n, d, true);
                ref_in.sort_unstable();
                let mut got_in = idx.callers(n, d).await.unwrap();
                got_in.sort_unstable();
                assert_eq!(got_in, ref_in, "callers({n}, {d}) shape={shape:?}");
            }
        }
    }

    /// Edge mode vs Path mode cho ra cùng kết quả trên depth ≤ limit.
    async fn check_modes_agree(edges: &[(u64, u64)], all_nodes: &[u64], limit: usize) {
        let mut edge_idx = CallIndex::in_memory(KeyShape::Edge);
        edge_idx.rebuild(to_edges(edges)).await.unwrap();
        let mut path_idx = CallIndex::in_memory(KeyShape::Path { limit });
        path_idx.rebuild(to_edges(edges)).await.unwrap();

        for &n in all_nodes {
            for d in 1..=limit.min(3) {
                let mut a = edge_idx.callees(n, d).await.unwrap();
                a.sort_unstable();
                let mut b = path_idx.callees(n, d).await.unwrap();
                b.sort_unstable();
                assert_eq!(a, b, "callees({n},{d}) edge vs path disagree");

                let mut c = edge_idx.callers(n, d).await.unwrap();
                c.sort_unstable();
                let mut d_ = path_idx.callers(n, d).await.unwrap();
                d_.sort_unstable();
                assert_eq!(c, d_, "callers({n},{d}) edge vs path disagree");
            }
        }
    }

    // ── Các đồ thị nhỏ ──

    const CHAIN: &[(u64, u64)] = &[(0, 1), (1, 2), (2, 3), (3, 4)];
    const STAR: &[(u64, u64)] = &[(0, 1), (0, 2), (0, 3), (0, 4), (4, 5)];
    const LAYERED: &[(u64, u64)] = &[(0, 2), (0, 3), (1, 2), (1, 3), (2, 4), (3, 4), (4, 5)];
    const CYCLE: &[(u64, u64)] = &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4)];
    const SELF_LOOP: &[(u64, u64)] = &[(0, 0), (0, 1), (1, 2)];

    #[tokio::test]
    async fn edge_mode_matches_bfs() {
        for (edges, nodes) in [
            (CHAIN, &[0u64, 1, 2, 3, 4][..]),
            (STAR, &[0u64, 1, 2, 3, 4, 5][..]),
            (LAYERED, &[0u64, 1, 2, 3, 4, 5][..]),
            (CYCLE, &[0u64, 1, 2, 3, 4][..]),
            (SELF_LOOP, &[0u64, 1, 2][..]),
        ] {
            check_shape(KeyShape::Edge, edges, nodes).await;
        }
    }

    #[tokio::test]
    async fn path_mode_matches_bfs() {
        for (edges, nodes) in [
            (CHAIN, &[0u64, 1, 2, 3, 4][..]),
            (STAR, &[0u64, 1, 2, 3, 4, 5][..]),
            (LAYERED, &[0u64, 1, 2, 3, 4, 5][..]),
            (CYCLE, &[0u64, 1, 2, 3, 4][..]),
            (SELF_LOOP, &[0u64, 1, 2][..]),
        ] {
            check_shape(KeyShape::Path { limit: 3 }, edges, nodes).await;
        }
    }

    #[tokio::test]
    async fn edge_and_path_modes_agree() {
        for (edges, nodes) in [
            (CHAIN, &[0u64, 1, 2, 3, 4][..]),
            (STAR, &[0u64, 1, 2, 3, 4, 5][..]),
            (LAYERED, &[0u64, 1, 2, 3, 4, 5][..]),
            (CYCLE, &[0u64, 1, 2, 3, 4][..]),
            (SELF_LOOP, &[0u64, 1, 2][..]),
        ] {
            check_modes_agree(edges, nodes, 3).await;
        }
    }

    #[tokio::test]
    async fn path_mode_incremental_insert_matches_rebuild() {
        // Insert edge từng cái một (incremental) == rebuild batch.
        let mut inc = CallIndex::in_memory(KeyShape::Path { limit: 3 });
        for &(f, t) in CYCLE {
            inc.insert_edge(f, t, b"").await.unwrap();
        }

        let mut batch = CallIndex::in_memory(KeyShape::Path { limit: 3 });
        batch.rebuild(to_edges(CYCLE)).await.unwrap();

        for &n in &[0u64, 1, 2, 3, 4] {
            for d in 1..=3 {
                let mut a = inc.callees(n, d).await.unwrap();
                a.sort_unstable();
                let mut b = batch.callees(n, d).await.unwrap();
                b.sort_unstable();
                assert_eq!(a, b, "incremental != batch callees({n},{d})");
            }
        }
    }

    #[tokio::test]
    async fn path_mode_depth_beyond_limit_errors() {
        let mut idx = CallIndex::in_memory(KeyShape::Path { limit: 2 });
        idx.rebuild(to_edges(CHAIN)).await.unwrap();
        assert!(idx.callees(0, 3).await.is_err());
        assert!(idx.callers(4, 3).await.is_err());
    }

    #[tokio::test]
    async fn insert_duplicate_edge_idempotent() {
        let mut idx = CallIndex::in_memory(KeyShape::Edge);
        idx.insert_edge(1, 2, b"meta-a").await.unwrap();
        idx.insert_edge(1, 2, b"meta-b").await.unwrap();
        assert!(idx.has_edge(1, 2).await.unwrap());
        assert_eq!(idx.direct_callees(1).await.unwrap(), vec![2]);
        assert_eq!(idx.direct_callers(2).await.unwrap(), vec![1]);
    }

    #[tokio::test]
    async fn edge_meta_roundtrip() {
        let mut idx = CallIndex::in_memory(KeyShape::Edge);
        idx.insert_edge(1, 2, b"file.rs:42:13").await.unwrap();
        // search_prefix_full trên forward index trả về meta của record edge.
        let hits = idx.forward.search_prefix_full(&[1, 2]).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].3.as_deref(), Some(b"file.rs:42:13".as_slice()));
    }

    #[tokio::test]
    async fn empty_graph_queries() {
        let mut idx = CallIndex::in_memory(KeyShape::Edge);
        idx.rebuild(std::iter::empty::<(u64, u64, Vec<u8>)>())
            .await
            .unwrap();
        assert_eq!(idx.direct_callees(1).await.unwrap(), Vec::<u64>::new());
        assert_eq!(idx.callees(1, 2).await.unwrap(), Vec::<u64>::new());
        assert!(!idx.has_edge(1, 2).await.unwrap());
    }

    #[tokio::test]
    async fn hard_limit_caps_results() {
        // Star lớn: callees(0, 1) vượt hard_limit nhỏ → bị cắt.
        let mut idx = CallIndex::in_memory(KeyShape::Edge);
        idx.set_hard_limit(3);
        let mut edges = Vec::new();
        for i in 1..20u64 {
            edges.push((0, i));
        }
        idx.rebuild(to_edges(&edges)).await.unwrap();
        let out = idx.callees(0, 1).await.unwrap();
        assert_eq!(out.len(), 3);
    }

    // ── SQLite backend (feature-gated) ──

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_backend_matches_bfs() {
        // Mỗi test dùng DB riêng để tránh đụng file giữa các lần chạy.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("call_index_test_{}.db", std::process::id()));
        let path_str = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_file(format!("{path_str}.rev"));

        let mut idx = CallIndex::open(KeyShape::Edge, &path_str).unwrap();
        idx.rebuild(to_edges(LAYERED)).await.unwrap();
        for &n in &[0u64, 1, 2, 3, 4, 5] {
            for d in 1..=3 {
                let adj = adjacency(LAYERED);
                let mut ref_out = bfs_ref(&adj, n, d, false);
                ref_out.sort_unstable();
                let mut got = idx.callees(n, d).await.unwrap();
                got.sort_unstable();
                assert_eq!(got, ref_out, "sqlite callees({n},{d})");
            }
        }

        // Reload từ file rồi query lại — phục hồi phải ra kết quả như cũ.
        let mut idx2 = CallIndex::open(KeyShape::Edge, &path_str).unwrap();
        idx2.reload().await.unwrap();
        let mut got = idx2.callees(0, 2).await.unwrap();
        got.sort_unstable();
        let adj = adjacency(LAYERED);
        let mut ref_out = bfs_ref(&adj, 0, 2, false);
        ref_out.sort_unstable();
        assert_eq!(got, ref_out, "sqlite reload callees(0,2)");

        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_file(format!("{path_str}.rev"));
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_path_mode_matches_bfs() {
        let path =
            std::env::temp_dir().join(format!("call_index_path_test_{}.db", std::process::id()));
        let path_str = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_file(format!("{path_str}.rev"));

        let mut idx = CallIndex::open(KeyShape::Path { limit: 3 }, &path_str).unwrap();
        idx.rebuild(to_edges(CYCLE)).await.unwrap();
        for &n in &[0u64, 1, 2, 3, 4] {
            for d in 1..=3 {
                let adj = adjacency(CYCLE);
                let mut ref_out = bfs_ref(&adj, n, d, false);
                ref_out.sort_unstable();
                let mut got = idx.callees(n, d).await.unwrap();
                got.sort_unstable();
                assert_eq!(got, ref_out, "sqlite path callees({n},{d})");
            }
        }

        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_file(format!("{path_str}.rev"));
    }
}
