//! Integration: Orchestrator walk + parse → GraphIndex::ingest → search.

use camino::Utf8PathBuf;
use codegraph_core::Symbol;
use codegraph_extract::Orchestrator;
use codegraph_graph::{GraphIndex, Pagination};

/// Tiện ích: search substring (resumable) trả `Vec<Symbol>` — thay thế
/// `GraphIndex::search_symbol` đã xoá (mọi search đều qua resumable path).
async fn search_symbol(idx: &GraphIndex, q: &str) -> Vec<Symbol> {
    idx.search_symbol_paged_resumable(
        q,
        None,
        codegraph_core::SymbolMatch::Contains,
        Pagination { limit: 10, offset: 0 },
        None,
        None,
    )
    .await
    .unwrap()
    .page
}

fn fixture_root() -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
    )
    .unwrap()
}

async fn index_fixtures() -> (GraphIndex, codegraph_extract::ExtractStats) {
    let mut index = GraphIndex::in_memory();
    let orch = Orchestrator::with_registry();
    let stats = orch
        .index_all(&fixture_root(), &mut index, None)
        .await
        .unwrap();
    (index, stats)
}

#[tokio::test]
async fn index_fixtures_dir() {
    let (index, stats) = index_fixtures().await;

    // 7 sample.* files + issue9_attr_specifiers.h
    assert!(stats.files >= 8, "expected >= 8 files, got {}", stats.files);
    assert!(stats.symbols > 0, "expected symbols");
    assert!(stats.chains > 0, "expected chains");
    assert!(stats.calls > 0, "expected calls");

    // Java
    let hits = search_symbol(&index,"UserService").await;
    assert!(
        hits.iter().any(|s| s.language == "java"),
        "expected java hit, got {hits:?}"
    );

    // Ruby
    let hits = search_symbol(&index,"UserService").await;
    assert!(
        hits.iter().any(|s| s.language == "ruby"),
        "expected ruby hit"
    );

    // Python
    let hits = search_symbol(&index,"process_user").await;
    assert!(
        hits.iter().any(|s| s.language == "python"),
        "expected python hit"
    );

    // Go
    let hits = search_symbol(&index,"ProcessUser").await;
    assert!(hits.iter().any(|s| s.language == "go"), "expected go hit");

    // JS
    let hits = search_symbol(&index,"processUser").await;
    assert!(
        hits.iter().any(|s| s.language == "javascript"),
        "expected js hit"
    );

    // TS
    let hits = search_symbol(&index,"processUser").await;
    assert!(
        hits.iter().any(|s| s.name == "processUser"),
        "missing processUser, got {hits:?}"
    );

    // Rust
    let hits = search_symbol(&index,"process_user").await;
    assert!(hits.iter().any(|s| s.name == "process_user"));

    // UserService from TS class + Rust struct
    let hits = search_symbol(&index,"UserService").await;
    assert!(
        hits.len() >= 2,
        "expected UserService from both TS and Rust, got {}",
        hits.len()
    );
}

#[tokio::test]
async fn chains_are_built_for_each_function() {
    let (index, _) = index_fixtures().await;
    let stats = index.stats();
    assert!(stats.chains > 0, "expected chains in index");

    // Flow của một function trả về chain có marker hoặc ít nhất là chính nó.
    let hits = search_symbol(&index,"process_user").await;
    let py = hits
        .iter()
        .find(|s| s.language == "python")
        .expect("python process_user");
    let flow = index.flow(py.id).await.unwrap();
    assert!(!flow.chain.is_empty(), "chain phải chứa chính function id");
    assert_eq!(flow.chain[0], py.id, "chain bắt đầu bằng owner");
}
