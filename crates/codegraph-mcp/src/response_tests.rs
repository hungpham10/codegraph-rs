use super::*;

fn text(output: ToolOutput) -> String {
    match output {
        ToolOutput::Text { text, .. } => text,
        ToolOutput::Error(error) => panic!("{error}"),
    }
}

fn symbol() -> Value {
    json!({"id":100,"name":"example","kind":"function","scope":"global",
        "scope_id":0,"type_ref":0,"type_name":null,"file":"/repo/a.rs","line":2,
        "end_line":9,"signature":"fn example()","doc":"Long documentation",
        "annotations":[],"language":"rust"})
}

#[test]
fn nested_symbols_respect_all_detail_and_format_combinations() {
    for (detail, size) in [
        (DetailLevel::Minimal, 5),
        (DetailLevel::Medium, 6),
        (DetailLevel::Verbose, 14),
    ] {
        for style in [OutputStyle::Minimize, OutputStyle::Medium] {
            let input = json!({"symbol":symbol(),"matches":[symbol()],"source":"fn example() {\n    false\n}"});
            let output =
                tools::format_response("/repo", &input.to_string(), detail, style).unwrap();
            let result: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(result["source"], input["source"]);
            if style == OutputStyle::Minimize {
                assert!(!output.contains('\n'));
                assert_eq!(result["symbol"].as_array().unwrap().len(), size);
                assert_eq!(result["matches"][0], result["symbol"]);
                assert_eq!(result["symbol"][if size == 14 { 7 } else { 3 }], "a.rs");
            } else {
                assert_eq!(result["symbol"]["file"], "a.rs");
                assert_eq!(
                    result["symbol"].get("doc").is_some(),
                    detail == DetailLevel::Verbose
                );
                assert_eq!(
                    result["symbol"].get("signature").is_some(),
                    detail != DetailLevel::Minimal
                );
            }
        }
    }
}

#[test]
fn repeated_records_are_smaller_and_decodable() {
    let records: Vec<_> = (0..30)
        .map(|i| json!({"path":"/repo/a.rs","language":"rust","bytes":i,"lines":0}))
        .collect();
    let input = json!({"files":records,"total":0,"resume":"cursor-1","chain":[0,100,101]});
    let compact = tools::format_response(
        "/repo",
        &input.to_string(),
        DetailLevel::Minimal,
        OutputStyle::Minimize,
    )
    .unwrap();
    let medium = tools::format_response(
        "/repo",
        &input.to_string(),
        DetailLevel::Minimal,
        OutputStyle::Medium,
    )
    .unwrap();
    let result: Value = serde_json::from_str(&compact).unwrap();
    assert_eq!(
        result["files"]["columns"],
        json!(["bytes", "language", "lines", "path"])
    );
    assert_eq!(result["files"]["rows"][0], json!([0, "rust", 0, "a.rs"]));
    assert_eq!(result["total"], 0);
    assert_eq!(result["resume"], "cursor-1");
    assert_eq!(result["chain"], input["chain"]);
    assert!(compact.len() < medium.len() / 2);
    println!(
        "Record fixture: compact={} bytes, medium={} bytes",
        compact.len(),
        medium.len()
    );
}

#[test]
fn api_emitted_payloads_keep_sentinels_through_the_formatter() {
    // Path thật: codegraph-api tools (diff/sandbox) → emit_value → MCP formatter.
    let full = json!({"symbols":[symbol()]});
    let raw = codegraph_api::tools::emit_value("/repo", full).unwrap();
    let out =
        tools::format_response("/repo", &raw, DetailLevel::Verbose, OutputStyle::Minimize).unwrap();
    let result: Value = serde_json::from_str(&out).unwrap();
    // Minimize dựng mảng 14 cell từ object-symbol (thứ tự theo symbol_json);
    // sentinel phải là số 0 / [] gốc, không phải null do prune xảy ra trước.
    let cells = result["symbols"][0].as_array().unwrap();
    assert_eq!(cells.len(), 14);
    assert_eq!(cells[7], "a.rs");
    assert_eq!(cells[4], json!(0));
    assert_eq!(cells[5], json!(0));
    assert_eq!(cells[9], json!(9));
    assert_eq!(cells[12], json!([]));
}

#[test]
fn document_values_and_annotation_args_are_preserved() {
    for value in [
        json!(false),
        json!(null),
        json!(""),
        json!([]),
        json!({"file":"/repo/literal","enabled":false}),
    ] {
        let input =
            json!({"value":value,"args":{"enabled":false,"empty":""},"path":"/repo/a.json"});
        for style in [OutputStyle::Minimize, OutputStyle::Medium] {
            let output =
                tools::format_response("/repo", &input.to_string(), DetailLevel::Minimal, style)
                    .unwrap();
            let result: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(result["value"], input["value"]);
            assert_eq!(result["args"], input["args"]);
            assert_eq!(result["path"], "a.json");
        }
    }
}

#[test]
fn every_registered_tool_advertises_output_controls() {
    let tools = tools::rmcp_tools();
    assert_eq!(tools.len(), 40);
    for tool in tools {
        let props = &tool.input_schema["properties"];
        assert!(props.get("detail").is_some(), "{}", tool.name);
        let key = if tool.name == "codegraph_graphdoc_ingest" {
            "output_format"
        } else {
            "format"
        };
        assert_eq!(
            props[key]["enum"],
            json!(["minimize", "medium"]),
            "{}",
            tool.name
        );
        if tool.name == "codegraph_graphdoc_ingest" {
            assert_eq!(
                props["format"]["enum"],
                json!(["hcl", "yaml", "json", "toml"])
            );
        }
    }
}

#[tokio::test]
async fn server_routes_share_formatting_and_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let server = CodegraphServer::new();
    let init = text(
        server
            .run_tool(
                "codegraph_init",
                json!({"path":dir.path(),"index":false,"detail":"minimal","format":"minimize"}),
            )
            .await
            .unwrap(),
    );
    assert!(!init.contains('\n'));
    assert_eq!(server.session.detail().await, DetailLevel::Minimal);
    for name in [
        "codegraph_status",
        "codegraph_graphcode_stats",
        "codegraph_graphdoc_stats",
        "codegraph_query_usage_report",
    ] {
        let compact = text(server.run_tool(name, json!({})).await.unwrap());
        assert!(!compact.contains('\n'), "{name}: {compact}");
        serde_json::from_str::<Value>(&compact).unwrap();
        let medium = text(
            server
                .run_tool(name, json!({"format":"medium"}))
                .await
                .unwrap(),
        );
        assert!(medium.contains('\n'), "{name}: {medium}");
    }
    let context = text(
        server
            .run_tool("codegraph_context", json!({"query":"missing"}))
            .await
            .unwrap(),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&context).unwrap()["query"],
        "missing"
    );
    let path = server.session.root().await.unwrap().join("data.json");
    std::fs::write(&path, r#"{"enabled":false,"empty":""}"#).unwrap();
    let ingest = text(
        server
            .run_tool(
                "codegraph_graphdoc_ingest",
                json!({"path":path,"format":"json","output_format":"medium"}),
            )
            .await
            .unwrap(),
    );
    assert!(ingest.contains('\n'));
    let ingest: Value = serde_json::from_str(&ingest).unwrap();
    assert_eq!(ingest["path"], "data.json");
    let listed = text(
        server
            .run_tool("codegraph_graphdoc_list", json!({}))
            .await
            .unwrap(),
    );
    assert!(!listed.contains('\n'));
    let search = text(
        server
            .run_tool("codegraph_graphdoc_search", json!({"pattern":"enabled"}))
            .await
            .unwrap(),
    );
    assert!(search.contains("false"), "{search}");
    let removed = text(
        server
            .run_tool(
                "codegraph_graphdoc_remove",
                json!({"doc_id":ingest["doc_id"]}),
            )
            .await
            .unwrap(),
    );
    assert!(!removed.contains('\n'));
    let deinit = text(
        server
            .run_tool("codegraph_deinit", json!({}))
            .await
            .unwrap(),
    );
    assert!(!deinit.contains('\n'));
    assert!(matches!(
        server
            .run_tool("codegraph_graphcode_stats", json!({}))
            .await
            .unwrap(),
        ToolOutput::Error(_)
    ));
}
