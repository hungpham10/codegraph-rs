//! Test gọi API Jev thật — `#[ignore]` vì cần mạng + token.
//!
//! ```bash
//! cargo test -p codegraph-context --test jev_live -- --ignored --nocapture
//! ```
//!
//! Cần `OPENCODE_API_KEY` (hoặc `[jev] api_key_env` trỏ biến khác).

use codegraph_context::{Candidate, JevJudge, Judge};
use codegraph_core::{JevConfig, SymbolKind};

fn candidate(id: u64, name: &str, kind: SymbolKind, file: &str, sig: &str) -> Candidate {
    Candidate {
        id,
        name: name.into(),
        kind,
        file: file.into(),
        line: 10,
        signature: Some(sig.into()),
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
            "validate_file_key",
            SymbolKind::Function,
            "tools/figma-cli/src/figma_lib/utils.py",
            "def validate_file_key(file_key: str) -> str:",
        ),
        candidate(
            2,
            "clear_embeddings",
            SymbolKind::Method,
            "crates/codegraph-graph/src/storage/sqlite.rs",
            "async fn clear_embeddings(&mut self) -> Result<()>",
        ),
        candidate(
            3,
            "parse_config",
            SymbolKind::Function,
            "crates/codegraph-extract/src/config.rs",
            "fn parse_config(text: &str) -> Config",
        ),
        candidate(
            123,
            "unrelated_worker",
            SymbolKind::Function,
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
