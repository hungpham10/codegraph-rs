//! End-to-end tests: build an in-memory CodeGraph from a fixture and evaluate
//! the policy. Fixtures live under `tests/fixtures/`.

use codegraph_graph::diff::parse_unified_diff;
use codesmell::engine::{evaluate, CheckScope};
use codesmell::index::build_index;
use codesmell::policy;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

async fn index_for(name: &str) -> (PathBuf, codegraph_graph::GraphIndex) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let root_utf8 = camino::Utf8PathBuf::from_path_buf(root.clone()).unwrap();
    let idx = build_index(&root_utf8).await.unwrap();
    (root, idx)
}

fn rules_for(report: &codesmell::engine::CheckReport, symbol: &str) -> HashSet<String> {
    report
        .violations
        .iter()
        .filter(|v| v.symbol == symbol)
        .map(|v| v.rule.clone())
        .collect()
}

#[tokio::test]
async fn rustshop_flags_every_rule_category() {
    let (root, idx) = index_for("rustshop").await;
    let (p, _) = policy::load_policy(&root);
    let report = evaluate(&idx, &CheckScope::All, &p, &root).await.unwrap();

    let place = rules_for(&report, "place_order");
    assert!(
        place.contains("architecture.boundary"),
        "expected boundary violation, got {place:?}"
    );
    assert!(
        place.contains("style.function.max_parameters"),
        "expected max_parameters violation, got {place:?}"
    );
    assert!(
        place.contains("style.naming"),
        "expected naming violation, got {place:?}"
    );

    assert!(rules_for(&report, "compute_big").contains("style.function.max_lines"));
    assert!(rules_for(&report, "unreached_logic").contains("testing.missing_test"));
}

#[tokio::test]
async fn rustshop_diff_scope_narrows_to_changed_file() {
    let (root, idx) = index_for("rustshop").await;
    let (p, _) = policy::load_policy(&root);

    // A hunk covering only `compute_big` (lines 6..41) of price_service.rs.
    let diff = "\
diff --git a/src/services/price_service.rs b/src/services/price_service.rs
--- a/src/services/price_service.rs
+++ b/src/services/price_service.rs
@@ -6,36 +6,36 @@ impl PriceService {
+// edited
";
    let parsed = parse_unified_diff(diff).unwrap();
    let report = evaluate(&idx, &CheckScope::Diff(parsed), &p, &root)
        .await
        .unwrap();

    // Only `compute_big` overlaps the hunk; the controller's issues are out of scope.
    assert_eq!(report.violations.len(), 1, "got {:?}", report.violations);
    assert_eq!(report.violations[0].symbol, "compute_big");
    assert_eq!(report.violations[0].rule, "style.function.max_lines");
}

#[tokio::test]
async fn cleanshop_has_no_violations() {
    let (root, idx) = index_for("cleanshop").await;
    let (p, _) = policy::load_policy(&root);
    let report = evaluate(&idx, &CheckScope::All, &p, &root).await.unwrap();
    assert!(
        report.violations.is_empty(),
        "expected zero violations, got {:?}",
        report.violations
    );
}
