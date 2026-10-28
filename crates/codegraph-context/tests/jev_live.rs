//! Test gọi API Jev thật — `#[ignore]` vì cần mạng + token.
//!
//! ```bash
//! cargo test -p codegraph-context --test jev_live -- --ignored --nocapture
//! ```
//!
//! Cần `OPENCODE_API_KEY` (hoặc `[jev] api_key_env` trỏ biến khác).

use codegraph_context::{Candidate, JevJudge, Judge};
use codegraph_core::JevConfig;

fn candidate(id: u64, kind: &str, name: &str, file: &str, sig: &str) -> Candidate {
    Candidate {
        id,
        label: format!("{kind} `{name}` in `{file}:10`"),
        detail: Some(sig.into()),
    }
}

fn cfg() -> JevConfig {
    let mut cfg = JevConfig {
        enabled: true,
        repo: Some("demo-repo".into()),
        timeout_ms: 20_000,
        ..JevConfig::default()
    };
    if let Ok(model) = std::env::var("JEV_MODEL") {
        cfg.model = model;
    }
    cfg
}

#[tokio::test]
#[ignore = "gọi API thật: cần OPENCODE_API_KEY + mạng"]
async fn jev_chọn_đúng_symbol_và_trả_xác_suất() {
    let cfg = cfg();
    assert!(cfg.api_key().is_some(), "thiếu env `{}`", cfg.api_key_env);
    let candidates = vec![
        candidate(
            1,
            "function",
            "validate_file_key",
            "tools/figma-cli/src/figma_lib/utils.py",
            "def validate_file_key(file_key: str) -> str:",
        ),
        candidate(
            2,
            "method",
            "clear_embeddings",
            "crates/codegraph-graph/src/storage/sqlite.rs",
            "async fn clear_embeddings(&mut self) -> Result<()>",
        ),
        candidate(
            3,
            "function",
            "parse_config",
            "crates/codegraph-extract/src/config.rs",
            "fn parse_config(text: &str) -> Config",
        ),
        candidate(
            123,
            "function",
            "unrelated_worker",
            "src/worker.rs",
            "async fn unrelated_worker(&self)",
        ),
    ];

    let verdict = JevJudge::new(cfg)
        .rank(
            "where do we check that config.toml parses correctly?",
            Some("demo-repo"),
            &candidates,
        )
        .await
        .expect("Jev phải trả được verdict (kiểm tra token/model)");

    println!("{verdict:#?}");
    assert!(!verdict.no_match, "pool có `parse_config` — phải match");
    assert_eq!(
        verdict.ranking.first().map(|(id, _)| *id),
        Some(3),
        "phải chọn parse_config"
    );
}

/// Cùng key ở nhiều document → chỉ **value** mới phân biệt được. Chạy khi
/// `[jev] doc_include_values = true` (mặc định tắt vì tốn token).
#[tokio::test]
#[ignore = "gọi API thật: cần OPENCODE_API_KEY + mạng"]
async fn jev_chọn_đúng_doc_node_nhờ_value() {
    let cfg = JevConfig {
        doc_include_values: true,
        ..cfg()
    };
    assert!(cfg.api_key().is_some(), "thiếu env `{}`", cfg.api_key_env);

    let doc = |id: u64, key: &str, path: &str, kind: &str, value: &str| {
        Candidate::doc(id, key, path, kind, Some(value), &cfg)
    };
    let candidates = vec![
        doc(
            1,
            "spec.rules.host",
            "deploy/ingress-public.yaml",
            "Field",
            "www.example.com",
        ),
        doc(
            2,
            "spec.rules.host",
            "deploy/ingress-internal.yaml",
            "Field",
            "db-staging.internal",
        ),
        doc(
            3,
            "spec.rules.host",
            "deploy/ingress-legacy.yaml",
            "Field",
            "legacy.example.com",
        ),
        doc(
            4,
            "spec.rules.backend.serviceName",
            "deploy/ingress-internal.yaml",
            "Field",
            "db",
        ),
    ];

    let verdict = JevJudge::new(cfg.clone())
        .rank(
            "which ingress rule routes traffic to the staging database host?",
            Some("demo-repo"),
            &candidates,
        )
        .await
        .expect("Jev phải trả được verdict (kiểm tra token/model)");

    println!("{verdict:#?}");
    assert!(!verdict.no_match, "phải match — có host staging trong pool");
    assert_eq!(
        verdict.ranking.first().map(|(id, _)| *id),
        Some(2),
        "phải chọn host `db-staging.internal`"
    );
}
