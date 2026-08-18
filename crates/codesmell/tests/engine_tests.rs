//! End-to-end tests: build an in-memory CodeGraph from a fixture and evaluate
//! the policy. Fixtures live under `tests/fixtures/`.

use codegraph_graph::diff::parse_unified_diff;
use codesmell::engine::{evaluate, CheckScope};
use codesmell::index::build_index;
use codesmell::packs::{self, Registry, SECURITY_PACK};
use codesmell::policy;
use codesmell::rhai::RhaiRuleLib;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

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

#[tokio::test]
async fn secshop_flags_declarative_and_custom_rules() {
    let (root, idx) = index_for("secshop").await;
    let (p, _) = policy::load_policy(&root);
    let report = evaluate(&idx, &CheckScope::All, &p, &root).await.unwrap();

    // declarative deny_symbol (constant name pattern)
    assert!(
        rules_for(&report, "API_KEY").contains("security.deny_symbol"),
        "expected security.deny_symbol on API_KEY, got {:?}",
        rules_for(&report, "API_KEY")
    );
    // declarative deny_call (denied callee)
    assert!(
        rules_for(&report, "run_script").contains("security.deny_call"),
        "expected security.deny_call on run_script, got {:?}",
        rules_for(&report, "run_script")
    );
    // team-authored rhai rule template (custom rule)
    assert!(
        rules_for(&report, "may_panic").contains("team.no_panic"),
        "expected team.no_panic on may_panic, got {:?}",
        rules_for(&report, "may_panic")
    );
}

#[test]
fn pack_install_copies_files_and_is_idempotent_and_merges() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // No policy yet, just install the pack.
    packs::add_pack(root, &SECURITY_PACK).unwrap();
    let rule = root.join(".codesmell/rules/security.dangerous_exec.rhai");
    let frag = root.join(".codesmell/packs/security.policy.toml");
    assert!(rule.exists(), "pack script not installed");
    assert!(frag.exists(), "pack fragment not installed");

    // Idempotent: second install must not overwrite (and must not error).
    let before = std::fs::read(&rule).unwrap();
    packs::add_pack(root, &SECURITY_PACK).unwrap();
    assert_eq!(
        std::fs::read(&rule).unwrap(),
        before,
        "pack install overwrote a file"
    );

    // With a minimal main policy, the fragment is merged and the pack scripts compile.
    std::fs::write(
        root.join(".codesmell/policy.toml"),
        "[rhai]\nrule_dirs = [\".codesmell/rules\"]\n",
    )
    .unwrap();
    let (p, found) = policy::load_policy(root);
    assert!(found.is_some(), "policy not found after pack install");
    let uses_security: Vec<&str> = p.rhai.rules.iter().map(|r| r.use_script.as_str()).collect();
    assert!(
        uses_security
            .iter()
            .any(|u| *u == "security.dangerous_exec"),
        "pack fragment rules not merged: {uses_security:?}"
    );

    // The pack's rhai scripts must compile as a rule library.
    let lib = RhaiRuleLib::load(root, &p.rhai.rule_dirs);
    assert!(
        lib.is_ok(),
        "pack scripts failed to compile: {:?}",
        lib.err()
    );
}

// A minimal rhai rule used by the include/registry tests.
const DEMO_RULE: &str = r#"
const ADVICE = "demo rule for testing includes";
fn check(sym) {
    if sym.name == "smell_me" {
        "found smell_me"
    }
}
"#;

/// `[[include]] path = "<pack>"` pulls a pack's fragment + rules into the
/// policy without copying files into the repo.
#[test]
fn include_local_path_pulls_pack_rules() {
    let reg = tempfile::tempdir().unwrap();
    let pack = reg.path().join("demo");
    std::fs::create_dir_all(pack.join("rules")).unwrap();
    std::fs::write(
        pack.join("policy.fragment.toml"),
        "[[rhai.rule]]\nuse = \"demo.smell\"\n",
    )
    .unwrap();
    std::fs::write(pack.join("rules/demo.smell.rhai"), DEMO_RULE).unwrap();

    let proj = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(proj.path().join(".codesmell")).unwrap();
    std::fs::write(
        proj.path().join(".codesmell/policy.toml"),
        format!(
            "version = 1\n\n[[include]]\npath = \"{}\"\n",
            pack.display()
        ),
    )
    .unwrap();

    let (mut policy, found) = policy::load_policy(proj.path());
    assert!(found.is_some(), "policy not found");
    packs::expand_includes(&mut policy, proj.path(), None).unwrap();

    assert!(
        policy
            .rhai
            .rules
            .iter()
            .any(|r| r.use_script == "demo.smell"),
        "pack fragment not merged into policy"
    );
    let lib = RhaiRuleLib::load(proj.path(), &policy.rhai.rule_dirs)
        .expect("rule library should compile");
    assert!(
        lib.instances(&policy)
            .iter()
            .any(|i| i.rule_id == "demo.smell"),
        "pack rule not instantiated"
    );
}

/// A `[[include]]` with `name` but no configured registry must error loudly.
#[test]
fn include_name_without_registry_errors() {
    let mut p = policy::Policy::default();
    p.includes.push(policy::IncludeEntry {
        name: Some("x".into()),
        ..Default::default()
    });
    let err = packs::expand_includes(&mut p, Path::new("."), None);
    assert!(err.is_err(), "expected error when registry is missing");
}

/// A git registry (here a local `file://` repo) is cloned into the cache by
/// `update_registry`, then resolved offline by `[[include]] name = ...`.
#[test]
fn include_from_git_registry_resolves_offline_after_update() {
    if Command::new("git").arg("--version").status().is_err() {
        eprintln!("git not available; skipping git registry test");
        return;
    }

    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("srcrepo");
    std::fs::create_dir_all(src.join("demo/rules")).unwrap();
    std::fs::write(
        src.join("demo/policy.fragment.toml"),
        "[[rhai.rule]]\nuse = \"demo.smell\"\n",
    )
    .unwrap();
    std::fs::write(src.join("demo/rules/demo.smell.rhai"), DEMO_RULE).unwrap();
    std::fs::write(
        src.join("demo/pack.toml"),
        "name = \"demo\"\ndescription = \"demo pack\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(&src)
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {:?} failed", args);
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.email=test@test",
        "-c",
        "user.name=test",
        "add",
        "-A",
    ]);
    git(&[
        "-c",
        "user.email=test@test",
        "-c",
        "user.name=test",
        "commit",
        "-q",
        "-m",
        "add pack",
    ]);

    let url = format!("file://{}", src.canonicalize().unwrap().display());
    let cache = work.path().join("cache");
    let reg = Registry::git(url, cache.clone());

    packs::update_registry(&reg).unwrap();
    assert!(cache.join("demo").is_dir(), "pack not cloned into cache");

    let proj = work.path().join("proj");
    std::fs::create_dir_all(proj.join(".codesmell")).unwrap();
    std::fs::write(
        proj.join(".codesmell/policy.toml"),
        "version = 1\n\n[[include]]\nname = \"demo\"\n",
    )
    .unwrap();

    let (mut policy, found) = policy::load_policy(&proj);
    assert!(found.is_some());
    packs::expand_includes(&mut policy, &proj, Some(&reg)).unwrap();

    assert!(
        policy
            .rhai
            .rules
            .iter()
            .any(|r| r.use_script == "demo.smell"),
        "pack fragment not merged from git registry"
    );
    let lib = RhaiRuleLib::load(&proj, &policy.rhai.rule_dirs).expect("rule lib compiles");
    assert!(
        lib.instances(&policy)
            .iter()
            .any(|i| i.rule_id == "demo.smell"),
        "pack rule not instantiated from git registry"
    );

    let infos = packs::list_registry_packs(&reg).unwrap();
    assert!(
        infos
            .iter()
            .any(|p| p.name == "demo" && p.version.as_deref() == Some("0.1.0")),
        "pack metadata not listed: {infos:?}"
    );
}
