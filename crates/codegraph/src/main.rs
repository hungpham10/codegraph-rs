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
    /// Run as MCP server over stdio.
    Serve {
        #[arg(long)]
        mcp: bool,
    },
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
        Cmd::Serve { mcp } => cmd_serve(&root, mcp).await,
    }
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

/// Workspace đã init chưa — dấu hiệu là thư mục `.codegraph/` tồn tại (do
/// `codegraph init` tạo). Backend-agnostic: không phụ thuộc db file tồn tại
/// (lmdb dùng thư mục, redis không có file địa phương).
fn is_initialized(root: &Utf8Path) -> bool {
    codegraph_extract::project_dir(root).exists()
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
async fn cmd_serve(root: &Utf8Path, mcp: bool) -> Result<()> {
    if !mcp {
        return Err(anyhow!("only --mcp transport supported"));
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
        CodegraphServer::with_root(root.to_path_buf()).await?
    } else {
        CodegraphServer::new()
    };
    codegraph_mcp::serve_stdio(server).await
}