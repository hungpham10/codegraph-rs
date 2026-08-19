//! Mutation resolvers — lifecycle session (init/deinit/index) + 4 heavy tools
//! (sandbox/diff/diffSimulate/originSimulate) nhận `args: JSON`, trả `JSON`
//! string (output phức tạp, ít dùng cho UI; passthrough qua `serde_json::Value`).

use async_graphql::{Context, Object, Result as GqlResult};
use camino::Utf8PathBuf;
use codegraph_api::session::{DetailLevel, OutputStyle};
use codegraph_api::tools;
use serde_json::{json, Value};
use std::sync::Arc;

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
}
