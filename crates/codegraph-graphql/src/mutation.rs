//! Mutation resolvers — lifecycle session (init/deinit/index) + 4 heavy tools
//! (sandbox/diff/diffSimulate/originSimulate) nhận `args: JSON`, trả `JSON`
//! string (output phức tạp, ít dùng cho UI; passthrough qua `serde_json::Value`).
//! + Document ingest/search/hydrate/list/stats.

use async_graphql::{Context, Object, Result as GqlResult};
use camino::Utf8PathBuf;
use codegraph_api::session::{DetailLevel, OutputStyle};
use codegraph_api::tools;
use codegraph_docs::{DocConfig, DocumentGraph};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;

use crate::AppState;

pub struct Mutation;

#[Object]
impl Mutation {
    /// Bind session vào một workspace root: tạo `.codegraph/` + config, index
    /// CHỈ khi `index = true` (mặc định false — bind nhanh, không block). Sau
    /// đó mới gọi được các query đọc. `detail` = minimal/medium/verbose;
    /// `format` = minimize/medium (không set → giữ seed từ CLI).
    async fn init(
        &self,
        ctx: &Context<'_>,
        path: String,
        index: Option<bool>,
        detail: Option<String>,
        format: Option<String>,
    ) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let root = Utf8PathBuf::from(path);
        let do_index = index.unwrap_or(false);
        let detail = detail
            .as_deref()
            .and_then(DetailLevel::parse)
            .unwrap_or(DetailLevel::Medium);
        let format = format.as_deref().and_then(OutputStyle::parse);
        let outcome = state
            .session
            .init(root, do_index, detail, format)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let v = json!({
            "root": outcome.root,
            "dir": outcome.dir,
            "indexed": outcome.indexed.map(|s| json!({
                "files": s.files,
                "symbols": s.symbols,
                "chains": s.chains,
                "calls": s.calls,
                "skipped": s.skipped,
            })),
        });
        Ok(serde_json::to_string_pretty(&v)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?)
    }

    /// Nhả session (`.codegraph/` + index để nguyên trên đĩa).
    async fn deinit(&self, ctx: &Context<'_>) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let prev = state
            .session
            .deinit()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(serde_json::to_string_pretty(&json!({
            "deinitialized": true,
            "previous_root": prev,
        }))
        .map_err(|e| async_graphql::Error::new(e.to_string()))?)
    }

    /// Full re-index của session hiện tại (chỉ khi đã init).
    async fn index(&self, ctx: &Context<'_>) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let stats = state
            .session
            .reindex()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(
            serde_json::to_string_pretty(&codegraph_api::session::stats_json(&stats))
                .map_err(|e| async_graphql::Error::new(e.to_string()))?,
        )
    }

    /// Sandbox một flow function (compile + run với Rhai mocks).
    /// `args: JSON` = `{ node?, name?, args?: [i64], mocks?: {callee: rhai}, branchPolicy?, loopCap? }`.
    async fn sandbox(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = state
            .session
            .ensure_ready()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let root = state
            .session
            .root()
            .await
            .ok_or_else(|| async_graphql::Error::new("session root unavailable"))?;
        tools::dispatch_sandbox(&root, sgi, args)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Diff → draft report (symbols/flows chạm vào unified diff).
    /// `args: JSON` = `{ diff: "...", entry?, baseRef?, ... }`.
    async fn diff(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = state
            .session
            .ensure_ready()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let root = state
            .session
            .root()
            .await
            .ok_or_else(|| async_graphql::Error::new("session root unavailable"))?;
        tools::dispatch_diff(&root, sgi, args)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Diff → simulate: so sánh trace sandbox trước/sau MR. `args: JSON` =
    /// `{ diff, entry?, baseRef?, args?, mocks?, branchPolicy?, loopCap? }`.
    async fn diff_simulate(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = state
            .session
            .ensure_ready()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let root = state
            .session
            .root()
            .await
            .ok_or_else(|| async_graphql::Error::new("session root unavailable"))?;
        tools::dispatch_diff_simulate(&root, sgi, args)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Ref → simulate: so sánh trace trên `git archive <ref>` vs working tree.
    /// `args: JSON` = `{ entry, ref?, args?, mocks?, branchPolicy?, loopCap? }`.
    async fn origin_simulate(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = state
            .session
            .ensure_ready()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let root = state
            .session
            .root()
            .await
            .ok_or_else(|| async_graphql::Error::new("session root unavailable"))?;
        tools::dispatch_origin_simulate(&root, sgi, args)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    // ── Document mutations ──

    /// Ingest a document file into the document graph.
    async fn doc_ingest(
        &self,
        ctx: &Context<'_>,
        path: String,
        format: Option<String>,
    ) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let source =
            std::fs::read_to_string(&path).map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let ext = std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let fmt: String = match format {
            Some(f) => f,
            None => match ext.as_str() {
                "tf" | "hcl" => "hcl".to_string(),
                "yaml" | "yml" => "yaml".to_string(),
                "json" => "json".to_string(),
                "toml" => "toml".to_string(),
                _ => {
                    return Err(async_graphql::Error::new(format!(
                        "unknown format for extension .{ext}"
                    )))
                }
            },
        };
        let _doc_graph = state.doc_graph.clone();
        let parser: Box<dyn codegraph_docs::DocParser> = match fmt.as_str() {
            "hcl" => Box::new(codegraph_docs::parsers::HclParser),
            "yaml" => Box::new(codegraph_docs::parsers::YamlParser),
            "json" => Box::new(codegraph_docs::parsers::JsonParser),
            "toml" => Box::new(codegraph_docs::parsers::TomlParser),
            _ => {
                return Err(async_graphql::Error::new(format!(
                    "unsupported format: {fmt}"
                )))
            }
        };
        let storage: Arc<TokioRwLock<dyn codegraph_graph::Storage>> =
            Arc::new(TokioRwLock::new(codegraph_graph::InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage, DocConfig::default());
        let doc_id = 1; // doc in-memory per-mutation — id không quan trọng
        let doc = parser
            .parse(&path, &source, doc_id)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let inserted = graph
            .upsert_document(doc)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(format!("ingested {path} → doc_id={inserted}"))
    }

    /// Search document nodes.
    async fn doc_search(
        &self,
        ctx: &Context<'_>,
        _pattern: String,
        depth: Option<i32>,
    ) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let depth = depth.unwrap_or(1).max(1) as usize;
        let ids = state
            .doc_graph
            .read()
            .await
            .search_path(&[codegraph_docs::tokenize::DocToken::root()], Some(depth))
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let mut results = Vec::new();
        for id in &ids {
            if let Some(payload) = state.doc_graph.read().await.hydrate(*id).await {
                results.push(json!({ "id": payload.id, "path": payload.path, "kind": format!("{:?}", payload.kind) }));
            }
        }
        Ok(serde_json::to_string_pretty(&results)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?)
    }

    /// Get document stats.
    async fn doc_stats(&self, ctx: &Context<'_>) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let stats = state
            .doc_graph
            .read()
            .await
            .stats()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(format!("documents: {}\nnodes: {}", stats.docs, stats.nodes))
    }
}
