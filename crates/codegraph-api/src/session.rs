//! Session — quản lý vòng đời index của một workspace root.
//!
//! Tách từ `codegraph-mcp` lên tầng `codegraph-api` để cả MCP server và
//! GraphQL server (và bất kỳ transport nào) cùng tiêu thụ chung một
//! implementation. Session quản lý **theo workspace**: `init` bind root +
//! tạo `.codegraph/` + index tùy chọn, `ensure_ready` trả `Arc<SharedGraphIndex>`
//! (snapshot mới nhất), `deinit`/`reindex` điều khiển vòng đời.
//!
//! Với MCP transport stdio (1 tiến trình = 1 kết nối) chỉ có đúng **1 session
//! slot** cho mỗi process; với HTTP (GraphQL) mỗi server giữ 1 session slot
//! (UI chủ động `init` root cần thiết).

use anyhow::{anyhow, Result};
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_core::StorageRoute;
use codegraph_extract::{init_project, project_dir, ExtractConfig, ExtractStats, Orchestrator};
use codegraph_graph::{GraphIndex, SharedGraphIndex};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Mức chi tiết mặc định của Symbol trong response các list tool — set tại
/// `codegraph_init {"detail": ...}`, có thể ghi đè từng call bằng arg `detail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailLevel {
    /// `{id, name, kind, file, line}` — tối ưu token cho reasoning.
    Minimal,
    /// Mặc định: thêm `signature` (dòng khai báo đầu tiên).
    #[default]
    Medium,
    /// Full `Symbol` (doc, annotations, scope, type_ref, ...) — như cũ.
    Verbose,
}

impl DetailLevel {
    /// Parse từ tên arg (`minimal`/`medium`/`verbose`) — `None` nếu lạ.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "minimal" => Self::Minimal,
            "medium" => Self::Medium,
            "verbose" => Self::Verbose,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Medium => "medium",
            Self::Verbose => "verbose",
        }
    }
}

/// Định dạng response kiểu Binance-style minimal — set tại
/// `codegraph_init {"format": ...}`, ghi đè từng call bằng arg `format`, và có
/// thể seed từ CLI lúc khởi động (`codegraph serve --mcp --format=...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputStyle {
    /// Mặc định — nhỏ gọn nhất: symbol thành mảng vị trí cố định (chỉ value,
    /// order được document; value thiếu = sentinel null/0/""/[]).
    #[default]
    Minimize,
    /// Giữ key, lược bỏ field có value mặc định (None/0/""/[]/{}).
    Medium,
}

impl OutputStyle {
    /// Parse từ tên arg (`minimize`/`medium`) — `None` nếu lạ.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "minimize" => Self::Minimize,
            "medium" => Self::Medium,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Medium => "medium",
        }
    }
}

/// Trạng thái session.
enum SessionState {
    /// Chưa có root nào được bind (hoặc đã `codegraph_deinit`).
    Empty,
    /// Đã bind vào một workspace root, storage + index dùng chung sẵn sàng.
    Ready {
        route: Option<StorageRoute>,
        shared_index: Arc<SharedGraphIndex>,
    },
}

/// Kết quả `codegraph_init` — root vừa bind + dir `.codegraph/` + stats nếu index.
pub struct InitOutcome {
    pub root: Utf8PathBuf,
    pub dir: Utf8PathBuf,
    pub indexed: Option<ExtractStats>,
}

/// Session quản lý vòng đời index của một workspace root.
pub struct Session {
    root: RwLock<Option<Utf8PathBuf>>,
    state: RwLock<SessionState>,
    detail: RwLock<DetailLevel>,
    format: RwLock<OutputStyle>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// Session trống — chưa có root nào; `codegraph_init` sẽ bind.
    pub fn new() -> Self {
        Self::new_with_format(OutputStyle::default())
    }

    /// `new()` nhưng seed sẵn output format từ CLI lúc khởi động.
    pub fn new_with_format(format: OutputStyle) -> Self {
        Self {
            root: RwLock::new(None),
            state: RwLock::new(SessionState::Empty),
            detail: RwLock::new(DetailLevel::default()),
            format: RwLock::new(format),
        }
    }

    /// Pre-seed root lúc khởi động (`--path`). Có `.codegraph/` → load storage
    /// ngay (Ready); chưa init → Empty, chờ `codegraph_init` bind lại.
    pub async fn with_root(root: Utf8PathBuf) -> Result<Self> {
        Self::with_root_and_format(root, OutputStyle::default()).await
    }

    /// `with_root()` nhưng seed sẵn output format từ CLI lúc khởi động.
    pub async fn with_root_and_format(root: Utf8PathBuf, format: OutputStyle) -> Result<Self> {
        let state = if project_dir(&root).exists() {
            // RDBMS cần repo_id — đảm bảo đã sinh (self-heal) trước khi tính route.
            let _ = ExtractConfig::ensure_repo_id(&root);
            let route = ExtractConfig::load(&root).storage_route(&root);
            let shared_index = Arc::new(SharedGraphIndex::open_route(route.clone()).await?);
            SessionState::Ready {
                route,
                shared_index,
            }
        } else {
            SessionState::Empty
        };
        Ok(Self {
            root: RwLock::new(Some(root)),
            state: RwLock::new(state),
            detail: RwLock::new(DetailLevel::default()),
            format: RwLock::new(format),
        })
    }

    /// Root hiện tại, nếu có (clone an toàn cho await qua biên).
    pub async fn root(&self) -> Option<Utf8PathBuf> {
        self.root.read().await.clone()
    }

    /// Workspace hiện tại đã init chưa (có `.codegraph/` không).
    pub async fn is_initialized(&self) -> bool {
        self.root
            .read()
            .await
            .as_deref()
            .map(|r| project_dir(r).exists())
            .unwrap_or(false)
    }

    /// `codegraph_init { path, index, detail, format }`: normalize/validate path,
    /// bind root, tạo `.codegraph/` + config, index CHỈ khi `do_index = true`
    /// (mặc định không index — bind nhanh, không block user; agent chủ động gọi
    /// `codegraph_index {}` khi cần data), rồi load storage theo config vừa tạo
    /// → session chuyển sang `Ready`.
    pub async fn init(
        &self,
        path: Utf8PathBuf,
        do_index: bool,
        detail: DetailLevel,
        format: Option<OutputStyle>,
    ) -> Result<InitOutcome> {
        let root = normalize_root(path)?;
        let dir = init_project(&root)?;
        // RDBMS backend (postgres/mysql) cần `repo_id` làm partition key —
        // sinh ngẫu nhiên rồi ghi vào config nếu thiếu (self-heal).
        let _ = ExtractConfig::ensure_repo_id(&root);
        let indexed = if do_index {
            Some(run_index(&root).await?)
        } else {
            None
        };

        // Config giờ đã tồn tại → load đúng backend (sqlite/lmdb/redis/rdbms/...).
        let route = ExtractConfig::load(&root).storage_route(&root);
        let shared_index = Arc::new(SharedGraphIndex::open_route(route.clone()).await?);

        // Root set trước state — mọi `ensure_ready` đồng thời đọc root mới sẽ
        // tự swap state theo route mới (xem `ensure_ready`).
        *self.root.write().await = Some(root.clone());
        *self.detail.write().await = detail;
        if let Some(f) = format {
            *self.format.write().await = f;
        }
        let mut st = self.state.write().await;
        *st = SessionState::Ready {
            route,
            shared_index,
        };
        Ok(InitOutcome { root, dir, indexed })
    }

    /// Detail level hiện tại (default mặc định cho symbol trong list tools).
    pub async fn detail(&self) -> DetailLevel {
        *self.detail.read().await
    }

    /// Output format hiện tại (minimize/medium) cho mọi response.
    pub async fn format(&self) -> OutputStyle {
        *self.format.read().await
    }

    /// `codegraph_deinit`: nhả session — trả root cũ (nếu có). `.codegraph/`
    /// và index để nguyên trên đĩa; `codegraph_init` có thể bind lại sau đó.
    pub async fn deinit(&self) -> Result<Option<Utf8PathBuf>> {
        let prev = self.root.write().await.take();
        let mut st = self.state.write().await;
        *st = SessionState::Empty;
        Ok(prev)
    }

    /// Index dùng chung — gọi trước mọi tool đọc. Chưa bind root / chưa init →
    /// **refuse** với hướng dẫn gọi `codegraph_init`. Khi root đã init, đảm bảo
    /// storage được load (swap nếu config đổi backend giữa chừng).
    pub async fn ensure_ready(&self) -> Result<Arc<SharedGraphIndex>> {
        let root = match self.root.read().await.as_ref() {
            Some(r) => r.clone(),
            None => {
                return Err(anyhow!(
                    "no session bound — call codegraph_init {{\"path\": \"/abs/path/to/project\"}} first"
                ));
            }
        };
        if !project_dir(&root).exists() {
            let mut st = self.state.write().await;
            *st = SessionState::Empty;
            return Err(anyhow!(
                "workspace not initialized at {root} — no CodeGraph index. \
                 Call codegraph_init (bind only, non-blocking) first, then \
                 codegraph_index {{}} to build the index."
            ));
        }
        // RDBMS cần repo_id — đảm bảo đã sinh (self-heal) trước khi tính route.
        let _ = ExtractConfig::ensure_repo_id(&root);
        let route = ExtractConfig::load(&root).storage_route(&root);
        let mut st = self.state.write().await;

        // Root được init giữa chừng (vd sau khi init() lỗi part-way) → chuyển
        // từ Empty sang Ready bằng cách load storage.
        let was_empty = matches!(&*st, SessionState::Empty);
        if was_empty {
            let shared_index = Arc::new(SharedGraphIndex::open_route(route.clone()).await?);
            *st = SessionState::Ready {
                route,
                shared_index,
            };
        } else if let SessionState::Ready {
            route: cur,
            shared_index,
        } = &mut *st
        {
            // Config đổi backend giữa chừng → load lại storage.
            if *cur != route {
                match SharedGraphIndex::open_route(route.clone()).await {
                    Ok(sgi) => {
                        *shared_index = Arc::new(sgi);
                        *cur = route;
                    }
                    Err(e) => eprintln!("[codegraph] open index for {route:?} failed: {e}"),
                }
            }
        }

        match &*st {
            SessionState::Ready { shared_index, .. } => Ok(shared_index.clone()),
            SessionState::Empty => unreachable!("handled above"),
        }
    }

    /// `codegraph_index`: full re-index của session hiện tại — chỉ khi đã init.
    pub async fn reindex(&self) -> Result<ExtractStats> {
        let root = match self.root.read().await.as_ref() {
            Some(r) => r.clone(),
            None => {
                return Err(anyhow!(
                    "no session bound — call codegraph_init {{\"path\": ...}} first"
                ));
            }
        };
        if !project_dir(&root).exists() {
            return Err(anyhow!(
                "workspace not initialized: missing .codegraph/. Run codegraph_init first."
            ));
        }
        run_index(&root).await
    }
}

/// Validate + canonicalize root: phải tồn tại, là directory, không phải `/`
/// (Claude Desktop launch MCP servers từ `/` — từ chối để khỏi index nhầm máy).
fn normalize_root(path: Utf8PathBuf) -> Result<Utf8PathBuf> {
    if !path.is_dir() {
        return Err(anyhow!("path is not a directory: {}", path));
    }
    let canon = std::fs::canonicalize(path.as_std_path())
        .map_err(|e| anyhow!("cannot resolve {}: {e}", path))?;
    let canon =
        Utf8PathBuf::from_path_buf(canon).map_err(|p| anyhow!("path is not valid UTF-8: {p:?}"))?;
    if canon.as_str() == "/" {
        return Err(anyhow!(
            "refusing to use `/` as the workspace root \
             (MCP hosts may launch servers from `/`). Pass an absolute project path."
        ));
    }
    Ok(canon)
}

/// Full re-index: mở index theo backend config → `Orchestrator::index_all`
/// (ingest = full re-index, bump version → snapshot cũ bị `ensure_fresh` thấy
/// stale và rebuild ở lần query kế).
async fn run_index(root: &Utf8Path) -> Result<ExtractStats> {
    // RDBMS cần repo_id (partition key) — sinh nếu thiếu trước khi mở index.
    let _ = ExtractConfig::ensure_repo_id(root);
    let mut idx = match ExtractConfig::load(root).storage_route(root) {
        Some(route) => GraphIndex::open_route(&route).await?,
        None => GraphIndex::in_memory(),
    };
    Orchestrator::with_registry()
        .index_all(root, &mut idx, None)
        .await
        .map_err(Into::into)
}

/// JSON thống kê index (dùng cho codegraph_init/codegraph_index response).
pub fn stats_json(s: &ExtractStats) -> Value {
    json!({
        "files": s.files,
        "symbols": s.symbols,
        "chains": s.chains,
        "calls": s.calls,
        "skipped": s.skipped,
    })
}
