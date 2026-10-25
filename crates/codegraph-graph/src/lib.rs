//! Graph index theo kiến trúc semgraph: SymbolRegistry + chain engine + name engine.
//!
//! Mọi symbol có một **id global** (registry monotonic, bắt đầu từ `SYMBOL_BASE`,
//! persist `next_id`); call chain của một hàm là chuỗi `u64` gồm **marker**
//! (luồng điều khiển, id `< SYMBOL_BASE`) và **symbol id** của callee:
//!
//! ```text
//! chain(F) = [F, m1, calleeA, m2, calleeB, ...]        // calleeA bị điều kiện m1
//! ```
//!
//! Chain bắt đầu bằng chính func id (vị trí 0). Edge `(caller, callee)` suy từ
//! chain: mỗi symbol element là một callee.
//!
//! ## Hai engine trên 1 file
//!
//! - **Chain engine** `Search<u64>`: key = chain (u64 element), record = func id.
//!   `callers(F)` = substring search `&[F]` (KMP trên shortcuts); `callees(F)` =
//!   đọc chain, skip marker/self/0. Persistent — dùng chung storage với entity
//!   store (`rt_*` radix tables + `sg_*` entity tables trong cùng sqlite).
//! - **Name engine** `Search<u8>`: key = tên symbol (lowercase bytes), record =
//!   synthetic id (1-based vào `name_records`). Persist ở dataset riêng
//!   (`names.lmdb`/`names.sqlite` cạnh DB chính) + version stamp — mở lại index
//!   chỉ tốn `load_name_records`; backend không có dataset file riêng
//!   (postgres/mysql/memory) fallback in-memory và rebuild từ `name_index`.
//!
//! `name_index: HashMap<lowercase name, Vec<id>>` là nguồn mở rộng symbol trùng
//! tên: radix chỉ lưu mỗi tên khác nhau 1 lần (insert_chain trả `Duplicated` với
//! key trùng), search trả về record → tên → toàn bộ id trùng tên.
//!
//! ## Pipeline ingest (2 phase, như semgraph)
//!
//! `ingest(parse_results)` = full re-index: clear entity + engines → register
//! toàn bộ symbol (id global) + remap (idMap bỏ `0` — placeholder phải giữ 0) →
//! `resolve_calls` (thay placeholder 0 trong chain bằng id thật: structural hint
//! → exact name → short name → best-candidate: @Override +10 / has-chain +5 /
//! same-file +3) → `build_call_indexes` (đếm edge = chain[position],
//! CallSite + var-type alias, gom SaveCallRecords) → files → rebuild engines →
//! bump version.

use crate::embeddings::{EmbeddingBackend, default_backend, embedding_enabled, make_backend};
pub use crate::radix::Element;
/// Error type của `Search::insert_chain` (ví dụ `Duplicated`) — re-export để
/// caller xử lý lỗi key trùng mà không cần `mod search` public.
pub use crate::search::Error as SearchError;
pub use crate::search::Search;
pub use crate::search::SearchResume;
use crate::storage::cached::CachedStorage;
#[cfg(feature = "lmdb")]
pub use crate::storage::lmdb::LmdbStorage;
#[cfg(feature = "mysql")]
pub use crate::storage::mysql::MySqlStorage;
#[cfg(feature = "postgres")]
pub use crate::storage::postgres::PostgresStorage;
#[cfg(feature = "sqlite")]
pub use crate::storage::sqlite::SqliteStorage;
pub use crate::storage::{InMemoryStorage, IndexCounts, Storage, StorageError, Tx};
// Sub-traits of `Storage` — callers that need only one facet (e.g. a chain-engine
// read path) can name it directly instead of taking the full umbrella.
#[cfg(feature = "bloom-search")]
pub use crate::storage::BloomStorage;
pub use crate::storage::{
    CategoryStorage, ChainStorage, EdgeDataStorage, EntityStorage, NodeMetaStorage,
    ShortcutsStorage,
};
use crate::vector_index::VectorIndex;
use codegraph_core::{
    BranchLabel, CallRecord, CallSite, CallSiteResult, ClassInfo, DependenciesReport, Dependency,
    Error, FileInfo, FlowCall, FlowResult, FlowStep, FullFlowPath, FunctionScope,
    MARKER_BRANCH_END, MARKER_BREAK, MARKER_CALL_ENTER, MARKER_CALL_EXIT, MARKER_CATCH_START,
    MARKER_CONTINUE, MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK, MARKER_RETURN,
    MARKER_SWITCH_CASE, MARKER_SWITCH_CLOSE, MARKER_SWITCH_END, MARKER_SWITCH_START, MARKER_THROW,
    MARKER_TRY_END, MARKER_TRY_START, MemberInfo, PathEnding, ResolutionKind, ResolveResult,
    SYMBOL_BASE, SearchFlowResult, SemgraphStats, StorageRoute, Symbol, SymbolKind, SymbolMatch,
    TraceAction, is_marker, marker_name,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

#[cfg(feature = "bloom-search")]
mod bloom;
pub mod diff;
pub mod embeddings;
mod lru;
pub mod meminfo;
pub mod memtrack;
mod radix;
mod search;
mod shared;
mod storage;
pub mod vector_index;

pub use shared::SharedGraphIndex;

/// Báo tiến độ cho `GraphIndex::ingest_with_progress`.
///
/// Graph crate không phụ thuộc indicatif — caller (CLI orchestrator / MCP) dựng
/// một impl translate các event này sang `ProgressBar` của nó. `total = 0` (chỉ
/// `phase`, không advance) nghĩa là phase không biết trước số đơn vị.
pub trait IngestProgress: Send + Sync {
    /// Bắt đầu một phase mới — `total` là số đơn vị sẽ `advance` (0 = không biết).
    fn phase(&self, name: &'static str, total: usize);
    /// Tiến thêm `n` đơn vị trong phase hiện tại.
    fn advance(&self, n: usize);
}

/// Số shard mặc định cho chain engine (`element % sharding`).
const CHAIN_SHARDING: usize = 64;

/// Kiểu `Result` của crate — alias từ `codegraph_core`.
pub type Result<T> = codegraph_core::Result<T>;

/// Map `StorageError` → `Error::Search`.
fn serr(e: crate::storage::StorageError) -> Error {
    Error::Search(e.to_string())
}

/// Lỗi khi DSN chỉ rõ scheme nhưng feature tương ứng không được bật.
#[allow(dead_code)] // fallback dispatch + lmdb/sqlite branch dùng khi feature tắt
fn backend_unavailable(name: &str) -> Error {
    Error::Db(format!(
        "Backend '{name}' được yêu cầu qua DSN nhưng feature '{name}' không được bật \
         trong bản build này"
    ))
}

/// Capacity mỗi LRU cache trong `CachedStorage` cho keyspace datasets
/// (docs/binary). Node meta ~vài trăm bytes/entry → 4096 entry ≈ vài MB.
/// Build không bật backend nào → các nhánh dùng nó bị cfg out.
#[cfg_attr(
    not(any(feature = "sqlite", feature = "lmdb", feature = "redis")),
    allow(dead_code)
)]
const DEFAULT_CACHE_CAPACITY: usize = 4096;

/// Map `search::Error` → `Error::Search`.
fn serr_search(e: crate::search::Error) -> Error {
    Error::Search(e.to_string())
}

/// Mở storage handle cho document graph từ DSN — dataset **riêng**, không share
/// instance với `GraphIndex` (doc tries dùng namespace shard/record riêng nên
/// phải là dataset riêng, và `DocumentGraph` cần `Arc<TokioRwLock<dyn Storage>>`).
///
/// - `sqlite://<path>` → `SqliteStorage` (feature `sqlite`)
/// - `lmdb://<path>`   → `LmdbStorage` (feature `lmdb`)
/// - `redis://...`     → `RedisStorage` với keyspace prefix `codegraph:docs`
///   (feature `redis`) — tách khỏi index `codegraph:idx:<db>`
pub async fn open_doc_storage(dsn: &str) -> Result<Arc<RwLock<dyn Storage>>> {
    open_keyspace_storage(dsn, "codegraph:docs").await
}

/// DSN dataset thứ hai (name engine) suy ra từ DSN chính — cùng backend, file
/// cạnh bên nhau (`db.lmdb` → `names.lmdb`). Mirror pattern của docs
/// (`db.lmdb` → `docs.lmdb`) nên không phải viết adapter namespace.
///
/// `None` = backend không có dataset file riêng (postgres/mysql/memory) —
/// giống `doc_storage_dsn`, name engine fallback in-memory.
#[cfg(any(feature = "sqlite", feature = "lmdb", feature = "redis"))]
fn name_dsn(main_dsn: &str) -> Option<String> {
    if let Some(path) = main_dsn.strip_prefix("sqlite://") {
        return Some(format!("sqlite://{}", sibling_file(path, "names.sqlite")));
    }
    if let Some(path) = main_dsn.strip_prefix("lmdb://") {
        return Some(format!("lmdb://{}", sibling_file(path, "names.lmdb")));
    }
    // Redis không tách file — cùng DSN, khác namespace ố do keyspace.
    if main_dsn.starts_with("redis://") || main_dsn.starts_with("rediss://") {
        return Some(main_dsn.to_string());
    }
    None
}

/// Thay tên file cuối của `path` (giữ nguyên thư mục cha) — `names.lmdb`
/// đặt cạnh `db.lmdb` dù `a/b/db.lmdb` → `a/b/names.lmdb`. Xử lý **cả** dấu
/// phân cách `/` (Unix/DSN) lẫn `\` (Windows) để path `<drive>:\...\db.sqlite`
/// giữ nguyên thư mục cha thay vì rơi về cwd.
#[cfg(any(feature = "sqlite", feature = "lmdb", feature = "redis"))]
fn sibling_file(path: &str, file: &str) -> String {
    let sep = match (path.rfind('/'), path.rfind('\\')) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    match sep {
        Some(i) => format!("{}{}", &path[..=i], file),
        None => file.to_string(),
    }
}

/// Mở dataset riêng cho name engine. `None` = không mừ đưọc (fallback
/// in-memory trong `new_with_storage`) — không chến nhạp và náy.
#[cfg(any(feature = "sqlite", feature = "lmdb", feature = "redis"))]
async fn open_name_store(main_dsn: &str) -> Option<Arc<RwLock<dyn crate::storage::Storage>>> {
    let dsn = name_dsn(main_dsn)?;
    match open_keyspace_storage(&dsn, "codegraph:names").await {
        Ok(store) => Some(store),
        Err(e) => {
            eprintln!("[codegraph] name engine dataset unavailable ({e}) — fallback in-memory");
            None
        }
    }
}

/// Mở storage cho một dataset theo DSN + keyspace (backend redis dùng prefix
/// này để tách dữ liệu; sqlite/lmdb tách bằng file riêng nên bỏ qua keyspace).
/// Dùng chung cho docs (`codegraph:docs`) và binary graph (`codegraph:binary`).
pub async fn open_keyspace_storage(dsn: &str, keyspace: &str) -> Result<Arc<RwLock<dyn Storage>>> {
    if let Some(path) = dsn.strip_prefix("sqlite://") {
        #[cfg(feature = "sqlite")]
        {
            let storage = crate::storage::sqlite::SqliteStorage::open(path)
                .await
                .map_err(serr)?;
            // LRU cache phía trước backend — per-node meta reads (doc graph
            // lazy hydrate) và hot tries đỡ round-trip sqlite.
            return Ok(crate::storage::cached::CachedStorage::wrap(
                Box::new(storage),
                DEFAULT_CACHE_CAPACITY,
            ));
        }
        #[cfg(not(feature = "sqlite"))]
        {
            let _ = path;
            return Err(backend_unavailable("sqlite"));
        }
    }
    if let Some(path) = dsn.strip_prefix("lmdb://") {
        #[cfg(feature = "lmdb")]
        {
            let storage = crate::storage::lmdb::LmdbStorage::open(path)
                .await
                .map_err(serr)?;
            return Ok(crate::storage::cached::CachedStorage::wrap(
                Box::new(storage),
                DEFAULT_CACHE_CAPACITY,
            ));
        }
        #[cfg(not(feature = "lmdb"))]
        {
            let _ = path;
            return Err(backend_unavailable("lmdb"));
        }
    }
    if dsn.starts_with("redis://") || dsn.starts_with("rediss://") {
        #[cfg(feature = "redis")]
        {
            let client =
                redis::Client::open(dsn).map_err(|e| Error::Db(format!("redis client: {e}")))?;
            let storage = crate::storage::redis::RedisStorage::new(client, keyspace)
                .await
                .map_err(serr)?;
            return Ok(crate::storage::cached::CachedStorage::wrap(
                Box::new(storage),
                DEFAULT_CACHE_CAPACITY,
            ));
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = keyspace;
            return Err(backend_unavailable("redis"));
        }
    }
    Err(Error::Db(format!(
        "open_keyspace_storage: DSN scheme không hỗ trợ: {dsn}"
    )))
}

/// Kết quả parse một file — input của `GraphIndex::ingest` (full re-index).
///
/// Mọi id trong `symbols`/`chains`/`calls` là **local per-file** (bắt đầu từ
/// `SYMBOL_BASE`, unique trong file); `ingest` remap sang id global (registry
/// monotonic) giống `processFileResult` của semgraph. `0` là placeholder — được
/// giữ nguyên (idMap bỏ `0`), `resolve_calls` thay bằng id thật sau.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseResult {
    pub path: String,
    pub language: String,
    pub bytes: u64,
    pub lines: u32,
    /// Symbols của file (id local ≥ `SYMBOL_BASE`).
    pub symbols: Vec<Symbol>,
    /// Chain của từng func (key = func id local; chain bắt đầu bằng chính func
    /// id ở vị trí 0, giống semgraph `[funcID, call1, Loop, ...]`).
    pub chains: HashMap<u64, Vec<u64>>,
    /// Call records (caller_id local; `position` trỏ placeholder `0` trong chain).
    pub calls: Vec<CallRecord>,
    /// Gán function pointer (`p = f;` / `void (*p)(int) = f;`) — nguyên liệu
    /// resolve indirect call trong C/C++ (`PointerAssign`).
    #[serde(default)]
    pub fn_ptr_assigns: Vec<FnPtrAssign>,

    /// Package + import của file (`None` nếu ngôn ngữ chưa implement).
    #[serde(default)]
    pub file_deps: Option<FileDeps>,
}

/// Gán function pointer: biến `var` nhận hàm `target` tại `line`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FnPtrAssign {
    pub var: String,
    pub target: String,
    pub line: u32,
}

/// Package + imports của một file — nguyên liệu resolve qualified call qua
/// import alias (Go `httpx.Get`, Java/C# `import`...). Generic, không khóa ngôn ngữ.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileDeps {
    /// Tên package của file (Go `package orders`).
    #[serde(default)]
    pub package: Option<String>,
    /// Import alias → path (`httpx "net/http"` → alias "httpx", path "net/http";
    /// import không alias: alias = segment cuối của path).
    #[serde(default)]
    pub imports: Vec<ImportAlias>,
}

/// Một entry import: `alias` là tên dùng trong code, `path` là module/package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportAlias {
    pub alias: String,
    pub path: String,
}

/// Một call record của `ParseResult` **mượn** (không clone) + `caller_id` đã
/// remap sang id global.
///
/// `ingest` chỉ cần sửa đúng một field của `CallRecord` là `caller_id` (mọi
/// field khác giữ nguyên), nên clone toàn bộ `CallRecord` (176 B + heap) vào
/// `all_calls` là phí vô ích — struct này giữ 16 B thay vì 176 B, trỏ về
/// `ParseResult` của caller (vốn sống suốt `ingest`).
struct CallRef<'a> {
    rec: &'a CallRecord,
    /// `caller_id` sau remap (giữ `0` nếu local id không có trong `id_map`).
    caller: u64,
}

/// Kết quả resolve một call placeholder.
enum CallResolution {
    /// Không resolve được (behavior cũ: giữ placeholder + raw name).
    None,
    /// Resolve tĩnh — ghi đè chain slot bằng id thật.
    Single(u64, ResolutionKind),
    /// Dynamic dispatch — fan-out candidates, chain giữ placeholder.
    FanOut(Vec<u64>, ResolutionKind),
}

// ==================== GraphIndex ====================

/// Index chính (semgraph-style): registry + 2 engine + inverted indexes.
///
/// `storage` giữ cả entity store (`sg_*` / InMemory maps) lẫn radix của chain
/// engine (`rt_*`); `chains` (Search) đọc ghi qua chính `storage` đó. Name
/// engine `names` chạy trên dataset **riêng** (`name_store`) — persist khi
/// backend có dataset file riêng, fallback in-memory nếu không.
pub struct GraphIndex {
    /// Entity + chain engine storage.
    storage: Arc<RwLock<dyn crate::storage::Storage>>,
    /// Chain engine: key = chain, record = func id.
    chains: Search<u64>,
    /// Name engine: key = tên lowercase (bytes), record = 1-based vào
    /// `name_records`.
    names: Search<u8>,
    /// Dataset riêng của name engine (`names.lmdb`/`names.sqlite` cạnh
    /// `db.lmdb`) — `None` = in-memory (không persist, luôn rebuild).
    /// Giữ handle để đọc/ghi version stamp, bỏ qua việc dựng lại trie.
    name_store: Option<Arc<RwLock<dyn crate::storage::Storage>>>,
    /// Tên distinct (lowercase) **đã sort**; `name_records[record - 1]` là tên
    /// của engine record `record` (name engine insert theo đúng thứ tự sort
    /// này). Cũng dùng trực tiếp cho scan Prefix/Suffix/Exact — không cần sort
    /// lại (resume chỉ lưu vị trí, không lưu mảng).
    name_records: Vec<String>,
    /// symbol id → Symbol (registry — nguồn chân lý in-memory).
    symbols: HashMap<u64, Symbol>,
    /// tên (lowercase) → symbol ids (mở rộng trùng tên khi search/resolve).
    name_index: HashMap<String, Vec<u64>>,
    /// scope id → symbol ids (scope query).
    scope_index: HashMap<u64, Vec<u64>>,
    /// func id → chain (nguồn chân lý; engine + storage phái sinh).
    chains_map: HashMap<u64, Vec<u64>>,
    /// call name (lowercase, kèm alias type-qualified) → call sites.
    call_names: HashMap<String, Vec<CallSite>>,
    /// Trait/interface name (lowercase) → impl method ids — fan-out dynamic
    /// dispatch. Build từ `Symbol.impl_trait` khi ingest; in-memory (blob call
    /// record đã persist candidates nên reopen không cần index này).
    impl_index: HashMap<String, Vec<u64>>,
    /// Function-pointer variable (lowercase) → các target function đã gán
    /// (`p = f;` qua assign-site tracking). In-memory như `impl_index`.
    fn_ptr_index: HashMap<String, Vec<u64>>,
    /// File path → import aliases (Go `httpx "net/http"`): nguyên liệu resolve
    /// qualified call qua import alias. In-memory, build khi ingest.
    file_imports: HashMap<String, Vec<ImportAlias>>,
    /// Go implicit interface: interface name (lower) → method ids của các type
    /// THỎA interface (method-set matching: interface method names ⊆ type
    /// method names). Build khi ingest từ receiver type trong `Symbol.type_name`.
    go_impls: HashMap<String, Vec<u64>>,
    /// Số edge `(caller, callee)` — đếm từ chains lúc ingest/rebuild.
    /// Metadata của edge (condition/effect/...) KHÔNG materialize thành map:
    /// đã nằm trong call records persist, `flow()` đọc thẳng từ đó.
    edge_count: u64,
    /// Files trong graph.
    files: Vec<FileInfo>,
    /// next_id của registry.
    next_id: u64,
    /// index version (bump mỗi lần ingest — SharedGraphIndex dò stale).
    version: u64,
    /// Embedding backend cho semantic search (KNN/k-means). Chỉ được khởi tạo
    /// thực sự khi `[embedding].backend = "fastembed"` (config opt-in); nếu
    /// embedding tắt (`embedding_enabled = false`) đây là `HashingEmbeddings`
    /// placeholder và KHÔNG bao giờ được gọi để sinh vector.
    embedding_backend: Arc<dyn EmbeddingBackend>,
    /// `true` khi semantic search được bật (config `[embedding].backend = "fastembed"`).
    /// Khi `false`, vector index rỗng và semantic search báo lỗi rõ ràng.
    embedding_enabled: bool,
    /// Vector index: symbol id → embedding. Build từ embeddings **persist trong
    /// storage** (load khi open; compute+save cho symbol thiếu khi ingest).
    vector_index: VectorIndex,
}

// ── Search resumable (deadline-aware, checkpointable) ──

/// Phase của một search resumable. Cursor nằm server-side (session store ở
/// tầng API) — LLM chỉ nhìn thấy một session id; state không serialize ra
/// ngoài mà chỉ được tái lập từ (query, mode, kind, phase).
#[derive(Debug, Clone)]
pub enum SearchCursorPhase {
    /// Mode `Contains`: đang giữa lúc chạy name engine (resumable). `names`
    /// engine trả records không theo thứ tự tên — phase A xong phải sort
    /// trước khi chuyển sang `Expand`.
    Engine(SearchResume),
    /// Mode `Prefix`/`Suffix`/`Exact`: đang quét `name_records` (đã sort) từ
    /// `name_pos` (mảng đã sort sẵn — resume chỉ cần lưu vị trí).
    ScanNames { name_pos: usize },
    /// Phase A xong: stream ids theo từng tên đã sort — filter `kind`, gom
    /// vào `collected`. `name_index[name]` luôn tăng dần theo id nên
    /// collected đã sort theo (name, id) — không cần sort cuối.
    Expand {
        names: Vec<String>,
        name_idx: usize,
        id_idx: usize,
        collected: Vec<u64>,
    },
    /// Search đã hoàn tất: `collected` + `total` giữ để phân trang tiếp mà
    /// không quét lại. Không chứa query — `SearchCursor.query` lo phần đó.
    Paged { collected: Vec<u64>, total: usize },
    /// Mode `Semantic`/`Hybrid`: candidate ids đã sort theo relevance (giảm
    /// dần) — Phase B lọc `kind` + gom vào `collected` giống `Expand` nhưng
    /// duyệt trực tiếp list id (không qua name engine).
    Candidates {
        ids: Vec<u64>,
        idx: usize,
        collected: Vec<u64>,
    },
}

/// Server-side cursor cho search resumable — validate theo (query, mode,
/// kind); `limit`/`offset` KHÔNG nằm trong cursor (page có thể đổi giữa
/// chừng khi retry).
#[derive(Debug, Clone)]
pub struct SearchCursor {
    /// Query lowercase (khớp với cursor — thay đổi → resume không hợp lệ).
    pub query: String,
    pub mode: SymbolMatch,
    pub kind: Option<SymbolKind>,
    pub phase: SearchCursorPhase,
}

/// BFS checkpoint; valid only for the query and snapshot that created it.
#[derive(Debug, Clone)]
pub struct CallersCursor {
    pub id: u64,
    pub depth: usize,
    pub index_version: u64,
    level: usize,
    frontier: Vec<u64>,
    frontier_pos: usize,
    next: Vec<u64>,
    visited: HashSet<u64>,
    out_ids: Vec<u64>,
    search: Option<SearchResume>,
    pending: Vec<usize>,
    pending_pos: usize,
    search_complete: bool,
    materialize_pos: usize,
    callers: Vec<Symbol>,
}

#[derive(Debug)]
pub struct CallersOutcome {
    /// Complete results only; partial work stays in the cursor.
    pub callers: Vec<Symbol>,
    pub cursor: Option<CallersCursor>,
    pub progress: usize,
}

/// Kết quả của [`GraphIndex::search_symbol_paged_resumable`].
#[derive(Debug)]
pub struct PagedSearchOutcome {
    /// Trang kết quả (chỉ đầy khi search hoàn tất; rỗng khi timed out).
    pub page: Vec<Symbol>,
    /// Tổng số khớp — chỉ chính xác khi search hoàn tất.
    pub total: usize,
    /// Deadline hết hạn giữa chừng → `cursor` là phase dở, phải retry.
    pub timed_out: bool,
    /// Tiến độ đo được lúc ngắt (names khớp khi đang phase A, symbols gom
    /// được khi đang phase B) — dùng cho message báo LLM.
    pub progress: usize,
    /// Cursor tiếp tục: `Some(phase dở)` khi timed_out; `Some(Paged)` khi
    /// hoàn tất nhưng còn page sau; `None` khi xong và hết page.
    pub cursor: Option<SearchCursor>,
}

/// Phân trang cho search symbol: `limit` chặn số symbol mỗi trang
/// (`0` = không giới hạn), `offset` bỏ qua `offset` symbol đầu.
#[derive(Debug, Clone, Copy)]
pub struct Pagination {
    pub limit: usize,
    pub offset: usize,
}

/// Text đầu vào cho embedding: gộp tên + signature + doc + annotations — capture
/// cả ý nghĩa lẫn loại của symbol để semantic search hữu dụng.
fn embedding_text(sym: &Symbol) -> String {
    let mut parts: Vec<&str> = Vec::new();
    parts.push(&sym.name);
    if let Some(sig) = &sym.signature {
        parts.push(sig);
    }
    if let Some(doc) = &sym.doc {
        parts.push(doc);
    }
    if !sym.annotations.is_empty() {
        parts.extend(sym.annotations.iter().map(|a| a.name.as_str()));
    }
    parts.join(" ")
}

/// Reciprocal Rank Fusion: gộp nhiều list (mỗi list đã sort theo relevance giảm
/// dần) thành một list fused. Điểm mỗi id = Σ 1/(k + rank). `k = 60` chuẩn.
/// Dùng cho `Hybrid` (lexical + vector).
fn rrf_fuse(lists: &[Vec<u64>]) -> Vec<u64> {
    const K: f32 = 60.0;
    let mut scores: HashMap<u64, f32> = HashMap::new();
    for list in lists {
        for (rank, &id) in list.iter().enumerate() {
            *scores.entry(id).or_default() += 1.0 / (K + (rank as f32) + 1.0);
        }
    }
    let mut out: Vec<(u64, f32)> = scores.into_iter().collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out.into_iter().map(|(id, _)| id).collect()
}

// ==================== Full-flow interpreter ====================
//
// Máy ảo call/return trên flow chain: mỗi path-state giữ một frame stack
// thật (descend vào chain của callee có chain), return pop frame về caller,
// throw unwind theo catch_stack tới handler khớp exception type. Khám phá
// path bằng worklist (không đệ quy clone) — code sau if/else luôn được trace.

/// Số frame tối đa khi descend vào callee (root frame + callee frames).
/// Chỉ descend 1 mức: root (1) + callee internals (2); sâu hơn = leaf.
const FLOW_DESCEND_MAX_FRAMES: usize = 2;
/// Chỉ descend vào callee có chain <= ngưỡng phần tử này (gọn, đọc nổi).
const MAX_DESCEND_CHAIN: usize = 50;

/// Tổng step budget cho mọi path-state của một trace (chống nổ tổ hợp).
const FLOW_TOTAL_STEPS: usize = 12000;
/// Số step tối đa của MỘT path — vượt thì kết thúc path bằng DepthLimit
/// (vẫn được trả về, nhưng rank sau path hoàn chỉnh) để output luôn đọc nổi.
const MAX_PATH_STEPS: usize = 800;

/// Vị trí matching BRANCH_END của một khối mở bằng IF_TRUE/CATCH_START
/// (đếm cân bằng cả hai loại open — try/catch lồng trong if và ngược lại).
fn matching_branch_end(chain: &[u64], open: usize) -> Option<usize> {
    let mut bal = 1i32;
    for (i, &e) in chain.iter().enumerate().skip(open + 1) {
        if e == MARKER_IF_TRUE || e == MARKER_CATCH_START {
            bal += 1;
        } else if e == MARKER_BRANCH_END {
            bal -= 1;
            if bal == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// IF_FALSE cùng cấp trong vùng [if_pos, end_pos) (nếu có else/elif).
fn find_if_false(chain: &[u64], if_pos: usize, end_pos: usize) -> Option<usize> {
    let mut bal = 0i32;
    for (i, &e) in chain
        .iter()
        .enumerate()
        .take(end_pos.min(chain.len()))
        .skip(if_pos + 1)
    {
        if e == MARKER_IF_TRUE {
            bal += 1;
        } else if e == MARKER_BRANCH_END {
            bal -= 1;
        } else if e == MARKER_IF_FALSE && bal == 0 {
            return Some(i);
        }
    }
    None
}

/// LOOP_BACK khớp với LOOP tại loop_pos (đếm LOOP lồng).
fn matching_loop_back(chain: &[u64], loop_pos: usize) -> Option<usize> {
    let mut bal = 1i32;
    for (i, &e) in chain.iter().enumerate().skip(loop_pos + 1) {
        if e == MARKER_LOOP {
            bal += 1;
        } else if e == MARKER_LOOP_BACK {
            bal -= 1;
            if bal == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// TRY_END khớp TRY_START (đếm TRY lồng).
fn matching_try_end(chain: &[u64], try_start: usize) -> Option<usize> {
    let mut bal = 1i32;
    for (i, &e) in chain.iter().enumerate().skip(try_start + 1) {
        if e == MARKER_TRY_START {
            bal += 1;
        } else if e == MARKER_TRY_END {
            bal -= 1;
            if bal == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Các vị trí CATCH_START của handler ngay sau TRY_END (các khối
/// CATCH_START ... BRANCH_END liền nhau).
fn handler_positions(chain: &[u64], try_end: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut p = try_end + 1;
    while p < chain.len() && chain[p] == MARKER_CATCH_START {
        out.push(p);
        p = matching_branch_end(chain, p)
            .map(|e| e + 1)
            .unwrap_or(p + 1);
    }
    out
}

/// Bỏ qua các handler còn lại kể từ vị trí p (đứng ngay trước CATCH_START).
fn skip_handlers(chain: &[u64], mut p: usize) -> usize {
    while p < chain.len() && chain[p] == MARKER_CATCH_START {
        p = matching_branch_end(chain, p)
            .map(|e| e + 1)
            .unwrap_or(p + 1);
    }
    p
}

/// Layout switch: (các vị trí SWITCH_CASE cùng cấp, vị trí SWITCH_CLOSE).
fn switch_layout(chain: &[u64], start: usize) -> (Vec<usize>, Option<usize>) {
    let mut cases = Vec::new();
    let mut depth = 0usize;
    for (i, &e) in chain.iter().enumerate().skip(start + 1) {
        match e {
            MARKER_SWITCH_START => depth += 1,
            MARKER_SWITCH_CLOSE => {
                if depth == 0 {
                    return (cases, Some(i));
                }
                depth -= 1;
            }
            MARKER_SWITCH_CASE if depth == 0 => cases.push(i),
            _ => {}
        }
    }
    (cases, None)
}

/// Exception type từ nhãn THROW (`throw ValueError`) và handler
/// (`catch ValueError` / `catch` không type). Không type ở một bên = khớp.
fn catch_matches(thrown: Option<&str>, handler: Option<&str>) -> bool {
    let h = handler
        .and_then(|l| l.strip_prefix("catch"))
        .map(|t| t.trim())
        .filter(|t| !t.is_empty());
    let t = thrown
        .and_then(|l| l.strip_prefix("throw"))
        .map(|t| t.trim())
        .filter(|t| !t.is_empty());
    match (t, h) {
        (_, None) | (None, _) => true,
        (Some(t), Some(h)) => t.eq_ignore_ascii_case(h) || t.starts_with(h),
    }
}

// ── State model ──

#[derive(Clone)]
struct SimFrame {
    func_id: u64,
    chain: Vec<u64>,
    /// position → (tên call, line, condition) — loại record nhãn nhánh.
    calls: HashMap<usize, (String, u32, Option<String>)>,
    /// position → nhãn marker (điều kiện if, case, exception type...).
    labels: HashMap<usize, String>,
    pos: usize,
    /// Stack TRY_START đang mở (vị trí marker).
    catch_stack: Vec<usize>,
    /// Stack (loop_pos, loop_back_pos) của loop đang chạy.
    loops: Vec<(usize, usize)>,
    /// Stack SWITCH_CLOSE của switch đang mở.
    switches: Vec<usize>,
    /// Vị trí BRANCH_END kết thúc handler đang thực thi (throw nhảy vào).
    handlers: Vec<usize>,
    /// Call vừa xử lý là leaf (không descend) — CALL_EXIT không ghi Return
    /// step cho leaf call (call thư viện kết thúc ngay, Return là nhiễu).
    leaf_pending: bool,
}

impl SimFrame {
    fn from_flow(func_id: u64, fr: &FlowResult) -> Self {
        SimFrame {
            func_id,
            chain: fr.chain.clone(),
            calls: fr
                .calls
                .iter()
                .filter(|c| !c.to_name.is_empty())
                .map(|c| (c.position, (c.to_name.clone(), c.line, c.condition.clone())))
                .collect(),
            labels: fr
                .branch_labels
                .iter()
                .map(|b| (b.position, b.label.clone()))
                .collect(),
            // chain[0] là func id của chính frame — bắt đầu từ phần tử kế.
            pos: 1,
            catch_stack: Vec::new(),
            loops: Vec::new(),
            switches: Vec::new(),
            handlers: Vec::new(),
            leaf_pending: false,
        }
    }
}

#[derive(Clone)]
struct SimState {
    frames: Vec<SimFrame>,
    steps: Vec<FlowStep>,
    /// (func_id, loop_pos) → số lần quay lại loop.
    loop_iters: HashMap<(u64, usize), usize>,
    /// Số frame của caller path trước root (target) — offset stack_depth.
    base_depth: usize,
    /// Caller path còn lại (outermost..innermost, không gồm target) — throw
    /// unwind qua đây khi frames cạn.
    pending_callers: Vec<u64>,
}

/// Key dedup state: vị trí (func_id, pos) của mọi frame + số step.
fn state_key(st: &SimState) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in &st.frames {
        f.func_id.hash(&mut h);
        f.pos.hash(&mut h);
    }
    st.steps.len().hash(&mut h);
    h.finish()
}

impl GraphIndex {
    /// Index in-memory (test/dev, không persist). Embedding mặc định TẮT
    /// (config chưa set → `[embedding].backend = "hashing"`), nên không load
    /// model. Nếu process đã set config fastembed trước đó mà model lỗi → panic.
    pub fn in_memory() -> Self {
        let storage: Box<dyn crate::storage::Storage> = Box::new(InMemoryStorage::default());
        Self::new_with_storage(storage, None).expect("in_memory embedding backend init failed")
    }

    /// Mở index từ một backend persistent bằng DSN — rebuild từ entity store.
    ///
    /// DSN mang scheme cho biết backend, phần còn lại là path:
    /// - `sqlite://<path>` → sqlite (feature `sqlite`)
    /// - `lmdb://<path>` → LMDB (feature `lmdb`)
    /// - `redis://<url>` → redis (feature `redis`)
    ///
    /// Không có scheme (plain path) → fallback backend mặc định **nếu chỉ có
    /// đúng 1 backend** được compile (backward compat: main.rs/mcp truyền
    /// plain path với build chỉ bật `sqlite`). Nếu ≥2 backend — thay vì chọn
    /// ngầm một backend (gây nhầm) — báo lỗi bắt caller chỉ rõ scheme.
    pub async fn open(dsn: &str) -> Result<Self> {
        match Self::split_dsn(dsn) {
            Some(("sqlite", path)) => Self::open_sqlite_dispatch(path).await,
            Some(("lmdb", path)) => Self::open_lmdb_dispatch(path).await,
            Some(("redis", _)) => Self::open_redis_dispatch(dsn).await,
            _ => Self::open_default(dsn).await,
        }
    }

    /// `sqlite://` rõ ràng — compile cả sqlite; không compile → báo lỗi.
    #[cfg(feature = "sqlite")]
    async fn open_sqlite_dispatch(path: &str) -> Result<Self> {
        Self::open_sqlite(path).await
    }
    /// `sqlite://` rõ ràng nhưng feature không bật → không thể mở.
    #[cfg(not(feature = "sqlite"))]
    async fn open_sqlite_dispatch(_path: &str) -> Result<Self> {
        Err(backend_unavailable("sqlite"))
    }

    /// `lmdb://` rõ ràng — compile trường lmdb; không compile → báo lỗi.
    #[cfg(feature = "lmdb")]
    async fn open_lmdb_dispatch(path: &str) -> Result<Self> {
        Self::open_lmdb(path).await
    }

    /// `lmdb://` rõ ràng nhưng feature không bổ — lỗi.
    #[cfg(not(feature = "lmdb"))]
    async fn open_lmdb_dispatch(_path: &str) -> Result<Self> {
        Err(backend_unavailable("lmdb"))
    }

    /// `redis://` rõ ràng — compile trường redis; không compile → báo lỗi.
    #[cfg(feature = "redis")]
    async fn open_redis_dispatch(dsn: &str) -> Result<Self> {
        Self::open_redis(dsn).await
    }
    /// `redis://` rõ ràng nhưng feature không bật → không thể mở.
    #[cfg(not(feature = "redis"))]
    async fn open_redis_dispatch(_dsn: &str) -> Result<Self> {
        Err(backend_unavailable("redis"))
    }

    /// Tách `scheme://` khỏi DSN: trả `(scheme, phần còn lại)` hoặc `None`
    /// nếu không có scheme (plain path).
    fn split_dsn(dsn: &str) -> Option<(&'static str, &str)> {
        if let Some(rest) = dsn.strip_prefix("sqlite://") {
            return Some(("sqlite", rest));
        }
        if let Some(rest) = dsn.strip_prefix("lmdb://") {
            return Some(("lmdb", rest));
        }
        if dsn.starts_with("redis://") || dsn.starts_with("rediss://") {
            return Some(("redis", dsn));
        }
        None
    }

    /// Mở backend mặc định khi DSN không có scheme. Chỉ được phép ngầm chọn
    /// khi **đúng 1** backend persistent được compile; nhiều hơn → lỗi bắt
    /// buộc scheme (tránh chọn nhầm). Các nhánh cfg mutual-exclusive nên
    /// không có unreachable code.
    #[allow(unused_variables)] // dsn chỉ dùng trong nhánh single-backend
    async fn open_default(dsn: &str) -> Result<Self> {
        // Chỉ sqlite được compile — plain path = sqlite (backward compat).
        #[cfg(all(feature = "sqlite", not(any(feature = "lmdb", feature = "redis"))))]
        {
            return Self::open_sqlite(dsn).await;
        }
        // Chỉ lmdb được compile — plain path = lmdb.
        #[cfg(all(feature = "lmdb", not(any(feature = "sqlite", feature = "redis"))))]
        {
            return Self::open_lmdb(dsn).await;
        }
        // Nhiều backend (≥2) — DSN không nói scheme → mơ hồ.
        #[cfg(any(
            all(feature = "sqlite", feature = "lmdb"),
            all(feature = "sqlite", feature = "redis"),
            all(feature = "lmdb", feature = "redis")
        ))]
        {
            return Err(Error::Db(
                "Nhiều backend persistent được bật nhưng DSN không chỉ rõ scheme. \
                 Ghi rõ `sqlite://`, `lmdb://` hoặc `redis://` trong --dbdsn."
                    .into(),
            ));
        }
        // Không backend nào — không thể mở persistent.
        #[allow(unreachable_code)]
        {
            Err(Error::Db(
                "Phải bật ít nhất một feature 'sqlite', 'lmdb' hoặc 'redis'".into(),
            ))
        }
    }

    #[cfg(feature = "lmdb")]
    async fn open_lmdb(path: &str) -> Result<Self> {
        let storage = crate::storage::lmdb::LmdbStorage::open(path)
            .await
            .map_err(serr)?;
        let name_store = open_name_store(&format!("lmdb://{path}")).await;
        let storage: Box<dyn crate::storage::Storage> = Box::new(storage);
        let mut idx = Self::new_with_storage(storage, name_store)?;
        idx.rebuild().await?;
        Ok(idx)
    }

    #[cfg(feature = "sqlite")]
    async fn open_sqlite(path: &str) -> Result<Self> {
        let storage = crate::storage::sqlite::SqliteStorage::open(path)
            .await
            .map_err(serr)?;
        let name_store = open_name_store(&format!("sqlite://{path}")).await;
        let storage: Box<dyn crate::storage::Storage> = Box::new(storage);
        let mut idx = Self::new_with_storage(storage, name_store)?;
        idx.rebuild().await?;
        Ok(idx)
    }

    /// Mở index trên Redis (`redis://` / `rediss://`). Keyspace prefix được
    /// dẫn xuất từ số DB trong DSN (`redis://host:port/15` → `codegraph:idx:15`)
    /// để nhiều index không đụng nhau. Redis không multi-tenant theo `repo_id`
    /// như RDBMS — mỗi DB number = một index riêng.
    #[cfg(feature = "redis")]
    async fn open_redis(dsn: &str) -> Result<Self> {
        let db = dsn
            .trim_start_matches("rediss://")
            .trim_start_matches("redis://")
            .rsplit('/')
            .next()
            .and_then(|s| {
                if s.is_empty() {
                    None
                } else {
                    s.parse::<u32>().ok()
                }
            })
            .unwrap_or(0);
        let client =
            redis::Client::open(dsn).map_err(|e| Error::Db(format!("redis client: {e}")))?;
        let storage =
            crate::storage::redis::RedisStorage::new(client, &format!("codegraph:idx:{db}"))
                .await
                .map_err(serr)?;
        let name_store = open_name_store(dsn).await;
        let storage: Box<dyn crate::storage::Storage> = Box::new(storage);
        let mut idx = Self::new_with_storage(storage, name_store)?;
        idx.rebuild().await?;
        Ok(idx)
    }

    /// Mở index theo `StorageRoute` — hỗ trợ multi-tenant + sharding RDBMS.
    ///
    /// - `Memory` → in-memory.
    /// - `Local(dsn)` → `open(dsn)` (sqlite/lmdb/redis).
    /// - `Sharded { dsns, repo_id, root }` → tính `shard = repo_id % N`
    ///   (`StorageRoute::shard_of`), mở backend per-repo (`PostgresStorage`/
    ///   `MySqlStorage`) trên `dsns[shard]`, ensure row `repos`, rebuild.
    pub async fn open_route(route: &StorageRoute) -> Result<Self> {
        match route {
            StorageRoute::Memory => Ok(Self::in_memory()),
            StorageRoute::Local(dsn) => Self::open(dsn).await,
            StorageRoute::Sharded { dsns, repo_id, .. } => {
                let repo_id = repo_id.ok_or_else(|| {
                    Error::Db(
                        "StorageRoute::Sharded thiếu repo_id — chạy `codegraph init` để sinh"
                            .into(),
                    )
                })?;
                if dsns.is_empty() {
                    return Err(Error::Db("StorageRoute::Sharded không có DSN nào".into()));
                }
                let shard = route.shard_of(repo_id).ok_or_else(|| {
                    Error::Db("không tính được shard từ StorageRoute::Sharded".into())
                })?;
                let dsn = dsns
                    .get(shard)
                    .ok_or_else(|| Error::Db(format!("shard {shard} vượt quá số lượng DSN")))?;
                let result: Result<Self> = if dsn.starts_with("postgres://") {
                    #[cfg(feature = "postgres")]
                    {
                        let storage = crate::storage::postgres::PostgresStorage::open(dsn, repo_id)
                            .await
                            .map_err(serr)?;
                        storage
                            .ensure_registered(shard, route.root())
                            .await
                            .map_err(serr)?;
                        // RDBMS không có dataset file riêng — name engine
                        // fallback in-memory (giống `doc_storage_dsn`).
                        let storage: Box<dyn crate::storage::Storage> = Box::new(storage);
                        let mut idx = Self::new_with_storage(storage, None)?;
                        idx.rebuild().await?;
                        Ok(idx)
                    }
                    #[cfg(not(feature = "postgres"))]
                    {
                        Err(Error::Db("feature 'postgres' chưa bật".into()))
                    }
                } else if dsn.starts_with("mysql://") {
                    #[cfg(feature = "mysql")]
                    {
                        let storage = crate::storage::mysql::MySqlStorage::open(dsn, repo_id)
                            .await
                            .map_err(serr)?;
                        storage
                            .ensure_registered(shard, route.root())
                            .await
                            .map_err(serr)?;
                        // RDBMS không có dataset file riêng — name engine
                        // fallback in-memory (giống `doc_storage_dsn`).
                        let storage: Box<dyn crate::storage::Storage> = Box::new(storage);
                        let mut idx = Self::new_with_storage(storage, None)?;
                        idx.rebuild().await?;
                        Ok(idx)
                    }
                    #[cfg(not(feature = "mysql"))]
                    {
                        Err(Error::Db("feature 'mysql' chưa bật".into()))
                    }
                } else {
                    Err(Error::Db(format!("Sharded DSN scheme không hỗ trợ: {dsn}")))
                };
                result
            }
        }
    }

    fn new_with_storage(
        storage: Box<dyn crate::storage::Storage>,
        name_store: Option<Arc<RwLock<dyn crate::storage::Storage>>>,
    ) -> Result<Self> {
        // Wrap mọi backend bằng LRU read-cache để giảm gọi xuống storage
        // (SQL/remote) cho các read path nóng (node/children/chain/entity).
        const CACHE_CAPACITY: usize = 8192;
        let storage = CachedStorage::wrap(storage, CACHE_CAPACITY);
        // Name engine dùng dataset RIÉNG (cùng backend theo config) — record id
        // (1..N) không đụng func id của chain trie, và `Search::clear()`
        // không cần global nữa. Không mừ dataset (in-memory / RDBMS)
        // → fallback in-memory như cư.
        let name_storage = name_store.clone().unwrap_or_else(|| {
            CachedStorage::wrap(Box::new(InMemoryStorage::default()), CACHE_CAPACITY)
        });
        // Embedding chỉ bật khi config `[embedding].backend = "fastembed"` (opt-in).
        // Nếu bật mà model tải thất bại → lỗi rõ ràng (KHÔNG fallback silent).
        let (backend, enabled) = if embedding_enabled() {
            let b = make_backend()
                .map_err(|e| Error::Db(format!("embedding backend init failed: {e}")))?;
            (Arc::from(b), true)
        } else {
            // Tắt → placeholder (không bao giờ gọi embed; vector index rỗng).
            (Arc::from(default_backend()), false)
        };
        Ok(Self {
            chains: Search::new(CHAIN_SHARDING, storage.clone()),
            names: Search::new(CHAIN_SHARDING, name_storage.clone()),
            name_store,
            storage,
            name_records: Vec::new(),
            symbols: HashMap::new(),
            name_index: HashMap::new(),
            scope_index: HashMap::new(),
            chains_map: HashMap::new(),
            call_names: HashMap::new(),
            impl_index: HashMap::new(),
            fn_ptr_index: HashMap::new(),
            go_impls: HashMap::new(),
            file_imports: HashMap::new(),
            edge_count: 0,
            files: Vec::new(),
            next_id: SYMBOL_BASE,
            version: 0,
            embedding_backend: backend,
            embedding_enabled: enabled,
            vector_index: VectorIndex::new(crate::embeddings::VECTOR_DIM),
        })
    }

    // ── Build / rebuild ──

    /// Rebuild toàn bộ index từ entity store trong storage (open/reopen).
    #[cfg_attr(
        not(any(feature = "sqlite", feature = "lmdb", feature = "redis")),
        allow(dead_code)
    )]
    // chỉ open() dùng — không backend thì không ai gọi.
    async fn rebuild(&mut self) -> Result<()> {
        // Đo bằng sink `codegraph_core::prof` — stderr bị MCP host nuốt mất
        // khi `serve --mcp`, nên log chỉ ra được khi bật qua env (xem prof.rs).
        macro_rules! cg_t {
            ($n:expr) => {{ codegraph_core::prof::stage($n) }};
        }
        macro_rules! cg_d {
            ($t:expr, $n:expr) => {{
                codegraph_core::prof::stage_done($n, $t);
            }};
        }
        let __t = cg_t!("rebuild TOTAL");
        self.next_id = self
            .storage
            .read()
            .await
            .load_next_id()
            .await
            .map_err(serr)?;
        let __t_load_all_symbols = cg_t!("load_all_symbols");
        let symbols = self
            .storage
            .read()
            .await
            .load_all_symbols()
            .await
            .map_err(serr)?;
        cg_d!(__t_load_all_symbols, "load_all_symbols");
        let __t_all_chains = cg_t!("all_chains");
        let chains_raw = self.storage.read().await.all_chains().await.map_err(serr)?;
        cg_d!(__t_all_chains, "all_chains");
        let __t_all_call_name_indexes = cg_t!("all_call_name_indexes");
        let call_names_raw = self
            .storage
            .read()
            .await
            .all_call_name_indexes()
            .await
            .map_err(serr)?;
        cg_d!(__t_all_call_name_indexes, "all_call_name_indexes");
        let __t_load_all_files = cg_t!("load_all_files");
        self.files = self
            .storage
            .read()
            .await
            .load_all_files()
            .await
            .map_err(serr)?;
        cg_d!(__t_load_all_files, "load_all_files");
        self.version = self.storage.read().await.version().await.map_err(serr)?;

        // Registry (scope ids trong entity đã là global — persist sau remap).
        self.symbols.clear();
        self.name_index.clear();
        self.scope_index.clear();
        let __t_a = cg_t!("index_symbol_loop");
        for sym in symbols {
            self.index_symbol(sym);
        }
        cg_d!(__t_a, "index_symbol_loop");

        // Chains.
        let __t_b = cg_t!("chains_decode");
        self.chains_map.clear();
        for (func_id, bytes) in chains_raw {
            self.chains_map
                .insert(func_id, crate::storage::decode_chain(&bytes));
        }

        cg_d!(__t_b, "chains_decode");
        let __t_c = cg_t!("call_names_json");
        self.call_names.clear();
        for (name, bytes) in call_names_raw {
            if let Ok(sites) = serde_json::from_slice::<Vec<CallSite>>(&bytes) {
                self.call_names.insert(name, sites);
            }
        }

        // Edge count — đếm thẳng từ chains (không materialize EdgeMeta:
        // metadata đã nằm trong call records persist, `flow()` đọc từ đó).
        // Không cần `all_call_records` ở đây (đó là JSON deserialize ~80ms).
        self.edge_count = self
            .chains_map
            .values()
            .map(|chain| {
                chain
                    .iter()
                    .skip(1)
                    .filter(|&&e| e != 0 && !is_marker(e))
                    .count() as u64
            })
            .sum();
        cg_d!(__t_c, "call_names_json");

        // Engines.
        let __t_e = cg_t!("rebuild_chain_engine");
        // Chain trie đã persist trong `rt_*` — version khớp thì bỏ qua
        // `clear()` + re-insert toàn bộ func id (chiếm >99% thời gian rebuild:
        // ~11s@5k, ~113s@20k). `chains_map` đã decode ở trên nên `callees`/`flow`
        // vẫn đủ; chỉ trie (dùng cho `callers`/`search_flow`) là giữ nguyên.
        if !self.chain_engine_is_current().await {
            self.rebuild_chain_engine(None).await?;
            self.stamp_chain_engine().await;
        }
        cg_d!(__t_e, "rebuild_chain_engine");
        let __t_f = cg_t!("rebuild_name_engine");
        // Name trie đã persist ở dataset riêng — nếu version khớp thì dựng
        // lại 2 Vec phụ, không clear + insert lại ~70k tên (11-20s).
        if self.name_engine_is_current().await {
            self.load_name_records();
        } else {
            self.rebuild_name_engine(None).await?;
            self.stamp_name_engine().await;
        }
        cg_d!(__t_f, "rebuild_name_engine");
        let __t_g = cg_t!("rebuild_vector_index");
        self.rebuild_vector_index(None).await?;
        cg_d!(__t_g, "rebuild_vector_index");
        // Persist counts để `codegraph_status` đọc O(1) (không rebuild lại).
        {
            let mut st = self.storage.write().await;
            st.set_stats(IndexCounts {
                symbols: self.symbols.len() as u64,
                chains: self.chains_map.len() as u64,
                edges: self.edge_count,
                files: self.files.len() as u64,
                next_id: self.next_id,
            })
            .await
            .map_err(serr)?;
        }
        cg_d!(__t, "rebuild TOTAL");
        Ok(())
    }

    /// Insert symbol vào registry + index (scope id đã global — path rebuild).
    #[cfg_attr(
        not(any(feature = "sqlite", feature = "lmdb", feature = "redis")),
        allow(dead_code)
    )]
    // chỉ rebuild() dùng — không backend thì không ai gọi.
    fn index_symbol(&mut self, sym: Symbol) {
        let id = sym.id;
        if !sym.name.is_empty() {
            self.name_index
                .entry(sym.name.to_lowercase())
                .or_default()
                .push(id);
        }
        if sym.scope_id != 0 {
            self.scope_index.entry(sym.scope_id).or_default().push(id);
        }
        self.symbols.insert(id, sym);
    }

    /// Rebuild scope index từ symbols hiện tại (sau khi remap scope id).
    fn rebuild_scope_index(&mut self) {
        self.scope_index.clear();
        for (&id, sym) in &self.symbols {
            if sym.scope_id != 0 {
                self.scope_index.entry(sym.scope_id).or_default().push(id);
            }
        }
    }

    /// Rebuild chain engine từ `chains_map` (clear + insert tuần tự).
    async fn rebuild_chain_engine(&mut self, progress: Option<&dyn IngestProgress>) -> Result<()> {
        self.chains.clear().await.map_err(serr_search)?;
        let mut funcs: Vec<u64> = self.chains_map.keys().copied().collect();
        funcs.sort_unstable();
        if let Some(p) = progress {
            p.phase("rebuild call-chain engine", funcs.len());
        }
        for func_id in funcs {
            let chain = &self.chains_map[&func_id];
            // Mọi element meta = None → không ghi node stream (record = func id
            // dùng trực tiếp, không cần indirection như CallIndex cũ).
            let metas: Vec<Option<&[u8]>> = vec![None; chain.len()];
            self.chains
                .insert_chain(func_id as usize, chain, &metas)
                .await
                .map_err(serr_search)?;
            if let Some(p) = progress {
                p.advance(1);
            }
        }
        Ok(())
    }

    /// Slot record dành cho version stamp — name record bắt đầu từ 1 nên không
    /// va chạm. Stamp = `[version: u64 LE][count: u64 LE]`.
    const NAME_STAMP_SLOT: usize = 0;
    /// Name trie trên đĩa còn đúng version + khớp số record đã ghi (chống dataset
    /// bị xoá giữa chừng → coi như stale).
    async fn name_engine_is_current(&self) -> bool {
        let Some(store) = &self.name_store else {
            return false;
        };
        let guard = store.read().await;
        let Ok(Some(bytes)) = guard.get_meta(Self::NAME_STAMP_SLOT).await else {
            return false;
        };
        if bytes.len() != 16 {
            return false;
        }
        let version = u64::from_le_bytes(bytes[0..8].try_into().unwrap_or([0; 8]));
        let count = u64::from_le_bytes(bytes[8..16].try_into().unwrap_or([0; 8])) as usize;
        if version != self.version {
            return false;
        }
        guard.get_key_len(count).await.ok().flatten().is_some()
    }

    /// Ghi stamp sau khi trie dựng lại — `version` + số tên distinct.
    async fn stamp_name_engine(&self) {
        let Some(store) = &self.name_store else {
            return;
        };
        let mut bytes = [0u8; 16];
        bytes[0..8].copy_from_slice(&self.version.to_le_bytes());
        bytes[8..16].copy_from_slice(&(self.name_index.len() as u64).to_le_bytes());
        let mut guard = store.write().await;
        if let Err(e) = guard.set_meta(Self::NAME_STAMP_SLOT, &bytes).await {
            eprintln!("[codegraph] name engine stamp failed: {e}");
        }
    }

    /// Slot record cho version stamp của **chain engine** — chain trie lưu trên
    /// storage chính (`rt_*`), func id ≥ `SYMBOL_BASE` nên slot `0` không va chạm
    /// với node nào. Stamp = `[version: u64 LE][chain_count: u64 LE][sample_func:
    /// u64 LE]` (24 byte) — `sample_func` = func id nhỏ nhất, dùng để probe trie
    /// còn sống (record id của chain engine là func id **thưa**, không phải 1..N).
    const CHAIN_STAMP_SLOT: usize = 0;

    /// Chain trie trên đĩa còn đúng version + khớp số func + còn node mẫu (chống
    /// `rt_*` bị xoá dở → coi như stale). Giống `name_engine_is_current` nhưng
    /// đọc trên storage chính, và probe bằng `sample_func` (func id thật).
    async fn chain_engine_is_current(&self) -> bool {
        let guard = self.storage.read().await;
        let Ok(Some(bytes)) = guard.get_meta(Self::CHAIN_STAMP_SLOT).await else {
            return false;
        };
        if bytes.len() != 24 {
            return false;
        }
        let version = u64::from_le_bytes(bytes[0..8].try_into().unwrap_or([0; 8]));
        let count = u64::from_le_bytes(bytes[8..16].try_into().unwrap_or([0; 8])) as usize;
        let sample = u64::from_le_bytes(bytes[16..24].try_into().unwrap_or([0; 8])) as usize;
        if version != self.version || count != self.chains_map.len() {
            return false;
        }
        // Index rỗng là hợp lệ (không có chain nào).
        if count == 0 {
            return true;
        }
        // Triet còn sống nếu record mẫu (func id thật) còn độ dài key.
        guard.get_key_len(sample).await.ok().flatten().is_some()
    }

    /// Ghi stamp sau khi chain trie dựng lại — `version` + số func + func id nhỏ
    /// nhất làm mẫu probe.
    async fn stamp_chain_engine(&self) {
        let mut bytes = [0u8; 24];
        bytes[0..8].copy_from_slice(&self.version.to_le_bytes());
        bytes[8..16].copy_from_slice(&(self.chains_map.len() as u64).to_le_bytes());
        let sample = self.chains_map.keys().min().copied().unwrap_or(0);
        bytes[16..24].copy_from_slice(&sample.to_le_bytes());
        let mut guard = self.storage.write().await;
        if let Err(e) = guard.set_meta(Self::CHAIN_STAMP_SLOT, &bytes).await {
            eprintln!("[codegraph] chain engine stamp failed: {e}");
        }
    }

    /// Dựng lại `name_records` từ `name_index` (nằm sẵn trong RAM) — thay cho
    /// vòng trie insert khi đã bỏ qua rebuild. Mảng giữ thứ tự sort để vừa map
    /// record→tên vừa dùng cho scan Prefix/Suffix/Exact.
    fn load_name_records(&mut self) {
        let mut distinct: Vec<&String> = self.name_index.keys().collect();
        distinct.sort();
        self.name_records = distinct.into_iter().cloned().collect();
    }

    /// Rebuild name engine từ `name_index` (clear + insert mỗi tên distinct).
    async fn rebuild_name_engine(&mut self, progress: Option<&dyn IngestProgress>) -> Result<()> {
        self.names.clear().await.map_err(serr_search)?;
        self.name_records.clear();
        let mut distinct: Vec<&String> = self.name_index.keys().collect();
        distinct.sort();
        if let Some(p) = progress {
            p.phase("rebuild name-search engine", distinct.len());
        }
        let mut record = 0usize;
        for name in distinct {
            record += 1;
            let metas: Vec<Option<&[u8]>> = vec![None; name.len()];
            self.names
                .insert_chain(record, name.as_bytes(), &metas)
                .await
                .map_err(serr_search)?;
            self.name_records.push(name.clone());
            if let Some(p) = progress {
                p.advance(1);
            }
        }
        Ok(())
    }

    /// Rebuild vector index (semantic search) từ symbols hiện tại.
    ///
    /// - Nếu embedding **TẮT** (`embedding_enabled = false`): vector index để
    ///   rỗng, semantic search sẽ báo lỗi rõ ràng (không fallback silent).
    /// - Nếu bật: ưu tiên load vector đã **persist trong storage** (tái dùng cho
    ///   KNN/k-means, không re-embed). Chỉ những symbol thiếu vector mới được
    ///   embed + `save_embedding` (incremental). Chạy sau khi registry + name
    ///   engine đã sẵn sàng.
    async fn rebuild_vector_index(&mut self, progress: Option<&dyn IngestProgress>) -> Result<()> {
        if !self.embedding_enabled {
            // Embedding tắt → không build vector index.
            self.vector_index = VectorIndex::new(crate::embeddings::VECTOR_DIM);
            return Ok(());
        }
        let dim = self.embedding_backend.dim();
        let mut vi = VectorIndex::new(dim);
        // Load các vector đã persist (tái dùng, không re-embed).
        let stored = {
            let st = self.storage.read().await;
            st.load_all_embeddings().await.map_err(serr)?
        };
        // Symbol thiếu vector → compute + save.
        let mut missing: Vec<(u64, String)> = Vec::new();
        for sym in self.symbols.values() {
            match stored.get(&sym.id) {
                Some(v) if v.len() == dim => {
                    vi.insert(sym.id, v.clone());
                }
                _ => missing.push((sym.id, embedding_text(sym))),
            }
        }
        if !missing.is_empty() {
            if let Some(p) = progress {
                p.phase("embed vectors", missing.len());
            }
            eprintln!(
                "codegraph: embedding {} symbols (batch) — this may take a while for large repos",
                missing.len(),
            );
            // Batch-embed theo chunk để tận dụng tính toán hàng loạt (fastembed)
            // và báo tiến độ từng bước — tránh quét tuần tự chậm + bar đứng im.
            let mut st = self.storage.write().await;
            let texts: Vec<String> = missing.iter().map(|(_, t)| t.clone()).collect();
            const CHUNK: usize = 512;
            let mut base = 0usize;
            for chunk in texts.chunks(CHUNK) {
                let vecs = self.embedding_backend.embed_batch(chunk);
                for (v, (id, _)) in vecs.into_iter().zip(&missing[base..base + chunk.len()]) {
                    vi.insert(*id, v.clone());
                    st.save_embedding(*id, &v).await.map_err(serr)?;
                }
                base += chunk.len();
                if let Some(p) = progress {
                    p.advance(chunk.len());
                }
            }
        }
        self.vector_index = vi;
        Ok(())
    }

    // ── Ingest (full re-index — pipeline 2 phase như semgraph) ──

    /// Ingest toàn bộ parse results — **full re-index**: xoá dữ liệu cũ, register
    /// symbol (id global) + remap, resolve placeholder 0, build call-name index
    /// + đếm edges, persist + bump version (bản có tiến độ: `ingest_with_progress`).
    pub async fn ingest(
        &mut self,
        results: &[ParseResult],
        progress: Option<Arc<dyn IngestProgress>>,
    ) -> Result<()> {
        self.ingest_with_progress(results, progress).await
    }

    /// Như [`ingest`](Self::ingest), nhưng báo tiến độ qua `IngestProgress`
    /// (phase + số đơn vị). Graph crate không phụ thuộc indicatif — caller nối
    /// các event này vào ProgressBar của nó.
    pub async fn ingest_with_progress(
        &mut self,
        results: &[ParseResult],
        progress: Option<Arc<dyn IngestProgress>>,
    ) -> Result<()> {
        let p = progress.as_deref();

        // ── Reset ──
        self.storage
            .write()
            .await
            .clear_entities()
            .await
            .map_err(serr)?;
        self.chains.clear().await.map_err(serr_search)?;
        self.names.clear().await.map_err(serr_search)?;
        self.symbols.clear();
        self.name_index.clear();
        self.scope_index.clear();
        self.chains_map.clear();
        self.call_names.clear();
        self.impl_index.clear();
        self.fn_ptr_index.clear();
        self.go_impls.clear();
        self.file_imports.clear();
        self.edge_count = 0;
        self.files.clear();
        self.name_records.clear();
        self.next_id = SYMBOL_BASE;

        // ── Phase 1: register + remap ──
        let total_symbols: usize = results.iter().map(|r| r.symbols.len()).sum();
        if let Some(p) = p {
            p.phase("register symbols", total_symbols);
        }
        let mut all_calls: Vec<CallRef<'_>> = Vec::new();
        for result in results {
            let mut id_map: HashMap<u64, u64> = HashMap::new();
            for sym in &result.symbols {
                let new_id = self.register(sym.clone()).await?;
                if sym.id != 0 {
                    id_map.insert(sym.id, new_id);
                }
            }
            // Remap scope_id + type_ref (id local → global) trên bản đã lưu.
            for sym in &result.symbols {
                let new_id = id_map.get(&sym.id).copied().unwrap_or(sym.id);
                self.remap_scope_type(new_id, &id_map).await?;
            }
            // Chains — remap từng element (0 giữ nguyên — placeholder).
            for (func_id, chain) in &result.chains {
                let nf = id_map.get(func_id).copied().unwrap_or(*func_id);
                let mut nchain: Vec<u64> = chain
                    .iter()
                    .map(|e| id_map.get(e).copied().unwrap_or(*e))
                    .collect();
                if nchain.is_empty() {
                    nchain.push(nf);
                }
                self.chains_map.insert(nf, nchain);
            }
            // Calls — remap caller_id (position giữ nguyên — đã trỏ đúng chain).
            for c in &result.calls {
                let caller = id_map.get(&c.caller_id).copied().unwrap_or(c.caller_id);
                all_calls.push(CallRef { rec: c, caller });
            }
            if let Some(p) = p {
                p.advance(result.symbols.len());
            }
        }
        // Scope index chỉ rebuild sau khi toàn bộ scope id đã là global.
        self.rebuild_scope_index();
        // Import map: file → alias/path (resolve qualified call `httpx.Get`).
        for result in results {
            if let Some(deps) = &result.file_deps {
                self.file_imports
                    .insert(result.path.clone(), deps.imports.clone());
            }
        }
        // Function-pointer assign-sites: resolve target name → symbol id
        // (name_index đã đủ sau Phase 1).
        for result in results {
            for a in &result.fn_ptr_assigns {
                let targets = self
                    .name_index
                    .get(&a.target.to_lowercase())
                    .cloned()
                    .unwrap_or_default();
                self.fn_ptr_index
                    .entry(a.var.to_lowercase())
                    .or_default()
                    .extend(targets);
            }
        }

        // ── Phase 2: resolve placeholder 0 trong chains ──
        self.build_impl_index();
        self.build_go_method_sets();
        let mut call_meta: HashMap<(u64, usize), (ResolutionKind, Vec<u64>)> = HashMap::new();
        self.resolve_calls(&all_calls, &mut call_meta);

        // ── Phase 3: call records + call-name index + edge count ──
        self.build_call_indexes(&all_calls, &call_meta, p).await?;

        // ── Phase 4: files ──
        if let Some(p) = p {
            p.phase("save files", results.len());
        }
        for result in results {
            let f = FileInfo {
                path: result.path.clone(),
                language: result.language.clone(),
                bytes: result.bytes,
                lines: result.lines,
            };
            self.storage
                .write()
                .await
                .upsert_file(&f)
                .await
                .map_err(serr)?;
            self.files.push(f);
            if let Some(p) = p {
                p.advance(1);
            }
        }

        // ── Phase 5: engines + version bump ──
        self.rebuild_chain_engine(p).await?;
        self.rebuild_name_engine(p).await?;
        self.rebuild_vector_index(p).await?;
        self.version += 1;
        // Stamp SAU khi bump version: đây là chỗ duy nhất ghi name trie ra
        // đĩa, nên `rebuild()` lúc mở index chỉ cần đọc stamp là bỏ qua
        // `rebuild_name_engine` (11-32s) — thay vì dựng lại mỗi lần mở.
        self.stamp_name_engine().await;
        // Chain trie cũng persist trong `rt_*` — stamp version để reopen bỏ qua
        // `rebuild_chain_engine` (>99% thời gian rebuild: ~113s@20k).
        self.stamp_chain_engine().await;
        {
            let mut st = self.storage.write().await;
            st.save_next_id(self.next_id).await.map_err(serr)?;
            st.set_version(self.version).await.map_err(serr)?;
            st.set_stats(IndexCounts {
                symbols: self.symbols.len() as u64,
                chains: self.chains_map.len() as u64,
                edges: self.edge_count,
                files: self.files.len() as u64,
                next_id: self.next_id,
            })
            .await
            .map_err(serr)?;
        }
        Ok(())
    }

    /// Gán id global cho symbol, lưu storage + index tên. Không đụng scope index
    /// — scope id còn local, `rebuild_scope_index` chạy sau khi remap.
    /// `next_id` không save per-symbol (chậm) — `ingest` persist 1 lần ở cuối.
    async fn register(&mut self, mut sym: Symbol) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        sym.id = id;
        self.storage
            .write()
            .await
            .save_symbol(&sym)
            .await
            .map_err(serr)?;
        if !sym.name.is_empty() {
            self.name_index
                .entry(sym.name.to_lowercase())
                .or_default()
                .push(id);
        }
        self.symbols.insert(id, sym);
        Ok(id)
    }

    /// Remap scope_id/type_ref của symbol (đã lưu) sang id global, rồi **ghi lại
    /// storage** — nếu không, reopen đọc phải scope_id local cũ (sai khi multi-file,
    /// vì id local trùng nhau giữa các file).
    async fn remap_scope_type(&mut self, new_id: u64, id_map: &HashMap<u64, u64>) -> Result<()> {
        let to_save = {
            let Some(sym) = self.symbols.get_mut(&new_id) else {
                return Ok(());
            };
            if sym.scope_id != 0
                && let Some(&g) = id_map.get(&sym.scope_id)
            {
                sym.scope_id = g;
            }
            if sym.type_ref != 0
                && let Some(&g) = id_map.get(&sym.type_ref)
            {
                sym.type_ref = g;
            }
            sym.clone()
        };
        self.storage
            .write()
            .await
            .save_symbol(&to_save)
            .await
            .map_err(serr)?;
        Ok(())
    }

    /// Thay placeholder `0` trong chain bằng id thật (resolve per-caller).
    ///
    /// Metadata resolution (`resolved_via` + `candidates` cho dynamic fan-out)
    /// gom vào `meta` — không sửa được `CallRecord` vì `CallRef` mượn `&` từ
    /// `ParseResult`; `build_call_indexes` inject metadata khi persist blob.
    fn resolve_calls(
        &mut self,
        calls: &[CallRef<'_>],
        meta: &mut HashMap<(u64, usize), (ResolutionKind, Vec<u64>)>,
    ) {
        let mut caller_calls: HashMap<u64, Vec<&CallRecord>> = HashMap::new();
        for c in calls {
            if c.caller != 0 {
                caller_calls.entry(c.caller).or_default().push(c.rec);
            }
        }
        for (caller_id, ccs) in caller_calls {
            // Collect dưới borrow immutable (resolve cần đọc `self`), rồi apply
            // qua get_mut — tránh E0502 (chain mutable + self immutable).
            let mut resolved: Vec<(usize, u64)> = Vec::new();
            {
                let Some(chain) = self.chains_map.get(&caller_id) else {
                    continue;
                };
                for c in ccs {
                    if c.position >= chain.len() || chain[c.position] != 0 {
                        continue;
                    }
                    match self.resolve_call_placeholder(c, caller_id) {
                        CallResolution::None => {}
                        CallResolution::Single(real, kind) => {
                            resolved.push((c.position, real));
                            meta.insert((caller_id, c.position), (kind, Vec::new()));
                        }
                        CallResolution::FanOut(cands, kind) => {
                            // Dynamic dispatch — KHÔNG ghi đè chain slot
                            // (placeholder 0 giữ raw name hiển thị).
                            meta.insert((caller_id, c.position), (kind, cands));
                        }
                    }
                }
            }
            if let Some(chain) = self.chains_map.get_mut(&caller_id) {
                for (pos, real) in resolved {
                    chain[pos] = real;
                }
            }
        }
    }

    /// Index dynamic-dispatch fan-out: trait/interface name → impl methods.
    /// Nguyên liệu: `Symbol.impl_trait` (Rust `impl Trait for Type`).
    ///
    /// Lưu ý: `link_impl_methods_to_def` (extractor) re-parent method của impl
    /// về struct def, nên KHÔNG thể map method → impl qua `scope_id`. Dùng
    /// file + line range của impl block để gán method (method nằm trong
    /// [line..end_line] của đúng một impl block cùng file).
    fn build_impl_index(&mut self) {
        // (file, line, end_line, trait_name lower) của các impl block có trait.
        let mut trait_impls: Vec<(String, u32, u32, String)> = Vec::new();
        for s in self.symbols.values() {
            if let Some(tr) = &s.impl_trait {
                // Java implements nhiều interface ("A,B") — mỗi interface một
                // entry fan-out riêng.
                for name in tr.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                    trait_impls.push((s.file.clone(), s.line, s.end_line, name.to_lowercase()));
                }
            }
        }
        if trait_impls.is_empty() {
            return;
        }
        for sym in self.symbols.values() {
            if !matches!(sym.kind, SymbolKind::Method | SymbolKind::Function) {
                continue;
            }
            if let Some((_, _, _, tr)) = trait_impls
                .iter()
                .find(|(f, l, e, _)| f == &sym.file && sym.line >= *l && sym.line <= *e)
            {
                self.impl_index.entry(tr.clone()).or_default().push(sym.id);
            }
        }
    }

    /// Method-set matching cho Go implicit interface (structural typing):
    /// type T implements interface I ⟺ tập method name của I ⊆ tập method name
    /// của T. Nguồn: method symbols có `type_name` = receiver base type
    /// (`go_receiver_type`) + method scope vào Interface symbol (language go).
    fn build_go_method_sets(&mut self) {
        let mut type_methods: HashMap<String, HashSet<String>> = HashMap::new();
        let mut iface_methods: HashMap<String, Vec<String>> = HashMap::new();
        for sym in self.symbols.values() {
            if sym.language != "go"
                || !matches!(sym.kind, SymbolKind::Method | SymbolKind::Function)
            {
                continue;
            }
            if let Some(recv) = &sym.type_name {
                type_methods
                    .entry(recv.to_lowercase())
                    .or_default()
                    .insert(sym.name.to_lowercase());
            }
            if let Some(tr) = self.symbols.get(&sym.scope_id)
                && tr.kind == SymbolKind::Interface
                && tr.language == "go"
            {
                iface_methods
                    .entry(tr.name.to_lowercase())
                    .or_default()
                    .push(sym.name.to_lowercase());
            }
        }
        // Satisfaction: mọi method name của iface nằm trong method set của
        // type → mọi method CỦA TYPE ĐÓ (receiver type khớp) là candidates.
        let mut go_impls: HashMap<String, Vec<u64>> = HashMap::new();
        for (iface, needed) in &iface_methods {
            for (ty, methods) in &type_methods {
                if !needed.iter().all(|m| methods.contains(m)) {
                    continue;
                }
                let implementing: Vec<u64> = self
                    .symbols
                    .values()
                    .filter(|s| {
                        s.language == "go"
                            && matches!(s.kind, SymbolKind::Method | SymbolKind::Function)
                            && s.type_name.as_deref().map(|t| t.to_lowercase()) == Some(ty.clone())
                    })
                    .map(|s| s.id)
                    .collect();
                go_impls
                    .entry(iface.clone())
                    .or_default()
                    .extend(implementing);
            }
        }
        self.go_impls = go_impls;
    }

    /// Resolve một call record về real symbol id — trả `None` nếu không được.
    ///
    /// Ưu tiên: structural hint (TargetClass/TargetMethod — VD Java class
    /// literal) → exact name → short name (phần sau dấu chấm) → best-candidate
    /// (@Override +10 / has-chain +5 / same-file +3) → hierarchy fan-out
    /// (dynamic dispatch — chỉ khi mọi đường tĩnh thất bại).
    fn resolve_call_placeholder(&self, call: &CallRecord, caller_id: u64) -> CallResolution {
        // 1. Try class/method target hints (used mainly for Java).
        if let (Some(tc), Some(tm)) = (&call.target_class, &call.target_method)
            && let Some(id) = self.lookup_method_of_class(tc, tm)
        {
            return CallResolution::Single(id, ResolutionKind::Exact);
        }

        // 2. Direct name lookup (full qualified name).
        let mut candidates: Vec<u64> = self
            .name_index
            .get(&call.call_name.to_lowercase())
            .cloned()
            .unwrap_or_default();

        // 3. Independent lookup paths: alias ("var.method" → "TypeName.method")
        //    AND short name fallback — merge results instead of sequential fallback.
        let mut via_alias = false;
        if candidates.is_empty() {
            // 3a. Go/Import alias: resolve "var.method" via field type to "TypeName.method".
            if let Some(qualified) = self.alias_qualified_name(caller_id, &call.call_name) {
                let alias_ids = self.name_index.get(&qualified).cloned().unwrap_or_default();
                if !alias_ids.is_empty() {
                    via_alias = true;
                }
                candidates.extend(alias_ids);
            }

            // 3a-bis. Import alias: `httpx.Get` với `httpx "corp/api/orders"`
            // → method `Get` trong file chứa segment "orders".
            let import_ids = self.import_alias_candidates(caller_id, &call.call_name);
            if !import_ids.is_empty() {
                via_alias = true;
                candidates.extend(import_ids);
            }

            // 3b. Short name fallback (after last dot).
            let short = call
                .call_name
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_lowercase();
            if !short.is_empty() {
                let short_ids = self.name_index.get(&short).cloned().unwrap_or_default();
                if !short_ids.is_empty() {
                    via_alias = true;
                }
                candidates.extend(short_ids);
            }
        }

        // 4. Filter to Function/Method kinds AND exclude the caller itself.
        //    The short name fallback can return the caller's own id (e.g. both
        //    LegacyAdapter.doWork and Service.doWork match "dowork"), so we must
        //    eliminate the caller before scoring.
        candidates.retain(|&id| {
            id != caller_id
                && self
                    .symbols
                    .get(&id)
                    .is_some_and(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        });

        if !candidates.is_empty() {
            // Dynamic dispatch ưu tiên best-guess: nếu candidates chứa method
            // KHAI BÁO trong trait/interface (không phải impl), đây gần như chắc
            // chắn là dispatch động — chọn 1 best guess ở đây chính là bug
            // "resolve sai một cách có tự tin". Fan-out mọi impl thay vì chọn.
            // C++: method của Class nằm trong `impl_index` key (là base class
            // của class khác) cũng là virtual dispatch — base đóng vai interface.
            let mut trait_hits: Vec<(u64, u64)> = Vec::new(); // (trait id, decl method id)
            for &id in &candidates {
                let Some(m) = self.symbols.get(&id) else {
                    continue;
                };
                if !matches!(m.kind, SymbolKind::Method | SymbolKind::Function) {
                    continue;
                }
                let Some(tr) = self.symbols.get(&m.scope_id) else {
                    continue;
                };
                let is_iface = tr.kind == SymbolKind::Interface;
                let is_base = tr.kind == SymbolKind::Class
                    && self.impl_index.contains_key(&tr.name.to_lowercase());
                if is_iface || is_base {
                    trait_hits.push((tr.id, id));
                }
            }
            if !trait_hits.is_empty() {
                // Fan-out chỉ áp dụng cho method dispatch (có receiver —
                // `h.handle`). Bare call `handle` với ứng viên Function tĩnh
                // thì resolve tĩnh thắng (free function trùng tên trait method
                // là trùng hợp tên, không phải dispatch).
                let is_method_dispatch = call.call_name.contains('.');
                let all_dynamic = candidates.iter().all(|&id| {
                    self.symbols
                        .get(&id)
                        .and_then(|m| self.symbols.get(&m.scope_id))
                        .is_some_and(|tr| {
                            tr.kind == SymbolKind::Interface
                                || (tr.kind == SymbolKind::Class
                                    && self.impl_index.contains_key(&tr.name.to_lowercase()))
                        })
                });
                if is_method_dispatch || all_dynamic {
                    let short = call.call_name.rsplit('.').next().unwrap_or("");
                    return self.collect_fanout(&trait_hits, short, caller_id);
                }
            }
            let kind = if via_alias {
                ResolutionKind::Alias
            } else {
                ResolutionKind::Exact
            };
            return CallResolution::Single(
                self.pick_best_candidate(&candidates, caller_id, &call.call_name),
                kind,
            );
        }

        // 5. Dynamic dispatch fan-out: short name khớp method khai báo trong
        //    trait/interface → fan-out mọi impl (CHA-style over-approximation).
        match self.hierarchy_fanout(call, caller_id) {
            CallResolution::None => {}
            res => return res,
        }

        // 6. Function pointer assign-site (C/C++): `p = f;` tracked qua
        //    `FnPtrAssign` → call `p(...)` fan-out các target đã gán.
        if !call.call_name.contains('.')
            && let Some(targets) = self.fn_ptr_index.get(&call.call_name.to_lowercase())
        {
            let cands: Vec<u64> = targets
                .iter()
                .copied()
                .filter(|&id| id != caller_id)
                .collect();
            let kind = if cands.is_empty() {
                ResolutionKind::UnresolvedDynamic
            } else {
                ResolutionKind::PointerAssign
            };
            return CallResolution::FanOut(cands, kind);
        }

        CallResolution::None
    }

    /// Gom candidates fan-out từ các "interface source" (trait/interface Rust,
    /// interface Java/Go, base class C++).
    ///
    /// - Rust/Java/C++: candidates = impl methods của source (qua `impl_index`),
    ///   lọc cùng tên method. C++ thêm chính method của base (non-pure virtual
    ///   có body — là target khả dĩ).
    /// - Go: interface thỏa ngầm, không có implements clause → candidates =
    ///   mọi method cùng tên trong repo (CHA naive).
    /// - Không candidate nào → `FanOut([], UnresolvedDynamic)` (flag ranh giới
    ///   soundiness — reflection/monkey-patch rơi vào đây).
    fn collect_fanout(&self, hits: &[(u64, u64)], short: &str, caller_id: u64) -> CallResolution {
        let mut cands: Vec<u64> = Vec::new();
        for &(tr_id, decl_id) in hits {
            let Some(tr) = self.symbols.get(&tr_id) else {
                continue;
            };
            let key = tr.name.to_lowercase();
            if tr.language == "go" {
                let short_lower = short.to_lowercase();
                // Ưu tiên method-set matching: mọi method của các type THỎA
                // interface (structural typing) — chính xác hơn naive CHA.
                let mut matched = false;
                if let Some(ids) = self.go_impls.get(&key) {
                    let hit: Vec<u64> = ids
                        .iter()
                        .copied()
                        .filter(|&id| {
                            id != caller_id
                                && self
                                    .symbols
                                    .get(&id)
                                    .is_some_and(|s| s.name.eq_ignore_ascii_case(short))
                        })
                        .collect();
                    if !hit.is_empty() {
                        cands.extend(hit);
                        matched = true;
                    }
                }
                if matched {
                    continue;
                }
                // Fallback naive CHA: interface method chưa extract đủ để build
                // method-set → fan-out mọi method cùng tên (không mất soundness).
                if let Some(ids) = self.name_index.get(&short_lower) {
                    cands.extend(ids.iter().copied().filter(|&id| {
                        id != caller_id
                            && self.symbols.get(&id).is_some_and(|s| {
                                s.kind == SymbolKind::Method
                                    && self
                                        .symbols
                                        .get(&s.scope_id)
                                        .is_none_or(|p| p.kind != SymbolKind::Interface)
                            })
                    }));
                }
                continue;
            }
            if let Some(ids) = self.impl_index.get(&key) {
                cands.extend(ids.iter().copied().filter(|&id| {
                    id != caller_id
                        && self
                            .symbols
                            .get(&id)
                            .is_some_and(|s| s.name.eq_ignore_ascii_case(short))
                }));
            }
            // C++: method của base class (non-pure virtual) là target khả dĩ.
            // Java/Rust interface method là abstract → loại decl.
            if tr.kind == SymbolKind::Class {
                cands.push(decl_id);
            }
        }
        cands.sort_unstable();
        cands.dedup();
        let kind = if cands.is_empty() {
            ResolutionKind::UnresolvedDynamic
        } else {
            ResolutionKind::HierarchyFanOut
        };
        CallResolution::FanOut(cands, kind)
    }

    /// Fan-out qua trait/interface hierarchy — đường resolve cuối cùng, chỉ chạy
    /// khi mọi đường tĩnh thất bại. Short name khớp method thuộc `Interface`
    /// symbol → candidates = mọi impl method cùng tên (`impl_index`).
    ///
    /// - Có impl → `FanOut(candidates, HierarchyFanOut)`.
    /// - Trait method nhưng repo không có impl nào → `FanOut([], UnresolvedDynamic)`
    ///   (flag ranh giới soundiness — reflection/monkey-patch rơi vào đây).
    /// - Không khớp trait method nào → `None` (behavior cũ).
    fn hierarchy_fanout(&self, call: &CallRecord, caller_id: u64) -> CallResolution {
        let short = call
            .call_name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();
        if short.is_empty() {
            return CallResolution::None;
        }
        let Some(method_ids) = self.name_index.get(&short) else {
            return CallResolution::None;
        };
        // Method của trait/interface nào mang tên này?
        let mut trait_hits: Vec<(u64, u64)> = Vec::new(); // (trait id, decl method id)
        for &mid in method_ids {
            let Some(m) = self.symbols.get(&mid) else {
                continue;
            };
            if !matches!(m.kind, SymbolKind::Method | SymbolKind::Function) {
                continue;
            }
            if let Some(tr) = self.symbols.get(&m.scope_id)
                && tr.kind == SymbolKind::Interface
            {
                trait_hits.push((tr.id, mid));
            }
        }
        if trait_hits.is_empty() {
            return CallResolution::None;
        }
        self.collect_fanout(&trait_hits, &short, caller_id)
    }

    /// Tìm method của class theo tên (scope_id == class id).
    fn lookup_method_of_class(&self, class_name: &str, method_name: &str) -> Option<u64> {
        let class_ids = self.name_index.get(&class_name.to_lowercase())?;
        let method_ids = self.name_index.get(&method_name.to_lowercase())?;
        for &cid in class_ids {
            let class_sym = self.symbols.get(&cid)?;
            if !matches!(class_sym.kind, SymbolKind::Class | SymbolKind::Interface) {
                continue;
            }
            for &mid in method_ids {
                let m = self.symbols.get(&mid)?;
                if matches!(m.kind, SymbolKind::Function | SymbolKind::Method) && m.scope_id == cid
                {
                    return Some(mid);
                }
            }
        }
        None
    }

    /// Chọn ứng viên tốt nhất trong danh sách trùng tên.
    ///
    /// Khi nhiều method cùng tên, dùng typed-proximity tie-breaker: nếu call_name
    /// dạng `var.method` và `var` là field khai báo kiểu `TypeName` trong caller
    /// scope, ưu tiên method thuộc class `TypeName` (bug của Router.route:
    /// `legacyAdapter.doWork` từng bị resolve nhầm sang `Service.doWork` vì cả
    /// hai cùng score +9).
    fn pick_best_candidate(&self, candidates: &[u64], caller_id: u64, call_name: &str) -> u64 {
        if candidates.len() == 1 {
            return candidates[0];
        }

        // Trích declared type của field từ call_name (nếu call_name dạng "var.method").
        // VD: "legacyAdapter.doWork" → field `legacyAdapter` trong caller scope
        //     → type_name = "LegacyAdapter" → trả "legacyadapter" (lower).
        let field_type = self.field_declared_type(caller_id, call_name);

        let caller_file = self.symbols.get(&caller_id).map(|s| s.file.clone());
        let mut best = candidates[0];
        let mut best_score = i32::MIN;
        for &id in candidates {
            let Some(sym) = self.symbols.get(&id) else {
                continue;
            };
            let mut score = 0;
            if sym.annotations.iter().any(|a| a.name == "Override") {
                score += 10;
            }
            if matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
                // Ưu tiên callee-thực-sự (hàm/method) hơn symbol trùng tên khác
                // kind (Variable/Parameter/Field...). (bug C)
                score += 4;
            }
            if self.chains_map.contains_key(&id) {
                score += 5;
            }
            if let Some(f) = &caller_file
                && &sym.file == f
            {
                score += 3;
            }
            // Typed proximity: +8 nếu enclosing class của candidate khớp với
            // declared type của field gọi (cao hơn same-file +3 để thắng).
            if let Some(ref ft) = field_type
                && let Some(class_sym) = self
                    .symbols
                    .get(&sym.scope_id)
                    .filter(|cs| matches!(cs.kind, SymbolKind::Class | SymbolKind::Interface))
                && class_sym.name.to_lowercase() == *ft
            {
                score += 8;
            }
            if score > best_score {
                best_score = score;
                best = id;
            }
        }
        best
    }

    /// Tìm declared type của field từ call_name dạng `var.method`.
    ///
    /// Trả về `Some("legacyadapter")` nếu caller scope có field `legacyAdapter`
    /// với `type_name = "LegacyAdapter"`. Trả `None` nếu call_name không có dấu
    /// chấm, field chưa được index, hoặc field không có type_name.
    fn field_declared_type(&self, caller_id: u64, call_name: &str) -> Option<String> {
        // Lấy tên field/receiver phía trước dấu `.` cuối.
        // VD: "legacyAdapter.doWork" → "legacyAdapter", "doWork" → None (không có receiver).
        let field_name = call_name
            .rsplit('.')
            .nth(1)
            .or_else(|| call_name.rsplit('.').next())?;
        if field_name == call_name {
            // Không có dấu '.' → call_name chính là tên method, không phải dạng `var.method`.
            return None;
        }
        let caller_scope_id = self.symbols.get(&caller_id)?.scope_id;

        for sym in self.symbols.values() {
            if sym.scope_id == caller_scope_id
                && sym.name == field_name
                && matches!(
                    sym.kind,
                    SymbolKind::Field | SymbolKind::Variable | SymbolKind::Parameter
                )
            {
                return sym.type_name.as_ref().map(|t| t.to_lowercase());
            }
        }
        None
    }

    /// Build call-name index từ chains (đã resolve) + call records; persist call
    /// records + call-name index (kèm alias type-qualified `svc.validate` →
    /// `type.validate`). Đồng thời đếm `edge_count` cho stats.
    ///
    /// Edge model: mọi symbol element trong chain là một callee — call record
    /// chỉ bổ sung metadata theo position. Chain dựng thẳng (không qua
    /// placeholder) vẫn sinh edge đủ. Metadata KHÔNG materialize thành map
    /// (xem `edge_count`): `flow()` đọc thẳng từ call records persist.
    async fn build_call_indexes(
        &mut self,
        calls: &[CallRef<'_>],
        call_meta: &HashMap<(u64, usize), (ResolutionKind, Vec<u64>)>,
        progress: Option<&dyn IngestProgress>,
    ) -> Result<()> {
        // Chỉ lưu **index** vào `calls` (4 B) thay vì clone `CallRecord`
        // (176 B + heap) — `calls` sống tới hết hàm nên vẫn tra được.
        let mut recs_by_caller: HashMap<u64, Vec<u32>> = HashMap::new();
        for (i, entry) in calls.iter().enumerate() {
            let (c, caller) = (entry.rec, entry.caller);
            recs_by_caller.entry(caller).or_default().push(i as u32);

            // Record nhãn nhánh (`call_name` rỗng, `condition` = trigger) vẫn được
            // persist để `flow()` đọc lại — nhưng KHÔNG vào call-name index
            // (không phải call thật).
            if c.call_name.is_empty() {
                continue;
            }

            // Call-site index: key theo tên thô + alias type-qualified (nếu có).
            let (resolved_via, candidates) = call_meta
                .get(&(caller, c.position))
                .map(|(k, cands)| (*k, cands.len() as u32))
                .unwrap_or((ResolutionKind::Exact, 0));
            let site = CallSite {
                caller_id: caller,
                call_name: c.call_name.clone(),
                line: c.line,
                condition: c.condition.clone(),
                is_loop_body: c.is_loop_body,
                arg_exprs: c.arg_exprs.clone(),
                resolved_via,
                candidates,
            };
            let raw_key = c.call_name.to_lowercase();
            self.call_names
                .entry(raw_key.clone())
                .or_default()
                .push(site.clone());
            if let Some(alias) = self.alias_qualified_name(caller, &c.call_name)
                && alias != raw_key
            {
                self.call_names.entry(alias).or_default().push(site);
            }
        }

        // Đếm edges từ mọi chain — metadata không materialize (đã nằm trong
        // call records persist; `flow()` đọc thẳng từ đó).
        // Vị trí 0 = chính func id (owner) — không phải call.
        // Dynamic fan-out: mỗi candidate là một edge ẩn trên chain slot 0.
        self.edge_count = self
            .chains_map
            .values()
            .map(|chain| {
                chain
                    .iter()
                    .skip(1)
                    .filter(|&&e| e != 0 && !is_marker(e))
                    .count() as u64
            })
            .sum::<u64>()
            + call_meta
                .values()
                .map(|(_, cands)| cands.len() as u64)
                .sum::<u64>();

        // Persist call records (gom theo caller) — inject metadata resolve
        // (`resolved_via` + `candidates`) vào bản clone trước khi serialize.
        if let Some(p) = progress
            && !recs_by_caller.is_empty()
        {
            p.phase("save call records", recs_by_caller.len());
        }
        for (caller, idxs) in recs_by_caller {
            let recs: Vec<CallRecord> = idxs
                .iter()
                .map(|&i| {
                    let c = calls[i as usize].rec;
                    match call_meta.get(&(caller, c.position)) {
                        Some((kind, cands)) => {
                            let mut r = c.clone();
                            r.resolved_via = *kind;
                            r.candidates = cands.clone();
                            r
                        }
                        None => c.clone(),
                    }
                })
                .collect();
            // `rec.caller_id` ghi ra đây là id **local** của file, không phải
            // id global — vô hại: mọi reader (`flow`, `bingraph`) lấy caller id
            // từ **key** của blob, không đọc field này. Nếu sau này cần id
            // global thì đừng đọc field — dùng key.
            let bytes = serde_json::to_vec(&recs).map_err(|e| Error::Search(e.to_string()))?;
            self.storage
                .write()
                .await
                .set_call_records(caller, &bytes)
                .await
                .map_err(serr)?;
            if let Some(p) = progress {
                p.advance(1);
            }
        }
        // Persist call-name index.
        if let Some(p) = progress
            && !self.call_names.is_empty()
        {
            p.phase("save call-name index", self.call_names.len());
        }
        for (name, sites) in &self.call_names {
            let bytes = serde_json::to_vec(sites).map_err(|e| Error::Search(e.to_string()))?;
            self.storage
                .write()
                .await
                .set_call_name_index(name, &bytes)
                .await
                .map_err(serr)?;
            if let Some(p) = progress {
                p.advance(1);
            }
        }
        Ok(())
    }

    /// Resolve call `var.Method` khi `var` là import alias của file caller:
    /// tìm method `Method` trong các symbol mà file chứa segment cuối của
    /// import path (monorepo: import "corp/api/orders" → file chứa "orders").
    /// Stdlib ngoài index → trả rỗng (giữ raw, không fake).
    fn import_alias_candidates(&self, caller_id: u64, call_name: &str) -> Vec<u64> {
        let Some(dot) = call_name.find('.') else {
            return Vec::new();
        };
        let var = &call_name[..dot];
        let method = &call_name[dot + 1..];
        if method.is_empty() || method.contains('.') {
            return Vec::new();
        }
        let Some(file) = self.symbols.get(&caller_id).map(|s| s.file.as_str()) else {
            return Vec::new();
        };
        let Some(imports) = self.file_imports.get(file) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for imp in imports {
            if imp.alias != var {
                continue;
            }
            let tail = imp.path.rsplit('/').next().unwrap_or("");
            if let Some(ids) = self.name_index.get(&method.to_lowercase()) {
                out.extend(ids.iter().copied().filter(|&id| {
                    self.symbols.get(&id).is_some_and(|s| {
                        matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
                            && !tail.is_empty()
                            && s.file.contains(tail)
                    })
                }));
            }
        }
        out
    }

    /// Alias type-qualified: `svc.validate` → `orderservice.validate` khi caller
    /// có var `svc` với `type_name = "orderservice.OrderService"` trong scope.
    fn alias_qualified_name(&self, caller_id: u64, call_name: &str) -> Option<String> {
        let dot = call_name.find('.')?;
        let var = &call_name[..dot];
        let mut scopes = vec![caller_id];
        if let Some(s) = self.symbols.get(&caller_id)
            && s.scope_id != 0
        {
            scopes.push(s.scope_id);
        }
        for sid in scopes {
            if let Some(ids) = self.scope_index.get(&sid) {
                for id in ids {
                    let sym = self.symbols.get(id)?;
                    if sym.name == var
                        && let Some(tn) = &sym.type_name
                    {
                        let rest = &call_name[dot + 1..];
                        return Some(format!("{}.{}", tn.to_lowercase(), rest.to_lowercase()));
                    }
                }
            }
        }
        None
    }

    // ── Queries (mọi search đều qua `search_symbol_paged_resumable` — resumable,
    // deadline-aware; các hàm tiện ích còn lại chỉ wrap nó, không có implementation
    // song song) ──

    /// Symbol theo id.
    pub fn symbol_by_id(&self, id: u64) -> Option<Symbol> {
        self.symbols.get(&id).cloned()
    }

    /// Resolve theo id (kèm kiểm tra tên) hoặc theo tên chính xác (case-
    /// insensitive). Trùng tên → `ambiguous = true` + toàn bộ matches.
    pub fn resolve_by_name_or_id(&self, name: &str, symbol_id: u64) -> Result<ResolveResult> {
        if symbol_id != 0 {
            let s = self
                .symbols
                .get(&symbol_id)
                .cloned()
                .ok_or_else(|| Error::Invalid(format!("symbol id {symbol_id} not found")))?;
            if !name.is_empty() && s.name != name {
                return Err(Error::Invalid(format!(
                    "symbol id {symbol_id} has name {:?}, not {name:?}",
                    s.name
                )));
            }
            return Ok(ResolveResult {
                symbol: Some(s),
                matches: Vec::new(),
                ambiguous: false,
            });
        }

        let ids = self
            .name_index
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default();
        if ids.is_empty() {
            return Err(Error::Invalid(format!("symbol {name:?} not found")));
        }
        let matches: Vec<Symbol> = ids
            .iter()
            .filter_map(|id| self.symbols.get(id).cloned())
            .collect();
        if matches.is_empty() {
            return Err(Error::Invalid(format!("symbol {name:?} not found")));
        }
        // Narrow ambiguous same-name matches to type definitions (type_ref == 0),
        // e.g. Rust `struct Foo` vs `impl Foo` which are both Class symbols with
        // the same name. Prefer the definition so class tools resolve correctly.
        if matches.len() > 1 {
            let defs: Vec<Symbol> = matches
                .iter()
                .filter(|s| s.type_ref == 0)
                .cloned()
                .collect();
            if defs.len() == 1 {
                return Ok(ResolveResult {
                    symbol: Some(defs.into_iter().next().unwrap()),
                    matches: Vec::new(),
                    ambiguous: false,
                });
            }
        }
        if matches.len() > 1 {
            return Ok(ResolveResult {
                symbol: None,
                matches,
                ambiguous: true,
            });
        }
        Ok(ResolveResult {
            symbol: Some(matches[0].clone()),
            matches: Vec::new(),
            ambiguous: false,
        })
    }

    /// Callers (transitive BFS) — `depth` = số hop tối đa (1 = direct).
    pub async fn callers(&self, id: u64, depth: usize) -> Result<Vec<Symbol>> {
        Ok(self.callers_resumable(id, depth, None, None).await?.callers)
    }

    /// Cooperative deadline across BFS, chain searches and result cloning.
    pub async fn callers_resumable(
        &self,
        id: u64,
        depth: usize,
        resume: Option<CallersCursor>,
        deadline: Option<Instant>,
    ) -> Result<CallersOutcome> {
        let depth = depth.max(1);
        if !self.symbols.contains_key(&id) {
            return Err(Error::Invalid(format!("symbol id {id} not found")));
        }
        let mut state = match resume {
            Some(c) => {
                if c.id != id || c.depth != depth || c.index_version != self.version() {
                    return Err(Error::Invalid(
                        "callers cursor does not match query or index version".into(),
                    ));
                }
                c
            }
            None => CallersCursor {
                id,
                depth,
                index_version: self.version(),
                level: 0,
                frontier: vec![id],
                frontier_pos: 0,
                next: Vec::new(),
                visited: HashSet::from([id]),
                out_ids: Vec::new(),
                search: None,
                pending: Vec::new(),
                pending_pos: 0,
                search_complete: false,
                materialize_pos: 0,
                callers: Vec::new(),
            },
        };
        loop {
            if deadline.is_some_and(|dl| Instant::now() >= dl) {
                return Ok(CallersOutcome {
                    progress: state.out_ids.len(),
                    callers: Vec::new(),
                    cursor: Some(state),
                });
            }
            if state.level >= depth || state.frontier.is_empty() {
                if state.materialize_pos < state.out_ids.len() {
                    let id = state.out_ids[state.materialize_pos];
                    if let Some(symbol) = self.symbols.get(&id) {
                        state.callers.push(symbol.clone());
                    }
                    state.materialize_pos += 1;
                    continue;
                }
                return Ok(CallersOutcome {
                    progress: state.out_ids.len(),
                    callers: state.callers,
                    cursor: None,
                });
            }
            if state.frontier_pos == state.frontier.len() {
                state.frontier = std::mem::take(&mut state.next);
                state.frontier_pos = 0;
                state.level += 1;
                continue;
            }
            let current = state.frontier[state.frontier_pos];
            if !state.search_complete {
                let page = self
                    .chains
                    .search_resumable(&[current], None, state.search.take(), deadline)
                    .await?;
                if page.timed_out {
                    state.search = Some(page.resume.ok_or_else(|| {
                        Error::Invalid("timed out chain search has no checkpoint".into())
                    })?);
                    return Ok(CallersOutcome {
                        progress: state.out_ids.len(),
                        callers: Vec::new(),
                        cursor: Some(state),
                    });
                }
                state.pending = page.record_ids;
                state.pending_pos = 0;
                state.search_complete = true;
                continue;
            }
            if state.pending_pos < state.pending.len() {
                let caller = state.pending[state.pending_pos] as u64;
                state.pending_pos += 1;
                if caller != current
                    && self.symbols.contains_key(&caller)
                    && state.visited.insert(caller)
                {
                    state.out_ids.push(caller);
                    state.next.push(caller);
                }
                continue;
            }
            state.pending.clear();
            state.search_complete = false;
            state.frontier_pos += 1;
        }
    }

    /// Callees trực tiếp — đọc chain, skip marker/0/self/seen. Không có chain
    /// (symbol không phải function / không có body) → rỗng, không lỗi.
    pub async fn callees(&self, id: u64) -> Result<Vec<Symbol>> {
        let Some(chain) = self.chains_map.get(&id).cloned() else {
            return Ok(Vec::new());
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in chain {
            if is_marker(e) || e == 0 || e == id || !seen.insert(e) {
                continue;
            }
            if let Some(s) = self.symbols.get(&e) {
                out.push(s.clone());
            }
        }
        Ok(out)
    }

    /// Flow của một hàm — chain render (marker name / symbol name / call thô
    /// cho unresolved) + call edges kèm line/condition/effect/args.
    pub async fn flow(&self, id: u64) -> Result<FlowResult> {
        let sym = self
            .symbols
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("symbol id {id} not found")))?;
        let chain = self
            .chains_map
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("chain for {:?} not found", sym.name)))?;

        // Call records (position → record) — hiện call không resolve được thành
        // symbol với tên thật thay vì "unknown(0)".
        let recs: Vec<CallRecord> = match self
            .storage
            .read()
            .await
            .get_call_records(id)
            .await
            .map_err(serr)?
        {
            Some(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            None => Vec::new(),
        };
        let rec_by_pos: HashMap<usize, &CallRecord> =
            recs.iter().map(|r| (r.position, r)).collect();

        // Nhãn trigger của marker nhánh: record có `call_name` rỗng + `condition`
        // đặt tại đúng vị trí marker (xem `emit_branch_label` phía extractor).
        let branch_labels: Vec<BranchLabel> = recs
            .iter()
            .filter(|r| {
                r.call_name.is_empty()
                    && r.condition.is_some()
                    && chain.get(r.position).is_some_and(|&e| is_marker(e))
            })
            .map(|r| BranchLabel {
                position: r.position,
                label: r.condition.clone().unwrap_or_default(),
            })
            .collect();

        let chain_desc = chain
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                if is_marker(e) {
                    marker_name(e).unwrap_or("MARKER").to_string()
                } else if let Some(s) = self.symbols.get(&e) {
                    s.name.clone()
                } else if let Some(rec) = rec_by_pos.get(&i) {
                    if !rec.call_name.is_empty() {
                        rec.call_name.clone()
                    } else {
                        format!("unknown({e})")
                    }
                } else {
                    format!("unknown({e})")
                }
            })
            .collect();

        let mut calls = Vec::new();
        for (i, &e) in chain.iter().enumerate() {
            if is_marker(e) || e == id {
                continue;
            }
            if e == 0 {
                if let Some(rec) = rec_by_pos.get(&i) {
                    if rec.call_name.is_empty() {
                        continue; // record nhãn nhánh, không phải call
                    }
                    calls.push(FlowCall {
                        position: i,
                        to_name: rec.call_name.clone(),
                        to_id: None,
                        line: rec.line,
                        condition: rec.condition.clone(),
                        effect: rec.effect,
                        effect_desc: rec.effect_desc.clone(),
                        args: rec.arg_exprs.clone(),
                        into_var: rec.into_var.clone(),
                        is_new_var: rec.is_new_var,
                        resolved_via: rec.resolved_via,
                        candidate_names: rec
                            .candidates
                            .iter()
                            .filter_map(|&cid| self.symbols.get(&cid).map(|s| s.name.clone()))
                            .collect(),
                    });
                }
                continue;
            }
            let Some(callee) = self.symbols.get(&e) else {
                continue;
            };
            let rec = rec_by_pos.get(&i);
            // Metadata (condition/effect/...) đọc thẳng từ call record — cùng
            // nguồn mà EdgeMeta cũ cũng dùng (không còn map trung gian).
            calls.push(FlowCall {
                position: i,
                to_name: callee.name.clone(),
                to_id: Some(e),
                line: rec.map(|r| r.line).unwrap_or(0),
                condition: rec.and_then(|r| r.condition.clone()),
                effect: rec.map(|r| r.effect).unwrap_or_default(),
                effect_desc: rec.and_then(|r| r.effect_desc.clone()),
                args: rec.map(|r| r.arg_exprs.clone()).unwrap_or_default(),
                into_var: rec.and_then(|r| r.into_var.clone()),
                is_new_var: rec.map(|r| r.is_new_var).unwrap_or(false),
                resolved_via: rec.map(|r| r.resolved_via).unwrap_or_default(),
                candidate_names: rec
                    .map(|r| {
                        r.candidates
                            .iter()
                            .filter_map(|&cid| self.symbols.get(&cid).map(|s| s.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }

        Ok(FlowResult {
            symbol: sym,
            chain,
            chain_desc,
            calls,
            branch_labels,
        })
    }

    // ── Full-flow trace (debug-style call/return path) ──

    /// Trace toàn bộ luồng từ các hàm caller đến `target_id`, rồi dò hết
    /// các tuyến đường khả dĩ bên trong `target_id` (theo chain markers:
    /// IF_TRUE/IF_FALSE/BRANCH_END/RETURN/THROW/LOOP/...). Mỗi path kèm
    /// stacktrace để biết lúc nào gọi xuống / lần về.
    ///
    /// - `max_depth`: giới hạn độ sâu tìm caller ngược lên.
    /// - `max_paths`: số path tối đa trả về (tránh explosion do nhánh).
    /// - `max_steps`: tổng số bước (call+return+branch) tối đa mỗi path.
    pub async fn trace_full_flow(
        &self,
        target_id: u64,
        max_depth: usize,
        max_paths: usize,
        max_loop_iter: usize,
    ) -> Result<Vec<FullFlowPath>> {
        // max_loop_iter: số lần tối đa trace loop body (mặc định 1 — body chạy
        // 1 lần rồi exit qua max_iter; unroll thêm chỉ nhân bản y hệt body).
        let max_loop_iter = max_loop_iter.max(1);

        // ── Kiểm tra target tồn tại ──
        if !self.symbols.contains_key(&target_id) {
            return Err(Error::Invalid(format!("symbol id {target_id} not found")));
        }

        // ── Dùng flow chain của target để trace nội bộ ──
        let flow = self.flow(target_id).await?;

        let mut all_paths = Vec::new();

        // ── Tìm caller đến target qua edges — transitive tới max_depth để
        // throw unwind được qua nhiều tầng caller ──
        let direct_callers = self.callers(target_id, max_depth).await?;

        if direct_callers.is_empty() {
            // Không có caller - trace chỉ nội bộ target bắt đầu từ target
            let mut base_steps: Vec<FlowStep> = Vec::new();
            let target_sym = self.symbols.get(&target_id);
            base_steps.push(FlowStep {
                symbol_id: target_id,
                symbol_name: target_sym.map(|s| s.name.clone()).unwrap_or_default(),
                file: target_sym.map(|s| s.file.clone()).unwrap_or_default(),
                line: target_sym.map(|s| s.line).unwrap_or(0),
                action: TraceAction::Call,
                stack_depth: 1,
                condition: None,
            });

            self.run_flow_sim(
                target_id,
                &flow,
                base_steps,
                Vec::new(),
                &mut all_paths,
                max_paths,
                max_loop_iter,
            )
            .await;
        } else {
            // Với mỗi caller, tìm đường caller → ... → target
            for caller in &direct_callers {
                if all_paths.len() >= max_paths {
                    break;
                }
                let caller_id = caller.id;
                // Tìm path từ caller đến target
                let mut current: Vec<u64> = vec![caller_id];
                let mut visited: HashSet<u64> = HashSet::from([caller_id]);
                let mut paths_to_target: Vec<Vec<u64>> = Vec::new();

                Box::pin(self.dfs_call_paths_to_target(
                    caller_id,
                    target_id,
                    &mut current,
                    &mut visited,
                    max_depth,
                    max_paths,
                    &mut paths_to_target,
                ))
                .await;

                for call_path in paths_to_target
                    .into_iter()
                    .take(max_paths - all_paths.len())
                {
                    // Steps từ call path
                    let mut base_steps: Vec<FlowStep> = Vec::new();
                    for (i, &sid) in call_path.iter().enumerate() {
                        let sym = self.symbols.get(&sid);
                        base_steps.push(FlowStep {
                            symbol_id: sid,
                            symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                            file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                            line: sym.map(|s| s.line).unwrap_or(0),
                            action: TraceAction::Call,
                            stack_depth: i + 1,
                            condition: None,
                        });
                    }

                    // Trace nội bộ target (descend vào callee, throw→catch...)
                    let base_callers = call_path[..call_path.len() - 1].to_vec();
                    self.run_flow_sim(
                        target_id,
                        &flow,
                        base_steps,
                        base_callers,
                        &mut all_paths,
                        max_paths,
                        max_loop_iter,
                    )
                    .await;
                    if all_paths.len() >= max_paths {
                        break;
                    }
                }
            }
        }

        // Rank paths theo độ giàu thông tin trước khi cắt max_paths: path có
        // nhiều Call steps (đi sâu vào callee) đứng trước — early-return paths
        // hoàn tất sớm trong DFS không được phép chiếm hết max_paths.
        let completed =
            |p: &FullFlowPath| matches!(p.ended_with, PathEnding::Returned | PathEnding::Threw);
        all_paths.sort_by(|a, b| {
            let calls = |p: &FullFlowPath| {
                p.steps
                    .iter()
                    .filter(|s| s.action == TraceAction::Call && s.symbol_id != 0)
                    .count()
            };
            let depth = |p: &FullFlowPath| p.steps.iter().map(|s| s.stack_depth).max().unwrap_or(0);
            completed(b)
                .cmp(&completed(a))
                .then(calls(b).cmp(&calls(a)))
                .then(depth(b).cmp(&depth(a)))
        });
        if all_paths.len() > max_paths {
            all_paths.truncate(max_paths);
        }

        Ok(all_paths)
    }

    /// DFS tìm đường từ start đến target (dùng call graph - callees)
    #[allow(clippy::too_many_arguments)]
    async fn dfs_call_paths_to_target(
        &self,
        _start_id: u64,
        target_id: u64,
        current: &mut Vec<u64>,
        visited: &mut HashSet<u64>,
        max_depth: usize,
        max_paths: usize,
        paths: &mut Vec<Vec<u64>>,
    ) {
        if paths.len() >= max_paths {
            return;
        }
        // max_depth = số caller hops; path caller→target dài hops+1.
        if current.len() > max_depth + 1 {
            return;
        }
        let last = *current.last().unwrap();
        if last == target_id && current.len() > 1 {
            paths.push(current.clone());
            return;
        }
        if last == target_id && current.len() == 1 {
            paths.push(current.clone());
            return;
        }
        // Lấy callees của last
        if let Ok(callees_list) = self.callees(last).await {
            for c in callees_list {
                if paths.len() >= max_paths {
                    break;
                }
                if visited.contains(&c.id) {
                    continue;
                }
                current.push(c.id);
                visited.insert(c.id);
                Box::pin(self.dfs_call_paths_to_target(
                    c.id, target_id, current, visited, max_depth, max_paths, paths,
                ))
                .await;
                visited.remove(&c.id);
                current.pop();
                if current.is_empty() {
                    break;
                }
            }
        }
    }

    /// DFS tìm tất cả call path từ `start_id` đến `target_id` trên call graph
    /// (dùng `callees`). `visited` tránh cycle. Giới hạn `max_depth` hop.
    #[allow(dead_code)]
    async fn find_call_paths(
        &self,
        start_id: u64,
        target_id: u64,
        max_depth: usize,
        max_paths: usize,
    ) -> Result<Vec<Vec<u64>>> {
        let mut paths: Vec<Vec<u64>> = Vec::new();
        let mut current: Vec<u64> = vec![start_id];
        let mut visited: HashSet<u64> = HashSet::from([start_id]);

        self.dfs_call_paths(
            start_id,
            target_id,
            0,
            max_depth,
            max_paths,
            &mut current,
            &mut visited,
            &mut paths,
        )
        .await?;

        Ok(paths)
    }

    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    async fn dfs_call_paths(
        &self,
        current_id: u64,
        target_id: u64,
        depth: usize,
        max_depth: usize,
        max_paths: usize,
        current: &mut Vec<u64>,
        visited: &mut HashSet<u64>,
        paths: &mut Vec<Vec<u64>>,
    ) -> Result<()> {
        if paths.len() >= max_paths {
            return Ok(());
        }
        if depth > max_depth {
            return Ok(());
        }
        if current_id == target_id && current.len() > 1 {
            paths.push(current.clone());
            return Ok(());
        }

        let callees = self.callees(current_id).await?;
        for callee in &callees {
            let cid = callee.id;
            if visited.contains(&cid) {
                continue;
            }
            visited.insert(cid);
            current.push(cid);
            Box::pin(self.dfs_call_paths(
                cid,
                target_id,
                depth + 1,
                max_depth,
                max_paths,
                current,
                visited,
                paths,
            ))
            .await?;
            current.pop();
            visited.remove(&cid);
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    async fn run_flow_sim(
        &self,
        root_func_id: u64,
        root_flow: &FlowResult,
        base_steps: Vec<FlowStep>,
        base_callers: Vec<u64>,
        paths: &mut Vec<FullFlowPath>,
        max_paths: usize,
        max_loop_iter: usize,
    ) {
        let base_depth = base_callers.len();
        let mut work: Vec<SimState> = vec![SimState {
            frames: vec![SimFrame::from_flow(root_func_id, root_flow)],
            steps: base_steps,
            loop_iters: HashMap::new(),
            base_depth,
            pending_callers: base_callers,
        }];
        let mut seen: HashSet<u64> = HashSet::new();
        seen.insert(state_key(&work[0]));
        let mut total_steps = 0usize;
        let mut flow_cache: HashMap<u64, Option<FlowResult>> = HashMap::new();

        while let Some(st) = work.pop() {
            if paths.len() >= max_paths || total_steps >= FLOW_TOTAL_STEPS {
                break;
            }
            if let Some(rest) = self
                .sim_run_state(
                    st,
                    &mut work,
                    &mut seen,
                    &mut flow_cache,
                    paths,
                    max_paths,
                    max_loop_iter,
                    &mut total_steps,
                )
                .await
            {
                // State dở dang (hết ngân sách state): đẩy về ĐẦU worklist để
                // resume theo kiểu round-robin — path sâu không bị bỏ chết.
                work.insert(0, rest);
            }
        }
    }

    /// Chạy một path-state đến khi path kết thúc (push vào `paths`), state hết
    /// ngân sách, hoặc fork — các fork được push vào `work` để xử lý tiếp.
    #[allow(clippy::too_many_arguments)]
    async fn sim_run_state(
        &self,
        mut st: SimState,
        work: &mut Vec<SimState>,
        seen: &mut HashSet<u64>,
        flow_cache: &mut HashMap<u64, Option<FlowResult>>,
        paths: &mut Vec<FullFlowPath>,
        max_paths: usize,
        max_loop_iter: usize,
        total_steps: &mut usize,
    ) -> Option<SimState> {
        // Ngân sách mỗi lượt chạy state — hết预算 thì trả state về để resume
        // (round-robin), path sâu không bị bỏ giữa đường.
        let state_budget = 2000usize;
        let mut used = 0usize;

        while !st.frames.is_empty() {
            if *total_steps >= FLOW_TOTAL_STEPS || paths.len() >= max_paths || work.len() > 4096 {
                return None;
            }
            if used >= state_budget {
                return Some(st);
            }
            used += 1;
            *total_steps += 1;
            if st.steps.len() >= MAX_PATH_STEPS {
                paths.push(FullFlowPath {
                    steps: st.steps.clone(),
                    ended_with: PathEnding::DepthLimit,
                });
                return None;
            }

            // Vị trí hiện tại của frame top.
            let (pos, elem) = {
                let f = st.frames.last().unwrap();
                let Some(&e) = f.chain.get(f.pos) else {
                    // Chain hết tự nhiên → implicit return: pop frame.
                    let f = st.frames.pop().unwrap();
                    let d = st.base_depth + st.frames.len() + 1;
                    let sym = self.symbols.get(&f.func_id);
                    st.steps.push(FlowStep {
                        symbol_id: f.func_id,
                        symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                        file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                        line: sym.map(|s| s.line).unwrap_or(0),
                        action: TraceAction::Return,
                        stack_depth: d,
                        condition: None,
                    });
                    if st.frames.is_empty() {
                        paths.push(FullFlowPath {
                            steps: st.steps.clone(),
                            ended_with: PathEnding::Returned,
                        });
                        return None;
                    }
                    continue;
                };
                (f.pos, e)
            };

            if is_marker(elem) {
                match elem {
                    MARKER_RETURN => {
                        let f = st.frames.pop().unwrap();
                        let d = st.base_depth + st.frames.len() + 1;
                        let sym = self.symbols.get(&f.func_id);
                        st.steps.push(FlowStep {
                            symbol_id: f.func_id,
                            symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                            file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                            line: sym.map(|s| s.line).unwrap_or(0),
                            action: TraceAction::Return,
                            stack_depth: d,
                            condition: None,
                        });
                        if st.frames.is_empty() {
                            paths.push(FullFlowPath {
                                steps: st.steps.clone(),
                                ended_with: PathEnding::Returned,
                            });
                            return None;
                        }
                        continue;
                    }
                    MARKER_THROW => {
                        let thrown_label = st.frames.last().unwrap().labels.get(&pos).cloned();
                        let thrown_type = thrown_label.as_deref();
                        {
                            let f = st.frames.last().unwrap();
                            let d = st.base_depth + st.frames.len();
                            let sym = self.symbols.get(&f.func_id);
                            st.steps.push(FlowStep {
                                symbol_id: f.func_id,
                                symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                                file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                                line: sym.map(|s| s.line).unwrap_or(0),
                                action: TraceAction::Throw,
                                stack_depth: d,
                                condition: thrown_label.clone(),
                            });
                        }
                        // Unwind: tìm frame có handler khớp exception type; hết
                        // frame thì thử qua caller path (pending_callers).
                        let mut caught = false;
                        loop {
                            let f = st.frames.last_mut().unwrap();
                            if let Some(&try_start) = f.catch_stack.last() {
                                let try_end =
                                    matching_try_end(&f.chain, try_start).unwrap_or(f.chain.len());
                                let handlers = handler_positions(&f.chain, try_end);
                                if let Some(&cp) = handlers.iter().find(|&&p| {
                                    catch_matches(thrown_type, f.labels.get(&p).map(|s| s.as_str()))
                                }) {
                                    f.catch_stack.pop();
                                    let hend = matching_branch_end(&f.chain, cp)
                                        .map(|e| e + 1)
                                        .unwrap_or(f.chain.len());
                                    f.handlers.push(hend);
                                    f.pos = cp + 1;
                                    caught = true;
                                    break;
                                }
                                f.catch_stack.pop();
                            }
                            // Frame này không catch → pop, ghi step unwind.
                            let f = st.frames.pop().unwrap();
                            let mut unwound_from = f.func_id;
                            if st.frames.is_empty() {
                                // Unwind qua caller path (base path của trace).
                                while let Some(cid) = st.pending_callers.pop() {
                                    let d = st.pending_callers.len() + 1;
                                    st.steps.push(FlowStep {
                                        symbol_id: cid,
                                        symbol_name: self
                                            .symbols
                                            .get(&cid)
                                            .map(|s| s.name.clone())
                                            .unwrap_or_default(),
                                        file: self
                                            .symbols
                                            .get(&cid)
                                            .map(|s| s.file.clone())
                                            .unwrap_or_default(),
                                        line: self.symbols.get(&cid).map(|s| s.line).unwrap_or(0),
                                        action: TraceAction::Throw,
                                        stack_depth: d,
                                        condition: Some("unwound".into()),
                                    });
                                    // Caller có try bọc call site khớp type?
                                    let fr = match flow_cache.get(&cid) {
                                        Some(r) => r.clone(),
                                        None => {
                                            let r = self.flow(cid).await.ok();
                                            flow_cache.insert(cid, r.clone());
                                            r
                                        }
                                    };
                                    let Some(fr) = fr else {
                                        unwound_from = cid;
                                        continue;
                                    };
                                    let call_pos = fr
                                        .chain
                                        .iter()
                                        .position(|&e| e == unwound_from)
                                        .filter(|&p| p > 0);
                                    let Some(cp_pos) = call_pos else {
                                        unwound_from = cid;
                                        continue;
                                    };
                                    // Các TRY_START còn mở tại call site.
                                    let mut open_tries: Vec<usize> = Vec::new();
                                    for (i, &e) in fr.chain.iter().enumerate().take(cp_pos) {
                                        if e == MARKER_TRY_START {
                                            open_tries.push(i);
                                        } else if e == MARKER_TRY_END {
                                            open_tries.pop();
                                        }
                                    }
                                    let mut handler_hit = None;
                                    eprintln!(
                                        "DBG3 cid={cid} cp={cp_pos} tries={open_tries:?} labels={:?} thrown={thrown_type:?}",
                                        fr.branch_labels
                                    );
                                    for &ts in open_tries.iter().rev() {
                                        let te = matching_try_end(&fr.chain, ts)
                                            .unwrap_or(fr.chain.len());
                                        let handlers = handler_positions(&fr.chain, te);
                                        if let Some(&cp) = handlers.iter().find(|&&hpos| {
                                            catch_matches(
                                                thrown_type,
                                                fr.branch_labels
                                                    .iter()
                                                    .find(|b| b.position == hpos)
                                                    .map(|b| b.label.as_str()),
                                            )
                                        }) {
                                            handler_hit = Some((cp, open_tries.clone(), ts));
                                            break;
                                        }
                                    }
                                    if let Some((cp, open_tries, ts)) = handler_hit {
                                        let mut nf = SimFrame::from_flow(cid, &fr);
                                        nf.pos = cp + 1;
                                        let hend = matching_branch_end(&fr.chain, cp)
                                            .map(|e| e + 1)
                                            .unwrap_or(fr.chain.len());
                                        nf.handlers.push(hend);
                                        // Try còn mở BÊN NGOÀI try vừa catch (rethrow).
                                        let idx =
                                            open_tries.iter().position(|&t| t == ts).unwrap_or(0);
                                        nf.catch_stack = open_tries[..idx].to_vec();
                                        // Caller frame thật thay thế phần base đã unwind.
                                        st.base_depth = st.pending_callers.len();
                                        st.frames.push(nf);
                                        caught = true;
                                        break;
                                    }
                                    unwound_from = cid;
                                }
                                if caught {
                                    break;
                                }
                                paths.push(FullFlowPath {
                                    steps: st.steps.clone(),
                                    ended_with: PathEnding::Threw,
                                });
                                return None;
                            }
                            let d = st.base_depth + st.frames.len() + 1;
                            let sym = self.symbols.get(&f.func_id);
                            st.steps.push(FlowStep {
                                symbol_id: f.func_id,
                                symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                                file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                                line: sym.map(|s| s.line).unwrap_or(0),
                                action: TraceAction::Throw,
                                stack_depth: d,
                                condition: Some("unwound".into()),
                            });
                        }
                        if caught {
                            continue;
                        }
                        // (unreachable — mọi nhánh của loop đã break/return)
                        continue;
                    }
                    MARKER_IF_TRUE => {
                        let (cond, end, iff, d, top_id) = {
                            let f = st.frames.last().unwrap();
                            let end = matching_branch_end(&f.chain, pos);
                            let iff = end.and_then(|e| find_if_false(&f.chain, pos, e));
                            (
                                f.labels.get(&pos).cloned(),
                                end,
                                iff,
                                st.base_depth + st.frames.len(),
                                f.func_id,
                            )
                        };
                        let branch_step = |condition: Option<String>| {
                            let sym = self.symbols.get(&top_id);
                            FlowStep {
                                symbol_id: top_id,
                                symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                                file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                                line: sym.map(|s| s.line).unwrap_or(0),
                                action: TraceAction::Branch,
                                stack_depth: d,
                                condition,
                            }
                        };
                        // Fork nhánh false: vào thẳng else body (sau IF_FALSE,
                        // nhãn !cond gắn tại fork) hoặc past matching
                        // BRANCH_END (if không else).
                        if let Some(end) = end {
                            let mut fst = st.clone();
                            {
                                let ff = fst.frames.last_mut().unwrap();
                                ff.pos = match iff {
                                    Some(p) => p + 1,
                                    None => end + 1,
                                };
                            }
                            fst.steps
                                .push(branch_step(cond.as_deref().map(|c| format!("!{c}"))));
                            let k = state_key(&fst);
                            if seen.insert(k) {
                                work.push(fst);
                            }
                        }
                        st.steps.push(branch_step(cond));
                        let f = st.frames.last_mut().unwrap();
                        f.pos += 1;
                        continue;
                    }
                    MARKER_IF_FALSE => {
                        // Chỉ true path tới đây (false path đã fork tại IF_TRUE
                        // và vào thẳng else body): nhảy qua if/else — code sau
                        // if/else luôn được trace.
                        let f = st.frames.last_mut().unwrap();
                        let end = matching_branch_end(&f.chain, pos);
                        f.pos = end.map(|e| e + 1).unwrap_or(pos + 1);
                        continue;
                    }
                    MARKER_BRANCH_END => {
                        let f = st.frames.last_mut().unwrap();
                        if f.handlers.last() == Some(&pos) {
                            // Kết thúc handler mà throw nhảy vào: bỏ qua các
                            // handler còn lại, tiếp tục flow sau try/catch.
                            f.handlers.pop();
                            f.pos = skip_handlers(&f.chain, pos + 1);
                        } else {
                            f.pos += 1;
                        }
                        continue;
                    }
                    MARKER_CALL_ENTER => {
                        let callee = st
                            .frames
                            .last()
                            .unwrap()
                            .chain
                            .get(pos + 1)
                            .copied()
                            .unwrap_or(0);
                        self.sim_handle_call(
                            &mut st, work, seen, flow_cache, pos, callee, paths, max_paths,
                        )
                        .await;
                        continue;
                    }
                    MARKER_CALL_EXIT => {
                        // Điều khiển đã về frame này sau khi call (leaf hoặc
                        // callee vừa return): ghi step Return của caller — trừ
                        // khi phần tử kế là RETURN của chính frame (tránh double
                        // return: return-from-call rồi return-of-function).
                        let f0 = st.frames.last().unwrap();
                        let next_is_return =
                            f0.chain.get(pos + 1).is_none_or(|&e| e == MARKER_RETURN);
                        let leaf = f0.leaf_pending;
                        let f0 = st.frames.last_mut().unwrap();
                        f0.leaf_pending = false;
                        if !next_is_return && !leaf {
                            let (d, top_id) = {
                                let f = st.frames.last().unwrap();
                                (st.base_depth + st.frames.len(), f.func_id)
                            };
                            let sym = self.symbols.get(&top_id);
                            st.steps.push(FlowStep {
                                symbol_id: top_id,
                                symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                                file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                                line: sym.map(|s| s.line).unwrap_or(0),
                                action: TraceAction::Return,
                                stack_depth: d,
                                condition: None,
                            });
                        }
                        let f = st.frames.last_mut().unwrap();
                        f.pos += 1;
                        continue;
                    }
                    MARKER_LOOP => {
                        let (back, label, d, top_id) = {
                            let f = st.frames.last().unwrap();
                            (
                                matching_loop_back(&f.chain, pos).unwrap_or(pos),
                                f.labels.get(&pos).cloned(),
                                st.base_depth + st.frames.len(),
                                f.func_id,
                            )
                        };
                        let sym = self.symbols.get(&top_id);
                        st.steps.push(FlowStep {
                            symbol_id: top_id,
                            symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                            file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                            line: sym.map(|s| s.line).unwrap_or(0),
                            action: TraceAction::Loop,
                            stack_depth: d,
                            condition: label,
                        });
                        let f = st.frames.last_mut().unwrap();
                        f.loops.push((pos, back));
                        f.pos += 1;
                        continue;
                    }
                    MARKER_LOOP_BACK => {
                        let (top_loop, d, top_id) = {
                            let f = st.frames.last().unwrap();
                            (
                                f.loops.last().copied(),
                                st.base_depth + st.frames.len(),
                                f.func_id,
                            )
                        };
                        if let Some((lp, back)) = top_loop {
                            let cnt = st
                                .loop_iters
                                .entry((top_id, lp))
                                .and_modify(|c| *c += 1)
                                .or_insert(1);
                            if *cnt >= max_loop_iter {
                                // Hết iter: thoát loop, chạy code sau loop.
                                let sym = self.symbols.get(&top_id);
                                st.steps.push(FlowStep {
                                    symbol_id: top_id,
                                    symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                                    file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                                    line: sym.map(|s| s.line).unwrap_or(0),
                                    action: TraceAction::Loop,
                                    stack_depth: d,
                                    condition: Some("max_iter".into()),
                                });
                                let f = st.frames.last_mut().unwrap();
                                f.loops.pop();
                                f.pos = back + 1;
                            } else {
                                let f = st.frames.last_mut().unwrap();
                                f.pos = lp + 1;
                            }
                        } else {
                            let f = st.frames.last_mut().unwrap();
                            f.pos += 1;
                        }
                        continue;
                    }
                    MARKER_BREAK => {
                        let f = st.frames.last_mut().unwrap();
                        if let Some((_, back)) = f.loops.pop() {
                            f.pos = back + 1;
                        } else {
                            f.pos += 1;
                        }
                        continue;
                    }
                    MARKER_CONTINUE => {
                        let f = st.frames.last_mut().unwrap();
                        if let Some(&(lp, back)) = f.loops.last() {
                            let cnt = st.loop_iters.entry((f.func_id, lp)).or_insert(0);
                            if *cnt >= max_loop_iter {
                                // Vượt cap: thoát loop luôn (chống continue-chain).
                                f.loops.pop();
                                f.pos = back + 1;
                            } else {
                                *cnt += 1;
                                f.pos = lp + 1;
                            }
                        } else {
                            f.pos += 1;
                        }
                        continue;
                    }
                    MARKER_SWITCH_START => {
                        let (cases, close, d, top_id, case_labels) = {
                            let f = st.frames.last().unwrap();
                            let (cases, close) = switch_layout(&f.chain, pos);
                            let labels: Vec<(usize, Option<String>)> = cases
                                .iter()
                                .map(|&cp| (cp, f.labels.get(&cp).cloned()))
                                .collect();
                            (
                                cases,
                                close,
                                st.base_depth + st.frames.len(),
                                f.func_id,
                                labels,
                            )
                        };
                        // Fork path-per-case (mỗi case một path, gặp SWITCH_END
                        // thì thoát switch); path chính = no-match, nhảy past close.
                        for (&cp, label) in
                            cases.iter().zip(case_labels.into_iter().map(|(_, l)| l))
                        {
                            let mut cst = st.clone();
                            {
                                let cf = cst.frames.last_mut().unwrap();
                                cf.pos = cp + 1;
                            }
                            let sym = self.symbols.get(&top_id);
                            cst.steps.push(FlowStep {
                                symbol_id: top_id,
                                symbol_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                                file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                                line: sym.map(|s| s.line).unwrap_or(0),
                                action: TraceAction::Branch,
                                stack_depth: d,
                                condition: label,
                            });
                            let k = state_key(&cst);
                            if seen.insert(k) {
                                work.push(cst);
                            }
                        }
                        let f = st.frames.last_mut().unwrap();
                        if let Some(c) = close {
                            f.switches.push(c);
                        }
                        f.pos = close.map(|c| c + 1).unwrap_or(pos + 1);
                        continue;
                    }
                    MARKER_SWITCH_END => {
                        // Hết body case của switch hiện tại: thoát switch.
                        let f = st.frames.last_mut().unwrap();
                        if let Some(close) = f.switches.pop() {
                            f.pos = close + 1;
                        } else {
                            f.pos += 1;
                        }
                        continue;
                    }
                    MARKER_TRY_START => {
                        let f = st.frames.last_mut().unwrap();
                        f.catch_stack.push(pos);
                        f.pos += 1;
                        continue;
                    }
                    MARKER_TRY_END => {
                        // Try body xong không exception: bỏ qua handler, chạy tiếp.
                        let f = st.frames.last_mut().unwrap();
                        f.catch_stack.pop();
                        f.pos = skip_handlers(&f.chain, pos + 1);
                        continue;
                    }
                    MARKER_CATCH_START | MARKER_SWITCH_CASE | MARKER_SWITCH_CLOSE => {
                        let f = st.frames.last_mut().unwrap();
                        f.pos += 1;
                        continue;
                    }
                    _ => {
                        let f = st.frames.last_mut().unwrap();
                        f.pos += 1;
                        continue;
                    }
                }
            }

            // Non-marker: symbol id (bare) hoặc placeholder 0.
            let callee = elem;
            let (is_leaf, call_info) = {
                let f = st.frames.last().unwrap();
                (
                    callee == 0 || callee == f.func_id || is_marker(callee),
                    f.calls
                        .get(&pos)
                        .cloned()
                        .unwrap_or_else(|| (String::new(), 0, None)),
                )
            };
            if is_leaf {
                // Leaf: unresolved / self — record từ call records.
                let (name, line, cond) = call_info;
                let d = st.base_depth + st.frames.len() + 1;
                st.steps.push(FlowStep {
                    symbol_id: 0,
                    symbol_name: name,
                    file: String::new(),
                    line,
                    action: TraceAction::Call,
                    stack_depth: d,
                    condition: cond,
                });
                let f = st.frames.last_mut().unwrap();
                f.leaf_pending = true; // CALL_EXIT sẽ không ghi Return (leaf)
                f.pos += 1;
                continue;
            }
            self.sim_handle_call(
                &mut st, work, seen, flow_cache, pos, callee, paths, max_paths,
            )
            .await;
        }
        None
    }

    /// Xử lý một call: descend vào chain của callee (nếu được) hoặc leaf.
    #[allow(clippy::too_many_arguments)]
    async fn sim_handle_call(
        &self,
        st: &mut SimState,
        _work: &mut Vec<SimState>,
        _seen: &mut HashSet<u64>,
        flow_cache: &mut HashMap<u64, Option<FlowResult>>,
        pos: usize,
        callee: u64,
        paths: &mut Vec<FullFlowPath>,
        _max_paths: usize,
    ) {
        let _ = paths;
        let f = st.frames.last().unwrap();
        let d = st.base_depth + st.frames.len() + 1;
        // Call nằm trong vòng lặp đã re-iterate (>1 lần): chỉ leaf — cấu trúc
        // bên trong callee đã được trace ở iteration đầu, re-descend mỗi
        // iteration chỉ nhân bản state (nguồn chính của nổ budget/độ trễ).
        let in_reiterated_loop = f
            .loops
            .last()
            .is_some_and(|&(lp, _)| st.loop_iters.get(&(f.func_id, lp)).is_some_and(|c| *c > 0));
        // Cycle guard: không descend vào hàm đã nằm trên stack (đệ quy trực
        // tiếp/gián tiếp) — chain của nó đã được trace ở frame đó.
        // Chỉ descend khi chain của callee gọn (<= MAX_DESCEND_CHAIN phần tử)
        // và depth không quá sâu — chain lớn (ingest, dispatch...) sẽ làm path
        // phình hàng nghìn step nhiễu; chúng hiện như leaf call.
        let descend = callee != 0
            && callee != f.func_id
            && !in_reiterated_loop
            && !st.frames.iter().any(|fr| fr.func_id == callee)
            && st.frames.len() <= FLOW_DESCEND_MAX_FRAMES
            && self
                .symbols
                .get(&callee)
                .is_some_and(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method));
        let fr = if descend {
            match flow_cache.get(&callee) {
                Some(r) => r.clone(),
                None => {
                    let r = self.flow(callee).await.ok();
                    flow_cache.insert(callee, r.clone());
                    r
                }
            }
        } else {
            None
        };
        let fr = fr.filter(|fr| fr.chain.len() <= MAX_DESCEND_CHAIN);
        let sym = self.symbols.get(&callee);
        st.steps.push(FlowStep {
            symbol_id: callee,
            symbol_name: sym.map(|s| s.name.clone()).unwrap_or_else(|| {
                f.calls
                    .get(&(pos + 1))
                    .map(|(n, _, _)| n.clone())
                    .unwrap_or_default()
            }),
            file: sym.map(|s| s.file.clone()).unwrap_or_default(),
            line: sym.map(|s| s.line).unwrap_or(0),
            action: TraceAction::Call,
            stack_depth: d,
            condition: None,
        });
        let f = st.frames.last_mut().unwrap();
        match fr.filter(|fr| fr.chain.len() > 1) {
            Some(fr) => {
                // Descend: caller dừng tại phần tử callee id, resume ở CALL_EXIT.
                f.pos = pos + 1;
                st.frames.push(SimFrame::from_flow(callee, &fr));
            }
            None => {
                // Leaf: skip tới CALL_EXIT — CALL_EXIT không ghi Return (nhiễu).
                f.leaf_pending = true;
                f.pos = pos + 2;
            }
        }
    }

    /// Tìm tất cả caller gọi trực tiếp đến `target_id` (depth=1).
    #[allow(dead_code)]
    async fn find_direct_callers(&self, target_id: u64) -> Result<Vec<Symbol>> {
        // Dùng callers với depth=1 để lấy direct callers
        self.callers(target_id, 1).await
    }

    /// Tìm hàm có chain chứa pattern (KMP substring qua chain engine).
    pub async fn search_flow(&self, pattern: &[u64]) -> Result<Vec<SearchFlowResult>> {
        if pattern.is_empty() {
            return Ok(Vec::new());
        }
        let hits = match self.chains.search(pattern, None).await {
            Ok(h) => h,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for (record, _) in hits {
            let func_id = record as u64;
            if let Some(sym) = self.symbols.get(&func_id) {
                let chain = self.chains_map.get(&func_id).cloned().unwrap_or_default();
                out.push(SearchFlowResult {
                    function_id: func_id,
                    function_name: sym.name.clone(),
                    chain,
                    match_count: 1,
                });
            }
        }
        Ok(out)
    }

    /// Resumable + deadline-aware của [`Self::search_flow`]. `deadline` hết hạn
    /// giữa chừng → trả `(Vec::new(), Some(offset))` (không kết quả nửa chừng),
    /// caller retry với `offset` tiếp tục. `deadline = None` = chạy tới cùng.
    pub async fn search_flow_resumable(
        &self,
        pattern: &[u64],
        offset: usize,
        limit: usize,
        deadline: Option<Instant>,
    ) -> Result<(Vec<SearchFlowResult>, Option<usize>)> {
        if pattern.is_empty() {
            return Ok((Vec::new(), None));
        }
        let hits = match self.chains.search(pattern, None).await {
            Ok(h) => h,
            Err(_) => return Ok((Vec::new(), None)),
        };
        let limit = if limit == 0 { usize::MAX } else { limit };
        let cap = if limit == usize::MAX {
            usize::MAX
        } else {
            offset + limit
        };
        let mut out = Vec::new();
        for (record, _) in hits {
            if deadline.is_some_and(|dl| Instant::now() >= dl) {
                return Ok((Vec::new(), Some(offset)));
            }
            let func_id = record as u64;
            if let Some(sym) = self.symbols.get(&func_id) {
                let chain = self.chains_map.get(&func_id).cloned().unwrap_or_default();
                out.push(SearchFlowResult {
                    function_id: func_id,
                    function_name: sym.name.clone(),
                    chain,
                    match_count: 1,
                });
            }
            if out.len() >= cap {
                break;
            }
        }
        let page = out.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        Ok((page, None))
    }

    /// Tìm function gọi một library call có tên chứa `query` (case-insensitive
    /// substring trên call-name index, kể cả call unresolved). Gom theo caller,
    /// sort theo FuncName rồi FuncID.
    pub async fn callers_by_call_name(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<CallSiteResult>> {
        let q = query.to_lowercase();
        let mut matched: Vec<(&String, &Vec<CallSite>)> = self
            .call_names
            .iter()
            .filter(|(name, _)| name.contains(&q))
            .collect();
        matched.sort_by_key(|(name, _)| (*name).clone());

        let mut by_func: HashMap<u64, CallSiteResult> = HashMap::new();
        for (_, sites) in matched {
            for site in sites {
                let entry = by_func.entry(site.caller_id).or_insert_with(|| {
                    let sym = self.symbols.get(&site.caller_id);
                    CallSiteResult {
                        func_id: site.caller_id,
                        func_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                        file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                        call_sites: Vec::new(),
                    }
                });
                entry.call_sites.push(site.clone());
            }
        }
        let mut out: Vec<CallSiteResult> = by_func.into_values().collect();
        out.sort_by(|a, b| {
            a.func_name
                .cmp(&b.func_name)
                .then(a.func_id.cmp(&b.func_id))
        });
        let limit = if limit == 0 { usize::MAX } else { limit };
        out.truncate(limit);
        Ok(out)
    }

    /// Resumable + deadline-aware của [`Self::callers_by_call_name`]. Tương tự
    /// [`Self::search_flow_resumable`]: timeout → `(Vec::new(), Some(offset))`,
    /// caller retry với `offset` tiếp tục. `deadline = None` = chạy tới cùng.
    pub async fn callers_by_call_name_resumable(
        &self,
        query: &str,
        offset: usize,
        limit: usize,
        deadline: Option<Instant>,
    ) -> Result<(Vec<CallSiteResult>, Option<usize>)> {
        let q = query.to_lowercase();
        let mut matched: Vec<(&String, &Vec<CallSite>)> = self
            .call_names
            .iter()
            .filter(|(name, _)| name.contains(&q))
            .collect();
        if deadline.is_some_and(|dl| Instant::now() >= dl) {
            return Ok((Vec::new(), Some(offset)));
        }
        matched.sort_by_key(|(name, _)| (*name).clone());
        let mut by_func: HashMap<u64, CallSiteResult> = HashMap::new();
        for (_, sites) in matched {
            if deadline.is_some_and(|dl| Instant::now() >= dl) {
                return Ok((Vec::new(), Some(offset)));
            }
            for site in sites {
                let entry = by_func.entry(site.caller_id).or_insert_with(|| {
                    let sym = self.symbols.get(&site.caller_id);
                    CallSiteResult {
                        func_id: site.caller_id,
                        func_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                        file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                        call_sites: Vec::new(),
                    }
                });
                entry.call_sites.push(site.clone());
            }
        }
        let mut out: Vec<CallSiteResult> = by_func.into_values().collect();
        out.sort_by(|a, b| {
            a.func_name
                .cmp(&b.func_name)
                .then(a.func_id.cmp(&b.func_id))
        });
        let limit = if limit == 0 { usize::MAX } else { limit };
        let cap = if limit == usize::MAX {
            usize::MAX
        } else {
            offset + limit
        };
        if out.len() > cap {
            out.truncate(cap);
        }
        let page = out.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        Ok((page, None))
    }

    /// Files trong graph.
    pub fn files(&self) -> Vec<FileInfo> {
        self.files.clone()
    }

    // ── Class / scope queries (tương ứng semgraph_get_class* / function_scope) ──

    /// Toàn bộ symbol con của một scope id (methods/fields/nested của class,
    /// params/locals của function).
    pub fn members_of(&self, id: u64) -> Vec<Symbol> {
        self.scope_index
            .get(&id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|i| self.symbols.get(i).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolve một class id về definition id. Rust: nếu `id` là impl symbol
    /// (`type_ref != 0`) không có method riêng, theo `type_ref` về struct def để
    /// vẫn trả đúng methods khi caller truyền impl id.
    fn class_target_id(&self, id: u64) -> u64 {
        let Some(sym) = self.symbols.get(&id) else {
            return id;
        };
        if sym.type_ref != 0 {
            let has_own_methods = self
                .members_of(id)
                .iter()
                .any(|m| matches!(m.kind, SymbolKind::Function | SymbolKind::Method));
            if !has_own_methods {
                return sym.type_ref;
            }
        }
        id
    }

    /// Methods của class (kind Function/Method), projection `MemberInfo` gọn.
    pub fn list_methods_of_class(&self, id: u64) -> Vec<MemberInfo> {
        let target = self.class_target_id(id);
        let mut members: Vec<MemberInfo> = self
            .members_of(target)
            .into_iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
            .map(|s| MemberInfo::from_symbol(&s))
            .collect();
        members.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        members
    }

    /// Thông tin class: symbol + fields và methods tách riêng. `None` nếu symbol
    /// không phải class/interface/enum (function có scope params → không class).
    pub fn get_class_info(&self, id: u64) -> Option<ClassInfo> {
        let target = self.class_target_id(id);
        let class = self.symbols.get(&target)?;
        if !matches!(
            class.kind,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
        ) {
            return None;
        }
        let class = class.clone();
        let members = self.members_of(target);
        let fields: Vec<MemberInfo> = members
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    SymbolKind::Field | SymbolKind::Variable | SymbolKind::Constant
                )
            })
            .map(MemberInfo::from_symbol)
            .collect();
        let methods: Vec<MemberInfo> = members
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
            .map(MemberInfo::from_symbol)
            .collect();
        Some(ClassInfo {
            class,
            fields,
            methods,
        })
    }

    /// Scope của function: parameters + locals (kind Variable/Constant).
    pub fn function_scope(&self, id: u64) -> Option<FunctionScope> {
        let function = self.symbols.get(&id)?.clone();
        let members = self.members_of(id);
        let parameters = members
            .iter()
            .filter(|s| s.kind == SymbolKind::Parameter)
            .cloned()
            .collect();
        let locals = members
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Variable | SymbolKind::Constant))
            .cloned()
            .collect();
        Some(FunctionScope {
            function,
            parameters,
            locals,
        })
    }

    /// Với kind == Class, nhiều symbol cùng tên có thể tồn tại (VD Rust
    /// `struct Foo` + `impl Foo` đều là Class). Chỉ giữ 1 symbol mỗi tên, ưu
    /// tiên definition (type_ref == 0) — impl symbol không có method riêng sau
    /// khi re-parent nên không cần hiện riêng.
    fn dedup_class_symbols(all: Vec<Symbol>) -> Vec<Symbol> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut kept: Vec<Symbol> = Vec::new();
        for s in &all {
            if s.type_ref == 0 && seen.insert(s.name.clone()) {
                kept.push(s.clone());
            }
        }
        for s in &all {
            if s.type_ref != 0 && seen.insert(s.name.clone()) {
                kept.push(s.clone());
            }
        }
        kept
    }

    /// Liệt kê symbol theo kind (class/interface/enum/...), phân trang —
    /// sort theo name rồi id để ổn định giữa các trang.
    pub fn list_symbols_by_kind(
        &self,
        kind: SymbolKind,
        limit: usize,
        offset: usize,
    ) -> (Vec<Symbol>, usize) {
        let mut all: Vec<Symbol> = self
            .symbols
            .values()
            .filter(|s| s.kind == kind)
            .cloned()
            .collect();
        if kind == SymbolKind::Class {
            all = Self::dedup_class_symbols(all);
        }
        all.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let total = all.len();
        let limit = if limit == 0 { usize::MAX } else { limit };
        (all.into_iter().skip(offset).take(limit).collect(), total)
    }

    /// Resumable + deadline-aware của [`Self::list_symbols_by_kind`]. Timeout →
    /// `(Vec::new(), 0, Some(offset))` (không kết quả nửa chừng), retry với
    /// `offset` tiếp tục. `deadline = None` = chạy tới cùng.
    pub fn list_symbols_by_kind_resumable(
        &self,
        kind: SymbolKind,
        offset: usize,
        limit: usize,
        deadline: Option<Instant>,
    ) -> (Vec<Symbol>, usize, Option<usize>) {
        let mut all: Vec<Symbol> = Vec::new();
        for s in self.symbols.values() {
            if deadline.is_some_and(|dl| Instant::now() >= dl) {
                return (Vec::new(), 0, Some(offset));
            }
            if s.kind == kind {
                all.push(s.clone());
            }
        }
        if kind == SymbolKind::Class {
            all = Self::dedup_class_symbols(all);
        }
        all.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let total = all.len();
        let limit = if limit == 0 { usize::MAX } else { limit };
        let page = all.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        (page, total, None)
    }

    /// Tìm symbol theo annotation (case-insensitive substring trên tên
    /// annotation). Trả về (page, total, truncated) — total là con số thật,
    /// truncated=true khi còn trang sau.
    pub fn search_by_annotation(
        &self,
        annotation: &str,
        kind: Option<SymbolKind>,
        offset: usize,
        limit: usize,
    ) -> (Vec<Symbol>, usize, bool) {
        let q = annotation.to_lowercase();
        let mut all: Vec<Symbol> = self
            .symbols
            .values()
            .filter(|s| {
                s.annotations
                    .iter()
                    .any(|a| a.name.to_lowercase().contains(&q))
            })
            .filter(|s| kind.is_none_or(|k| s.kind == k))
            .cloned()
            .collect();
        all.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let total = all.len();
        let limit = if limit == 0 { usize::MAX } else { limit };
        let page = all.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        let truncated = offset + page.len() < total;
        (page, total, truncated)
    }

    /// Resumable + deadline-aware của [`Self::search_by_annotation`]. Timeout →
    /// `(Vec::new(), 0, Some(offset))` (không kết quả nửa chừng), retry với
    /// `offset` tiếp tục. `deadline = None` = chạy tới cùng.
    pub fn search_by_annotation_resumable(
        &self,
        annotation: &str,
        kind: Option<SymbolKind>,
        offset: usize,
        limit: usize,
        deadline: Option<Instant>,
    ) -> (Vec<Symbol>, usize, Option<usize>) {
        let q = annotation.to_lowercase();
        let mut all: Vec<Symbol> = Vec::new();
        for s in self.symbols.values() {
            if deadline.is_some_and(|dl| Instant::now() >= dl) {
                return (Vec::new(), 0, Some(offset));
            }
            if s.annotations
                .iter()
                .any(|a| a.name.to_lowercase().contains(&q))
                && kind.is_none_or(|k| s.kind == k)
            {
                all.push(s.clone());
            }
        }
        all.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let total = all.len();
        let limit = if limit == 0 { usize::MAX } else { limit };
        let page = all.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        (page, total, None)
    }

    /// Ước lượng dependencies từ call names: tách module prefix (phần trước dấu
    /// chấm đầu tiên) — internal nếu có symbol trong repo mang chính tên đó,
    /// external còn lại. Sort theo số call sites giảm dần.
    pub fn dependencies_report(&self) -> DependenciesReport {
        let mut internal: BTreeMap<String, usize> = BTreeMap::new();
        let mut external: BTreeMap<String, usize> = BTreeMap::new();
        // Dedup theo (caller, line, raw call_name): alias type-qualified index
        // (`svc.validate` → `type.validate`) đẩy cùng site vào nhiều key — mỗi
        // call site chỉ tính một lần, dùng tên thô để rút module prefix.
        let mut seen: HashSet<(u64, u32, String)> = HashSet::new();
        for sites in self.call_names.values() {
            for site in sites {
                if !seen.insert((site.caller_id, site.line, site.call_name.clone())) {
                    continue;
                }
                let Some((mod_part, _)) = site.call_name.split_once('.') else {
                    continue;
                };
                let mod_part = mod_part.to_lowercase();
                let entry = if self.name_index.contains_key(&mod_part) {
                    &mut internal
                } else {
                    &mut external
                };
                *entry.entry(mod_part).or_default() += 1;
            }
        }
        let to_list = |m: BTreeMap<String, usize>| -> Vec<Dependency> {
            let mut v: Vec<Dependency> = m
                .into_iter()
                .map(|(name, count)| Dependency { name, count })
                .collect();
            v.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
            v
        };
        let internal = to_list(internal);
        let external = to_list(external);
        let total = internal.len() + external.len();
        DependenciesReport {
            internal,
            external,
            total,
        }
    }

    /// Phiên bản resumable + deadline-aware của search symbol nâng cao (kind
    /// filter + match mode contains/prefix/suffix/exact + phân trang): ngắt
    /// giữa chừng khi `deadline` hết hạn, trả `PagedSearchOutcome { timed_out:
    /// true, cursor: Some(phase dở) }` — caller gọi lại với `resume =
    /// Some(cursor)` để tiếp tục từ đúng vị trí (không lặp phần đã duyệt).
    ///
    /// - Phase A (Contains: engine resumable; khác: scan `name_records`)
    ///   sinh danh sách tên khớp **đã sort** — chỉ hoàn tất sau khi quét hết
    ///   (không thể sort từng phần vì tên mới có thể chèn vào giữa).
    /// - Phase B (`Expand`) stream ids theo tên đã sort — `name_index[name]`
    ///   tăng dần theo id nên collected đã sort (name, id) — **không cần sort
    ///   cuối** (diệt O(n log n) của bản cũ). total chỉ chính xác khi quét xong.
    /// - Hoàn tất + còn page sau → `cursor = Some(Paged)` để phân trang tiếp
    ///   không cần quét lại.
    ///
    /// KNN: ưu tiên backend-native (SQLite + sqlite-vss `vss0` HNSW ANN) nếu
    /// khả dụng, ngược lại brute-force in-memory `VectorIndex` (đúng cho mọi
    /// backend). Trả `Vec<(symbol_id, sim)>` với `sim` cao = gần hơn.
    async fn knn_hits(&self, qvec: &[f32], k: usize) -> Vec<(u64, f32)> {
        if let Ok(guard) = self.storage.try_read()
            && let Ok(Some(hits)) = guard.knn(qvec, k).await
        {
            return hits;
        }
        self.vector_index.knn(qvec, k)
    }

    /// `resume` phải khớp (query, mode, kind) — sai → `InvalidArgument`.
    pub async fn search_symbol_paged_resumable(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        mode: SymbolMatch,
        pagination: Pagination,
        resume: Option<SearchCursor>,
        deadline: Option<Instant>,
    ) -> Result<PagedSearchOutcome> {
        let q = query.to_lowercase();

        // Validate resume: query/mode/kind phải khớp (limit/offset không — page
        // có thể đổi giữa chừng khi retry).
        if let Some(c) = &resume
            && (c.query != q || c.mode != mode || c.kind != kind)
        {
            return Err(Error::Invalid(
                "resume cursor does not match this query".into(),
            ));
        }

        // Semantic/Hybrid cần embedding bật (config `[embedding].backend = "fastembed"`).
        // Nếu tắt → lỗi rõ ràng (KHÔNG fallback silent sang lexical/hashing).
        if matches!(mode, SymbolMatch::Semantic | SymbolMatch::Hybrid) && !self.embedding_enabled {
            return Err(Error::Invalid(
                "semantic/hybrid search requires embedding; enable `[embedding] backend = \"fastembed\"` in config".into(),
            ));
        }

        // ── Khôi phục / khởi tạo phase ──
        let (mut phase, mut timed_out) = match resume.map(|c| c.phase) {
            Some(p) => (p, false),
            None => (
                match mode {
                    SymbolMatch::Contains => SearchCursorPhase::Engine(SearchResume::default()),
                    // Semantic/Hybrid dùng placeholder — Phase A ghi đè thành
                    // `Candidates` (tính KNN/RRF).
                    SymbolMatch::Semantic | SymbolMatch::Hybrid => {
                        SearchCursorPhase::Engine(SearchResume::default())
                    }
                    _ => SearchCursorPhase::ScanNames { name_pos: 0 },
                },
                false,
            ),
        };

        // ── Phase A: sinh danh sách candidate (sort) — dispatch theo mode ──
        match mode {
            SymbolMatch::Contains => {
                if let SearchCursorPhase::Engine(sr) = &mut phase {
                    let page = self
                        .names
                        .search_resumable(q.as_bytes(), None, Some(sr.clone()), deadline)
                        .await?;
                    if page.timed_out {
                        phase = SearchCursorPhase::Engine(page.resume.unwrap_or_default());
                        timed_out = true;
                    } else {
                        // record → tên, sort → Expand.
                        let mut names: Vec<String> = page
                            .record_ids
                            .iter()
                            .filter_map(|&r| {
                                if r == 0 {
                                    return None;
                                }
                                self.name_records.get(r - 1).cloned()
                            })
                            .collect();
                        names.sort();
                        phase = SearchCursorPhase::Expand {
                            names,
                            name_idx: 0,
                            id_idx: 0,
                            collected: Vec::new(),
                        };
                    }
                }
            }
            SymbolMatch::Prefix | SymbolMatch::Suffix | SymbolMatch::Exact => {
                if let SearchCursorPhase::ScanNames { name_pos } = &mut phase {
                    let mut matched: Vec<String> = Vec::new();
                    let mut pos = *name_pos;
                    loop {
                        if let Some(dl) = deadline
                            && Instant::now() >= dl
                        {
                            phase = SearchCursorPhase::ScanNames { name_pos: pos };
                            timed_out = true;
                            break;
                        }
                        if pos >= self.name_records.len() {
                            break;
                        }
                        let name = &self.name_records[pos];
                        let ok = match mode {
                            SymbolMatch::Prefix => name.starts_with(&q),
                            SymbolMatch::Suffix => name.ends_with(&q),
                            SymbolMatch::Exact => name == &q,
                            _ => false,
                        };
                        if ok {
                            matched.push(name.clone());
                        }
                        pos += 1;
                    }
                    if !timed_out {
                        phase = SearchCursorPhase::Expand {
                            names: matched,
                            name_idx: 0,
                            id_idx: 0,
                            collected: Vec::new(),
                        };
                    }
                }
            }
            SymbolMatch::Semantic => {
                if !matches!(
                    phase,
                    SearchCursorPhase::Candidates { .. } | SearchCursorPhase::Paged { .. }
                ) {
                    let qvec = self.embedding_backend.embed(&q);
                    let k = (pagination.limit * 4).max(32);
                    let ids: Vec<u64> = self
                        .knn_hits(&qvec, k)
                        .await
                        .into_iter()
                        .map(|(id, _)| id)
                        .collect();
                    phase = SearchCursorPhase::Candidates {
                        ids,
                        idx: 0,
                        collected: Vec::new(),
                    };
                }
            }
            SymbolMatch::Hybrid => {
                if !matches!(
                    phase,
                    SearchCursorPhase::Candidates { .. } | SearchCursorPhase::Paged { .. }
                ) {
                    // Lexical (Contains) trên name engine.
                    let lex: Vec<u64> = self
                        .names
                        .search(q.as_bytes(), None)
                        .await
                        .map(|recs| {
                            let mut out = Vec::new();
                            for (r, _) in recs {
                                if r == 0 {
                                    continue;
                                }
                                if let Some(name) = self.name_records.get(r - 1)
                                    && let Some(ids) = self.name_index.get(name)
                                {
                                    out.extend_from_slice(ids);
                                }
                            }
                            out
                        })
                        .unwrap_or_default();
                    // Vector (Semantic).
                    let qvec = self.embedding_backend.embed(&q);
                    let k = (pagination.limit * 4).max(32);
                    let vec_ids: Vec<u64> = self
                        .knn_hits(&qvec, k)
                        .await
                        .into_iter()
                        .map(|(id, _)| id)
                        .collect();
                    let ids = rrf_fuse(&[lex, vec_ids]);
                    phase = SearchCursorPhase::Candidates {
                        ids,
                        idx: 0,
                        collected: Vec::new(),
                    };
                }
            }
        }

        // ── Phase B: stream ids theo tên đã sort → collected ──
        if !timed_out
            && let SearchCursorPhase::Expand {
                names,
                name_idx,
                id_idx,
                collected,
            } = &mut phase
        {
            loop {
                if let Some(dl) = deadline
                    && Instant::now() >= dl
                {
                    timed_out = true;
                    break;
                }
                if *name_idx >= names.len() {
                    break;
                }
                let name = &names[*name_idx];
                let Some(name_ids) = self.name_index.get(name) else {
                    *name_idx += 1;
                    *id_idx = 0;
                    continue;
                };
                if *id_idx >= name_ids.len() {
                    *name_idx += 1;
                    *id_idx = 0;
                    continue;
                }
                let id = name_ids[*id_idx];
                *id_idx += 1;
                if let Some(k) = kind {
                    if self.symbols.get(&id).is_some_and(|s| s.kind == k) {
                        collected.push(id);
                    }
                } else {
                    collected.push(id);
                }
            }
        }

        // ── Phase B (Candidates): stream id vector (Semantic/Hybrid) → collected ──
        if !timed_out
            && let SearchCursorPhase::Candidates {
                ids,
                idx,
                collected,
            } = &mut phase
        {
            loop {
                if let Some(dl) = deadline
                    && Instant::now() >= dl
                {
                    timed_out = true;
                    break;
                }
                if *idx >= ids.len() {
                    break;
                }
                let id = ids[*idx];
                *idx += 1;
                if let Some(k) = kind {
                    if self.symbols.get(&id).is_some_and(|s| s.kind == k) {
                        collected.push(id);
                    }
                } else {
                    collected.push(id);
                }
            }
        }

        // ── Trang kết quả + cursor ──
        if timed_out {
            let progress = match &phase {
                SearchCursorPhase::Engine(sr) => sr.record_ids.len(),
                SearchCursorPhase::ScanNames { name_pos } => *name_pos,
                SearchCursorPhase::Expand { collected, .. } => collected.len(),
                SearchCursorPhase::Candidates { collected, .. } => collected.len(),
                SearchCursorPhase::Paged { .. } => 0,
            };
            return Ok(PagedSearchOutcome {
                page: Vec::new(),
                total: 0,
                timed_out: true,
                progress,
                cursor: Some(SearchCursor {
                    query: q,
                    mode,
                    kind,
                    phase,
                }),
            });
        }

        // Hoàn tất: lấy collected + total từ Expand/Candidates, hoặc dùng thẳng từ Paged.
        let (collected, total) = match &phase {
            SearchCursorPhase::Expand { collected, .. } => {
                let total = collected.len();
                (collected.clone(), total)
            }
            SearchCursorPhase::Candidates { collected, .. } => {
                let total = collected.len();
                (collected.clone(), total)
            }
            SearchCursorPhase::Paged { collected, total } => (collected.clone(), *total),
            // Không thể tới đây khi chưa hoàn tất phase A.
            _ => (Vec::new(), 0),
        };

        let cap = if pagination.limit == 0 {
            usize::MAX
        } else {
            pagination.limit
        };
        let page: Vec<Symbol> = collected
            .iter()
            .skip(pagination.offset)
            .take(cap)
            .filter_map(|id| self.symbols.get(id).cloned())
            .collect();
        let page_end = pagination.offset + page.len();
        let more = page_end < total;

        Ok(PagedSearchOutcome {
            page,
            total,
            timed_out: false,
            progress: total,
            cursor: more.then_some(SearchCursor {
                query: q,
                mode,
                kind,
                phase: SearchCursorPhase::Paged { collected, total },
            }),
        })
    }

    /// Deep size từng cấu trúc in-memory + occupancy LRU cache.
    ///
    /// Dùng để **quy kết quả RSS về đúng cấu trúc**: `GraphIndex` in-memory-first
    /// nên tổng bytes ở đây mới là thứ quyết định repo lớn tốn bao nhiêu RAM.
    /// Xem [`crate::memtrack`] để biết phần nào exact và phần nào ước lượng.
    ///
    /// Chạy O(tổng số entry) — dùng cho profiler/diagnostics, không phải
    /// hot path.
    pub async fn mem_breakdown(&self) -> crate::memtrack::MemBreakdown {
        use crate::memtrack as mt;
        let caches = self.storage.read().await.cache_occupancy();
        mt::MemBreakdown {
            symbols: mt::symbols_mem(&self.symbols),
            chains_map: mt::chains_map_mem(&self.chains_map),
            call_names: mt::call_names_mem(&self.call_names),
            name_index: mt::name_index_mem(&self.name_index),
            scope_index: mt::scope_index_mem(&self.scope_index),
            name_keys: mt::name_keys_mem(&self.name_records),
            files: mt::files_mem(&self.files),
            caches,
        }
    }

    /// Số liệu tổng hợp.
    pub fn stats(&self) -> SemgraphStats {
        SemgraphStats {
            symbols: self.symbols.len() as u64,
            chains: self.chains_map.len() as u64,
            edges: self.edge_count,
            files: self.files.len() as u64,
            next_id: self.next_id,
        }
    }

    /// Version index hiện tại (bump mỗi lần ingest).
    pub fn version(&self) -> u64 {
        self.version
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{
        Annotation, EffectType, MARKER_BRANCH_END, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK,
        ScopeLevel,
    };

    #[test]
    #[cfg(any(feature = "sqlite", feature = "lmdb", feature = "redis"))]
    fn sibling_file_handles_both_separators() {
        // Unix / DSN style.
        assert_eq!(sibling_file("db.sqlite", "names.sqlite"), "names.sqlite");
        assert_eq!(sibling_file("a/b/db.lmdb", "names.lmdb"), "a/b/names.lmdb");
        // Windows backslash must keep the parent dir (regression:
        // name_engine_stamp_survives_reopen failed on windows).
        assert_eq!(
            sibling_file(r"C:\repo\.codegraph\db.sqlite", "names.sqlite"),
            r"C:\repo\.codegraph\names.sqlite"
        );
    }

    fn sym(file: &str, name: &str, id: u64) -> Symbol {
        Symbol {
            id,
            name: name.to_string(),
            kind: SymbolKind::Function,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: file.to_string(),
            line: 1,
            end_line: 1,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "test".to_string(),
            impl_trait: None,
        }
    }

    fn result(
        path: &str,
        symbols: Vec<Symbol>,
        chains: HashMap<u64, Vec<u64>>,
        calls: Vec<CallRecord>,
    ) -> ParseResult {
        ParseResult {
            path: path.to_string(),
            language: "test".to_string(),
            bytes: 0,
            lines: 0,
            symbols,
            chains,
            calls,
            fn_ptr_assigns: Vec::new(),
            file_deps: None,
        }
    }

    async fn check_callers_without_metadata(idx: &mut GraphIndex) {
        let a = SYMBOL_BASE;
        let b = a + 1;
        let c = a + 2;
        let isolated = a + 3;
        idx.ingest(
            &[result(
                "callers.rs",
                vec![
                    sym("callers.rs", "a", a),
                    sym("callers.rs", "b", b),
                    sym("callers.rs", "c", c),
                    sym("callers.rs", "isolated", isolated),
                ],
                HashMap::from([
                    (a, vec![a, b, b, MARKER_IF_TRUE, c, MARKER_BRANCH_END]),
                    (b, vec![b, c]),
                    (c, vec![c, a]),
                ]),
                vec![],
            )],
            None,
        )
        .await
        .unwrap();

        for id in [a, b, c, isolated] {
            let expected: Vec<u64> = idx
                .chains
                .search(&[id], None)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(record, _)| record as u64)
                .filter(|&caller| caller != id && idx.symbols.contains_key(&caller))
                .collect();
            for _ in 0..2 {
                let actual: Vec<_> = idx
                    .callers(id, 1)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|s| s.id)
                    .collect();
                assert_eq!(actual, expected);
            }
        }
        let direct = idx.callers(c, 1).await.unwrap();
        let ids = |v: &[Symbol]| v.iter().map(|s| s.id).collect::<Vec<_>>();
        let mut sorted_ids = ids(&direct);
        sorted_ids.sort_unstable();
        assert_eq!(sorted_ids, vec![a, b]);
        assert_eq!(ids(&idx.callers(c, 0).await.unwrap()), ids(&direct));
        assert_eq!(ids(&idx.callers(c, 10).await.unwrap()), ids(&direct));
        assert!(idx.callers(isolated, 10).await.unwrap().is_empty());
        assert!(idx.callers(isolated + 1, 1).await.is_err());

        let expired = Some(Instant::now());
        let paused = idx.callers_resumable(c, 10, None, expired).await.unwrap();
        assert!(paused.callers.is_empty());
        let cursor = paused.cursor.unwrap();
        assert!(
            idx.callers_resumable(b, 10, Some(cursor.clone()), None)
                .await
                .is_err()
        );
        assert!(
            idx.callers_resumable(c, 1, Some(cursor.clone()), None)
                .await
                .is_err()
        );
        let mut stale = cursor.clone();
        stale.index_version = stale.index_version.wrapping_add(1);
        assert!(
            idx.callers_resumable(c, 10, Some(stale), None)
                .await
                .is_err()
        );
        let again = idx
            .callers_resumable(c, 10, Some(cursor.clone()), expired)
            .await
            .unwrap();
        assert_eq!(again.progress, 0);
        let done = idx
            .callers_resumable(c, 10, again.cursor, None)
            .await
            .unwrap();
        assert!(done.cursor.is_none());
        assert_eq!(ids(&done.callers), ids(&direct));

        // Resume with an unfinished inner search.
        let mut searching = cursor.clone();
        searching.search = idx
            .chains
            .search_resumable(&[c], None, None, expired)
            .await
            .unwrap()
            .resume;
        assert!(searching.search.is_some());
        let done = idx
            .callers_resumable(c, 10, Some(searching), None)
            .await
            .unwrap();
        assert_eq!(ids(&done.callers), ids(&direct));

        // Resume after consuming one record of the current frontier node.
        let mut pending = cursor.clone();
        pending.pending = idx
            .chains
            .search_resumable(&[c], None, None, None)
            .await
            .unwrap()
            .record_ids;
        pending.search_complete = true;
        let first = pending.pending[0] as u64;
        pending.pending_pos = 1;
        if first != c && idx.symbols.contains_key(&first) && pending.visited.insert(first) {
            pending.out_ids.push(first);
            pending.next.push(first);
        }
        let done = idx
            .callers_resumable(c, 10, Some(pending), None)
            .await
            .unwrap();
        assert_eq!(ids(&done.callers), ids(&direct));

        // Resume in a later BFS level and while materializing the output.
        let mut next_level = cursor;
        next_level.level = 1;
        next_level.frontier = ids(&direct);
        next_level.out_ids = ids(&direct);
        next_level.visited.extend(ids(&direct));
        let done = idx
            .callers_resumable(c, 10, Some(next_level.clone()), None)
            .await
            .unwrap();
        assert_eq!(ids(&done.callers), ids(&direct));
        next_level.level = 10;
        next_level.materialize_pos = 1;
        next_level.callers.push(direct[0].clone());
        let done = idx
            .callers_resumable(c, 10, Some(next_level), None)
            .await
            .unwrap();
        assert_eq!(ids(&done.callers), ids(&direct));
    }

    #[tokio::test]
    async fn callers_without_metadata_matches_legacy() {
        check_callers_without_metadata(&mut GraphIndex::in_memory()).await;
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn callers_without_metadata_matches_legacy_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let dsn = format!("sqlite://{}", dir.path().join("callers.db").display());
        let mut idx = GraphIndex::open(&dsn).await.unwrap();
        check_callers_without_metadata(&mut idx).await;
    }

    #[tokio::test]
    async fn ingest_and_query_basic() {
        let mut idx = GraphIndex::in_memory();
        // a gọi b; b có if → gọi c (local ids trùng global khi ingest 1 file).
        let chains = HashMap::from([
            (SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1]),
            (
                SYMBOL_BASE + 1,
                vec![
                    SYMBOL_BASE + 1,
                    MARKER_IF_TRUE,
                    SYMBOL_BASE + 2,
                    MARKER_BRANCH_END,
                ],
            ),
        ]);
        let r = result(
            "a.ts",
            vec![
                sym("a.ts", "a", SYMBOL_BASE),
                sym("b.ts", "b", SYMBOL_BASE + 1),
                sym("c.ts", "c", SYMBOL_BASE + 2),
            ],
            chains,
            vec![],
        );
        idx.ingest(&[r], None).await.unwrap();

        // search_symbol (substring, case-insensitive) — qua resumable.
        let hits = idx
            .search_symbol_paged_resumable(
                "b",
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap()
            .page;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "b");
        assert!(
            idx.search_symbol_paged_resumable(
                "zzz",
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: 10,
                    offset: 0
                },
                None,
                None,
            )
            .await
            .unwrap()
            .page
            .is_empty()
        );

        // callees của a = [b]; của b = [c].
        let cees = idx.callees(SYMBOL_BASE).await.unwrap();
        assert_eq!(cees.len(), 1);
        assert_eq!(cees[0].name, "b");
        assert_eq!(idx.callees(SYMBOL_BASE + 1).await.unwrap()[0].name, "c");
        assert!(idx.callees(SYMBOL_BASE + 2).await.unwrap().is_empty());

        // callers direct + BFS.
        let callers1 = idx.callers(SYMBOL_BASE + 2, 1).await.unwrap();
        assert_eq!(callers1.len(), 1);
        assert_eq!(callers1[0].name, "b");
        let callers2 = idx.callers(SYMBOL_BASE + 2, 2).await.unwrap();
        assert_eq!(callers2.len(), 2);
        assert!(idx.callers(SYMBOL_BASE + 2, 3).await.unwrap().len() <= 2);

        // flow render.
        let flow = idx.flow(SYMBOL_BASE + 1).await.unwrap();
        assert_eq!(flow.symbol.name, "b");
        assert_eq!(
            flow.chain,
            vec![
                SYMBOL_BASE + 1,
                MARKER_IF_TRUE,
                SYMBOL_BASE + 2,
                MARKER_BRANCH_END
            ]
        );
        assert_eq!(flow.chain_desc[0], "b");
        assert_eq!(flow.chain_desc[1], "IF_TRUE");
        assert_eq!(flow.chain_desc[2], "c");
        assert_eq!(flow.chain_desc[3], "BRANCH_END");
        assert_eq!(flow.calls.len(), 1);
        assert_eq!(flow.calls[0].to_name, "c");
        assert_eq!(flow.calls[0].to_id, Some(SYMBOL_BASE + 2));

        // search_flow pattern [IF_TRUE, c].
        let sf = idx
            .search_flow(&[MARKER_IF_TRUE, SYMBOL_BASE + 2])
            .await
            .unwrap();
        assert_eq!(sf.len(), 1);
        assert_eq!(sf[0].function_name, "b");
        assert!(idx.search_flow(&[MARKER_LOOP]).await.unwrap().is_empty());

        // stats.
        let st = idx.stats();
        assert_eq!(st.symbols, 3);
        assert_eq!(st.chains, 2);
        assert_eq!(st.edges, 2); // a→b, b→c
        assert_eq!(st.next_id, SYMBOL_BASE + 3);
        assert_eq!(idx.version(), 1);

        // symbol_by_id / resolve.
        assert_eq!(idx.symbol_by_id(SYMBOL_BASE).unwrap().name, "a");
        let res = idx.resolve_by_name_or_id("a", 0).unwrap();
        assert!(!res.ambiguous);
        assert_eq!(res.symbol.unwrap().id, SYMBOL_BASE);
        assert!(idx.resolve_by_name_or_id("nope", 0).is_err());
    }

    #[tokio::test]
    async fn resolve_placeholder_from_call_record() {
        let mut idx = GraphIndex::in_memory();
        let calls = vec![
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "g".to_string(),
                position: 1,
                arg_exprs: vec![],
                line: 1,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            },
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "h".to_string(),
                position: 2,
                arg_exprs: vec![],
                line: 2,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            },
        ];
        let r = result(
            "f.ts",
            vec![
                sym("f.ts", "f", SYMBOL_BASE),
                sym("g.ts", "g", SYMBOL_BASE + 1),
                sym("h.ts", "h", SYMBOL_BASE + 2),
            ],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0, 0])]),
            calls,
        );
        idx.ingest(&[r], None).await.unwrap();

        // Placeholder 0 đã được resolve về id thật (exact name match).
        let flow = idx.flow(SYMBOL_BASE).await.unwrap();
        assert_eq!(
            flow.chain,
            vec![SYMBOL_BASE, SYMBOL_BASE + 1, SYMBOL_BASE + 2]
        );
        assert_eq!(flow.chain_desc, vec!["f", "g", "h"]);
        let cees = idx.callees(SYMBOL_BASE).await.unwrap();
        assert_eq!(cees.len(), 2);
    }

    // ── Dynamic dispatch: trait/interface fan-out ──

    /// Symbol với kind/scope/impl_trait tùy ý — cho test dynamic dispatch.
    /// `line` quan trọng: `build_impl_index` gán method vào trait impl theo
    /// line range của impl block (giống extractor re-parent method về def).
    #[allow(clippy::too_many_arguments)]
    fn dsym(
        id: u64,
        name: &str,
        kind: SymbolKind,
        scope_id: u64,
        impl_trait: Option<&str>,
        line: u32,
    ) -> Symbol {
        Symbol {
            id,
            name: name.to_string(),
            kind,
            scope: ScopeLevel::Global,
            scope_id,
            type_ref: 0,
            type_name: None,
            file: "t.rs".to_string(),
            line,
            // Impl block cần range chứa method con (method line = line + 1).
            end_line: if impl_trait.is_some() { line + 9 } else { line },
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "rust".to_string(),
            impl_trait: impl_trait.map(String::from),
        }
    }

    #[tokio::test]
    async fn dyn_trait_call_fans_out_to_impls() {
        let mut idx = GraphIndex::in_memory();
        let (caller, trait_id, m1, m2) = (
            SYMBOL_BASE,
            SYMBOL_BASE + 1,
            SYMBOL_BASE + 2,
            SYMBOL_BASE + 3,
        );
        let r = result(
            "t.rs",
            vec![
                dsym(caller, "caller", SymbolKind::Function, 0, None, 50),
                // trait Handler { fn handle(&self); }
                dsym(trait_id, "Handler", SymbolKind::Interface, 0, None, 1),
                dsym(
                    SYMBOL_BASE + 4,
                    "handle",
                    SymbolKind::Method,
                    trait_id,
                    None,
                    2,
                ),
                // impl Handler for A { fn handle ... } / impl Handler for B {...}
                dsym(
                    SYMBOL_BASE + 5,
                    "ImplA",
                    SymbolKind::Class,
                    0,
                    Some("Handler"),
                    10,
                ),
                dsym(m1, "handle", SymbolKind::Method, SYMBOL_BASE + 5, None, 11),
                dsym(
                    SYMBOL_BASE + 7,
                    "ImplB",
                    SymbolKind::Class,
                    0,
                    Some("Handler"),
                    30,
                ),
                dsym(m2, "handle", SymbolKind::Method, SYMBOL_BASE + 7, None, 31),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "h.handle".to_string(),
                position: 1,
                line: 51,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                receiver_text: Some("h".to_string()),
                ..Default::default()
            }],
        );
        idx.ingest(&[r], None).await.unwrap();

        // Chain giữ placeholder + raw name (không ghi đè bằng 1 best guess).
        let flow = idx.flow(caller).await.unwrap();
        assert_eq!(flow.chain, vec![caller, 0]);
        assert_eq!(flow.chain_desc[1], "h.handle");
        let fc = &flow.calls[0];
        assert_eq!(fc.resolved_via, ResolutionKind::HierarchyFanOut);
        let mut names = fc.candidate_names.clone();
        names.sort();
        assert_eq!(names, vec!["handle", "handle"]);

        // Mỗi candidate là một edge ẩn trên slot 0 → edge_count = 2.
        assert_eq!(idx.stats().edges, 2);
    }

    #[tokio::test]
    async fn trait_method_without_impl_is_unresolved_dynamic() {
        let mut idx = GraphIndex::in_memory();
        let (caller, trait_id) = (SYMBOL_BASE, SYMBOL_BASE + 1);
        let r = result(
            "t.rs",
            vec![
                dsym(caller, "caller", SymbolKind::Function, 0, None, 10),
                dsym(trait_id, "Missing", SymbolKind::Interface, 0, None, 1),
                dsym(
                    SYMBOL_BASE + 2,
                    "run",
                    SymbolKind::Method,
                    trait_id,
                    None,
                    2,
                ),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "x.run".to_string(),
                position: 1,
                line: 11,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            }],
        );
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(caller).await.unwrap();
        assert_eq!(flow.chain, vec![caller, 0]);
        assert_eq!(flow.calls[0].to_name, "x.run");
        assert_eq!(
            flow.calls[0].resolved_via,
            ResolutionKind::UnresolvedDynamic
        );
        assert!(flow.calls[0].candidate_names.is_empty());
        assert_eq!(idx.stats().edges, 0);
    }

    #[tokio::test]
    async fn static_resolution_wins_over_hierarchy_fanout() {
        let mut idx = GraphIndex::in_memory();
        let (caller, static_fn, trait_id) = (SYMBOL_BASE, SYMBOL_BASE + 1, SYMBOL_BASE + 2);
        let r = result(
            "t.rs",
            vec![
                dsym(caller, "caller", SymbolKind::Function, 0, None, 10),
                // hàm tĩnh trùng tên method của trait.
                dsym(static_fn, "handle", SymbolKind::Function, 0, None, 5),
                dsym(trait_id, "Handler", SymbolKind::Interface, 0, None, 1),
                dsym(
                    SYMBOL_BASE + 3,
                    "handle",
                    SymbolKind::Method,
                    trait_id,
                    None,
                    2,
                ),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "handle".to_string(),
                position: 1,
                line: 11,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            }],
        );
        idx.ingest(&[r], None).await.unwrap();

        // Resolve tĩnh thắng — chain slot được ghi đè, không fan-out.
        let flow = idx.flow(caller).await.unwrap();
        assert_eq!(flow.chain, vec![caller, static_fn]);
        assert_eq!(flow.calls[0].to_id, Some(static_fn));
        assert_eq!(flow.calls[0].resolved_via, ResolutionKind::Exact);
        assert!(flow.calls[0].candidate_names.is_empty());
    }

    #[tokio::test]
    async fn java_interface_call_fans_out_to_implementations() {
        let mut idx = GraphIndex::in_memory();
        let (caller, iface_id, m1, m2) = (
            SYMBOL_BASE,
            SYMBOL_BASE + 1,
            SYMBOL_BASE + 2,
            SYMBOL_BASE + 3,
        );
        let r = result(
            "T.java",
            vec![
                dsym(caller, "caller", SymbolKind::Function, 0, None, 30),
                dsym(iface_id, "Payment", SymbolKind::Interface, 0, None, 1),
                dsym(
                    SYMBOL_BASE + 4,
                    "pay",
                    SymbolKind::Method,
                    iface_id,
                    None,
                    2,
                ),
                dsym(
                    SYMBOL_BASE + 5,
                    "CardPayment",
                    SymbolKind::Class,
                    0,
                    Some("Payment"),
                    10,
                ),
                dsym(m1, "pay", SymbolKind::Method, SYMBOL_BASE + 5, None, 11),
                dsym(
                    SYMBOL_BASE + 7,
                    "CashPayment",
                    SymbolKind::Class,
                    0,
                    Some("Payment"),
                    20,
                ),
                dsym(m2, "pay", SymbolKind::Method, SYMBOL_BASE + 7, None, 21),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "p.pay".to_string(),
                position: 1,
                line: 31,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            }],
        );
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(caller).await.unwrap();
        assert_eq!(flow.calls[0].resolved_via, ResolutionKind::HierarchyFanOut);
        assert_eq!(flow.calls[0].candidate_names.len(), 2);
        assert_eq!(idx.stats().edges, 2);
    }

    #[tokio::test]
    async fn cpp_base_class_call_includes_overrides_and_base_method() {
        let mut idx = GraphIndex::in_memory();
        let (caller, base_id, derived_impl_id, derived_run, base_run) = (
            SYMBOL_BASE,
            SYMBOL_BASE + 1,
            SYMBOL_BASE + 2,
            SYMBOL_BASE + 3,
            SYMBOL_BASE + 4,
        );
        let r = result(
            "T.cpp",
            vec![
                dsym(caller, "caller", SymbolKind::Function, 0, None, 30),
                dsym(base_id, "Base", SymbolKind::Class, 0, None, 1),
                dsym(base_run, "run", SymbolKind::Method, base_id, None, 2),
                // Derived : public Base — impl trait "Base".
                dsym(
                    derived_impl_id,
                    "Derived",
                    SymbolKind::Class,
                    0,
                    Some("Base"),
                    10,
                ),
                dsym(
                    derived_run,
                    "run",
                    SymbolKind::Method,
                    derived_impl_id,
                    None,
                    11,
                ),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "b.run".to_string(),
                position: 1,
                line: 31,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            }],
        );
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(caller).await.unwrap();
        assert_eq!(flow.calls[0].resolved_via, ResolutionKind::HierarchyFanOut);
        // Derived::run (override) + Base::run (non-pure virtual, có body).
        let mut names = flow.calls[0].candidate_names.clone();
        names.sort();
        assert_eq!(names, vec!["run", "run"]);
        assert_eq!(idx.stats().edges, 2);
    }

    #[tokio::test]
    async fn go_interface_call_fans_out_to_all_same_name_methods() {
        // Go thỏa interface ngầm — không có implements clause. dsym set
        // language "rust", nên dựng symbol inline với language "go".
        let mut idx = GraphIndex::in_memory();
        let gsym = |id: u64, name: &str, kind: SymbolKind, scope_id: u64, line: u32| Symbol {
            id,
            name: name.to_string(),
            kind,
            scope: ScopeLevel::Global,
            scope_id,
            type_ref: 0,
            type_name: None,
            file: "w.go".to_string(),
            line,
            end_line: line,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "go".to_string(),
            impl_trait: None,
        };
        let (caller, iface_id) = (SYMBOL_BASE, SYMBOL_BASE + 1);
        let r = result(
            "w.go",
            vec![
                gsym(caller, "sink", SymbolKind::Function, 0, 20),
                gsym(iface_id, "Writer", SymbolKind::Interface, 0, 1),
                gsym(SYMBOL_BASE + 2, "Write", SymbolKind::Method, iface_id, 2),
                // 2 type có method Write (thỏa ngầm).
                gsym(SYMBOL_BASE + 3, "Buf", SymbolKind::Class, 0, 5),
                gsym(
                    SYMBOL_BASE + 4,
                    "Write",
                    SymbolKind::Method,
                    SYMBOL_BASE + 3,
                    6,
                ),
                gsym(SYMBOL_BASE + 5, "File", SymbolKind::Class, 0, 10),
                gsym(
                    SYMBOL_BASE + 6,
                    "Write",
                    SymbolKind::Method,
                    SYMBOL_BASE + 5,
                    11,
                ),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "w.Write".to_string(),
                position: 1,
                line: 21,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            }],
        );
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(caller).await.unwrap();
        assert_eq!(flow.calls[0].resolved_via, ResolutionKind::HierarchyFanOut);
        // Buf.Write + File.Write (method của interface bị loại).
        assert_eq!(flow.calls[0].candidate_names.len(), 2);
        assert_eq!(idx.stats().edges, 2);
    }

    #[tokio::test]
    async fn fn_ptr_assign_resolves_indirect_call() {
        let mut idx = GraphIndex::in_memory();
        let (caller, compile_file, dtrace) = (SYMBOL_BASE, SYMBOL_BASE + 1, SYMBOL_BASE + 2);
        let mut r = result(
            "zend.c",
            vec![
                dsym(caller, "call_site", SymbolKind::Function, 0, None, 10),
                dsym(
                    compile_file,
                    "compile_file",
                    SymbolKind::Function,
                    0,
                    None,
                    1,
                ),
                dsym(
                    dtrace,
                    "dtrace_compile_file",
                    SymbolKind::Function,
                    0,
                    None,
                    2,
                ),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "zend_compile_file".to_string(),
                position: 1,
                line: 11,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            }],
        );
        // Hai assign-site: init (mặc định) + override ở runtime.
        r.fn_ptr_assigns = vec![
            FnPtrAssign {
                var: "zend_compile_file".to_string(),
                target: "compile_file".to_string(),
                line: 5,
            },
            FnPtrAssign {
                var: "zend_compile_file".to_string(),
                target: "dtrace_compile_file".to_string(),
                line: 9,
            },
        ];
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(caller).await.unwrap();
        // Không ghi đè chain (raw name giữ nguyên).
        assert_eq!(flow.chain, vec![caller, 0]);
        assert_eq!(flow.calls[0].resolved_via, ResolutionKind::PointerAssign);
        let mut names = flow.calls[0].candidate_names.clone();
        names.sort();
        assert_eq!(names, vec!["compile_file", "dtrace_compile_file"]);
        assert_eq!(idx.stats().edges, 2);
    }

    #[tokio::test]
    async fn go_method_set_fanout_only_implementing_types() {
        // Writer{Write, Flush}; Buf có đủ {Write, Flush}; File chỉ có {Write}
        // → File KHÔNG thỏa Writer. w.Write() fan-out chỉ Buf.Write.
        let mut idx = GraphIndex::in_memory();
        let gsym = |id: u64,
                    name: &str,
                    kind: SymbolKind,
                    scope_id: u64,
                    type_name: Option<&str>,
                    line: u32| Symbol {
            id,
            name: name.to_string(),
            kind,
            scope: ScopeLevel::Global,
            scope_id,
            type_ref: 0,
            type_name: type_name.map(String::from),
            file: "w.go".to_string(),
            line,
            end_line: line,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "go".to_string(),
            impl_trait: None,
        };
        let (caller, iface_id) = (SYMBOL_BASE, SYMBOL_BASE + 1);
        let r = result(
            "w.go",
            vec![
                gsym(caller, "sink", SymbolKind::Function, 0, None, 30),
                gsym(iface_id, "Writer", SymbolKind::Interface, 0, None, 1),
                gsym(
                    SYMBOL_BASE + 2,
                    "Write",
                    SymbolKind::Method,
                    iface_id,
                    None,
                    2,
                ),
                gsym(
                    SYMBOL_BASE + 3,
                    "Flush",
                    SymbolKind::Method,
                    iface_id,
                    None,
                    3,
                ),
                gsym(SYMBOL_BASE + 4, "Buf", SymbolKind::Class, 0, None, 6),
                gsym(
                    SYMBOL_BASE + 5,
                    "Write",
                    SymbolKind::Method,
                    SYMBOL_BASE + 4,
                    Some("Buf"),
                    7,
                ),
                gsym(
                    SYMBOL_BASE + 6,
                    "Flush",
                    SymbolKind::Method,
                    SYMBOL_BASE + 4,
                    Some("Buf"),
                    8,
                ),
                gsym(SYMBOL_BASE + 7, "File", SymbolKind::Class, 0, None, 11),
                gsym(
                    SYMBOL_BASE + 8,
                    "Write",
                    SymbolKind::Method,
                    SYMBOL_BASE + 7,
                    Some("File"),
                    12,
                ),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "w.Write".to_string(),
                position: 1,
                line: 31,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            }],
        );
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(caller).await.unwrap();
        assert_eq!(flow.calls[0].resolved_via, ResolutionKind::HierarchyFanOut);
        // Chỉ Buf.Write — File không thỏa method set của Writer.
        assert_eq!(flow.calls[0].candidate_names, vec!["Write"]);
    }

    #[tokio::test]
    async fn import_alias_resolves_qualified_call() {
        // File caller import alias `orders` → path "corp/api/orders"; symbol Get
        // nằm trong file chứa segment "orders" → resolve qua Alias.
        let mut idx = GraphIndex::in_memory();
        let (caller, get_id) = (SYMBOL_BASE, SYMBOL_BASE + 1);
        let mut r = result(
            "cmd/main.go",
            vec![
                dsym(caller, "main", SymbolKind::Function, 0, None, 10),
                dsym(get_id, "Get", SymbolKind::Function, 0, None, 1),
            ],
            HashMap::from([(caller, vec![caller, 0])]),
            vec![CallRecord {
                caller_id: caller,
                call_name: "orders.Get".to_string(),
                position: 1,
                line: 11,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            }],
        );
        // Symbol Get sống trong file package orders (path chứa segment cuối).
        if let Some(get) = r.symbols.iter_mut().find(|s| s.name == "Get") {
            get.file = "corp/api/orders/handler.go".to_string();
        }
        r.file_deps = Some(crate::FileDeps {
            package: Some("main".to_string()),
            imports: vec![crate::ImportAlias {
                alias: "orders".to_string(),
                path: "corp/api/orders".to_string(),
            }],
        });
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(caller).await.unwrap();
        assert_eq!(flow.chain, vec![caller, get_id]);
        assert_eq!(flow.calls[0].to_id, Some(get_id));
        assert_eq!(flow.calls[0].resolved_via, ResolutionKind::Alias);
    }

    #[tokio::test]
    async fn serde_old_call_record_blob_defaults() {
        // Blob cũ (trước khi thêm receiver_text/resolved_via/candidates) phải
        // deserialize đúng với giá trị default.
        let json = r#"{"caller_id":1,"call_name":"fmt.Println","position":1,"line":7,
            "condition":null,"is_loop_body":false,"effect":"log","effect_desc":null,
            "target_class":null,"target_method":null,"into_var":null,"is_new_var":false}"#;
        let rec: CallRecord = serde_json::from_str(json).unwrap();
        assert_eq!(rec.resolved_via, ResolutionKind::Exact);
        assert_eq!(rec.receiver_text, None);
        assert!(rec.candidates.is_empty());
    }

    #[tokio::test]
    async fn unresolved_call_keeps_raw_name_in_flow() {
        let mut idx = GraphIndex::in_memory();
        // "fmt.Println" không có symbol tương ứng → không resolve được.
        let calls = vec![CallRecord {
            caller_id: SYMBOL_BASE,
            call_name: "fmt.Println".to_string(),
            position: 1,
            arg_exprs: vec!["msg".to_string()],
            line: 7,
            condition: None,
            is_loop_body: false,
            effect: EffectType::Log,
            effect_desc: None,
            target_class: None,
            target_method: None,
            into_var: None,
            is_new_var: false,
            ..Default::default()
        }];
        let r = result(
            "f.ts",
            vec![sym("f.ts", "f", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0])]),
            calls,
        );
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(SYMBOL_BASE).await.unwrap();
        assert_eq!(flow.chain, vec![SYMBOL_BASE, 0]);
        assert_eq!(flow.chain_desc[1], "fmt.Println");
        assert_eq!(flow.calls.len(), 1);
        assert_eq!(flow.calls[0].to_name, "fmt.Println");
        assert_eq!(flow.calls[0].to_id, None);
        assert_eq!(flow.calls[0].line, 7);
        assert_eq!(flow.calls[0].effect, EffectType::Log);

        // callees chỉ trả symbol đã resolve.
        assert!(idx.callees(SYMBOL_BASE).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn resolve_ambiguous_on_duplicate_name() {
        let mut idx = GraphIndex::in_memory();
        let r1 = result(
            "a.ts",
            vec![sym("a.ts", "process", SYMBOL_BASE)],
            HashMap::new(),
            vec![],
        );
        let r2 = result(
            "b.ts",
            vec![sym("b.ts", "process", SYMBOL_BASE)],
            HashMap::new(),
            vec![],
        );
        idx.ingest(&[r1, r2], None).await.unwrap();

        // Cùng tên 2 file → ambiguous với đủ matches.
        let res = idx.resolve_by_name_or_id("process", 0).unwrap();
        assert!(res.ambiguous);
        assert_eq!(res.matches.len(), 2);
        assert!(res.symbol.is_none());

        // Resolve theo id → không ambiguous.
        let res2 = idx.resolve_by_name_or_id("process", SYMBOL_BASE).unwrap();
        assert!(!res2.ambiguous);
        assert_eq!(res2.symbol.unwrap().id, SYMBOL_BASE);

        // search_symbol mở rộng cả 2 symbol trùng tên — qua resumable.
        let hits = idx
            .search_symbol_paged_resumable(
                "process",
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap()
            .page;
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn callers_by_call_name_finds_unresolved_calls() {
        let mut idx = GraphIndex::in_memory();
        let calls = vec![CallRecord {
            caller_id: SYMBOL_BASE,
            call_name: "fmt.Println".to_string(),
            position: 1,
            arg_exprs: vec![],
            line: 3,
            condition: None,
            is_loop_body: false,
            effect: EffectType::Log,
            effect_desc: None,
            target_class: None,
            target_method: None,
            into_var: None,
            is_new_var: false,
            ..Default::default()
        }];
        let r = result(
            "f.ts",
            vec![sym("f.ts", "f", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0])]),
            calls,
        );
        idx.ingest(&[r], None).await.unwrap();

        let hits = idx.callers_by_call_name("println", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].func_name, "f");
        assert_eq!(hits[0].call_sites.len(), 1);
        assert_eq!(hits[0].call_sites[0].call_name, "fmt.Println");
        assert_eq!(hits[0].call_sites[0].line, 3);
    }

    /// Call records phải gom theo caller id **đã remap** (global), không theo
    /// `caller_id` local của file — hai file đều dùng `SYMBOL_BASE` làm caller
    /// local. Gom nhầm theo id local thì blob của file 1 nuốt luôn record của
    /// file 2 và `flow` của caller thứ hai mất sạch call.
    #[tokio::test]
    async fn call_records_grouped_by_global_caller_id() {
        let call = |name: &str| CallRecord {
            caller_id: SYMBOL_BASE,
            call_name: name.to_string(),
            position: 1,
            arg_exprs: vec![],
            line: 7,
            condition: None,
            is_loop_body: false,
            effect: EffectType::None,
            effect_desc: None,
            target_class: None,
            target_method: None,
            into_var: None,
            is_new_var: false,
            ..Default::default()
        };
        let mut idx = GraphIndex::in_memory();
        let first = result(
            "first.ts",
            vec![sym("first.ts", "first_fn", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0])]),
            vec![call("alpha.only_a")],
        );
        let second = result(
            "second.ts",
            vec![sym("second.ts", "second_fn", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0])]),
            vec![call("beta.only_b")],
        );
        idx.ingest(&[first, second], None).await.unwrap();

        let a = SYMBOL_BASE;
        let b = SYMBOL_BASE + 1;
        let fa = idx.flow(a).await.unwrap();
        let fb = idx.flow(b).await.unwrap();
        assert_eq!(fa.calls.len(), 1);
        assert_eq!(fa.calls[0].to_name, "alpha.only_a");
        assert_eq!(fb.calls.len(), 1);
        assert_eq!(fb.calls[0].to_name, "beta.only_b");
    }

    #[tokio::test]
    async fn loop_marker_golden_chain() {
        let mut idx = GraphIndex::in_memory();
        // Python-style: for item: validate(item); save(item)
        // → [F, LOOP, validate, save, LOOP_BACK]
        let chains = HashMap::from([(
            SYMBOL_BASE,
            vec![
                SYMBOL_BASE,
                MARKER_LOOP,
                SYMBOL_BASE + 1,
                SYMBOL_BASE + 2,
                MARKER_LOOP_BACK,
            ],
        )]);
        let r = result(
            "f.py",
            vec![
                sym("f.py", "f", SYMBOL_BASE),
                sym("f.py", "validate", SYMBOL_BASE + 1),
                sym("f.py", "save", SYMBOL_BASE + 2),
            ],
            chains,
            vec![],
        );
        idx.ingest(&[r], None).await.unwrap();

        let flow = idx.flow(SYMBOL_BASE).await.unwrap();
        assert_eq!(
            flow.chain,
            vec![
                SYMBOL_BASE,
                MARKER_LOOP,
                SYMBOL_BASE + 1,
                SYMBOL_BASE + 2,
                MARKER_LOOP_BACK
            ]
        );
        assert_eq!(
            flow.chain_desc,
            vec!["f", "LOOP", "validate", "save", "LOOP_BACK"]
        );

        // search_flow: pattern [LOOP, validate] tìm được f.
        let sf = idx
            .search_flow(&[MARKER_LOOP, SYMBOL_BASE + 1])
            .await
            .unwrap();
        assert_eq!(sf.len(), 1);
        assert_eq!(sf[0].function_name, "f");
    }

    /// Reopen file sqlite → rebuild index giữ nguyên dữ liệu + version.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_persist_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = format!("sqlite://{}/db.sqlite", dir.path().to_string_lossy());
        let chains = HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1])]);
        let r = result(
            "a.ts",
            vec![
                sym("a.ts", "a", SYMBOL_BASE),
                sym("b.ts", "b", SYMBOL_BASE + 1),
            ],
            chains,
            vec![],
        );
        {
            let mut idx = GraphIndex::open(&path).await.unwrap();
            assert_eq!(idx.version(), 0);
            idx.ingest(&[r], None).await.unwrap();
            assert_eq!(idx.version(), 1);
            assert_eq!(idx.callees(SYMBOL_BASE).await.unwrap().len(), 1);
        }
        // Reopen: version giữ nguyên, dữ liệu query lại được.
        let idx = GraphIndex::open(&path).await.unwrap();
        assert_eq!(idx.version(), 1);
        assert_eq!(idx.stats().symbols, 2);
        assert_eq!(idx.stats().chains, 1);
        assert_eq!(idx.stats().edges, 1);
        assert_eq!(idx.callees(SYMBOL_BASE).await.unwrap()[0].name, "b");
        assert_eq!(idx.symbol_by_id(SYMBOL_BASE + 1).unwrap().file, "b.ts");
    }

    /// Embedding là OPT-IN + PERSIST: bật backend "hashing", ingest (lưu vector
    /// vào sqlite), reopen → vector index được rebuild TỪ embeddings đã lưu
    /// (không re-embed), semantic search vẫn chạy.
    #[tokio::test]
    async fn sqlite_embeddings_persist_and_reopen() {
        crate::embeddings::set_embedding_config(crate::embeddings::EmbeddingConfig::from_raw(
            Some("hashing"),
            None,
            None,
            None,
            None,
        ));
        let dir = tempfile::tempdir().unwrap();
        let path = format!("sqlite://{}/db.sqlite", dir.path().to_string_lossy());
        let r = result(
            "a.ts",
            vec![
                sym("auth.rs", "authenticate_user", SYMBOL_BASE),
                sym("db.rs", "query_database", SYMBOL_BASE + 1),
            ],
            HashMap::new(),
            vec![],
        );
        {
            let mut idx = GraphIndex::open(&path).await.unwrap();
            idx.ingest(&[r], None).await.unwrap();
            // Semantic chạy được (embedding enabled).
            let sem = idx
                .search_symbol_paged_resumable(
                    "authenticte",
                    None,
                    SymbolMatch::Semantic,
                    Pagination {
                        limit: 10,
                        offset: 0,
                    },
                    None,
                    None,
                )
                .await
                .unwrap()
                .page;
            assert_eq!(sem[0].name, "authenticate_user");
        }
        // Reopen — vector index phải được load từ embeddings đã persist.
        let idx = GraphIndex::open(&path).await.unwrap();
        let sem = idx
            .search_symbol_paged_resumable(
                "authenticte",
                None,
                SymbolMatch::Semantic,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap()
            .page;
        assert_eq!(sem[0].name, "authenticate_user");
    }

    /// Ingest 2 lần = full re-index — dữ liệu cũ biến mất, id gán lại từ đầu.
    #[tokio::test]
    async fn ingest_twice_is_full_reindex() {
        let mut idx = GraphIndex::in_memory();
        let r1 = result(
            "a.ts",
            vec![sym("a.ts", "a", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
            vec![],
        );
        idx.ingest(&[r1], None).await.unwrap();
        assert_eq!(idx.stats().symbols, 1);

        let r2 = result(
            "b.ts",
            vec![
                sym("b.ts", "b", SYMBOL_BASE),
                sym("b.ts", "c", SYMBOL_BASE + 1),
            ],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1])]),
            vec![],
        );
        idx.ingest(&[r2], None).await.unwrap();
        assert_eq!(idx.stats().symbols, 2);
        assert!(idx.symbol_by_id(SYMBOL_BASE + 2).is_none());
        let res = idx.resolve_by_name_or_id("a", 0);
        assert!(res.is_err(), "symbol cũ phải biến mất sau full re-index");
        assert_eq!(idx.version(), 2);
    }

    /// Class/scope queries: methods, class info, function scope, list by kind,
    /// annotation search, paged symbol search.
    #[tokio::test]
    async fn class_and_scope_queries() {
        let mut idx = GraphIndex::in_memory();
        let mut cls = sym("svc.rs", "OrderService", SYMBOL_BASE);
        cls.kind = SymbolKind::Class;
        let mut method1 = sym("svc.rs", "getOrders", SYMBOL_BASE + 1);
        method1.kind = SymbolKind::Method;
        method1.scope_id = SYMBOL_BASE;
        method1.signature = Some("fn getOrders(userId: i32) -> Vec<Order>".to_string());
        let mut field = sym("svc.rs", "repo", SYMBOL_BASE + 2);
        field.kind = SymbolKind::Field;
        field.scope_id = SYMBOL_BASE;
        let mut func = sym("util.rs", "validate", SYMBOL_BASE + 3);
        func.kind = SymbolKind::Function;
        let mut param = sym("util.rs", "user", SYMBOL_BASE + 4);
        param.kind = SymbolKind::Parameter;
        param.scope_id = SYMBOL_BASE + 3;
        let mut local = sym("util.rs", "tmp", SYMBOL_BASE + 5);
        local.kind = SymbolKind::Variable;
        local.scope_id = SYMBOL_BASE + 3;
        let mut controller = sym("ctrl.rs", "OrderController", SYMBOL_BASE + 6);
        controller.kind = SymbolKind::Class;
        controller.annotations = vec![Annotation {
            name: "RestController".to_string(),
            args: HashMap::new(),
            line: 1,
        }];
        let mut iface = sym("repo.rs", "OrderRepository", SYMBOL_BASE + 7);
        iface.kind = SymbolKind::Interface;

        let r = result(
            "svc.rs",
            vec![cls, method1, field, func, param, local, controller, iface],
            HashMap::new(),
            vec![],
        );
        idx.ingest(&[r], None).await.unwrap();

        // list_methods_of_class — chỉ Function/Method, sort theo tên.
        let methods = idx.list_methods_of_class(SYMBOL_BASE);
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "getOrders");
        assert_eq!(
            methods[0].signature.as_deref(),
            Some("fn getOrders(userId: i32) -> Vec<Order>")
        );

        // get_class_info — fields và methods tách riêng.
        let info = idx.get_class_info(SYMBOL_BASE).unwrap();
        assert_eq!(info.class.name, "OrderService");
        assert_eq!(info.fields.len(), 1);
        assert_eq!(info.fields[0].name, "repo");
        assert_eq!(info.methods.len(), 1);
        assert!(
            idx.get_class_info(SYMBOL_BASE + 3).is_none(),
            "function không phải class"
        );

        // function_scope — parameters + locals.
        let scope = idx.function_scope(SYMBOL_BASE + 3).unwrap();
        assert_eq!(scope.function.name, "validate");
        assert_eq!(scope.parameters.len(), 1);
        assert_eq!(scope.parameters[0].name, "user");
        assert_eq!(scope.locals.len(), 1);
        assert_eq!(scope.locals[0].name, "tmp");

        // list_symbols_by_kind — sort theo tên, phân trang.
        let (classes, total) = idx.list_symbols_by_kind(SymbolKind::Class, 10, 0);
        assert_eq!(total, 2);
        assert_eq!(classes[0].name, "OrderController");
        assert_eq!(classes[1].name, "OrderService");
        let (one, total) = idx.list_symbols_by_kind(SymbolKind::Class, 1, 0);
        assert_eq!(total, 2);
        assert_eq!(one.len(), 1);
        let (second, _) = idx.list_symbols_by_kind(SymbolKind::Class, 1, 1);
        assert_eq!(second[0].name, "OrderService");
        let (_interfaces, total) = idx.list_symbols_by_kind(SymbolKind::Interface, 10, 0);
        assert_eq!(total, 1);

        // search_by_annotation — case-insensitive substring.
        let (hits, total, truncated) = idx.search_by_annotation("restcontroller", None, 0, 10);
        assert_eq!(total, 1);
        assert_eq!(hits[0].name, "OrderController");
        assert!(!truncated);
        let (hits, total, truncated) = idx.search_by_annotation("GetMapping", None, 0, 10);
        assert_eq!(total, 0);
        assert!(hits.is_empty());
        assert!(!truncated);
        let (_, total, _) = idx.search_by_annotation("controller", Some(SymbolKind::Class), 0, 1);
        assert_eq!(total, 1, "kind filter loại bỏ match không đúng kind");

        // search_symbol_paged — prefix/suffix/exact + kind filter (resumable).
        let out = idx
            .search_symbol_paged_resumable(
                "order",
                Some(SymbolKind::Class),
                SymbolMatch::Prefix,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap();
        let hits = out.page;
        let total = out.total;
        assert_eq!(
            total, 2,
            "OrderService + OrderController khớp prefix 'order' + kind class"
        );
        assert_eq!(hits[0].name, "OrderController");
        assert_eq!(hits[1].name, "OrderService");
        let out = idx
            .search_symbol_paged_resumable(
                "service",
                Some(SymbolKind::Class),
                SymbolMatch::Suffix,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap();
        let hits = out.page;
        let total = out.total;
        assert_eq!(total, 1);
        assert_eq!(hits[0].name, "OrderService");
        let out = idx
            .search_symbol_paged_resumable(
                "validate",
                None,
                SymbolMatch::Exact,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap();
        let hits = out.page;
        let total = out.total;
        assert_eq!(total, 1);
        assert_eq!(hits[0].name, "validate");
        // contains + pagination. Sort theo tên lowercase (nhất quán với search
        // case-insensitive): "getorders" đứng trước "order*".
        let out = idx
            .search_symbol_paged_resumable(
                "order",
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: 2,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap();
        let page0 = out.page;
        let total = out.total;
        assert_eq!(
            total, 4,
            "OrderService, OrderController, OrderRepository + getOrders"
        );
        assert_eq!(page0.len(), 2);
        assert_eq!(page0[0].name, "getOrders");
        assert_eq!(page0[1].name, "OrderController");
        let out = idx
            .search_symbol_paged_resumable(
                "order",
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: 2,
                    offset: 2,
                },
                None,
                None,
            )
            .await
            .unwrap();
        let page1 = out.page;
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].name, "OrderRepository");
        assert_eq!(page1[1].name, "OrderService");
    }

    /// Resumable search == direct search, mọi mode + kind filter + offset + tên
    /// trùng. Dùng deadline ĐÃ HẾT HẠN ở call đầu — monotonic clock nên check
    /// `now >= deadline` luôn true → chắc chắn timed_out ngay, sinh checkpoint
    /// hợp lệ; call sau resume không deadline → hoàn tất. Kết quả phải khớp
    /// `search_symbol_paged` (không lặp, không mất, cùng thứ tự).
    #[tokio::test]
    async fn search_paged_resumable_matches_direct() {
        let mut idx = GraphIndex::in_memory();
        let mut results = Vec::new();
        let mut id = SYMBOL_BASE;
        // 600 tên "order_*" (Function).
        for i in 0..600 {
            let name = format!("order_{i:04}");
            results.push(result(
                "a.ts",
                vec![sym("a.ts", &name, id)],
                HashMap::new(),
                vec![],
            ));
            id += 1;
        }
        // 300 symbol Class TRÙNG TÊN "OrderService" — dedup theo (name, id),
        // mỗi id là 1 kết quả riêng (test kind filter + duplicate names).
        for _ in 0..300 {
            let mut s = sym("a.ts", "OrderService", id);
            s.kind = SymbolKind::Class;
            results.push(result("a.ts", vec![s], HashMap::new(), vec![]));
            id += 1;
        }
        // 100 tên "x_order_*" — khớp contains, không khớp prefix/exact/suffix "service".
        for i in 0..100 {
            let name = format!("x_order_{i:03}");
            results.push(result(
                "a.ts",
                vec![sym("a.ts", &name, id)],
                HashMap::new(),
                vec![],
            ));
            id += 1;
        }
        idx.ingest(&results, None).await.unwrap();

        // Driver: call đầu deadline hết hạn → timed_out (sinh checkpoint); call
        // sau resume không deadline → hoàn tất. So với direct (không deadline).
        async fn chained(
            idx: &GraphIndex,
            q: &str,
            kind: Option<SymbolKind>,
            mode: SymbolMatch,
            limit: usize,
            offset: usize,
        ) -> PagedSearchOutcome {
            let first = idx
                .search_symbol_paged_resumable(
                    q,
                    kind,
                    mode,
                    Pagination { limit, offset },
                    None,
                    Some(Instant::now()),
                )
                .await
                .unwrap();
            assert!(first.timed_out, "expired deadline must time out");
            assert!(first.cursor.is_some(), "timeout must carry a cursor");
            let out = idx
                .search_symbol_paged_resumable(
                    q,
                    kind,
                    mode,
                    Pagination { limit, offset },
                    first.cursor,
                    None,
                )
                .await
                .unwrap();
            assert!(!out.timed_out, "resume without deadline must complete");
            out
        }

        let cases: Vec<(&str, Option<SymbolKind>, SymbolMatch, usize, usize)> = vec![
            ("order", None, SymbolMatch::Contains, 20, 0),
            (
                "order",
                Some(SymbolKind::Function),
                SymbolMatch::Contains,
                20,
                0,
            ),
            (
                "order",
                Some(SymbolKind::Class),
                SymbolMatch::Contains,
                30,
                7,
            ),
            ("order", None, SymbolMatch::Prefix, 10, 5),
            ("service", None, SymbolMatch::Suffix, 20, 0),
            (
                "orderservice",
                Some(SymbolKind::Class),
                SymbolMatch::Exact,
                20,
                0,
            ),
        ];
        for (q, kind, mode, limit, offset) in cases {
            let direct = idx
                .search_symbol_paged_resumable(
                    q,
                    kind,
                    mode,
                    Pagination { limit, offset },
                    None,
                    None,
                )
                .await
                .unwrap();
            let direct_page = direct.page;
            let direct_total = direct.total;
            let chained = chained(&idx, q, kind, mode, limit, offset).await;
            assert_eq!(
                chained.total, direct_total,
                "total differs for {q} {mode:?} {kind:?}"
            );
            let got: Vec<(u64, String)> = chained
                .page
                .iter()
                .map(|s| (s.id, s.name.clone()))
                .collect();
            let want: Vec<(u64, String)> =
                direct_page.iter().map(|s| (s.id, s.name.clone())).collect();
            assert_eq!(got, want, "page differs for {q} {mode:?} {kind:?}");
        }
    }

    /// Multi-step resume chain: seed đủ lớn, deadline ngắn thật → mỗi call làm
    /// ≥1 bước rồi timed out (có thể nhiều lần), chain tới khi hoàn tất. Kết
    /// quả cuối phải khớp direct. Vòng lặp có cận phòng hờ (entry-expiry dưới
    /// tải nặng có thể làm 1 call không tiến) — fail hẳn thay vì treo.
    #[tokio::test]
    async fn search_paged_resumable_multistep_chain() {
        let mut idx = GraphIndex::in_memory();
        let mut results = Vec::new();
        for (id, i) in (SYMBOL_BASE..).zip(0..4000) {
            let name = format!("order_{i:04}");
            results.push(result(
                "a.ts",
                vec![sym("a.ts", &name, id)],
                HashMap::new(),
                vec![],
            ));
        }
        idx.ingest(&results, None).await.unwrap();

        let direct = idx
            .search_symbol_paged_resumable(
                "order",
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap();
        let direct_page = direct.page;
        let direct_total = direct.total;
        assert_eq!(direct_total, 4000);

        // Call đầu deadline hết hạn → chắc chắn timed_out (tạo checkpoint).
        let mut cursor = idx
            .search_symbol_paged_resumable(
                "order",
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                Some(Instant::now()),
            )
            .await
            .unwrap()
            .cursor;
        assert!(cursor.is_some());

        // Resume với deadline thật ngắn — lặp tới khi hoàn tất (mỗi lần tiến ≥1 bước).
        let mut completed = None;
        for _ in 0..2000 {
            let out = idx
                .search_symbol_paged_resumable(
                    "order",
                    None,
                    SymbolMatch::Contains,
                    Pagination {
                        limit: 10,
                        offset: 0,
                    },
                    cursor,
                    Some(Instant::now() + std::time::Duration::from_millis(10)),
                )
                .await
                .unwrap();
            if out.timed_out {
                cursor = out.cursor;
                continue;
            }
            completed = Some(out);
            break;
        }
        let out = completed.expect("resume chain must terminate");
        assert_eq!(out.total, direct_total);
        let got: Vec<(u64, String)> = out.page.iter().map(|s| (s.id, s.name.clone())).collect();
        let want: Vec<(u64, String)> = direct_page.iter().map(|s| (s.id, s.name.clone())).collect();
        assert_eq!(got, want);
    }

    /// dependencies_report — module prefix từ call names, internal vs external.
    #[tokio::test]
    async fn dependencies_report_splits_internal_external() {
        let mut idx = GraphIndex::in_memory();
        let calls = vec![
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "fmt.Println".to_string(),
                position: 1,
                arg_exprs: vec![],
                line: 1,
                condition: None,
                is_loop_body: false,
                effect: EffectType::Log,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            },
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "requests.get".to_string(),
                position: 2,
                arg_exprs: vec![],
                line: 2,
                condition: None,
                is_loop_body: false,
                effect: EffectType::HttpCall,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            },
            // Internal call: class "OrderService" trong repo, method getOrders.
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "OrderService.getOrders".to_string(),
                position: 3,
                arg_exprs: vec![],
                line: 3,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
                into_var: None,
                is_new_var: false,
                ..Default::default()
            },
        ];
        let mut cls = sym("svc.rs", "OrderService", SYMBOL_BASE + 1);
        cls.kind = SymbolKind::Class;
        let r = result(
            "f.rs",
            vec![sym("f.rs", "f", SYMBOL_BASE), cls],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0, 0, 0])]),
            calls,
        );
        idx.ingest(&[r], None).await.unwrap();

        let report = idx.dependencies_report();
        assert_eq!(report.total, 3);
        let internal_names: Vec<&str> = report.internal.iter().map(|d| d.name.as_str()).collect();
        assert!(internal_names.contains(&"orderservice"));
        let external_names: Vec<&str> = report.external.iter().map(|d| d.name.as_str()).collect();
        assert!(external_names.contains(&"fmt"));
        assert!(external_names.contains(&"requests"));
    }

    #[tokio::test]
    async fn semantic_and_hybrid_search() {
        // Embedding là OPT-IN — test này bật tường minh backend "hashing"
        // (dependency-free, không tải model) để semantic/hybrid search chạy.
        crate::embeddings::set_embedding_config(crate::embeddings::EmbeddingConfig::from_raw(
            Some("hashing"),
            None,
            None,
            None,
            None,
        ));
        let mut idx = GraphIndex::in_memory();
        // Tên gần giống nhau → hashing embeddings sinh vector tương tự →
        // KNN cosine tìm được dù query sai chính tả.
        let syms = vec![
            sym("auth.rs", "authenticate_user", SYMBOL_BASE),
            sym("auth.rs", "authorize_request", SYMBOL_BASE + 1),
            sym("db.rs", "query_database", SYMBOL_BASE + 2),
            sym("db.rs", "parse_config", SYMBOL_BASE + 3),
        ];
        let r = result("f.rs", syms, HashMap::new(), vec![]);
        idx.ingest(&[r], None).await.unwrap();

        // Semantic: query lệch chính tả "authenticte" vẫn phải rank
        // authenticate_user lên đầu (KNN cosine trên embedding).
        let sem = idx
            .search_symbol_paged_resumable(
                "authenticte",
                None,
                SymbolMatch::Semantic,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap()
            .page;
        assert!(!sem.is_empty(), "semantic search must return candidates");
        assert_eq!(sem[0].name, "authenticate_user");

        // Hybrid: "auth" (lexical) + vector → vẫn phải có authenticate_user.
        let hyb = idx
            .search_symbol_paged_resumable(
                "auth",
                None,
                SymbolMatch::Hybrid,
                Pagination {
                    limit: 10,
                    offset: 0,
                },
                None,
                None,
            )
            .await
            .unwrap()
            .page;
        assert!(!hyb.is_empty());
        assert!(hyb.iter().any(|s| s.name == "authenticate_user"));
    }

    #[tokio::test]
    async fn field_declared_type_extracts_receiver_not_method() {
        // field_declared_type("legacyAdapter.doWork") phải trả "legacyadapter"
        // không phải "doWork".
        let mut idx = GraphIndex::in_memory();
        // Tạo class LegacyAdapter (id=100) với field legacyAdapter (id=101, type_name="LegacyAdapter")
        // và method doWork (id=102).
        // Tạo class Service (id=103) với method doWork (id=104).
        let legacy_adapter = Symbol {
            id: 100,
            name: "LegacyAdapter".to_string(),
            kind: SymbolKind::Class,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "LegacyAdapter.java".into(),
            line: 3,
            end_line: 8,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "java".into(),
            impl_trait: None,
        };
        let legacy_field = Symbol {
            id: 101,
            name: "legacyAdapter".to_string(),
            kind: SymbolKind::Field,
            scope: ScopeLevel::ObjectField,
            scope_id: 100,
            type_ref: 0,
            type_name: Some("LegacyAdapter".to_string()),
            file: "Router.java".into(),
            line: 5,
            end_line: 5,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "java".into(),
            impl_trait: None,
        };
        let legacy_do_work = Symbol {
            id: 102,
            name: "doWork".to_string(),
            kind: SymbolKind::Method,
            scope: ScopeLevel::ObjectField,
            scope_id: 100,
            type_ref: 0,
            type_name: None,
            file: "LegacyAdapter.java".into(),
            line: 5,
            end_line: 7,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "java".into(),
            impl_trait: None,
        };
        let service = Symbol {
            id: 103,
            name: "Service".to_string(),
            kind: SymbolKind::Class,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "Service.java".into(),
            line: 3,
            end_line: 8,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "java".into(),
            impl_trait: None,
        };
        let service_do_work = Symbol {
            id: 104,
            name: "doWork".to_string(),
            kind: SymbolKind::Method,
            scope: ScopeLevel::ObjectField,
            scope_id: 103,
            type_ref: 0,
            type_name: None,
            file: "Service.java".into(),
            line: 5,
            end_line: 7,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "java".into(),
            impl_trait: None,
        };
        // Tạo router method với scope_id = 200 (không phải class scope)
        // Để field_declared_type tìm trong scope_id=200 sẽ fail → return None.
        // Thay vào đó tạo router method với scope_id=100 để field legacyAdapter match.
        let router_method = Symbol {
            id: 105,
            name: "route".to_string(),
            kind: SymbolKind::Method,
            scope: ScopeLevel::Local,
            scope_id: 100, // scope_id = LegacyAdapter class id để field legacyAdapter match
            type_ref: 0,
            type_name: None,
            file: "Router.java".into(),
            line: 7,
            end_line: 9,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "java".into(),
            impl_trait: None,
        };
        idx.ingest(
            &[ParseResult {
                path: "dummy.java".into(),
                language: "java".into(),
                bytes: 0,
                lines: 0,
                symbols: vec![
                    legacy_adapter,
                    legacy_field,
                    legacy_do_work,
                    service,
                    service_do_work,
                    router_method,
                ],
                chains: HashMap::new(),
                calls: vec![],
                fn_ptr_assigns: Vec::new(),
                file_deps: None,
            }],
            None,
        )
        .await
        .unwrap();

        // "legacyAdapter.doWork" → field_name = "legacyAdapter" → type_name = "LegacyAdapter" → "legacyadapter"
        assert_eq!(
            idx.field_declared_type(105, "legacyAdapter.doWork"),
            Some("legacyadapter".to_string())
        );
        // "doWork" (không có '.') → None (không phải dạng var.method)
        assert_eq!(idx.field_declared_type(105, "doWork"), None);
        // "svc.doWork" với field svc không tồn tại trong scope → None
        assert_eq!(idx.field_declared_type(105, "svc.doWork"), None);
    }
}
