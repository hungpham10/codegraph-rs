//! Session — quản lý vòng đời index của MCP server.
//!
//! Server start lên rồi quản lý **theo session**. Với MCP transport stdio
//! (1 tiến trình = 1 kết nối) chỉ có đúng **1 session slot** cho mỗi process,
//! và đường dẫn workspace do AGENT chọn ngay trong phiên làm việc:
//! - `codegraph_init { "path": ... }` → bind session vào workspace root đó
//!   (tạo `.codegraph/` + config, index tùy chọn) → session `Ready`;
//! - `codegraph_deinit {}` → nhả session (`root = None`), `.codegraph/` và
//!   index để nguyên trên đĩa; mọi tool khác bị **refuse** cho tới khi
//!   `codegraph_init` bind lại;
//! - `codegraph_index {}` → full re-index của session hiện tại.
//!
//! `--path` lúc khởi động là **pre-seed** (`with_root`): tương đương đã bind
//! sẵn root đó mà không cần tool call — giữ cho CLI/watcher flow cũ không vỡ.
//! Với luồng HTTP (tương lai) session không đi theo process — mỗi kết nối mang
//! `mcp-session-id` riêng và session store quản lý nhiều session song song.

use anyhow::{anyhow, Result};
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_extract::{init_project, project_dir, ExtractConfig, ExtractStats, Orchestrator};
use codegraph_graph::{GraphIndex, SharedGraphIndex};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Trạng thái session.
enum SessionState {
    /// Chưa có root nào được bind (hoặc đã `codegraph_deinit`).
    Empty,
    /// Đã bind vào một workspace root, storage + index dùng chung sẵn sàng.
    Ready {
        dsn: Option<String>,
        shared_index: Arc<SharedGraphIndex>,
    },
}

/// Kết quả `codegraph_init` — root vừa bind + dir `.codegraph/` + stats nếu index.
pub struct InitOutcome {
    pub root: Utf8PathBuf,
    pub dir: Utf8PathBuf,
    pub indexed: Option<ExtractStats>,
}

/// Session của MCP server (stdio = 1 process = 1 session slot).
pub struct Session {
    root: RwLock<Option<Utf8PathBuf>>,
    state: RwLock<SessionState>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// Session trống — chưa có root nào; `codegraph_init` sẽ bind.
    pub fn new() -> Self {
        Self {
            root: RwLock::new(None),
            state: RwLock::new(SessionState::Empty),
        }
    }

    /// Pre-seed root lúc khởi động (`--path`). Có `.codegraph/` → load storage
    /// ngay (Ready); chưa init → Empty, chờ `codegraph_init` bind lại.
    pub async fn with_root(root: Utf8PathBuf) -> Result<Self> {
        let state = if project_dir(&root).exists() {
            let dsn = ExtractConfig::load(&root).storage_dsn(&root);
            let shared_index = Arc::new(SharedGraphIndex::open(dsn.clone()).await?);
            SessionState::Ready { dsn, shared_index }
        } else {
            SessionState::Empty
        };
        Ok(Self {
            root: RwLock::new(Some(root)),
            state: RwLock::new(state),
        })
    }

    /// Root hiện tại, nếu có (không clone `&Utf8Path` khi root là Option trong
    /// RwLock — clone an toàn cho await qua biên).
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

    /// `codegraph_init { path, index }`: normalize/validate path, bind root,
    /// tạo `.codegraph/` + config, index CHỈ khi `do_index = true` (mặc định
    /// không index — bind nhanh, không block user; agent chủ động gọi
    /// `codegraph_index {}` khi cần data), rồi load storage theo config vừa
    /// tạo → session chuyển sang `Ready`.
    pub async fn init(&self, path: Utf8PathBuf, do_index: bool) -> Result<InitOutcome> {
        let root = normalize_root(path)?;
        let dir = init_project(&root)?;
        let indexed = if do_index {
            Some(run_index(&root).await?)
        } else {
            None
        };

        // Config giờ đã tồn tại → load đúng backend (sqlite/lmdb/redis/...).
        let dsn = ExtractConfig::load(&root).storage_dsn(&root);
        let shared_index = Arc::new(SharedGraphIndex::open(dsn.clone()).await?);

        // Root set trước state — mọi `ensure_ready` đồng thời đọc root mới sẽ
        // tự swap state theo DSN mới (xem `ensure_ready`).
        *self.root.write().await = Some(root.clone());
        let mut st = self.state.write().await;
        *st = SessionState::Ready { dsn, shared_index };
        Ok(InitOutcome { root, dir, indexed })
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
        let dsn = ExtractConfig::load(&root).storage_dsn(&root);
        let mut st = self.state.write().await;

        // Root được init giữa chừng (vd sau khi init() lỗi part-way) → chuyển
        // từ Empty sang Ready bằng cách load storage.
        let was_empty = matches!(&*st, SessionState::Empty);
        if was_empty {
            let shared_index = Arc::new(SharedGraphIndex::open(dsn.clone()).await?);
            *st = SessionState::Ready { dsn, shared_index };
        } else if let SessionState::Ready {
            dsn: cur,
            shared_index,
        } = &mut *st
        {
            // Config đổi backend giữa chừng → load lại storage.
            if *cur != dsn {
                match SharedGraphIndex::open(dsn.clone()).await {
                    Ok(sgi) => {
                        *shared_index = Arc::new(sgi);
                        *cur = dsn;
                    }
                    Err(e) => eprintln!("[codegraph] open index for {dsn:?} failed: {e}"),
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
    let mut idx = match ExtractConfig::load(root).storage_dsn(root) {
        Some(dsn) => GraphIndex::open(&dsn).await?,
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
