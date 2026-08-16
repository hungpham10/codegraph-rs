//! CodeSmell CLI — a team convention linter (like eslint/clippy).

use clap::{Parser, Subcommand, ValueEnum};
use codesmell::engine::{CheckScope, evaluate};
use codesmell::guide;
use codesmell::index::build_index;
use codesmell::policy;
use codegraph_graph::diff::parse_unified_diff;
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "codesmell",
    about = "Team convention linter for maintainable, LLM-friendly code"
)]
struct Cli {
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
    /// Print the conventions pack an LLM should read before writing code.
    Guide {
        /// Optional path to show conventions effective for that area.
        path: Option<PathBuf>,
    },
    /// Write a starter `.codesmell/policy.toml` and print an AGENTS.md snippet.
    Init,
    /// Print the effective resolved policy as TOML.
    Policy,
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
        } => check(&root, paths, diff, format, fail_on).await,
        Cmd::Guide { path } => {
            let (p, _) = policy::load_policy(&root);
            let p = if let Some(path) = path {
                let rel = path.to_string_lossy().into_owned();
                p.effective_for(&rel)
            } else {
                p
            };
            println!("{}", guide::render_guide(&p));
            Ok(())
        }
        Cmd::Init => init(&root),
        Cmd::Policy => {
            let (p, _) = policy::load_policy(&root);
            println!(
                "{}",
                toml::to_string_pretty(&p).unwrap_or_else(|_| "# (policy could not be serialized)".into())
            );
            Ok(())
        }
    }
}

async fn check(
    root: &std::path::Path,
    paths: Vec<PathBuf>,
    diff: Option<PathBuf>,
    format: OutFormat,
    fail_on: policy::Severity,
) -> anyhow::Result<()> {
    let (policy, found) = policy::load_policy(root);
    if found.is_none() {
        eprintln!(
            "codesmell: no .codesmell/policy.toml found; using built-in defaults. \
             Run `codesmell init` to create one."
        );
    }

    let scope = if let Some(dp) = diff {
        let text = if dp.as_os_str() == "-" {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        } else {
            std::fs::read_to_string(&dp)?
        };
        let parsed = parse_unified_diff(&text).map_err(|e| anyhow::anyhow!("failed to parse diff: {e}"))?;
        CheckScope::Diff(parsed)
    } else if paths.is_empty() {
        CheckScope::All
    } else {
        CheckScope::Paths(paths.iter().map(|p| p.to_string_lossy().into_owned()).collect())
    };

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

fn init(root: &std::path::Path) -> anyhow::Result<()> {
    let dir = root.join(".codesmell");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("policy.toml");
    if path.exists() {
        eprintln!("codesmell: {} already exists; not overwriting.", path.display());
    } else {
        std::fs::write(&path, guide::STARTER_POLICY)?;
        println!("codesmell: wrote {}", path.display());
    }
    println!("\nAdd this to AGENTS.md / CLAUDE.md so the LLM follows conventions:");
    println!("----");
    println!("{}", guide::AGENTS_SNIPPET);
    Ok(())
}
