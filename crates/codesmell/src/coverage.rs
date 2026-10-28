//! Call-graph coverage estimate.
//!
//! Answers "how much of the business logic is reached by a unit test?" without
//! running the tests: a logic function is *covered* when at least one of its
//! direct callers lives in a file matching a `test_paths` glob. This is the
//! same mechanism the `testing.missing_test` rule uses per function — here it
//! is aggregated into a percentage so a policy can set a threshold.
//!
//! This is a heuristic, not statement coverage: code paths inside a function
//! body are not tracked. See docs/codesmell-coverage.md.

use codegraph_core::Symbol;
use codegraph_graph::GraphIndex;
use serde::Serialize;
use std::path::Path;

use crate::engine::{collect_symbols, rel_path, CheckScope};
use crate::glob::GlobSet;
use crate::policy::{CoverageConfig, LayerSpec, SelectorSpec};
use crate::rhai::FN_KINDS;

/// One logic function no test file calls.
#[derive(Debug, Clone, Serialize)]
pub struct UncoveredSymbol {
    pub file: String,
    pub line: u32,
    pub symbol: String,
    /// Body length in lines — lets callers sort uncovered functions by size.
    pub lines: u32,
}

/// Result of a coverage measurement over a scope.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageStats {
    /// `covered / total` rounded down to a whole percent (100 when total = 0).
    pub percent: u64,
    pub total: usize,
    pub covered: usize,
    /// Threshold from `[testing.coverage] line`, if set.
    pub threshold: Option<u64>,
    /// `threshold.is_none() || percent >= threshold`.
    pub met: bool,
    /// Logic functions no test file calls, largest body first.
    pub uncovered: Vec<UncoveredSymbol>,
}

/// The first layer whose path globs match `file` (mirrors `layer_of` in the
/// `testing.missing_test` rhai script).
fn layer_of<'a>(layers: &'a [LayerSpec], file: &str) -> &'a str {
    for l in layers {
        let set = GlobSet::new(&l.paths);
        if set.matches(file) {
            return &l.name;
        }
    }
    ""
}

/// Selector semantics shared with `testing.missing_test`: with no selectors
/// every function counts; otherwise a function counts when its layer is listed
/// or its body is at least `min_lines` long.
fn is_logic(sym: &Symbol, layers: &[LayerSpec], selectors: &[SelectorSpec], rel: &str) -> bool {
    if selectors.is_empty() {
        return true;
    }
    let layer = layer_of(layers, rel);
    selectors.iter().any(|sel| {
        sel.layers.iter().any(|l| l == layer)
            || sel.min_lines > 0 && sym.end_line.saturating_sub(sym.line) + 1 >= sel.min_lines
    })
}

/// Measure call-graph coverage of the logic functions in `scope`.
pub async fn measure(
    index: &GraphIndex,
    scope: &CheckScope,
    cfg: &CoverageConfig,
    root: &Path,
) -> CoverageStats {
    let test_glob = GlobSet::new(&cfg.test_paths);
    let mut total = 0usize;
    let mut uncovered: Vec<UncoveredSymbol> = Vec::new();

    for s in collect_symbols(index, FN_KINDS, scope, root) {
        let rel = rel_path(&s.file, root);
        if !is_logic(&s, &cfg.layers, &cfg.selectors, &rel) {
            continue;
        }
        total += 1;
        let callers = index.callers(s.id, 1).await.unwrap_or_default();
        let covered = callers.iter().any(|c| {
            let crel = rel_path(&c.file, root);
            test_glob.matches(&crel)
        });
        if !covered {
            uncovered.push(UncoveredSymbol {
                file: rel,
                line: s.line,
                symbol: s.name.clone(),
                lines: s.end_line.saturating_sub(s.line) + 1,
            });
        }
    }

    let covered = total - uncovered.len();
    let percent = if total == 0 {
        100
    } else {
        (covered as u64 * 100) / total as u64
    };
    // Largest body first — the most valuable starting point for new tests.
    uncovered.sort_by_key(|u| std::cmp::Reverse(u.lines));
    let threshold = cfg.line;
    let met = threshold.is_none_or(|t| percent >= t);
    CoverageStats {
        percent,
        total,
        covered,
        threshold,
        met,
        uncovered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(line: u32, end_line: u32) -> Symbol {
        Symbol {
            id: 0,
            name: "f".into(),
            kind: codegraph_core::SymbolKind::Function,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "src/services/x.rs".into(),
            line,
            end_line,
            signature: None,
            doc: None,
            annotations: vec![],
            language: "rust".into(),
            impl_trait: None,
            sig_key: None,
        }
    }

    #[test]
    fn selector_semantics_match_missing_test_rule() {
        let layers = vec![LayerSpec {
            name: "service".into(),
            paths: vec!["src/services/**".into()],
        }];
        // Layer selector only.
        let sels = vec![SelectorSpec {
            layers: vec!["service".into()],
            min_lines: 0,
        }];
        assert!(is_logic(&sym(1, 3), &layers, &sels, "src/services/x.rs"));
        assert!(!is_logic(&sym(1, 3), &layers, &sels, "src/main.rs"));
        // min_lines selector accepts long functions anywhere.
        let sels = vec![SelectorSpec {
            layers: vec![],
            min_lines: 10,
        }];
        assert!(!is_logic(&sym(1, 5), &layers, &sels, "src/main.rs"));
        assert!(is_logic(&sym(1, 15), &layers, &sels, "src/main.rs"));
        // No selectors → everything is logic.
        assert!(is_logic(&sym(1, 2), &layers, &[], "anything.rs"));
    }
}
