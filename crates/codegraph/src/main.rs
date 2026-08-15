use anyhow::{anyhow, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{ArgAction, Parser, Subcommand};
use codegraph_extract::{ExtractStats, Orchestrator};
use codegraph_graph::GraphIndex;
use codegraph_mcp::CodegraphServer;

mod watcher;

/// CLI tối giản: chỉ còn lifecycle (`init`/`deinit`) + MCP server (`serve --mcp`).
/// Mọi query/interact đi qua MCP tools (`codegraph_search`, `codegraph_context`,
/// `codegraph_status`, …) — CLI không lặp lại các lệnh đọc index nữa.
#[derive(Parser, Debug)]
#[command(
    name = "codegraph",
    version,
    about = "Local-first code intelligence",
    disable_version_flag = true
)]
struct Cli {
    /// Workspace root (default: current dir).
    #[arg(long, global = true)]
    path: Option<Utf8PathBuf>,

    /// Print version.
    #[arg(short = 'v', long = "version", action = ArgAction::Version)]
    version: Option<bool>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Initialize .codegraph/ in the current directory and index immediately.
    /// Pass --no-index to skip indexing.
    Init {
        #[arg(long, default_value_t = false, help = "Disable indexing")]
        no_index: bool,
        #[arg(
            long,
            default_value_t = true,
            help = "Show live progress bar during indexing"
        )]
        progress: bool,
    },
    /// Remove the .codegraph/ directory.
    Deinit,
    /// Run as MCP server (stdio qua `--mcp`, hoặc Streamable HTTP qua `--http`).
    Serve {
        #[arg(long)]
        mcp: bool,
        /// Serve qua Streamable HTTP (POST/GET/DELETE + SSE) thay vì stdio —
        /// mount ở cả `/` và `/mcp`. Default bind 0.0.0.0:8123 (docker-friendly).
        #[arg(long)]
        http: bool,
        /// Địa chỉ bind cho `--http` (`HOST:PORT`).
        #[arg(long, default_value = "0.0.0.0:8123")]
        addr: std::net::SocketAddr,
        /// `Host` header được chấp nhận bởi `--http` (lặp được) — thêm IP hoặc
        /// hostname LAN để mở ngoài loopback (rmcp chặn host lạ chống DNS rebinding).
        #[arg(long = "allow-host")]
        allow_host: Vec<String>,
        /// Bỏ kiểm tra `Host` header cho `--http` (trusted LAN / docker) — chấp
        /// nhận mọi host. Không khuyến khích cho deployment công khai.
        #[arg(long = "allow-any-host")]
        allow_any_host: bool,
        /// Output format cho mọi response (Binance-style minimal):
        /// minimize (mặc định) = symbol thành mảng vị trí cố định; medium = giữ
        /// key, lược field có value mặc định. Ghi đè được theo session
        /// (codegraph_init {"format": ...}) và từng call (arg "format").
        #[arg(long, value_enum, default_value_t = OutputFormat::Minimize)]
        format: OutputFormat,
        /// Bật endpoint observability: `/health`, `/metrics`, `/metrics/prometheus`.
        #[arg(long = "enable-observability", default_value_t = true)]
        enable_observability: bool,
        /// API key cho HTTP MCP server (lặp được). Nếu set, yêu cầu header
        /// `Authorization: Bearer <key>` cho route MCP (`/` và `/mcp`).
        /// Health/metrics endpoints KHÔNG yêu cầu auth.
        #[arg(long = "api-key")]
        api_key: Vec<String>,
    },
}

/// Giá trị `--format` của CLI — map sang `codegraph_mcp::OutputStyle`.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum OutputFormat {
    #[default]
    Minimize,
    Medium,
}

impl OutputFormat {
    fn style(self) -> codegraph_mcp::OutputStyle {
        match self {
            Self::Minimize => codegraph_mcp::OutputStyle::Minimize,
            Self::Medium => codegraph_mcp::OutputStyle::Medium,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("codegraph=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let root = match &cli.path {
        Some(p) => p.clone(),
        None => Utf8PathBuf::from_path_buf(std::env::current_dir()?)
            .map_err(|p| anyhow!("non-UTF8 cwd: {}", p.display()))?,
    };

    let cmd = match cli.cmd {
        Some(c) => c,
        None => {
            cmd_default(&root).await?;
            return Ok(());
        }
    };
    match cmd {
        Cmd::Init { no_index, progress } => cmd_init(&root, !no_index, progress).await,
        Cmd::Deinit => cmd_deinit(&root),
        Cmd::Serve {
            mcp,
            http,
            addr,
            allow_host,
            allow_any_host,
            format,
            enable_observability,
            api_key,
        } => {
            cmd_serve(
                &root,
                mcp,
                http,
                addr,
                allow_host,
                allow_any_host,
                format.style(),
                enable_observability,
                api_key,
            )
            .await
        }
    }
}

/// Workspace đã init chưa — dấu hiệu là thư mục `.codegraph/` tồn tại (do
/// `codegraph init` tạo). Backend-agnostic: không phụ thuộc db file tồn tại
/// (lmdb dùng thư mục, redis không có file địa phương).
fn is_initialized(root: &Utf8Path) -> bool {
    codegraph_extract::project_dir(root).exists()
}

/// Không có subcommand → in help. Banner console cũ bị bỏ: giao diện chính giờ
/// là MCP (agent dùng `codegraph_init`/`codegraph_status` qua tools).
async fn cmd_default(_root: &Utf8Path) -> Result<()> {
    use clap::CommandFactory;
    Cli::command().print_help()?;
    println!();
    Ok(())
}

/// DSN (kèm scheme) của backend storage trong config — `None` = in-memory.
fn storage_dsn(root: &Utf8Path) -> Option<String> {
    codegraph_extract::ExtractConfig::load(root).storage_dsn(root)
}

/// Mở index theo backend đã config (DSN scheme → `GraphIndex::open`).
async fn open_index(root: &Utf8Path) -> Result<GraphIndex> {
    // `.codegraph/` đã được init (có config) — lúc này storage dsn đã biết.
    match storage_dsn(root) {
        Some(dsn) => Ok(GraphIndex::open(&dsn).await?),
        None => Ok(GraphIndex::in_memory()),
    }
}

/// `codegraph init`: tạo `.codegraph/` + config, index ngay nếu `do_index`
/// (progress bar khi `show_progress`). không gọi installer nữa.
async fn cmd_init(root: &Utf8Path, do_index: bool, show_progress: bool) -> Result<()> {
    let dir = codegraph_extract::init_project(root)?;
    eprintln!("initialized {}", dir);

    if do_index {
        let stats = index_all(root, show_progress).await?;
        eprintln!(
            "indexed {} files, {} symbols, {} chains, {} calls (skipped {})",
            stats.files, stats.symbols, stats.chains, stats.calls, stats.skipped
        );
    }
    Ok(())
}

/// Full re-index: mở index theo backend config → `Orchestrator::index_all`
/// (ingest = full re-index).
async fn index_all(root: &Utf8Path, progress: bool) -> Result<ExtractStats> {
    let mut idx = open_index(root).await?;
    // Create progress bar if requested.
    let progress_bar = if progress {
        let bar = indicatif::ProgressBar::new(0);
        bar.set_style(
            indicatif::ProgressStyle::default_bar()
                .template("[{elapsed_precise}] [{wide_bar}] {pos}/{len} ({percent}%)")
                .expect("valid progress bar template")
                .progress_chars("#>-"),
        );
        Some(std::sync::Arc::new(bar))
    } else {
        None
    };
    Orchestrator::with_registry()
        .index_all(root, &mut idx, progress_bar)
        .await
        .map_err(Into::into)
}

/// `codegraph deinit`: xóa `.codegraph/` (đảo của `init`).
fn cmd_deinit(root: &Utf8Path) -> Result<()> {
    let dir = codegraph_extract::project_dir(root);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
        eprintln!("removed {}", dir);
    }
    Ok(())
}

/// `codegraph serve --mcp`: chạy MCP server trên stdio.
/// `codegraph serve --http`: chạy MCP server trên Streamable HTTP.
async fn cmd_serve(
    root: &Utf8Path,
    mcp: bool,
    http: bool,
    addr: std::net::SocketAddr,
    allow_host: Vec<String>,
    allow_any_host: bool,
    format: codegraph_mcp::OutputStyle,
    enable_observability: bool,
    api_key: Vec<String>,
) -> Result<()> {
    if http {
        // Mỗi session HTTP (mcp-session-id) được rmcp cấp một CodegraphServer
        // riêng → session bắt đầu TRỐNG; agent bind root bằng codegraph_init
        // trong phiên của mình. `--path` lúc khởi động chỉ gắn watcher (như
        // stdio), không pre-seed root cho mọi phiên HTTP.
        let mut allowed_hosts = vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ];
        if allow_any_host {
            allowed_hosts.clear(); // rỗng = rmcp chấp nhận mọi Host header
        } else {
            allowed_hosts.extend(allow_host);
        }
        let use_root = root.as_str() != "/";
        if use_root && is_initialized(root) {
            watcher::spawn(root.to_path_buf(), storage_dsn(root));
        }
        return codegraph_mcp::serve_http(format, addr, allowed_hosts, enable_observability, api_key).await;
    }
    if !mcp {
        return Err(anyhow!(
            "only --mcp (stdio) or --http (Streamable HTTP) supported"
        ));
    }

    // MCP is session-driven: the agent binds a workspace at runtime via
    // `codegraph_init {"path": ...}`. The startup `--path` (default: cwd) is
    // only a PRE-SEED so the file watcher attaches to a real project. MCP hosts
    // like Claude Desktop launch servers with cwd=/ and no `--path` — the root
    // resolving to `/` is NOT an error anymore: we just start with an EMPTY
    // session and let the agent bind the project path through the tool.
    let use_root = root.as_str() != "/";
    let initialized = use_root && is_initialized(root);
    let dsn = if initialized { storage_dsn(root) } else { None };
    if initialized {
        watcher::spawn(root.to_path_buf(), dsn.clone());
    }
    let server = if use_root {
        CodegraphServer::with_root_and_format(root.to_path_buf(), format).await?
    } else {
        CodegraphServer::new_with_format(format)
    };
    codegraph_mcp::serve_stdio(server).await
}
