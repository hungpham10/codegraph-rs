use anyhow::{anyhow, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{ArgAction, Parser, Subcommand};
use codegraph_extract::{ExtractStats, Orchestrator};
use codegraph_graph::GraphIndex;
use codegraph_mcp::CodegraphServer;
use std::sync::Arc;

#[cfg(feature = "fastembed")]
use codegraph_graph::embeddings::warm_model_cache;

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
    /// Register codegraph as an MCP server for an AI agent (e.g. Claude Code),
    /// so the agent can launch `codegraph serve --mcp`. Writes the agent's config
    /// (e.g. `~/.claude/settings.json`). After a Homebrew install, this points
    /// the agent at the brew-installed `codegraph`.
    Install {
        /// Target agent: claude (default), cursor, codex, opencode, hermes,
        /// antigravity, or `all`.
        #[arg(long, default_value = "claude")]
        target: String,
        /// Install globally (user home) instead of project-local.
        #[arg(long, default_value_t = false)]
        global: bool,
    },
    /// Remove codegraph's MCP server registration from an AI agent.
    Uninstall {
        /// Target agent (same values as `install`).
        #[arg(long, default_value = "claude")]
        target: String,
        /// Remove the global (user-home) registration instead of project-local.
        #[arg(long, default_value_t = false)]
        global: bool,
    },
    /// Diagnose the environment: OS, codegraph version, whether the workspace is
    /// initialized, index stats, and external tools (git/tar) on PATH.
    Doctor,
    /// Pre-download an embedding model into the global cache (so semantic search
    /// works offline). Model is cached under `[embedding].cache_dir` (default
    /// `~/.cache/codegraph/embeddings`). Requires the `fastembed` feature.
    #[cfg(feature = "fastembed")]
    Embed {
        /// Model name/alias to download, e.g. "bge-small-en-v1.5" (default).
        #[arg(long, default_value = "bge-small-en-v1.5")]
        model: String,
        /// Cache directory (global). Default: ~/.cache/codegraph/embeddings.
        #[arg(long)]
        cache_dir: Option<String>,
    },
    /// Run as MCP server (stdio via `--mcp`, hoặc Streamable HTTP via `--http`).
    Serve {
        #[arg(long)]
        mcp: bool,
        /// Serve GraphQL HTTP API (on-prem Dashboard) tại `--addr` — không qua
        /// MCP. Endpoint `/graphql` (POST) + `/graphiql` (dev explorer). UI chủ
        /// động `init` workspace root; `--path` pre-bind nếu đã có `.codegraph/`.
        #[arg(long)]
        graphql: bool,
        /// Bật Mermaid diagram output (`codegraph_mermaid` ở MCP, `mermaid` ở
        /// GraphQL). Tắt → những entry này trả lỗi rõ ràng. Có nghĩa cho cả
        /// `--graphql` và `--mcp`/`--http`.
        #[arg(long)]
        mermaid: bool,
        /// Serve qua Streamable HTTP (POST/GET/DELETE + SSE) thay vì stdio —
        /// mount ở cả `/` và `/mcp`. Default bind 0.0.0.0:8123 (docker-friendly).
        #[arg(long)]
        http: bool,
        /// Địa chỉ bind cho `--http` (`HOST:PORT`).
        #[arg(long, default_value = "0.0.0.0:8123")]
        addr: std::net::SocketAddr,
        /// `Host` header được chấp nhận bởi `--http` (lặp được). Thêm IP hoặc
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
    /// Document operations: ingest, search, hydrate, and manage structured documents (HCL, YAML, JSON, TOML, XML).
    Doc {
        #[command(subcommand)]
        cmd: DocCmd,
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

/// Document CLI subcommands.
#[derive(Subcommand, Debug)]
enum DocCmd {
    /// Parse and ingest a document file (HCL, YAML, JSON, TOML, XML).
    Ingest {
        /// Path to the document file. Đặt tên `file` — positional `path` đụng
        /// global `--path` (Utf8PathBuf parser) làm clap panic khi parse args.
        #[arg()]
        file: String,
        /// Override auto-detected format (hcl, yaml, json, toml, nginx).
        #[arg(long)]
        format: Option<String>,
    },
    /// Search document nodes by path pattern.
    Search {
        /// Search pattern (substring match on path tokens).
        #[arg()]
        pattern: String,
        /// Search depth (default: 1).
        #[arg(long, default_value_t = 1)]
        depth: usize,
    },
    /// Hydrate a node into a small payload for LLM reasoning.
    Hydrate {
        /// Node id to hydrate.
        #[arg()]
        node_id: u64,
    },
    /// List all ingested documents with stats.
    List,
    /// Show document graph statistics.
    Stats,
    /// Mine structural patterns (kind chains ending at scalar leaves) across
    /// all ingested documents and list them sorted by doc frequency ascending
    /// — rare/characteristic patterns first, background noise last.
    Patterns {
        /// Max patterns to keep.
        #[arg(long, default_value_t = 20)]
        top_k: usize,
        /// Min node occurrences for a pattern to be kept.
        #[arg(long, default_value_t = 3)]
        min_count: usize,
        /// Max kind-chain window length ending at the leaf.
        #[arg(long, default_value_t = 4)]
        max_depth: usize,
    },
    /// Search nodes by structural kind chain, ranked by IDF (rare structures
    /// first), e.g. `codegraph doc struct "MAP, FIELD, NUMBER"`.
    Struct {
        /// Comma-separated kind labels: MAP, ARRAY, FIELD, INDEX, STRING, NUMBER, BOOL, NULL, ROOT.
        #[arg()]
        pattern: String,
        /// Search depth (default: 1).
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Max results (default: 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
        Cmd::Doctor => cmd_doctor(&root).await,
        Cmd::Install { target, global } => cmd_install(&root, &target, global),
        Cmd::Uninstall { target, global } => cmd_uninstall(&root, &target, global),

        #[cfg(feature = "fastembed")]
        Cmd::Embed { model, cache_dir } => cmd_embed(&model, cache_dir.as_deref()).await,
        Cmd::Serve {
            mcp,
            graphql,
            mermaid,
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
                graphql,
                mermaid,
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
        Cmd::Doc { cmd } => cmd_doc(&root, cmd).await,
    }
}

/// Workspace đã init chưa — dấu hiệu là thư mục `.codegraph/` tồn tại (do
/// `codegraph init` tạo). Backend-agnostic: không phụ thuộc db file tồn tại
/// (lmdb dùng thư mục, redis không có file địa phương).
fn is_initialized(root: &Utf8Path) -> bool {
    codegraph_extract::project_dir(root).exists()
}

/// Đường dẫn có phải là gốc filesystem không (`/` trên Unix, `C:\` trên
/// Windows). Dùng để tránh bind workspace nhầm vào gốc ổ đĩa (MCP host thường
/// launch server từ `/`). Hoạt động cross-platform (không so sánh chuỗi `/`).
fn is_fs_root(p: &Utf8Path) -> bool {
    p.parent().is_none()
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
/// Chỉ dùng cho watcher (cần DSN string); RDBMS trả `None` (watcher không spawn).
fn storage_dsn(root: &Utf8Path) -> Option<String> {
    codegraph_extract::ExtractConfig::load(root).storage_dsn(root)
}

/// Mở index theo backend đã config (`StorageRoute` → `GraphIndex::open_route`).
async fn open_index(root: &Utf8Path) -> Result<GraphIndex> {
    // `.codegraph/` đã được init (có config) — lúc này storage route đã biết.
    // RDBMS cần repo_id (partition key) — sinh nếu thiếu (self-heal).
    let _ = codegraph_extract::ExtractConfig::ensure_repo_id(root);
    match codegraph_extract::ExtractConfig::load(root).storage_route(root) {
        Some(route) => Ok(GraphIndex::open_route(&route).await?),
        None => Ok(GraphIndex::in_memory()),
    }
}

/// Mở document graph theo config (`[docgraph]` + `[storage]`): dataset riêng
/// cho docs (mặc định `.codegraph/docs.sqlite` với sqlite), rebuild tries từ
/// storage. Dùng chung helper với MCP server.
async fn open_doc_graph(root: &Utf8Path) -> Result<codegraph_docs::DocumentGraph> {
    codegraph_extract::open_doc_graph(root)
        .await
        .map_err(|e| anyhow!("{e}"))
}

/// `codegraph init`: tạo `.codegraph/` + config, index ngay nếu `do_index`
/// (progress bar khi `show_progress`). không gọi installer nữa.
async fn cmd_init(root: &Utf8Path, do_index: bool, show_progress: bool) -> Result<()> {
    let dir = codegraph_extract::init_project(root)?;
    eprintln!("initialized {}", dir);
    // RDBMS cần repo_id (partition key) — sinh ngẫu nhiên nếu thiếu (self-heal).
    let _ = codegraph_extract::ExtractConfig::ensure_repo_id(root);

    if do_index {
        let stats = index_all(root, show_progress).await?;
        eprintln!(
            "indexed {} files, {} symbols, {} chains, {} calls (skipped {})",
            stats.files, stats.symbols, stats.chains, stats.calls, stats.skipped
        );
    }
    ingest_configured_docs(root).await?;
    Ok(())
}

/// Ingest các document khai báo trong `[docgraph] paths` của config.toml
/// (idempotent — doc trùng path được thay thế tại chỗ).
async fn ingest_configured_docs(root: &Utf8Path) -> Result<()> {
    let Some((_, files)) = codegraph_extract::ExtractConfig::load(root).doc_config(root) else {
        return Ok(());
    };
    if files.is_empty() {
        return Ok(());
    }
    let mut graph = open_doc_graph(root).await?;
    let bar = indicatif::ProgressBar::new(files.len() as u64);
    bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{wide_bar}] {pos}/{len} ({percent}%) {msg}")
            .expect("valid progress bar template")
            .progress_chars("#>-"),
    );
    let mut ingested = 0usize;
    for (path, format) in &files {
        bar.set_message(path.to_string());
        match graph.ingest_file(path.as_str(), format.as_deref()).await {
            Ok(_) => ingested += 1,
            Err(e) => {
                bar.suspend(|| eprintln!("doc ingest failed for {path}: {e}"));
            }
        }
        bar.inc(1);
    }
    bar.finish_and_clear();
    eprintln!("ingested {ingested}/{} documents", files.len());
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

/// `codegraph doctor`: in báo cáo chẩn đoán môi trường để người dùng (và agent)
/// biết trạng thái hiện tại — đặc biệt hữu ích sau khi merge hỗ trợ Windows, vì
/// codegraph giờ chạy cross-platform và có thể register cho nhiều agent (Claude,
/// Cursor, Codex, …) với config path khác nhau trên mỗi OS.
async fn cmd_doctor(root: &Utf8Path) -> Result<()> {
    use std::env::consts::{ARCH, OS};

    let binary = current_exe_path().unwrap_or_else(|_| Utf8PathBuf::from("codegraph"));
    let initialized = is_initialized(root);

    println!("codegraph doctor");
    println!("================");
    println!("Platform      : {OS} / {ARCH}");
    println!("Version       : {}", env!("CARGO_PKG_VERSION"));
    println!("Executable    : {binary}");
    println!(
        "Workspace     : {}",
        if initialized {
            root.as_str().to_string()
        } else {
            "<not initialized>".to_string()
        }
    );

    if initialized {
        match open_index(root).await {
            Ok(idx) => {
                let s = idx.stats();
                println!(
                    "Index stats  : {} files, {} symbols, {} chains, {} edges",
                    s.files, s.symbols, s.chains, s.edges
                );
            }
            Err(e) => println!("Index stats  : <unavailable: {e}>"),
        }
    }

    // External tools codegraph relies on. On Windows, native package managers
    // matter for install paths, so surface them too.
    #[cfg(target_os = "windows")]
    let tools: Vec<&str> = vec!["git", "tar", "r2"];
    #[cfg(not(target_os = "windows"))]
    let tools: Vec<&str> = vec!["git", "tar", "r2"];
    println!("Tools on PATH :");
    for t in tools {
        // radare2 doesn't support `--version` (it parses it as a file to open);
        // fall back to `-v` when `--version` fails.
        let version_flag = if t == "r2" { "-v" } else { "--version" };
        let ok = std::process::Command::new(t)
            .arg(version_flag)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        println!("  - {t:<8} : {}", if ok { "ok" } else { "missing" });
    }

    // MCP agent setup status: which agents are installed and whether they are
    // already wired to discover codegraph's tools. This is the cross-platform
    // "is my tool registered" check.
    println!("MCP agents   :");
    for t in codegraph_installer::registry() {
        for (scope, global) in [("global", true), ("project", false)] {
            let opts = codegraph_installer::InstallOpts {
                project_root: if global {
                    None
                } else {
                    Some(Utf8PathBuf::from(root))
                },
                global,
                binary_path: binary.clone(),
                home_dir: None,
            };
            match t.detect(&opts) {
                codegraph_installer::DetectStatus::NotFound => continue,
                codegraph_installer::DetectStatus::AlreadyConfigured => {
                    println!("  - {} [{}]: configured ✓", t.label(), scope);
                }
                codegraph_installer::DetectStatus::Found => {
                    println!(
                        "  - {} [{}]: agent present, codegraph NOT registered (run: codegraph install --target {} {})",
                        t.label(),
                        scope,
                        t.id(),
                        if global { "--global" } else { "" }
                    );
                }
            }
        }
    }

    Ok(())
}

/// Đường dẫn tuyệt đối tới binary `codegraph` đang chạy — dùng làm `command`
/// trong config MCP của agent (Claude/Cursor/…).
fn current_exe_path() -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(std::env::current_exe()?)
        .map_err(|p| anyhow!("non-UTF8 exe path: {}", p.display()))
}

/// Chọn target agent theo `--target` (`all` = mọi target trong registry tương
/// ứng với scope global/project).
fn select_targets(target: &str, global: bool) -> Vec<Arc<dyn codegraph_installer::AgentTarget>> {
    let all = if global {
        codegraph_installer::registry()
    } else {
        codegraph_installer::project_registry()
    };
    if target.eq_ignore_ascii_case("all") {
        return all;
    }
    all.into_iter().filter(|t| t.id() == target).collect()
}

/// Danh sách id target hợp lệ (dùng trong thông báo lỗi).
fn known_targets(global: bool) -> String {
    let all = if global {
        codegraph_installer::registry()
    } else {
        codegraph_installer::project_registry()
    };
    let mut ids: Vec<&str> = all.iter().map(|t| t.id()).collect();
    ids.push("all");
    ids.join(", ")
}

/// `codegraph install --target <agent> [--global]`: register codegraph làm MCP
/// server cho agent đã chọn, trỏ `command` vào binary hiện tại.
fn cmd_install(root: &Utf8Path, target: &str, global: bool) -> Result<()> {
    let binary_path = current_exe_path()?;
    let opts = codegraph_installer::InstallOpts {
        project_root: if global {
            None
        } else {
            Some(Utf8PathBuf::from(root))
        },
        global,
        binary_path,
        home_dir: None,
    };
    let targets = select_targets(target, global);
    if targets.is_empty() {
        anyhow::bail!(
            "unknown target '{target}' (known: {})",
            known_targets(global)
        );
    }
    for t in targets {
        match t.install(&opts)? {
            codegraph_installer::InstallReport::Installed(paths) => {
                eprintln!(
                    "✓ {}: installed → {}",
                    t.label(),
                    paths
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            codegraph_installer::InstallReport::Updated(paths) => {
                eprintln!(
                    "✓ {}: updated → {}",
                    t.label(),
                    paths
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            codegraph_installer::InstallReport::Unchanged => {
                eprintln!("• {}: already configured", t.label());
            }
            codegraph_installer::InstallReport::Skipped(reason) => {
                eprintln!("• {}: skipped ({reason})", t.label());
            }
        }
    }
    Ok(())
}

/// `codegraph uninstall --target <agent> [--global]`: gỡ registration MCP của
/// codegraph khỏi agent đã chọn.
fn cmd_uninstall(root: &Utf8Path, target: &str, global: bool) -> Result<()> {
    let binary_path = current_exe_path()?;
    let opts = codegraph_installer::InstallOpts {
        project_root: if global {
            None
        } else {
            Some(Utf8PathBuf::from(root))
        },
        global,
        binary_path,
        home_dir: None,
    };
    let targets = select_targets(target, global);
    if targets.is_empty() {
        anyhow::bail!(
            "unknown target '{target}' (known: {})",
            known_targets(global)
        );
    }
    for t in targets {
        match t.uninstall(&opts)? {
            codegraph_installer::InstallReport::Updated(paths) => {
                eprintln!(
                    "✓ {}: removed → {}",
                    t.label(),
                    paths
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            codegraph_installer::InstallReport::Unchanged => {
                eprintln!("• {}: not configured", t.label());
            }
            codegraph_installer::InstallReport::Skipped(reason) => {
                eprintln!("• {}: skipped ({reason})", t.label());
            }
            codegraph_installer::InstallReport::Installed(_) => unreachable!(),
        }
    }
    Ok(())
}

/// `codegraph embed --model <x>`: pre-download model vào global cache để
/// semantic search chạy offline.
#[cfg(feature = "fastembed")]
async fn cmd_embed(model: &str, cache_dir: Option<&str>) -> Result<()> {
    let dir = cache_dir.map(std::path::Path::new);
    warm_model_cache(model, dir).map_err(|e| anyhow!("failed to cache embedding model: {e}"))?;
    Ok(())
}

/// `codegraph serve --mcp`: chạy MCP server trên stdio.
/// `codegraph serve --http`: chạy MCP server trên Streamable HTTP.
#[allow(clippy::too_many_arguments)]
async fn cmd_serve(
    root: &Utf8Path,
    mcp: bool,
    graphql: bool,
    mermaid: bool,
    http: bool,
    addr: std::net::SocketAddr,
    allow_host: Vec<String>,
    allow_any_host: bool,
    format: codegraph_mcp::OutputStyle,
    enable_observability: bool,
    api_key: Vec<String>,
) -> Result<()> {
    if graphql {
        // GraphQL on-prem: Pre-bind `--path` nếu đã có `.codegraph/`, không thì
        // chờ UI `init`. CORS mở cho loopback (+ allow-host), api-key nếu set.
        let mut allowed = vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ];
        if allow_any_host {
            allowed.clear();
        } else {
            allowed.extend(allow_host);
        }
        let api_key = if api_key.is_empty() {
            None
        } else {
            Some(api_key.join(","))
        };
        let use_root = !is_fs_root(root);
        let cfg = codegraph_graphql::ServeConfig {
            addr,
            api_key,
            root: if use_root {
                Some(root.to_path_buf())
            } else {
                None
            },
            format,
            allow_hosts: allowed,
            mermaid,
        };
        return codegraph_graphql::serve(cfg).await;
    }
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
        return codegraph_mcp::serve_http(
            format,
            mermaid,
            addr,
            allowed_hosts,
            enable_observability,
            api_key,
        )
        .await;
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
    let use_root = !is_fs_root(root);
    let initialized = use_root && is_initialized(root);
    let _dsn = if initialized { storage_dsn(root) } else { None };
    let server = if use_root {
        CodegraphServer::with_root_and_format(root.to_path_buf(), format, mermaid).await?
    } else {
        CodegraphServer::new_with_format(format, mermaid)
    };
    if use_root {
        // Build symbol index ngầm sau khi server nhận request — call tool đầu
        // không block cả phút trên repo lớn (initialize không bao giờ chờ).
        server.prewarm_symbol_index();
    }
    codegraph_mcp::serve_stdio(server).await
}

/// `codegraph doc`: manage structured documents (HCL/Terraform, YAML, JSON, TOML).
/// Persist qua dataset docs theo config (`[docgraph]`/`[storage]`) — không còn
/// in-memory per-invocation.
async fn cmd_doc(root: &Utf8Path, cmd: DocCmd) -> Result<()> {
    let mut graph = open_doc_graph(root).await?;

    match cmd {
        DocCmd::Ingest { file, format } => {
            let inserted = graph.ingest_file(&file, format.as_deref()).await?;
            println!("ingested {file} → doc_id={inserted}");
        }
        DocCmd::Search { pattern, depth } => {
            use codegraph_docs::DocToken;
            // Full path search qua trie; không match → fallback quét key.
            let mut tokens = vec![DocToken::root()];
            let mut fuzzy_seg: Option<String> = None;
            for seg in pattern.split('.') {
                if let Some(fz) = seg.strip_prefix('~') {
                    fuzzy_seg = Some(fz.to_string());
                    break;
                }
                match graph.intern_id(seg) {
                    Some(id) => tokens.push(DocToken::field(id)),
                    None => break,
                }
            }
            // Có segment `~` → bỏ qua full-path, đi thẳng fuzzy.
            // depth = số tầng thừa dưới pattern; radix filter theo tổng key len.
            let mut ids = if fuzzy_seg.is_none() {
                let mut ids = graph
                    .search_path(&tokens, Some(tokens.len() - 1 + depth))
                    .await
                    .unwrap_or_default();
                ids.extend(graph.search_path_scan(&tokens, 100));
                ids.sort_unstable();
                ids.dedup();
                ids
            } else {
                Vec::new()
            };
            if ids.is_empty() {
                let last = fuzzy_seg.unwrap_or_else(|| {
                    pattern.rsplit('.').next().unwrap_or(&pattern).to_string()
                });
                let hits = graph.search_key_fuzzy(&last, 50);
                if hits.is_empty() {
                    println!("no nodes matched — key `{last}` not seen in any ingested document");
                    return Ok(());
                }
                for h in hits {
                    println!(
                        "node {} doc={} key={:?} score={:.3} kind={:?} value={:?}",
                        h.node.id, h.node.doc, h.matched_key, h.score, h.node.kind, h.node.value
                    );
                }
                return Ok(());
            }
            for id in ids.iter().take(100) {
                if let Some(payload) = graph.hydrate_depth(*id, Some(1)).await {
                    let json = serde_json::to_string_pretty(&payload)?;
                    println!("{json}");
                }
            }
        }
        DocCmd::Hydrate { node_id } => match graph.hydrate(node_id).await {
            Some(payload) => {
                let json = serde_json::to_string_pretty(&payload)?;
                println!("{json}");
            }
            None => println!("node {node_id} not found"),
        },
        DocCmd::List => {
            let stats = graph.stats().await?;
            println!("documents: {}, nodes: {}", stats.docs, stats.nodes);
        }
        DocCmd::Stats => {
            let stats = graph.stats().await?;
            println!("documents: {}", stats.docs);
            println!("nodes: {}", stats.nodes);
        }
        DocCmd::Patterns {
            top_k,
            min_count,
            max_depth,
        } => {
            let mined = graph.mine_patterns(top_k, min_count, max_depth).await?;
            if mined.is_empty() {
                println!("no patterns matched (min_count={min_count})");
            }
            for p in mined {
                println!(
                    "P#{:<4} docs={:.1}% nodes={} chain={}",
                    p.pattern_id,
                    p.doc_freq * 100.0,
                    p.node_count,
                    p.tokens.join(" → ")
                );
            }
        }
        DocCmd::Struct {
            pattern,
            depth,
            limit,
        } => {
            let tokens: Vec<codegraph_docs::DocToken> = pattern
                .split(',')
                .filter_map(|s| {
                    let s = s.trim();
                    (!s.is_empty()).then(|| codegraph_docs::parse_kind_label(s))
                })
                .collect();
            let ids = graph.search_kind_chain(&tokens, Some(depth));
            if ids.is_empty() {
                println!("no nodes matched this structural pattern");
                return Ok(());
            }
            let total_docs = graph.list_docs().len();
            let mut rows: Vec<(f64, u64)> = ids
                .iter()
                .take(limit * 5)
                .map(|id| (graph.pattern_uniqueness(*id, total_docs), *id))
                .collect();
            rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            for (idf, id) in rows.iter().take(limit) {
                match graph.hydrate_depth(*id, Some(1)).await {
                    Some(p) => println!(
                        "node {} doc={} idf={:.3} path={:?} key={:?} value={:?}",
                        p.id, p.doc, idf, p.path, p.key, p.value
                    ),
                    None => continue,
                }
            }
        }
    }
    Ok(())
}
