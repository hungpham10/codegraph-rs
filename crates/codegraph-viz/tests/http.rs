use axum::Router;
use codegraph_core::{ScopeLevel, Symbol, SymbolKind, SYMBOL_BASE};
use codegraph_graph::{GraphIndex, ParseResult, SharedGraphIndex};
use codegraph_viz::api::{self, AppState};
use codegraph_viz::{BootConfig, VizConfig};
use std::collections::HashMap;
use std::sync::Arc;

fn sym(id: u64, name: &str) -> Symbol {
    Symbol {
        id,
        name: name.to_string(),
        kind: SymbolKind::Function,
        scope: ScopeLevel::Global,
        scope_id: 0,
        type_ref: 0,
        type_name: None,
        file: "src/main.rs".into(),
        line: 1,
        end_line: 1,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        language: "rust".into(),
    }
}

/// Seed index sqlite: main → helper, cộng thêm 1 variable trùng tên `main` để
/// kiểm tra query-seed ưu tiên function thay vì rơi vào variable.
async fn seed_index(db_path: &str) {
    let mut idx = GraphIndex::open(db_path).await.unwrap();
    let mut var_main = sym(SYMBOL_BASE + 2, "main");
    var_main.kind = SymbolKind::Variable;
    let r = ParseResult {
        path: "src/main.rs".into(),
        language: "rust".into(),
        bytes: 10,
        lines: 5,
        symbols: vec![
            sym(SYMBOL_BASE, "main"),
            sym(SYMBOL_BASE + 1, "helper"),
            var_main,
        ],
        chains: HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1])]),
        calls: Vec::new(),
    };
    idx.ingest(&[r]).await.unwrap();
}

async fn test_router(db_path: std::path::PathBuf) -> Router {
    let boot = BootConfig {
        target: None,
        prefix: None,
        depth: 2,
    };
    let shared_index = Arc::new(SharedGraphIndex::open(Some(db_path)).await.unwrap());
    let state = AppState {
        shared_index,
        boot_json: serde_json::to_string(&boot).unwrap(),
    };
    Router::new()
        .route("/api/status", axum::routing::get(api::status))
        .route("/api/search", axum::routing::get(api::search))
        .route("/api/subgraph", axum::routing::get(api::subgraph))
        .route("/api/neighbors/{id}", axum::routing::get(api::neighbors))
        .route("/api/flow/{id}", axum::routing::get(api::flow))
        .with_state(state)
}

#[tokio::test]
async fn http_status_subgraph_and_flow() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    seed_index(&db_path.to_string_lossy()).await;
    let app = test_router(db_path).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let status: serde_json::Value = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["symbols"], 3);
    assert_eq!(status["chains"], 1);
    assert_eq!(status["edges"], 1);

    let sub: serde_json::Value = client
        .get(format!("{base}/api/subgraph?query=main&depth=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sub["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(sub["edges"].as_array().unwrap().len(), 1);
    // Có variable trùng tên `main` nhưng seed phải là function.
    assert_eq!(sub["seed"]["id"], SYMBOL_BASE);
    assert_eq!(sub["seed"]["kind"], "function");

    let flow: serde_json::Value = client
        .get(format!("{base}/api/flow/{SYMBOL_BASE}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(flow["chain"].as_array().unwrap().len(), 2);
    assert_eq!(flow["chain_desc"][0], "main");
    assert_eq!(flow["chain_desc"][1], "helper");
}

#[tokio::test]
async fn http_search_kind_match_and_neighbors() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    seed_index(&db_path.to_string_lossy()).await;
    let app = test_router(db_path).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // kind=function + match=exact → chỉ còn func main (bỏ variable cùng tên).
    let hits: serde_json::Value = client
        .get(format!("{base}/api/search?q=main&kind=function&match=exact"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = hits.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "main");
    assert_eq!(arr[0]["kind"], "function");
    assert_eq!(arr[0]["id"], SYMBOL_BASE);

    // kind=variable + match=exact → variable cùng tên.
    let hits: serde_json::Value = client
        .get(format!("{base}/api/search?q=main&kind=variable&match=exact"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = hits.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["kind"], "variable");
    assert_eq!(arr[0]["id"], SYMBOL_BASE + 2);

    // Subgraph query=main → seed phải là function (không rơi vào variable).
    let sub: serde_json::Value = client
        .get(format!("{base}/api/subgraph?query=main&depth=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sub["seed"]["id"], SYMBOL_BASE);
    assert_eq!(sub["seed"]["kind"], "function");

    // Neighbors 1 hop quanh main → main + helper (nút Expand trong UI).
    let nb: serde_json::Value = client
        .get(format!("{base}/api/neighbors/{SYMBOL_BASE}?depth=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(nb["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(nb["edges"].as_array().unwrap().len(), 1);
    assert_eq!(nb["seed"]["id"], SYMBOL_BASE);
}

#[test]
fn viz_config_serializes_boot() {
    let boot = BootConfig {
        target: Some("foo".into()),
        prefix: None,
        depth: 3,
    };
    let cfg = VizConfig {
        port: 7421,
        open_browser: false,
        boot,
    };
    assert_eq!(cfg.port, 7421);
}
