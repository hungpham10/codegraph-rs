use super::*;
use codegraph_core::{ScopeLevel, SYMBOL_BASE};
use codegraph_graph::{GraphIndex, ParseResult, SharedGraphIndex};
use std::collections::HashMap;

#[tokio::test]
async fn callers_timeout_resume_dispatch() {
    let schema = tool_defs()
        .into_iter()
        .find(|t| t.name == "codegraph_callers")
        .unwrap()
        .schema;
    assert_eq!(schema["properties"]["timeout_ms"]["default"], 20000);
    assert_eq!(schema["properties"]["resume"]["type"], "string");
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    let dsn = format!("sqlite://{}", root.join("index.db"));
    let a = SYMBOL_BASE;
    let b = a + 1;
    let symbol = |id, name: &str| Symbol {
        id,
        name: name.into(),
        kind: SymbolKind::Function,
        scope: ScopeLevel::Global,
        scope_id: 0,
        type_ref: 0,
        type_name: None,
        file: "a.rs".into(),
        line: 1,
        end_line: 1,
        signature: None,
        doc: None,
        annotations: vec![],
        language: "rust".into(),
    };
    let mut idx = GraphIndex::open(&dsn).await.unwrap();
    idx.ingest(&[ParseResult {
        path: "a.rs".into(),
        language: "rust".into(),
        bytes: 0,
        lines: 1,
        symbols: vec![symbol(a, "caller"), symbol(b, "callee")],
        chains: HashMap::from([(a, vec![a, b])]),
        calls: vec![],
    }])
    .await
    .unwrap();
    drop(idx);
    let api = GraphApi::new_with_index(Arc::new(SharedGraphIndex::open(Some(dsn)).await.unwrap()));
    let compact = dispatch_with_api(
        &api,
        root,
        DetailLevel::Medium,
        OutputStyle::Minimal,
        false,
        "codegraph_symbol",
        json!({"id": a, "format": "minimal"}),
    )
    .await
    .unwrap();
    let compact: Value = serde_json::from_str(&compact).unwrap();
    assert_eq!(compact.as_array().unwrap().len(), 6);

    let context = dispatch_with_api(
        &api,
        root,
        DetailLevel::Minimal,
        OutputStyle::Minimal,
        false,
        "codegraph_context",
        json!({"query":"caller","depth":1}),
    )
    .await
    .unwrap();
    let context = format_response(
        root.as_str(),
        &context,
        DetailLevel::Minimal,
        OutputStyle::Minimal,
    )
    .unwrap();
    let context: Value = serde_json::from_str(&context).unwrap();
    assert_eq!(context["hits"][0]["symbol"].as_array().unwrap().len(), 5);
    assert_eq!(context["hits"][0]["callees"][0][0], b);

    let call = |args| {
        dispatch_with_api(
            &api,
            root,
            DetailLevel::Minimal,
            OutputStyle::Medium,
            false,
            "codegraph_callers",
            args,
        )
    };
    let err = call(
        json!({"node": b, "depth": 2, "timeout_ms": codegraph_api::TIMEOUT_EXPIRE_IMMEDIATELY}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("timed out"));
    let token = err
        .split("\"resume\": \"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let resumed = call(json!({"node": b, "depth": 2, "timeout_ms": 0, "resume": token}))
        .await
        .unwrap();
    let normal = call(json!({"node": b, "depth": 2})).await.unwrap();
    assert_eq!(resumed, normal);
    let value: Value = serde_json::from_str(&resumed).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 1);
    assert_eq!(value[0]["id"], a);
    assert!(call(json!({"node": b, "depth": 2, "resume": token}))
        .await
        .is_err());
}
