//! Mutation resolvers — lifecycle session (init/deinit/index) + 4 heavy tools
//! (sandbox/diff/diffSimulate/originSimulate) nhận `args: JSON`, trả `JSON`
//! string (output phức tạp, ít dùng cho UI; passthrough qua `serde_json::Value`).
//! + Document ingest/search/hydrate/list/stats.

use async_graphql::{Context, Object, Result as GqlResult, ID};
use camino::Utf8PathBuf;
use codegraph_api::session::{DetailLevel, OutputStyle};
use codegraph_api::tools;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::types::*;
use crate::AppState;

pub struct Mutation;

#[Object]
impl Mutation {
    /// Bind session vào một workspace root: tạo `.codegraph/` + config, index
    /// CHỈ khi `index = true` (mặc định false — bind nhanh, không block). Sau
    /// đó mới gọi được các query đọc. `detail` = minimal/medium/verbose;
    /// `format` = minimal/medium (không set → giữ seed từ CLI).
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
            .init(root.clone(), do_index, detail, format)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        // Re-point doc graph sang root mới (lazy open lần doc op tiếp theo).
        state.doc_graph.reinit(root);
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
    async fn graphcode_sandbox(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = crate::query::fresh_index(ctx).await?;
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
    async fn graphcode_diff(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = crate::query::fresh_index(ctx).await?;
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
    async fn graphcode_diff_simulate(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = crate::query::fresh_index(ctx).await?;
        let root = state
            .session
            .root()
            .await
            .ok_or_else(|| async_graphql::Error::new("session root unavailable"))?;
        let git = codegraph_source::DiskGit::new(root.clone());
        tools::dispatch_diff_simulate(&root, sgi, &git, args)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Ref → simulate: so sánh trace trên `git export <ref>` vs working tree.
    /// `args: JSON` = `{ entry, ref?, args?, mocks?, branchPolicy?, loopCap? }`.
    async fn graphcode_origin_simulate(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = crate::query::fresh_index(ctx).await?;
        let root = state
            .session
            .root()
            .await
            .ok_or_else(|| async_graphql::Error::new("session root unavailable"))?;
        let git = codegraph_source::DiskGit::new(root.clone());
        tools::dispatch_origin_simulate(&root, sgi, &git, args)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Branch compare: diff giữa `base` và `head` + báo cáo tác động + (tuỳ
    /// chọn) mermaid flow 2 màu cho `entry`. `args: JSON` = `{ base, head?, entry? }`.
    async fn graphcode_branch_compare(&self, ctx: &Context<'_>, args: Value) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = crate::query::fresh_index(ctx).await?;
        let root = state
            .session
            .root()
            .await
            .ok_or_else(|| async_graphql::Error::new("session root unavailable"))?;
        let git = codegraph_source::DiskGit::new(root.clone());
        tools::dispatch_branch_compare(&root, sgi, &git, args)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    // ── Document mutations ──

    /// Ingest một file document (YAML/JSON/TOML/HCL) vào doc graph persist.
    /// Trả doc_id (u64 dạng string).
    async fn graphdoc_ingest(
        &self,
        ctx: &Context<'_>,
        path: String,
        format: Option<String>,
    ) -> GqlResult<ID> {
        let state = ctx.data::<Arc<AppState>>()?;
        let graph = state.doc_graph.graph().await;
        let mut graph = graph.write().await;
        let doc_id = graph
            .ingest_file(&path, format.as_deref())
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(ID::from(doc_id))
    }

    /// Ingest hàng loạt mọi file document trong thư mục (đệ quy, theo extension).
    async fn graphdoc_ingest_dir(
        &self,
        ctx: &Context<'_>,
        path: String,
        limit: Option<i32>,
    ) -> GqlResult<DocIngestSummary> {
        let state = ctx.data::<Arc<AppState>>()?;
        let limit = limit.unwrap_or(500).max(1) as usize;
        const EXTS: [&str; 6] = ["yaml", "yml", "json", "toml", "tf", "hcl"];
        let mut files = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(&path)];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| EXTS.contains(&ext.to_ascii_lowercase().as_str()))
                {
                    files.push(p);
                }
            }
        }
        files.sort();
        if files.len() > limit {
            files.truncate(limit);
        }
        let total = files.len();
        let graph = state.doc_graph.graph().await;
        let mut graph = graph.write().await;
        let mut ingested = 0usize;
        let mut failed = 0usize;
        for f in &files {
            let Some(p) = f.to_str() else { continue };
            match graph.ingest_file(p, None).await {
                Ok(_) => ingested += 1,
                Err(_) => failed += 1,
            }
        }
        Ok(DocIngestSummary {
            requested: total,
            ingested,
            failed,
        })
    }

    /// Xoá một document theo id.
    async fn graphdoc_remove(&self, ctx: &Context<'_>, id: ID) -> GqlResult<bool> {
        let state = ctx.data::<Arc<AppState>>()?;
        let id = id
            .parse::<u64>()
            .map_err(|_| async_graphql::Error::new("invalid id"))?;
        let graph = state.doc_graph.graph().await;
        let mut graph = graph.write().await;
        graph
            .remove_document(id)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(true)
    }

    /// Mine structural patterns (kind chain) trên toàn bộ node lá scalar.
    async fn graphdoc_mine_patterns(
        &self,
        ctx: &Context<'_>,
        top_k: Option<i32>,
        min_count: Option<i32>,
        max_depth: Option<i32>,
    ) -> GqlResult<Vec<DocPatternView>> {
        let state = ctx.data::<Arc<AppState>>()?;
        let top_k = top_k.unwrap_or(20).max(1) as usize;
        let min_count = min_count.unwrap_or(3).max(1) as usize;
        let max_depth = max_depth.unwrap_or(4).max(1) as usize;
        let graph = state.doc_graph.graph().await;
        let mut graph = graph.write().await;
        let mined = graph
            .mine_patterns(top_k, min_count, max_depth)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(mined.into_iter().map(Into::into).collect())
    }
}
