use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand};
use codegraph_extract::{ExtractStats, Orchestrator};
use codegraph_graph::GraphIndex;
use codegraph_mcp::McpServer;
use std::sync::Arc;

mod watcher;

pub(crate) const CODEGRAPH_DIR: &str = ".codegraph";
const DB_FILE: &str = "db.sqlite";

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
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
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
        #[arg(long, default_value_t = true, help = "Show live progress bar during indexing")]
        progress: bool,
    },
    /// Remove the .codegraph/ directory.
    Uninit,
    /// Full re-index.
    Index {
        #[arg(long, default_value_t = true, help = "Show live progress bar during indexing")]
        progress: bool,
    },
    /// Show index health.
    Status,
    /// Search symbols (substring, case-insensitive).
    Query {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// List indexed files under a path prefix.
    Files {
        /// Path prefix filter (indexed file paths starting with this value).
        #[arg(value_name = "PATH")]
        prefix: Option<String>,
    },
    /// Build markdown context for a symbol.
    Context {
        target: String,
        #[arg(long, default_value_t = 1)]
        depth: u32,
        #[arg(long)]
        source: bool,
    },
    /// Run as MCP server over stdio.
    Serve {
        #[arg(long)]
        mcp: bool,
    },
    /// Configure agents (alias for the agent setup step in `init`).
    Install,
    /// Launch local web UI to explore the knowledge graph.
    #[cfg(feature = "visualize")]
    Visualize {
        #[arg(long, default_value_t = 7421)]
        port: u16,
        #[arg(long)]
        open: bool,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long, default_value_t = 2)]
        depth: u32,
        #[arg(long)]
        no_browser: bool,
    },
}

fn main() -> Result<()> {
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
            cmd_default(&root)?;
            return Ok(());
        }
    };
    match cmd {
        Cmd::Init { no_index, progress } => cmd_init(&root, !no_index, progress),
        Cmd::Uninit => cmd_uninit(&root),
        Cmd::Index { progress } => cmd_index(&root, progress),
        Cmd::Status => cmd_status(&root),
        Cmd::Query { query, limit } => cmd_query(&root, &query, limit),
        Cmd::Files { prefix } => cmd_files(&root, prefix.as_deref()),
        Cmd::Context {
            target,
            depth,
            source,
        } => cmd_context(&root, &target, depth, source),
        Cmd::Serve { mcp } => cmd_serve(&root, mcp),
        Cmd::Install => cmd_agents(&root),
        #[cfg(feature = "visualize")]
        Cmd::Visualize {
            port,
            open,
            target,
            prefix,
            depth,
            no_browser,
        } => cmd_visualize(&root, port, open, target, prefix, depth, no_browser),
    }
}

fn cmd_default(root: &Utf8Path) -> Result<()> {
    if !db_path(root).exists() {
        use console::style;
        eprintln!();
        eprintln!(
            "  {} {}",
            style("CodeGraph").bold().cyan(),
            style(format!("v{}", env!("CARGO_PKG_VERSION"))).dim()
        );
        eprintln!("  ━");
        eprintln!(
            "  ⚠️  {}",
            style("Workspace not initialized").bold().yellow()
        );
        eprintln!("     No active database found in this directory.");
        eprintln!();
        eprintln!(
            "     {} {}",
            style("Root:").dim(),
            style(root.as_str()).italic()
        );
        eprintln!(
            "     👉 Run {} to set up CodeGraph!",
            style("codegraph init").bold().green()
        );
        eprintln!();
        std::process::exit(1);
    }

    use console::style;
    let db_str = db_path(root).as_str().to_string();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let s = rt.block_on(async {
        let idx = GraphIndex::open(&db_str).await?;
        Ok::<_, anyhow::Error>(idx.stats())
    })?;
    eprintln!();
    eprintln!(
        "  {} {}",
        style("CodeGraph").bold().cyan(),
        style(format!("v{}", env!("CARGO_PKG_VERSION"))).dim()
    );
    eprintln!("  ━");
    eprintln!(
        "  ✨  {}",
        style("Workspace Active & Indexed").bold().green()
    );
    eprintln!();
    eprintln!("     📊  {}", style("Database Statistics:").bold());
    eprintln!("         • {} indexed files", style(s.files).cyan());
    eprintln!("         • {} symbols", style(s.symbols).cyan());
    eprintln!("         • {} chains", style(s.chains).cyan());
    eprintln!("         • {} edges", style(s.edges).cyan());
    eprintln!();
    eprintln!("     🚀  {}", style("Quick Commands:").bold());
    eprintln!(
        "         • {}           Check status and statistics",
        style("codegraph status").green()
    );
    eprintln!(
        "         • {}    Search for symbols in the codebase",
        style("codegraph query <text>").green()
    );
    eprintln!(
        "         • {}            Configure/install AI agent integrations",
        style("codegraph install").green()
    );
    #[cfg(feature = "visualize")]
    eprintln!(
        "         • {}   Explore the graph in your browser",
        style("codegraph visualize").green()
    );
    eprintln!();
    Ok(())
}

fn db_path(root: &Utf8Path) -> Utf8PathBuf {
    root.join(CODEGRAPH_DIR).join(DB_FILE)
}

fn ensure_initialized(root: &Utf8Path) -> Result<()> {
    if !db_path(root).exists() {
        use console::style;
        eprintln!();
        eprintln!(
            "  {} {}",
            style("CodeGraph").bold().cyan(),
            style(format!("v{}", env!("CARGO_PKG_VERSION"))).dim()
        );
        eprintln!("  ━");
        eprintln!(
            "  ⚠️  {}",
            style("Workspace not initialized").bold().yellow()
        );
        eprintln!("     No active database found in this directory.");
        eprintln!();
        eprintln!(
            "     {} {}",
            style("Root:").dim(),
            style(root.as_str()).italic()
        );
        eprintln!(
            "     👉 Run {} to set up CodeGraph!",
            style("codegraph init").bold().green()
        );
        eprintln!();
        std::process::exit(1);
    }
    Ok(())
}

/// Full re-index: mở sqlite → `Orchestrator::index_all` (ingest = full re-index).
fn block_on_index(root: &Utf8Path, db_path: &Utf8Path, progress: bool) -> Result<ExtractStats> {
    let root = root.to_path_buf();
    let db_str = db_path.as_str().to_string();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut idx = GraphIndex::open(&db_str).await?;
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
        Ok::<_, anyhow::Error>(
            Orchestrator::with_registry()
                .index_all(&root, &mut idx, progress_bar)
                .await?,
        )
    })
}

fn cmd_init(root: &Utf8Path, do_index: bool, show_progress: bool) -> Result<()> {
    let dir = root.join(CODEGRAPH_DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(".gitignore"), "*\n")?;
    std::fs::write(dir.join("version"), env!("CARGO_PKG_VERSION"))?;
    let config_path = dir.join("config.toml");
    if !config_path.exists() {
        std::fs::write(&config_path, codegraph_extract::DEFAULT_CONFIG_TOML)?;
    }
    eprintln!("initialized {}", dir);

    if do_index {
        let stats = block_on_index(root, &db_path(root), show_progress)?;
        eprintln!(
            "indexed {} files, {} symbols, {} chains, {} edges",
            stats.files, stats.symbols, stats.chains, stats.calls
        );
    }

    eprintln!();
    cmd_agents(root)
}

fn cmd_agents(root: &Utf8Path) -> Result<()> {
    use codegraph_installer::{project_registry, DetectStatus, InstallOpts, InstallReport};
    use console::style;
    use dialoguer::{theme::ColorfulTheme, MultiSelect};

    let bin = std::env::current_exe()?;
    let bin = Utf8PathBuf::from_path_buf(bin)
        .map_err(|p| anyhow!("non-UTF8 bin path: {}", p.display()))?;
    let opts = InstallOpts {
        project_root: Some(root.to_path_buf()),
        global: false,
        binary_path: bin,
        home_dir: None,
    };

    let all_targets = project_registry();
    let statuses: Vec<DetectStatus> = all_targets.iter().map(|t| t.detect(&opts)).collect();

    let found_indices: Vec<usize> = statuses
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, DetectStatus::Found))
        .map(|(i, _)| i)
        .collect();

    let already_indices: Vec<usize> = statuses
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, DetectStatus::AlreadyConfigured))
        .map(|(i, _)| i)
        .collect();

    let not_found_indices: Vec<usize> = statuses
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, DetectStatus::NotFound))
        .map(|(i, _)| i)
        .collect();

    if !already_indices.is_empty() {
        eprintln!("{}", style("Already configured:").blue());
        for i in &already_indices {
            eprintln!("  {}", style(all_targets[*i].label()).blue());
        }
        eprintln!();
    }

    if !not_found_indices.is_empty() {
        eprintln!("{}", style("Not detected:").dim());
        for i in &not_found_indices {
            eprintln!("  {}", style(all_targets[*i].label()).dim());
        }
        eprintln!();
    }

    if found_indices.is_empty() {
        return Ok(());
    }

    let labels: Vec<String> = found_indices
        .iter()
        .map(|&i| all_targets[i].label().to_string())
        .collect();

    let chosen = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select agents to configure (space = toggle, enter = confirm)")
        .items(&labels)
        .defaults(&vec![false; found_indices.len()])
        .interact()?;

    if chosen.is_empty() {
        return Ok(());
    }

    eprintln!();
    for pos in chosen {
        let target = &all_targets[found_indices[pos]];
        let report = target.install(&opts)?;
        match report {
            InstallReport::Installed(p) | InstallReport::Updated(p) => {
                for f in &p {
                    eprintln!("[{}] wrote {}", target.id(), f);
                }
            }
            InstallReport::Unchanged => eprintln!("[{}] unchanged", target.id()),
            InstallReport::Skipped(r) => eprintln!("[{}] skipped: {}", target.id(), r),
        }
    }
    Ok(())
}

fn cmd_uninit(root: &Utf8Path) -> Result<()> {
    let dir = root.join(CODEGRAPH_DIR);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
        eprintln!("removed {}", dir);
    }
    Ok(())
}

fn cmd_index(root: &Utf8Path, progress: bool) -> Result<()> {
    ensure_initialized(root)?;
    let stats = block_on_index(root, &db_path(root), progress)?;
    eprintln!(
        "indexed {} files, {} symbols, {} chains, {} calls (skipped {})",
        stats.files, stats.symbols, stats.chains, stats.calls, stats.skipped
    );
    Ok(())
}

fn cmd_status(root: &Utf8Path) -> Result<()> {
    ensure_initialized(root)?;
    let db_str = db_path(root).as_str().to_string();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let s = rt.block_on(async {
        let idx = GraphIndex::open(&db_str).await?;
        Ok::<_, anyhow::Error>(idx.stats())
    })?;
    println!("files:   {}", s.files);
    println!("symbols: {}", s.symbols);
    println!("chains:  {}", s.chains);
    println!("edges:   {}", s.edges);
    Ok(())
}

fn cmd_query(root: &Utf8Path, q: &str, limit: u32) -> Result<()> {
    ensure_initialized(root)?;
    let db_str = db_path(root).as_str().to_string();
    let q = q.to_string();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let hits = rt.block_on(async {
        let idx = GraphIndex::open(&db_str).await?;
        Ok::<_, anyhow::Error>(idx.search_symbol(&q, None, limit as usize).await?)
    })?;
    for h in hits {
        println!(
            "[{}] {}  {}  {}:{}",
            h.id,
            h.kind.as_str(),
            h.name,
            h.file,
            h.line
        );
    }
    Ok(())
}

fn cmd_files(root: &Utf8Path, prefix: Option<&str>) -> Result<()> {
    use std::io::Write;

    ensure_initialized(root)?;
    let db_str = db_path(root).as_str().to_string();
    let prefix = prefix.unwrap_or("").to_string();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let files = rt.block_on(async {
        let idx = GraphIndex::open(&db_str).await?;
        let all = idx.files();
        Ok::<_, anyhow::Error>(if prefix.is_empty() {
            all
        } else {
            all.into_iter().filter(|f| f.path.starts_with(&prefix)).collect()
        })
    })?;
    let mut out = std::io::stdout().lock();
    for f in files {
        if writeln!(out, "{}  ({})", f.path, f.language).is_err() {
            break;
        }
    }
    Ok(())
}

fn cmd_context(root: &Utf8Path, target: &str, depth: u32, include_source: bool) -> Result<()> {
    ensure_initialized(root)?;
    let db_path = db_path(root);
    let req = codegraph_context::ContextRequest {
        query: target.into(),
        depth,
        include_source,
        limit: 5,
        format: codegraph_context::Format::Markdown,
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let output = rt.block_on(async {
        let sgi = Arc::new(
            codegraph_graph::SharedGraphIndex::open(Some(db_path.into_std_path_buf())).await?,
        );
        codegraph_context::build(&sgi, &req).await
    })?;
    print!("{}", output);
    Ok(())
}

fn cmd_serve(root: &Utf8Path, mcp: bool) -> Result<()> {
    if !mcp {
        return Err(anyhow!("only --mcp transport supported"));
    }
    ensure_initialized(root).context("init the index before serving")?;
    let db_path = db_path(root);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        watcher::spawn(root.to_path_buf(), db_path.clone());
        let mcp_server = McpServer::new(Some(db_path.into_std_path_buf())).await?;
        mcp_server.run_stdio().await
    })?;
    Ok(())
}

#[cfg(feature = "visualize")]
fn cmd_visualize(
    root: &Utf8Path,
    port: u16,
    open: bool,
    target: Option<String>,
    prefix: Option<String>,
    depth: u32,
    no_browser: bool,
) -> Result<()> {
    use codegraph_viz::{BootConfig, VizConfig};

    ensure_initialized(root).context("init the index before visualize")?;
    let db_path = db_path(root);
    let config = VizConfig {
        port,
        open_browser: open && !no_browser,
        boot: BootConfig {
            target,
            prefix,
            depth,
        },
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(codegraph_viz::run(db_path.into_std_path_buf(), config))?;
    Ok(())
}
