use super::*;
use codegraph_context::{RerankInfo, Verdict};

fn row(id: u64, key: &str, doc: &str, kind: &str, value: Value) -> Value {
    json!({ "id": id, "doc": doc, "key": key, "kind": kind, "value": value })
}

fn cfg(value: bool) -> codegraph_core::JevConfig {
    codegraph_core::JevConfig {
        enabled: true,
        doc_include_values: value,
        doc_value_chars: 10,
        ..codegraph_core::JevConfig::default()
    }
}

#[test]
fn scalar_text_bóc_lớp_scalar_và_bỏ_rỗng() {
    assert_eq!(
        scalar_text(&json!({"String": "nginx"})).as_deref(),
        Some("nginx")
    );
    assert_eq!(scalar_text(&json!({"Number": 3.0})).as_deref(), Some("3.0"));
    assert_eq!(scalar_text(&json!({"Bool": true})).as_deref(), Some("true"));
    assert_eq!(scalar_text(&json!({"Null": null})), None);
    assert_eq!(scalar_text(&json!({"String": "   "})), None);
    assert_eq!(scalar_text(&json!(["x"])), None);
}

#[test]
fn doc_candidate_chỉ_có_value_khi_bật_config() {
    let r = row(
        7,
        "spec.replicas",
        "deploy/api.yaml",
        "Field",
        json!({"Number": 3.0}),
    );

    let off = doc_candidate(&r, &cfg(false)).unwrap();
    assert_eq!(off.id, 7);
    assert_eq!(off.label, "Field `spec.replicas` in `deploy/api.yaml`");
    assert_eq!(off.detail, None, "mặc định không đưa value (tốn token)");

    let on = doc_candidate(&r, &cfg(true)).unwrap();
    assert_eq!(
        on.detail.as_deref(),
        Some("3.0"),
        "value giúp chọn đúng key"
    );
}

#[test]
fn doc_candidate_dùng_path_khi_không_có_key_và_từ_chối_row_thiếu_id() {
    let r = json!({ "id": 9, "doc": "a.yaml", "path": "a.b.c", "kind": "Field" });
    assert_eq!(
        doc_candidate(&r, &cfg(true)).unwrap().label,
        "Field `a.b.c` in `a.yaml`"
    );
    assert!(doc_candidate(&json!({ "doc": "a.yaml" }), &cfg(true)).is_none());
}

#[test]
fn apply_doc_outcome_fail_open_giữ_thứ_tự_cũ() {
    let rows: Vec<(Candidate, Value)> = (1..=4)
        .map(|id| {
            let v = row(id, "k", "a.yaml", "Field", Value::Null);
            let c = doc_candidate(&v, &cfg(false)).unwrap();
            (c, v)
        })
        .collect();
    let out = apply_doc_outcome(RankOutcome::unavailable("timeout"), rows, 3);
    assert!(!out.no_match);
    assert_eq!(out.rows.len(), 3, "fail-open vẫn cắt theo limit");
    assert_eq!(out.rows[0]["id"], 1);
    assert_eq!(out.info["used"], false);
    assert_eq!(out.info["reason"], "timeout");
}

#[test]
fn apply_doc_outcome_no_match_trả_rỗng() {
    let v = row(1, "k", "a.yaml", "Field", Value::Null);
    let c = doc_candidate(&v, &cfg(false)).unwrap();
    let mut outcome = RankOutcome::unavailable("");
    outcome.used = true;
    outcome.reason = None;
    outcome.no_match = true;
    let out = apply_doc_outcome(outcome, vec![(c, v)], 10);
    assert!(out.no_match);
    assert!(out.rows.is_empty());
    assert_eq!(out.info["used"], true);
}

#[test]
fn apply_doc_outcome_sắp_theo_xác_suất_và_gắn_match_score() {
    let rows: Vec<(Candidate, Value)> = (1..=3)
        .map(|id| {
            let v = row(id, "k", "a.yaml", "Field", Value::Null);
            let c = doc_candidate(&v, &cfg(false)).unwrap();
            (c, v)
        })
        .collect();
    let mut outcome = RankOutcome::unavailable("");
    outcome.used = true;
    outcome.reason = None;
    outcome.ranking = vec![(3, 0.7), (1, 0.2)];
    let out = apply_doc_outcome(outcome, rows, 2);
    assert!(!out.no_match);
    assert_eq!(out.rows.len(), 2);
    assert_eq!(out.rows[0]["id"], 3);
    assert_eq!(out.rows[1]["id"], 1);
    assert!((out.rows[0]["match_score"]["probability"].as_f64().unwrap() - 0.7).abs() < 1e-6);
    assert!(
        out.rows[1].get("idf").is_none(),
        "không đụng field khác của row"
    );
}

#[test]
fn rerank_info_serialize_gọn() {
    let info = RerankInfo {
        used: false,
        reason: None,
    };
    assert_eq!(
        serde_json::to_value(info).unwrap(),
        json!({ "used": false }),
        "reason None bị bỏ, không serialize null"
    );
    let v = Verdict::no_match(0.0);
    assert!(v.no_match);
}

/// 3 document → pool ≥ `DOC_MIN_POOL` để đi tới nhánh gọi Jev (và fail-open vì
/// `[jev] enabled = false` trong test).
async fn seed_doc() -> Arc<crate::SharedDocGraph> {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<std::path::PathBuf> = ["api.yaml", "worker.yaml", "cache.yaml"]
        .iter()
        .map(|name| {
            let path = dir.path().join(name);
            std::fs::write(&path, "spec:\n  replicas: 3\n  image: nginx\n").unwrap();
            path
        })
        .collect();
    let shared = Arc::new(crate::SharedDocGraph::in_memory());
    {
        let graph = shared.graph().await;
        for path in &files {
            graph
                .write()
                .await
                .ingest_file(path.to_str().unwrap(), None)
                .await
                .unwrap();
        }
    }
    shared
}

#[tokio::test]
async fn dispatch_doc_search_value_giữ_array_khi_tắt_rerank() {
    let shared = seed_doc().await;
    let text = dispatch_doc_search_value(shared, "nginx", 10, Rerank::None)
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v.is_array(), "rerank=none giữ nguyên shape cũ");
    assert_eq!(v.as_array().unwrap().len(), 3, "mỗi document 1 node value");
    assert_eq!(v[0]["key"], "image");
}

#[tokio::test]
async fn dispatch_doc_search_value_bọc_object_và_fail_open_khi_jev_tắt() {
    let shared = seed_doc().await;
    let text = dispatch_doc_search_value(shared, "nginx", 10, Rerank::Jev)
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v["results"].is_array(), "rerank=jev → object có `results`");
    assert_eq!(v["no_match"], false);
    assert_eq!(v["rerank"]["used"], false, "jev tắt mặc định → fail-open");
    assert!(v["rerank"]["reason"]
        .as_str()
        .is_some_and(|r| r.contains("enabled=false")));
    assert_eq!(v["results"][0]["key"], "image", "fail-open giữ thứ tự cũ");
}

async fn empty_api(root: &Utf8Path) -> GraphApi {
    let dsn = format!("sqlite://{}", root.join("index.db"));
    GraphApi::new_with_index(Arc::new(
        codegraph_graph::SharedGraphIndex::open(Some(dsn))
            .await
            .unwrap(),
    ))
}

#[tokio::test]
async fn search_symbol_rerank_jev_chỉ_nhận_trang_đầu() {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    let api = empty_api(root).await;

    for args in [
        json!({"query": "x", "rerank": "jev", "offset": 1}),
        json!({"query": "x", "rerank": "jev", "resume": "abc"}),
    ] {
        let err = dispatch_with_api(
            &api,
            root,
            DetailLevel::Minimal,
            OutputStyle::Minimal,
            false,
            "codegraph_search_symbol",
            args,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("trang đầu"),
            "offset/resume + rerank=jev phải bị từ chối: {err}"
        );
    }
}

#[tokio::test]
async fn search_symbol_không_rerank_vẫn_phân_trang_được() {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap();
    let api = empty_api(root).await;

    let out = dispatch_with_api(
        &api,
        root,
        DetailLevel::Minimal,
        OutputStyle::Minimal,
        false,
        "codegraph_search_symbol",
        json!({"query": "x", "offset": 1}),
    )
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["offset"], 1);
    assert!(v["no_match"].is_boolean(), "shape mới có no_match/rerank");
    assert!(v["rerank"].is_null());
}
