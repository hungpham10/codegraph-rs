//! CodeSmell CLI — a team convention linter (like eslint/clippy).

use clap::{Parser, Subcommand, ValueEnum};
use std::io::Read;
use std::path::PathBuf;

use codegraph_graph::diff::parse_unified_diff;
use codesmell::engine::{evaluate, CheckScope};
use codesmell::guide;
use codesmell::index::build_index;
use codesmell::packs;
use codesmell::packs::Registry;
use codesmell::policy;
use codesmell::rhai::RhaiRuleLib;

#[derive(Parser)]
#[command(
    name = "codesmell",
    about = "Team convention linter for maintainable, LLM-friendly code"
)]
struct Cli {
    /// Codesmell pack registry (path or git URL). Overrides CODESMELL_REGISTRY
    /// and ~/.config/codesmell/config.toml. Used by `[[include]]` in policy.toml.
    #[arg(long)]
    registry: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Evaluate the policy over the repository (whole repo, paths, or a diff).
    Check {
        /// File or directory paths to scope the check to (default: whole repo).
        paths: Vec<PathBuf>,
        /// Read a unified diff from a file path or `-` (stdin) and check only changed symbols.
        #[arg(long)]
        diff: Option<PathBuf>,
        /// Output format.
        #[arg(long, value_enum, default_value = "human")]
        format: OutFormat,
        /// Exit non-zero when a violation of at least this severity is found.
        #[arg(long, value_enum, default_value = "required")]
        fail_on: policy::Severity,
    },
    /// Estimate call-graph test coverage of the business logic (see
    /// docs/codesmell-coverage.md). Requires `[testing.coverage]` in policy.
    Coverage {
        /// File or directory paths to scope the measurement to (default: whole repo).
        paths: Vec<PathBuf>,
        /// Read a unified diff from a file path or `-` (stdin) and measure only changed symbols.
        #[arg(long)]
        diff: Option<PathBuf>,
        /// Output format.
        #[arg(long, value_enum, default_value = "human")]
        format: OutFormat,
        /// Exit non-zero when coverage is below the policy threshold.
        #[arg(long, value_enum, default_value = "info")]
        fail_on: policy::Severity,
    },
    /// Print the conventions pack an LLM should read before writing code.
    Guide {
        /// Optional path to show conventions effective for that area.
        path: Option<PathBuf>,
    },
    /// Write a starter `.codesmell/policy.toml` and print an AGENTS.md snippet.
    Init {
        /// Also install a built-in pack (e.g. `security`).
        #[arg(long)]
        pack: Option<String>,
    },
    /// Print the effective resolved policy as TOML.
    Policy,
    /// Manage built-in policy packs and the pack registry.
    Pack {
        #[command(subcommand)]
        command: PackCmd,
    },
}

#[derive(Subcommand)]
enum PackCmd {
    /// List built-in packs and (if a registry is configured) registry packs.
    List,
    /// Copy a built-in pack's scripts + fragment into `.codesmell/` (idempotent).
    Add {
        /// Pack name from `codesmell pack list`.
        name: String,
    },
    /// Pre-fetch a registry pack into the local cache (no files copied to repo).
    Pull {
        /// Pack name from `codesmell pack list`.
        name: String,
    },
    /// Refresh the registry cache (git pull; no-op for local path registries).
    Update,
}

#[derive(Copy, Clone, ValueEnum)]
enum OutFormat {
    Human,
    Json,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let root = cwd.canonicalize().unwrap_or(cwd);

    match cli.command {
        Cmd::Check {
            paths,
            diff,
            format,
            fail_on,
        } => {
            let registry = packs::resolve_registry(cli.registry.as_deref()).ok();
            check(&root, paths, diff, format, fail_on, registry.as_ref()).await
        }
        Cmd::Guide { path } => {
            let registry = packs::resolve_registry(cli.registry.as_deref()).ok();
            guide(&root, path, registry.as_ref())
        }
        Cmd::Coverage {
            paths,
            diff,
            format,
            fail_on,
        } => {
            let registry = packs::resolve_registry(cli.registry.as_deref()).ok();
            coverage(&root, paths, diff, format, fail_on, registry.as_ref()).await
        }
        Cmd::Init { pack } => init(&root, pack.as_deref()),
        Cmd::Policy => {
            let (p, _) = policy::load_policy(&root);
            println!(
                "{}",
                toml::to_string_pretty(&p)
                    .unwrap_or_else(|_| "# (policy could not be serialized)".into())
            );
            Ok(())
        }
        Cmd::Pack { command } => {
            let registry = packs::resolve_registry(cli.registry.as_deref());
            pack(&root, command, registry)
        }
    }
}

async fn check(
    root: &std::path::Path,
    paths: Vec<PathBuf>,
    diff: Option<PathBuf>,
    format: OutFormat,
    fail_on: policy::Severity,
    registry: Option<&Registry>,
) -> anyhow::Result<()> {
    let (mut policy, found) = policy::load_policy(root);
    if found.is_none() {
        eprintln!(
            "codesmell: no .codesmell/policy.toml found; using built-in defaults. \
             Run `codesmell init` to create one."
        );
    }
    packs::expand_includes(&mut policy, root, registry)?;

    let scope = build_scope(diff, paths)?;

    let root_utf8 = camino::Utf8PathBuf::from_path_buf(root.to_path_buf())
        .map_err(|_| anyhow::anyhow!("non-UTF8 repository path"))?;
    let index = build_index(&root_utf8).await?;
    let report = evaluate(&index, &scope, &policy, root).await?;

    match format {
        OutFormat::Human => print_human(&report),
        OutFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }

    let should_fail = report.violations.iter().any(|v| v.severity >= fail_on);
    if should_fail {
        std::process::exit(1);
    }
    Ok(())
}

/// Coverage measurement over a scope, shared by the `coverage` subcommand and
/// the threshold violation inside `check`.
async fn coverage(
    root: &std::path::Path,
    paths: Vec<PathBuf>,
    diff: Option<PathBuf>,
    format: OutFormat,
    fail_on: policy::Severity,
    registry: Option<&Registry>,
) -> anyhow::Result<()> {
    let (mut policy, found) = policy::load_policy(root);
    if found.is_none() {
        eprintln!(
            "codesmell: no .codesmell/policy.toml found; using built-in defaults. \
             Run `codesmell init` to create one."
        );
    }
    packs::expand_includes(&mut policy, root, registry)?;
    let Some(cfg) = policy.testing.coverage.clone() else {
        anyhow::bail!(
            "no [testing.coverage] section in the policy — \
             see docs/codesmell-coverage.md for the configuration guide"
        );
    };

    let scope = build_scope(diff, paths)?;
    let root_utf8 = camino::Utf8PathBuf::from_path_buf(root.to_path_buf())
        .map_err(|_| anyhow::anyhow!("non-UTF8 repository path"))?;
    let index = build_index(&root_utf8).await?;
    let stats = codesmell::coverage::measure(&index, &scope, &cfg, root).await;

    match format {
        OutFormat::Human => print_coverage_human(&stats),
        OutFormat::Json => println!("{}", serde_json::to_string_pretty(&stats)?),
    }

    if !stats.met && policy.severity_of(codesmell::policy::RULE_COVERAGE) >= fail_on {
        std::process::exit(1);
    }
    Ok(())
}

/// Build the evaluation scope from `--diff` / positional paths.
fn build_scope(diff: Option<PathBuf>, paths: Vec<PathBuf>) -> anyhow::Result<CheckScope> {
    if let Some(dp) = diff {
        let text = if dp.as_os_str() == "-" {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        } else {
            std::fs::read_to_string(&dp)?
        };
        let parsed =
            parse_unified_diff(&text).map_err(|e| anyhow::anyhow!("failed to parse diff: {e}"))?;
        Ok(CheckScope::Diff(parsed))
    } else if paths.is_empty() {
        Ok(CheckScope::All)
    } else {
        Ok(CheckScope::Paths(
            paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        ))
    }
}

fn print_coverage_human(stats: &codesmell::coverage::CoverageStats) {
    match stats.threshold {
        Some(t) => println!(
            "Coverage: {}% ({} of {} logic functions tested) — threshold {t}%: {}",
            stats.percent,
            stats.covered,
            stats.total,
            if stats.met { "MET" } else { "NOT MET" }
        ),
        None => println!(
            "Coverage: {}% ({} of {} logic functions tested)",
            stats.percent, stats.covered, stats.total
        ),
    }
    if stats.uncovered.is_empty() {
        return;
    }
    println!("\nUncovered:");
    for u in &stats.uncovered {
        println!("  --> {}:{}  {}", u.file, u.line, u.symbol);
    }
}

fn guide(
    root: &std::path::Path,
    path: Option<PathBuf>,
    registry: Option<&Registry>,
) -> anyhow::Result<()> {
    let (mut p, _) = policy::load_policy(root);
    packs::expand_includes(&mut p, root, registry)?;
    let lib = RhaiRuleLib::load(root, &p.rhai.rule_dirs)
        .map_err(|e| {
            eprintln!("codesmell: warning: {e}");
        })
        .ok();
    if let Some(path) = path {
        println!("# conventions effective for: {}", path.display());
    }
    println!("{}", guide::render_guide(&p, lib.as_ref()));
    Ok(())
}

fn print_human(report: &codesmell::engine::CheckReport) {
    if report.violations.is_empty() {
        println!("codesmell: no violations found.");
        return;
    }
    for v in &report.violations {
        println!("{}[{}]: {}", v.severity.as_label(), v.rule, v.message);
        println!("  --> {}:{}", v.file, v.line);
        println!("  hint: {}", v.fix_hint);
    }
    println!();
    let total: usize = report.summary.values().sum();
    let parts: Vec<String> = report
        .summary
        .iter()
        .map(|(k, n)| format!("{n} {k}"))
        .collect();
    println!("{total} violation(s): {}", parts.join(", "));
}

fn init(root: &std::path::Path, pack: Option<&str>) -> anyhow::Result<()> {
    let dir = root.join(".codesmell");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("policy.toml");
    if path.exists() {
        eprintln!(
            "codesmell: {} already exists; not overwriting.",
            path.display()
        );
    } else {
        std::fs::write(&path, guide::STARTER_POLICY)?;
        println!("codesmell: wrote {}", path.display());
    }
    println!("\nAdd this to AGENTS.md / CLAUDE.md so the LLM follows conventions:");
    println!("----");
    println!("{}", guide::AGENTS_SNIPPET);

    if let Some(name) = pack {
        println!();
        match packs::builtin_packs().iter().find(|p| p.name == name) {
            Some(pack) => packs::add_pack(root, pack)?,
            None => {
                eprintln!("codesmell: unknown pack `{name}`");
                eprintln!("available packs:");
                for p in packs::builtin_packs() {
                    eprintln!("  {}", p.name);
                }
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

fn pack(
    root: &std::path::Path,
    command: PackCmd,
    registry: anyhow::Result<Registry>,
) -> anyhow::Result<()> {
    match command {
        PackCmd::List => {
            println!("Built-in packs:");
            for p in packs::builtin_packs() {
                println!("  {} — {}", p.name, p.description);
            }
            match &registry {
                Ok(reg) => {
                    println!("\nRegistry packs ({}):", reg.describe());
                    for info in packs::list_registry_packs(reg)? {
                        let ver = info.version.map(|v| format!(" (v{v})")).unwrap_or_default();
                        println!("  {} — {}{}", info.name, info.description, ver);
                    }
                }
                Err(_) => {
                    println!(
                        "\n(No registry configured; set --registry, CODESMELL_REGISTRY, or \
                         ~/.config/codesmell/config.toml to list registry packs.)"
                    );
                }
            }
            Ok(())
        }
        PackCmd::Add { name } => match packs::builtin_packs().iter().find(|p| p.name == name) {
            Some(pack) => packs::add_pack(root, pack),
            None => {
                eprintln!("codesmell: unknown pack `{name}`");
                eprintln!("available packs:");
                for p in packs::builtin_packs() {
                    eprintln!("  {}", p.name);
                }
                std::process::exit(1);
            }
        },
        PackCmd::Pull { name } => {
            let reg = registry?;
            packs::pull_pack(&reg, &name)?;
            Ok(())
        }
        PackCmd::Update => {
            let reg = registry?;
            packs::update_registry(&reg)?;
            println!("codesmell: registry updated.");
            Ok(())
        }
    }
}
