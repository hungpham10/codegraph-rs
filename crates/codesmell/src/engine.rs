//! Evaluation engine: collect symbols by kind, run the rhai rule instances,
//! produce a sorted report.

use codegraph_core::SymbolKind;
use codegraph_graph::{diff::ParsedDiff, GraphIndex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::policy::Policy;
use crate::rhai;

/// A single policy violation surfaced to the developer / LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Violation {
    pub rule: String,
    pub severity: crate::policy::Severity,
    pub file: String,
    pub line: u32,
    pub symbol: String,
    pub message: String,
    /// Action that fixes the violation — guides the LLM instead of leaving it
    /// to guess.
    pub fix_hint: String,
}

/// Result of a `check` run.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CheckReport {
    pub violations: Vec<Violation>,
    /// Count of violations keyed by human label (`note`/`warning`/`error`).
    pub summary: HashMap<String, usize>,
}

/// What part of the repository to evaluate.
#[derive(Debug)]
pub enum CheckScope {
    /// Whole repository (default).
    All,
    /// Symbols whose file is under one of the given paths.
    Paths(Vec<String>),
    /// Symbols in files touched by a unified diff, within changed line ranges.
    Diff(ParsedDiff),
}

/// Repo-relative path for a symbol's file (used for layer / naming globs).
pub fn rel_path(file: &str, root: &Path) -> String {
    Path::new(file)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_string())
}

/// Collect symbol candidates of the given `kinds`, narrowed by `scope`.
pub fn collect_symbols(
    index: &GraphIndex,
    kinds: &[SymbolKind],
    scope: &CheckScope,
    root: &Path,
) -> Vec<codegraph_core::Symbol> {
    let mut all = Vec::new();
    for k in kinds {
        let (syms, _) = index.list_symbols_by_kind(*k, 0, 0);
        all.append(&mut syms.into_iter().collect());
    }

    match scope {
        CheckScope::All => all,
        CheckScope::Paths(paths) => {
            all.retain(|s| {
                let target = Path::new(root).join(&s.file);
                paths
                    .iter()
                    .any(|p| Path::new(&target).starts_with(Path::new(root).join(p)))
            });
            all
        }
        CheckScope::Diff(parsed) => {
            let mut changed: HashMap<PathBuf, std::collections::HashSet<u32>> = HashMap::new();
            for fd in &parsed.files {
                let rel = fd
                    .path
                    .trim_start_matches("a/")
                    .trim_start_matches("b/")
                    .to_string();
                let entry = changed.entry(PathBuf::from(rel)).or_default();
                for h in &fd.hunks {
                    let lo = h.new_start;
                    let hi = h.new_start.saturating_add(h.new_len).saturating_sub(1);
                    for l in lo..=hi {
                        entry.insert(l);
                    }
                    for l in &h.new_lines {
                        entry.insert(*l);
                    }
                }
            }
            all.retain(|s| {
                let rel = PathBuf::from(rel_path(&s.file, root));
                match changed.get(&rel) {
                    Some(lines) => (s.line..=s.end_line).any(|l| lines.contains(&l)),
                    None => false,
                }
            });
            all
        }
    }
}

/// Run the policy over the repository and return a severity-sorted report.
pub async fn evaluate(
    index: &GraphIndex,
    scope: &CheckScope,
    policy: &Policy,
    root: &Path,
) -> anyhow::Result<CheckReport> {
    let mut violations = rhai::run(index, scope, policy, root).await?;

    // Most serious first.
    violations.sort_by_key(|v| std::cmp::Reverse(v.severity));

    let mut summary: HashMap<String, usize> = HashMap::new();
    for v in &violations {
        *summary
            .entry(v.severity.as_label().to_string())
            .or_insert(0) += 1;
    }
    Ok(CheckReport {
        violations,
        summary,
    })
}
