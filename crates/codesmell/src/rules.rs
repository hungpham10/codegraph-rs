//! Policy rule implementations: style, architecture, testing.

use codegraph_core::{
    is_marker, Symbol, SymbolKind, MARKER_BRANCH_END, MARKER_BREAK, MARKER_CONTINUE, MARKER_IF_FALSE,
    MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK, MARKER_SWITCH_CASE, MARKER_SWITCH_END,
};
use codegraph_graph::GraphIndex;
use std::path::Path;

use crate::engine::{rel_path, Violation};
use crate::glob::GlobSet;
use crate::policy::{
    Layer, Policy, RULE_BOUNDARY, RULE_MAX_LINES, RULE_MAX_NESTING, RULE_MAX_PARAMS,
    RULE_MISSING_TEST, RULE_NAMING,
};

// ==================== Style ====================

/// Heuristic nesting depth from a call chain's control-flow markers.
/// Open markers (LOOP/IF/...) increase depth; close markers (BRANCH_END/...) decrease it.
fn max_nesting(chain: &[u64]) -> u32 {
    let mut depth = 0u32;
    let mut max = 0u32;
    for &e in chain {
        if !is_marker(e) {
            continue;
        }
        match e {
            MARKER_LOOP | MARKER_IF_TRUE | MARKER_IF_FALSE | MARKER_SWITCH_CASE => {
                depth += 1;
                max = max.max(depth);
            }
            MARKER_BRANCH_END | MARKER_LOOP_BACK | MARKER_SWITCH_END | MARKER_BREAK | MARKER_CONTINUE => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    max
}

fn loc_of(s: &Symbol) -> u32 {
    s.end_line.saturating_sub(s.line).saturating_add(1)
}

/// Count a function's parameters from its signature string.
///
/// The extractor does not emit `Parameter` symbols for every language, so we
/// parse the `(...)` parameter list directly. `self` (and `&self` / `&mut self`)
/// is excluded — team "max parameters" conventions count real arguments.
fn count_params(sig: &str) -> u32 {
    let Some(open) = sig.find('(') else {
        return 0;
    };
    let mut depth = 0i32;
    let mut end = None;
    for (i, c) in sig[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return 0;
    };
    let inner = &sig[open + 1..end];
    if inner.trim().is_empty() {
        return 0;
    }
    // Split on top-level commas only; commas inside `(...)` or `<...>` (e.g.
    // `Option<(i32, i32)>`) belong to a single parameter.
    let mut depth = 0i32;
    let mut seg = String::new();
    let mut count = 0u32;
    for c in inner.chars() {
        match c {
            '(' | '<' => {
                depth += 1;
                seg.push(c);
            }
            ')' | '>' => {
                depth -= 1;
                seg.push(c);
            }
            ',' if depth == 0 => {
                if is_real_param(&seg) {
                    count += 1;
                }
                seg.clear();
            }
            _ => seg.push(c),
        }
    }
    if is_real_param(&seg) {
        count += 1;
    }
    count
}

/// A parameter is real (counts toward the limit) if non-empty and not `self`
/// (or `&self` / `&mut self`).
fn is_real_param(seg: &str) -> bool {
    let t = seg.trim();
    if t.is_empty() {
        return false;
    }
    let s = t.trim_start_matches('&').trim_start_matches("mut ").trim();
    s != "self"
}

pub async fn run_style(
    index: &GraphIndex,
    candidates: &[Symbol],
    policy: &Policy,
    root: &Path,
) -> anyhow::Result<Vec<Violation>> {
    let mut out = Vec::new();
    for s in candidates {
        let p = policy.effective_for(&rel_path(&s.file, root));
        let style = &p.style;

        if let Some(max) = style.function.max_lines {
            let loc = loc_of(s);
            if loc > max {
                out.push(Violation {
                    rule: RULE_MAX_LINES.into(),
                    severity: p.severity_of(RULE_MAX_LINES),
                    file: s.file.clone(),
                    line: s.line,
                    symbol: s.name.clone(),
                    message: format!("function `{}` is {loc} lines (max {max})", s.name),
                    fix_hint: format!("split `{}` into smaller functions to stay under {max} lines", s.name),
                });
            }
        }

        if let Some(max) = style.function.max_parameters {
            let n = count_params(s.signature.as_deref().unwrap_or(""));
            if n > max {
                out.push(Violation {
                    rule: RULE_MAX_PARAMS.into(),
                    severity: p.severity_of(RULE_MAX_PARAMS),
                    file: s.file.clone(),
                    line: s.line,
                    symbol: s.name.clone(),
                    message: format!("function `{}` takes {n} parameters (max {max})", s.name),
                    fix_hint: "group parameters into a struct or options type".into(),
                });
            }
        }

        if let Some(max) = style.function.max_nesting {
            if let Ok(flow) = index.flow(s.id).await {
                let depth = max_nesting(&flow.chain);
                if depth > max {
                    out.push(Violation {
                        rule: RULE_MAX_NESTING.into(),
                        severity: p.severity_of(RULE_MAX_NESTING),
                        file: s.file.clone(),
                        line: s.line,
                        symbol: s.name.clone(),
                        message: format!("function `{}` nesting depth is {depth} (max {max})", s.name),
                        fix_hint: "flatten early returns and extract nested blocks".into(),
                    });
                }
            }
        }

        for nr in &style.naming.rules {
            let kind = match SymbolKind::parse(&nr.kind) {
                Some(k) => k,
                None => continue,
            };
            if kind != s.kind {
                continue;
            }
            if let Some(sig) = &nr.signature_contains {
                if !s.signature.as_deref().unwrap_or("").contains(sig.as_str()) {
                    continue;
                }
            }
            if !nr.paths.is_empty() {
                let rel = rel_path(&s.file, root);
                if !nr.paths.iter().any(|pp| crate::glob::glob_matches(pp, &rel)) {
                    continue;
                }
            }
            let ok = GlobSet::new(&[nr.pattern.clone()]).matches(&s.name);
            if !ok {
                out.push(Violation {
                    rule: RULE_NAMING.into(),
                    severity: p.severity_of(RULE_NAMING),
                    file: s.file.clone(),
                    line: s.line,
                    symbol: s.name.clone(),
                    message: format!("`{}` should match naming pattern `{}`", s.name, nr.pattern),
                    fix_hint: "rename to follow the team naming convention".into(),
                });
            }
        }
    }
    Ok(out)
}

// ==================== Architecture ====================

/// Maps file paths → layer names using per-layer path globs.
struct LayerIndex {
    map: Vec<(String, GlobSet)>,
}

impl LayerIndex {
    fn new(layers: &[Layer]) -> Self {
        let map = layers
            .iter()
            .map(|l| (l.name.clone(), GlobSet::new(&l.paths)))
            .collect();
        LayerIndex { map }
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn layer_of(&self, path: &str) -> Option<&str> {
        self.map
            .iter()
            .find(|(_, g)| g.matches(path))
            .map(|(name, _)| name.as_str())
    }
}

pub async fn run_architecture(
    index: &GraphIndex,
    candidates: &[Symbol],
    policy: &Policy,
    root: &Path,
) -> anyhow::Result<Vec<Violation>> {
    let layers = LayerIndex::new(&policy.architecture.layers);
    if layers.is_empty() || policy.architecture.boundary.is_empty() {
        return Ok(Vec::new());
    }
    let deny: Vec<String> = policy
        .architecture
        .boundary
        .iter()
        .flat_map(|b| b.deny.iter().cloned())
        .collect();
    if deny.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for s in candidates {
        let Some(caller_layer) = layers.layer_of(&rel_path(&s.file, root)) else {
            continue;
        };
        let callees = index.callees(s.id).await?;
        for callee in callees {
            if let Some(callee_layer) = layers.layer_of(&rel_path(&callee.file, root)) {
                let edge = format!("{} -> {}", caller_layer, callee_layer);
                if deny.iter().any(|d| d == &edge) {
                    out.push(Violation {
                        rule: RULE_BOUNDARY.into(),
                        severity: policy.severity_of(RULE_BOUNDARY),
                        file: s.file.clone(),
                        line: s.line,
                        symbol: s.name.clone(),
                        message: format!(
                            "`{}` ({}) calls `{}` ({}): edge `{}` is denied",
                            s.name, caller_layer, callee.name, callee_layer, edge
                        ),
                        fix_hint: format!(
                            "route `{}` through an allowed layer instead of calling `{}` directly",
                            s.name, callee_layer
                        ),
                    });
                }
            }
        }
    }
    Ok(out)
}

// ==================== Testing ====================

pub async fn run_testing(
    index: &GraphIndex,
    candidates: &[Symbol],
    policy: &Policy,
    root: &Path,
) -> anyhow::Result<Vec<Violation>> {
    if !policy.testing.require_tests_for_changed_logic {
        return Ok(Vec::new());
    }
    let test_globs = GlobSet::new(&policy.testing.test_paths);
    if test_globs.is_empty() {
        return Ok(Vec::new());
    }
    let layers = LayerIndex::new(&policy.architecture.layers);
    let selectors = &policy.testing.logic_selectors;

    let mut out = Vec::new();
    for s in candidates {
        let rel = rel_path(&s.file, root);
        let is_logic = if selectors.is_empty() {
            true
        } else {
            let layer = layers.layer_of(&rel);
            selectors.iter().any(|sel| {
                sel.layers.iter().any(|l| layer == Some(l.as_str()))
                    || sel.min_lines.is_some_and(|m| loc_of(s) >= m)
            })
        };
        if !is_logic {
            continue;
        }
        let refs = index.callers_by_call_name(&s.name, 0).await?;
        let tested = refs
            .iter()
            .any(|r| test_globs.matches(&rel_path(&r.file, root)));
        if !tested {
            out.push(Violation {
                rule: RULE_MISSING_TEST.into(),
                severity: policy.severity_of(RULE_MISSING_TEST),
                file: s.file.clone(),
                line: s.line,
                symbol: s.name.clone(),
                message: format!("business logic `{}` has no unit test", s.name),
                fix_hint: "add a unit test that covers this logic".into(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_real_parameters_and_skips_self() {
        assert_eq!(count_params("fn f()"), 0);
        assert_eq!(count_params("fn f(&self)"), 0);
        assert_eq!(count_params("fn f(&self, id: i32)"), 1);
        assert_eq!(
            count_params("pub async fn place_order(&self, repo: &OrderRepo, a: i32, b: i32) -> i32"),
            3
        );
        // nested parentheses inside a default value must not break matching
        assert_eq!(count_params("fn f(x: i32, y: Option<(i32, i32)>)"), 2);
    }

    #[test]
    fn nesting_depth_counts_open_close_markers() {
        let chain = vec![
            MARKER_IF_TRUE,
            MARKER_BRANCH_END,
            MARKER_LOOP,
            MARKER_LOOP_BACK,
        ];
        assert_eq!(max_nesting(&chain), 1);
        let nested = vec![
            MARKER_IF_TRUE,
            MARKER_LOOP,
            MARKER_IF_FALSE,
            MARKER_BRANCH_END,
            MARKER_LOOP_BACK,
        ];
        assert_eq!(max_nesting(&nested), 3);
    }
}
