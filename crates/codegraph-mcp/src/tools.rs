use crate::session::{DetailLevel, OutputStyle};
use camino::Utf8Path;
use codegraph_api::{GraphApi, Pagination};
use codegraph_context::{ContextRequest, Format};
use codegraph_core::{Error, Result, Symbol, SymbolKind, SymbolMatch};
use rmcp::model::Tool;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Định nghĩa một MCP tool — single source of truth cho `tools/list`.
struct ToolDef {
    name: &'static str,
    desc: &'static str,
    schema: Value,
}

fn tool(name: &'static str, desc: &'static str, schema: Value) -> ToolDef {
    ToolDef { name, desc, schema }
}

/// `tools/list` payload — chuyển mọi định nghĩa ở trên qua `rmcp::model::Tool`.
pub fn rmcp_tools() -> Vec<Tool> {
    tool_defs()
        .into_iter()
        .map(|d| Tool::new(d.name, d.desc, Arc::new(rmcp::model::object(d.schema))))
        .collect()
}

/// Tool name có tồn tại trong danh sách không — phân biệt protocol error
/// (unknown tool → `method_not_found`) với tool error (client-visible).
pub fn is_known_tool(name: &str) -> bool {
    tool_defs().iter().any(|d| d.name == name)
}

fn tool_defs() -> Vec<ToolDef> {
    vec![
        tool(
            "codegraph_symbol",
            "Look up a symbol by id or exact name. Duplicate names → ambiguous with the full match list; retry with symbol_id.",
            json!({ "type": "object", "properties": {
                "id": { "type": "integer" },
                "name": { "type": "string" },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = symbol as a fixed-order positional array (default), medium = full object with default-valued fields omitted." }
            } }),
        ),
        tool(
            "codegraph_callers",
            "Find functions that (transitively) call the given symbol.",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer" },
                "depth": { "type": "integer", "default": 1 },
                "detail": { "type": "string", "enum": ["minimal", "medium", "verbose"], "description": "Symbol detail for this call (overrides session default): minimal = id/name/kind/file/line, medium = + signature, verbose = full Symbol." },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = symbol items as fixed-order positional arrays (default), medium = objects with default-valued fields omitted." }
            }, "required": ["node"] }),
        ),
        tool(
            "codegraph_callees",
            "Find functions called directly by the given symbol.",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer" },
                "detail": { "type": "string", "enum": ["minimal", "medium", "verbose"], "description": "Symbol detail for this call (overrides session default): minimal = id/name/kind/file/line, medium = + signature, verbose = full Symbol." },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = symbol items as fixed-order positional arrays (default), medium = objects with default-valued fields omitted." }
            }, "required": ["node"] }),
        ),
        tool(
            "codegraph_impact",
            "Impact radius: who transitively depends on this symbol.",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer" },
                "max_depth": { "type": "integer", "default": 3 },
                "detail": { "type": "string", "enum": ["minimal", "medium", "verbose"], "description": "Symbol detail for this call (overrides session default): minimal = id/name/kind/file/line, medium = + signature, verbose = full Symbol." },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = symbol items as fixed-order positional arrays (default), medium = objects with default-valued fields omitted." }
            }, "required": ["node"] }),
        ),
        tool(
            "codegraph_flow",
            "Call chain of a symbol: markers (LOOP, IF_TRUE, …) + callee names + call sites with line/condition/effect.",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer" },
                "detail": { "type": "string", "enum": ["minimal", "medium", "verbose"], "description": "Detail for the embedded symbol (overrides session default): minimal = id/name/kind/file/line, medium = + signature, verbose = full Symbol." },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = symbol items as fixed-order positional arrays (default), medium = objects with default-valued fields omitted." }
            }, "required": ["node"] }),
        ),
        tool(
            "codegraph_search_flow",
            "Find functions whose call chain contains a pattern. Pattern = comma-separated tokens: numeric ids, marker names (LOOP, IF_TRUE, IF_FALSE, BRANCH_END, RETURN, LOOP_BACK, SWITCH_CASE, SWITCH_END, BREAK, CONTINUE, THROW) or symbol names. On large indexes pass timeout_ms (default 20000); if the call returns a timeout error containing \"resume\": \"<id>\", retry the SAME call with that resume id to continue.",
            json!({ "type": "object", "properties": {
                "pattern": { "type": "string" },
                "limit": { "type": "integer", "default": 20 },
                "offset": { "type": "integer", "default": 0 },
                "timeout_ms": { "type": "integer", "default": 20000, "description": "Soft time budget in ms; 0 = no limit. On timeout the tool errors with a resume id." },
                "resume": { "type": "string", "description": "Resume id from a previous timeout — retry the same call with this to continue where it stopped." }
            }, "required": ["pattern"] }),
        ),
        tool(
            "codegraph_context",
            "Composed context for a symbol or topic (search + callers + callees + optional source).",
            json!({ "type": "object", "properties": {
                "query": { "type": "string" },
                "depth": { "type": "integer", "default": 1 },
                "include_source": { "type": "boolean", "default": false },
                "limit": { "type": "integer", "default": 5 }
            }, "required": ["query"] }),
        ),
        tool(
            "codegraph_references",
            "Functions that call a library call whose name contains the query (includes unresolved external calls). On large indexes pass timeout_ms (default 20000); if the call returns a timeout error containing \"resume\": \"<id>\", retry the SAME call with that resume id to continue.",
            json!({ "type": "object", "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer", "default": 10 },
                "offset": { "type": "integer", "default": 0 },
                "timeout_ms": { "type": "integer", "default": 20000, "description": "Soft time budget in ms; 0 = no limit. On timeout the tool errors with a resume id." },
                "resume": { "type": "string", "description": "Resume id from a previous timeout — retry the same call with this to continue where it stopped." }
            }, "required": ["query"] }),
        ),
        tool(
            "codegraph_files",
            "List indexed files under a path prefix.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        ),
        tool(
            "codegraph_status",
            "Index health: symbol / chain / edge / file counts.",
            json!({ "type": "object", "properties": {} }),
        ),
        // ── Admin tools (init / deinit / index) — thao tác trên session slot ──
        tool(
            "codegraph_init",
            "Bind this MCP session to a workspace root (idempotent): creates .codegraph/ with .gitignore, version, and config.toml. Pass path (absolute workspace root) to select the directory for this session. index defaults to false — binding is quick and non-blocking (it does NOT index); call codegraph_index {} afterwards (or pass index=true here) only when you need a fresh index to query. detail sets the default symbol detail for list-tool responses (default medium). Re-running with a different path re-points the session.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "Absolute path of the workspace root to bind this session to." },
                "index": { "type": "boolean", "default": false },
                "detail": { "type": "string", "enum": ["minimal", "medium", "verbose"], "default": "medium", "description": "Default symbol detail for list-tool responses: minimal = id/name/kind/file/line (fewest tokens), medium = + signature, verbose = full Symbol (doc, annotations, ...). Per-call detail overrides this." },
                "format": { "type": "string", "enum": ["minimize", "medium"], "default": "minimize", "description": "Output format for every response: minimize = symbol items as fixed-order positional arrays (default), medium = objects with default-valued fields omitted. Per-call format overrides this." }
            }, "required": ["path"] }),
        ),
        tool(
            "codegraph_deinit",
            "Release this MCP session: unbind the current workspace root (root becomes null). The .codegraph/ directory and index stay on disk — call codegraph_init with a path again to re-bind. Every query tool refuses to run while the session is unbound.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "codegraph_index",
            "Full re-index of the workspace into .codegraph/db.sqlite. Requires the workspace to be initialized (run codegraph_init first).",
            json!({ "type": "object", "properties": {} }),
        ),
        // ── Enhanced symbol search (semgraph_search_symbol) ──
        tool(
            "codegraph_search_symbol",
            "Search symbols by name with optional kind filter, match mode, and pagination. match: 'contains' (substring anywhere, default), 'prefix' (name starts with), 'suffix' (name ENDS with — e.g. query=\"Service\" finds every *Service class), 'exact' (exact name, case-insensitive), 'semantic' (vector KNN over symbol embeddings — find symbols by similar/approximate names when you don't remember the exact spelling), 'hybrid' (merge 'contains' + 'semantic' via Reciprocal Rank Fusion). Use 'total' with 'offset' to fetch further pages until offset >= total. On large indexes pass timeout_ms (default 20000); if the call returns a timeout error containing \"resume\": \"<id>\", retry the SAME call with that resume id to continue. When more results remain, the response includes a resume id you can pass to page further without re-scanning.",
            json!({ "type": "object", "properties": {
                "query": { "type": "string" },
                "kind": { "type": "string", "enum": ["function", "method", "class", "interface", "enum", "variable", "constant", "parameter", "field", "module", "file"] },
                "match": { "type": "string", "enum": ["contains", "prefix", "suffix", "exact", "semantic", "hybrid"], "default": "contains" },
                "limit": { "type": "integer", "default": 20 },
                "offset": { "type": "integer", "default": 0 },
                "resume": { "type": "string", "description": "Resume id from a previous timeout (or from a previous response with more pages) — retry the same call with this to continue where it stopped." },
                "timeout_ms": { "type": "integer", "default": 20000, "description": "Soft time budget in ms; 0 = no limit. On timeout the tool errors with a resume id." },
                "detail": { "type": "string", "enum": ["minimal", "medium", "verbose"], "description": "Symbol detail for this call (overrides session default): minimal = id/name/kind/file/line, medium = + signature, verbose = full Symbol." },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = symbol items as fixed-order positional arrays (default), medium = objects with default-valued fields omitted." }
            }, "required": ["query"] }),
        ),
        // ── Class queries (codegraph_class / codegraph_list_types) ──
        tool(
            "codegraph_class",
            "Get class/interface/enum details with fields and methods as separate lists.",
            json!({ "type": "object", "properties": {
                "class_name": { "type": "string" },
                "id": { "type": "integer" },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = embedded class symbol as a fixed-order positional array (default), medium = objects with default-valued fields omitted." }
            } }),
        ),
        tool(
            "codegraph_list_types",
            "List all class/interface/enum symbols in the index (paginated). `kind` selects which: 'class', 'interface', or 'enum'. On large indexes pass timeout_ms (default 20000); if the call returns a timeout error containing \"resume\": \"<id>\", retry the SAME call with that resume id to continue.",
            json!({ "type": "object", "properties": {
                "kind": { "type": "string", "enum": ["class", "interface", "enum"], "default": "class", "description": "Which type symbols to list: class, interface, or enum." },
                "limit": { "type": "integer", "default": 20 },
                "offset": { "type": "integer", "default": 0 },
                "detail": { "type": "string", "enum": ["minimal", "medium", "verbose"], "description": "Symbol detail for this call (overrides session default): minimal = id/name/kind/file/line, medium = + signature, verbose = full Symbol." },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = symbol items as fixed-order positional arrays (default), medium = objects with default-valued fields omitted." },
                "timeout_ms": { "type": "integer", "default": 20000, "description": "Soft time budget in ms; 0 = no limit. On timeout the tool errors with a resume id." },
                "resume": { "type": "string", "description": "Resume id from a previous timeout — retry the same call with this to continue where it stopped." }
            } }),
        ),
        tool(
            "codegraph_function_scope",
            "Get a function's parameters and local variables. Disambiguate duplicate function names with 'id' from codegraph_search (pass 'id' alone).",
            json!({ "type": "object", "properties": {
                "func_name": { "type": "string" },
                "id": { "type": "integer" },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = function/params/locals as fixed-order positional arrays (default), medium = objects with default-valued fields omitted." }
            } }),
        ),
        // ── Annotation / call / dependency queries ──
        tool(
            "codegraph_search_by_annotation",
            "Search symbols by annotation (e.g. @RestController, @GetMapping, @Autowired, @Override). Case-insensitive substring match. Optional kind filter. On large indexes pass timeout_ms (default 20000); if the call returns a timeout error containing \"resume\": \"<id>\", retry the SAME call with that resume id to continue.",
            json!({ "type": "object", "properties": {
                "annotation": { "type": "string" },
                "kind": { "type": "string", "enum": ["function", "method", "class", "interface", "enum", "variable", "constant", "parameter", "field", "module", "file"] },
                "limit": { "type": "integer", "default": 20 },
                "offset": { "type": "integer", "default": 0 },
                "detail": { "type": "string", "enum": ["minimal", "medium", "verbose"], "description": "Symbol detail for this call (overrides session default): minimal = id/name/kind/file/line, medium = + signature, verbose = full Symbol." },
                "format": { "type": "string", "enum": ["minimize", "medium"], "description": "Output format for this call (overrides session default): minimize = symbol items as fixed-order positional arrays (default), medium = objects with default-valued fields omitted." },
                "timeout_ms": { "type": "integer", "default": 20000, "description": "Soft time budget in ms; 0 = no limit. On timeout the tool errors with a resume id." },
                "resume": { "type": "string", "description": "Resume id from a previous timeout — retry the same call with this to continue where it stopped." }
            }, "required": ["annotation"] }),
        ),
        tool(
            "codegraph_dependencies",
            "List dependencies (module prefixes) derived from indexed call names: internal (modules that resolve to in-repo symbols) vs external (e.g. fmt, requests, java.util). Sorted by call-site count.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "codegraph_query_usage_report",
            "MCP tool-usage telemetry: total calls/errors, answer_bytes (JSON returned to the LLM), and estimated source_bytes (bytes of source files the answers reference, i.e. code-reading avoided). Per-tool aggregates sorted by call count. Pass reset=true to clear accumulated stats.",
            json!({ "type": "object", "properties": {
                "limit": { "type": "integer", "default": 0 },
                "reset": { "type": "boolean", "default": false }
            } }),
        ),
        // ── Behavior sandbox (compile a flow to machine code + run with mocks) ──
        tool(
            "codegraph_sandbox",
            "Run a sandbox simulation of a function's flow: compile the entry function + its in-flow callees into machine code (Cranelift JIT) and run it with Rhai mocks. `mocks` maps a callee name to a Rhai body (auto-wrapped into `fn <name>(args) { … }` where `args` is the call's i64 array) or a full `fn <name>(args) { … }` script; inline mocks override `[sandbox].mock_dirs` files. Before compiling, every callee that will be mock-dispatched must have a mock (file or `mocks`); if any is unconfigured the call fails with `link failed: no mock configured for callee(s): …`. Returns the entry return value, the ordered mock invocations, control-flow decisions (if/loop/switch taken/skipped), and any callees that still ran without a mock (`missing_mocks`).",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer", "description": "Entry function symbol id (from codegraph_search / codegraph_flow)." },
                "name": { "type": "string", "description": "Entry function name (substring → first function match); used when node is omitted." },
                "args": { "type": "array", "items": { "type": "integer" }, "description": "Abstract i64 arguments passed to the entry function." },
                "mocks": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Callee name → Rhai mock body or full `fn` source." },
                "branch_policy": { "type": "string", "enum": ["if_true", "if_false"], "description": "Override config branch_policy (default from config.toml)." },
                "loop_cap": { "type": "integer", "description": "Override config loop_cap — max loop iterations, guarantees termination." }
            } }),
        ),
        // ── Diff draft (unified diff → graph impact, read-only) ──
        tool(
            "codegraph_diff",
            "Analyze a unified diff (MR / patch file / `git diff` output) against the indexed graph and produce a DRAFT report of what would change in codegraph-graph: which symbols (functions/methods/classes) are touched (by line overlap), which flows contain call sites on changed lines, the control-flow marker window around each affected call (IF_TRUE/LOOP/BRANCH_END…), and which flows call the touched functions. The index itself is NOT mutated — this is a dry-run assessment you can review before applying the diff.",
            json!({ "type": "object", "properties": {
                "diff": { "type": "string", "description": "Unified diff text: `git diff` output, a .patch file content, or the diff from an MR. Supports multi-file diffs, added/removed/renamed files, and `\\ No newline at end of file`." }
            }, "required": ["diff"] }),
        ),
        tool(
            "codegraph_diff_simulate",
            "Diff → behavior simulation (draft): take a unified diff, find the functions it touches, then run the sboxes sandbox on the entry flow BOTH on the current index (post-MR) and on a temporary index built from a git ref (`base_ref`, default HEAD = pre-MR), and compare the observed traces (ordered mock calls, condition decisions). The sandbox follows flow STRUCTURE: branch decisions follow `branch_policy` (if_true/if_false, it does not read the guard text), loops run up to `loop_cap`, and mock call order reflects the flow — numeric arithmetic on values is NOT modeled. Requires the workspace to be a git repo (pre-MR tree comes from `git archive`) and the entry flow to be sandbox-friendly (primitive args, library callees mocked via `mocks`). Read-only — the index is never mutated.",
            json!({ "type": "object", "properties": {
                "diff": { "type": "string", "description": "Unified diff text (MR / patch / git diff)." },
                "entry": { "type": "string", "description": "Optional entry function name (substring). Default: first function affected by the diff." },
                "base_ref": { "type": "string", "description": "Git ref for the BEFORE state (default HEAD)." },
                "args": { "type": "array", "items": { "type": "integer" }, "description": "Abstract i64 arguments passed to the entry function." },
                "mocks": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Callee name → Rhai mock body/fn." },
                "branch_policy": { "type": "string", "enum": ["if_true", "if_false"], "description": "Override config branch_policy." },
                "loop_cap": { "type": "integer", "description": "Override config loop_cap." }
            }, "required": ["diff"] }),
        ),
        tool(
            "codegraph_origin_simulate",
            "Ref vs working tree simulation (draft): run the sboxes sandbox on an entry flow at a git ref (default HEAD, e.g. `origin/main`) — a temporary index built from `git archive <ref>` — AND on the current index (working tree), then compare the observed traces (ordered mock calls, condition decisions). No diff needed: you pick any entry function and immediately see whether local uncommitted edits change its flow's behavior. The sandbox follows flow STRUCTURE: branch decisions follow `branch_policy` (if_true/if_false, guard text is not read), loops run up to `loop_cap`, mock call order reflects the flow — numeric arithmetic on values is NOT modeled. Entry is resolved by NAME in each index (symbol ids differ between ref and working tree). Requires a git repo. Read-only — the index is never mutated.",
            json!({ "type": "object", "properties": {
                "entry": { "type": "string", "description": "Entry function name (substring → first function match in each index)." },
                "ref": { "type": "string", "description": "Git ref for the ORIGIN state (default HEAD). Example: origin/main." },
                "args": { "type": "array", "items": { "type": "integer" }, "description": "Abstract i64 arguments passed to the entry function." },
                "mocks": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Callee name → Rhai mock body/fn." },
                "branch_policy": { "type": "string", "enum": ["if_true", "if_false"], "description": "Override config branch_policy." },
                "loop_cap": { "type": "integer", "description": "Override config loop_cap." }
            }, "required": ["entry"] }),
        ),
    ]
}

pub async fn dispatch_with_api(
    api: &GraphApi,
    root: &Utf8Path,
    session_detail: DetailLevel,
    session_format: OutputStyle,
    name: &str,
    args: Value,
) -> Result<String> {
    match name {
        "codegraph_symbol" => {
            let detail = detail_from_args(&args, session_detail);
            let format = format_from_args(&args, session_format);
            if let Some(id) = args.get("id").and_then(|v| v.as_u64()) {
                let s = api.symbol_by_id(id).await;
                return match s {
                    Some(s) => emit_value(
                        root.as_str(),
                        symbol_json(root.as_str(), &s, detail, format),
                    ),
                    None => emit_value(root.as_str(), Value::Null),
                };
            }
            if let Some(name) = args.get("name").and_then(|v| v.as_str()) {
                let r = api.resolve(name, 0).await?;
                if r.ambiguous {
                    // Trùng tên — trả matches để LLM retry với symbol_id.
                    let matches: Vec<Value> = r
                        .matches
                        .iter()
                        .map(|s| symbol_json(root.as_str(), s, detail, format))
                        .collect();
                    return Ok(format!(
                        "ambiguous ({} matches):\n{}",
                        matches.len(),
                        emit_value(root.as_str(), Value::Array(matches))?
                    ));
                }
                return match r.symbol {
                    Some(s) => emit_value(
                        root.as_str(),
                        symbol_json(root.as_str(), &s, detail, format),
                    ),
                    None => emit_value(root.as_str(), Value::Null),
                };
            }
            Err(Error::Invalid("provide id or name".into()))
        }
        "codegraph_callers" => {
            let id = arg_u64(&args, "node")?;
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let hits = api.callers(id, depth).await?;
            let detail = detail_from_args(&args, session_detail);
            let format = format_from_args(&args, session_format);
            let out: Vec<Value> = hits
                .iter()
                .map(|s| symbol_json(root.as_str(), s, detail, format))
                .collect();
            emit_value(root.as_str(), Value::Array(out))
        }
        "codegraph_callees" => {
            let id = arg_u64(&args, "node")?;
            let hits = api.callees(id).await?;
            let detail = detail_from_args(&args, session_detail);
            let format = format_from_args(&args, session_format);
            let out: Vec<Value> = hits
                .iter()
                .map(|s| symbol_json(root.as_str(), s, detail, format))
                .collect();
            emit_value(root.as_str(), Value::Array(out))
        }
        "codegraph_impact" => {
            let id = arg_u64(&args, "node")?;
            let depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
            let hits = api.impact(id, depth).await?;
            let detail = detail_from_args(&args, session_detail);
            let format = format_from_args(&args, session_format);
            let out: Vec<Value> = hits
                .iter()
                .map(|s| symbol_json(root.as_str(), s, detail, format))
                .collect();
            emit_value(root.as_str(), Value::Array(out))
        }
        "codegraph_flow" => {
            let id = arg_u64(&args, "node")?;
            let flow = api.flow(id).await?;
            let detail = detail_from_args(&args, session_detail);
            let format = format_from_args(&args, session_format);
            emit_value(
                root.as_str(),
                json!({
                    "symbol": symbol_json(root.as_str(), &flow.symbol, detail, format),
                    "chain": flow.chain,
                    "chain_desc": flow.chain_desc,
                    "calls": flow.calls,
                }),
            )
        }
        "codegraph_search_flow" => {
            let pattern = arg_str(&args, "pattern")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let resume = args
                .get("resume")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(20000);
            let out = api
                .search_flow_pattern_resumable(
                    pattern,
                    Pagination { limit, offset },
                    resume,
                    timeout_ms,
                )
                .await?;
            if out.timed_out {
                return Err(Error::Other(format!(
                    "codegraph_search_flow timed out after {}ms (collected {} results so far). \
                     Retry the same call with the same arguments plus \"resume\": \"{}\" \
                     to continue the search from where it stopped.",
                    timeout_ms,
                    out.progress,
                    out.resume.as_deref().unwrap_or("")
                )));
            }
            emit(root.as_str(), &out.page)
        }
        "codegraph_context" => {
            let req = ContextRequest {
                query: arg_str(&args, "query")?.to_string(),
                depth: args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
                include_source: args
                    .get("include_source")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                limit: args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as u32,
                format: Format::Markdown,
                strip_prefix: Some(root.as_str().to_string()),
            };
            Ok(api.context_markdown(&req).await?)
        }
        "codegraph_references" => {
            let q = arg_str(&args, "query")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let resume = args
                .get("resume")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(20000);
            let out = api
                .references_resumable(q, Pagination { limit, offset }, resume, timeout_ms)
                .await?;
            if out.timed_out {
                return Err(Error::Other(format!(
                    "codegraph_references timed out after {}ms (collected {} results so far). \
                     Retry the same call with the same arguments plus \"resume\": \"{}\" \
                     to continue the search from where it stopped.",
                    timeout_ms,
                    out.progress,
                    out.resume.as_deref().unwrap_or("")
                )));
            }
            emit(root.as_str(), &out.page)
        }
        "codegraph_files" => {
            let prefix = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            // Index lưu path absolute; output relativize theo root. Filter khớp
            // CẢ prefix absolute (path gốc) lẫn prefix tương đối (path hiển thị).
            let files = api.files("").await;
            let files: Vec<_> = if prefix.is_empty() {
                files
            } else {
                files
                    .into_iter()
                    .filter(|f| {
                        f.path.starts_with(prefix)
                            || strip_root_prefix(&f.path, root.as_str()).starts_with(prefix)
                    })
                    .collect()
            };
            emit(root.as_str(), &files)
        }
        "codegraph_status" => {
            let stats = api.stats_cached().await;
            emit(root.as_str(), &stats)
        }
        "codegraph_search_symbol" => {
            let q = arg_str(&args, "query")?;
            let kind = args
                .get("kind")
                .and_then(|v| v.as_str())
                .and_then(SymbolKind::parse);
            let mode = args
                .get("match")
                .and_then(|v| v.as_str())
                .and_then(SymbolMatch::parse)
                .unwrap_or(SymbolMatch::Contains);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let resume = args
                .get("resume")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(20000);
            let out = api
                .search_symbol_paged_resumable(
                    q,
                    kind,
                    mode,
                    Pagination { limit, offset },
                    resume,
                    timeout_ms,
                )
                .await?;
            if out.timed_out {
                return Err(Error::Other(format!(
                    "codegraph_search_symbol timed out after {}ms (collected {} symbols so far). \
                     Retry the same call with the same arguments plus \"resume\": \"{}\" \
                     to continue the search from where it stopped.",
                    timeout_ms,
                    out.progress,
                    out.resume.as_deref().unwrap_or("")
                )));
            }
            let detail = detail_from_args(&args, session_detail);
            let format = format_from_args(&args, session_format);
            let results: Vec<Value> = out
                .page
                .into_iter()
                .map(|s| symbol_json(root.as_str(), &s, detail, format))
                .collect();
            emit_value(
                root.as_str(),
                json!({
                    "results": results,
                    "total": out.total,
                    "limit": limit,
                    "offset": offset,
                    "has_more": offset as usize + results.len() < out.total,
                    "resume": out.resume,
                }),
            )
        }
        "codegraph_class" => {
            let target = resolve_target(
                api,
                &args,
                "id",
                "class_name",
                &[SymbolKind::Class, SymbolKind::Interface, SymbolKind::Enum],
            )
            .await?;
            match target {
                Target::Ambiguous(v) => emit_value(root.as_str(), v),
                Target::Symbol(sym) => match api.class_info(sym.id).await {
                    Some(info) => {
                        let detail = detail_from_args(&args, session_detail);
                        let format = format_from_args(&args, session_format);
                        emit_value(
                            root.as_str(),
                            json!({
                                "class": symbol_json(root.as_str(), &info.class, detail, format),
                                "fields": info.fields,
                                "methods": info.methods,
                            }),
                        )
                    }
                    None => Err(Error::Invalid(format!(
                        "symbol {:?} (id {}) is not a class/interface/enum",
                        sym.name, sym.id
                    ))),
                },
            }
        }
        "codegraph_list_types" => {
            let kind_str = args.get("kind").and_then(|v| v.as_str()).unwrap_or("class");
            let kind = SymbolKind::parse(kind_str).ok_or_else(|| {
                Error::Invalid(format!(
                    "unknown kind: {kind_str:?} (expected class|interface|enum)"
                ))
            })?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let resume = args
                .get("resume")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(20000);
            let out = api
                .list_by_kind_resumable(kind, Pagination { limit, offset }, resume, timeout_ms)
                .await?;
            if out.timed_out {
                return Err(Error::Other(format!(
                    "codegraph_list_types timed out after {}ms (collected {} symbols so far). \
                     Retry the same call with the same arguments plus \"resume\": \"{}\" \
                     to continue from where it stopped.",
                    timeout_ms,
                    out.progress,
                    out.resume.as_deref().unwrap_or("")
                )));
            }
            let detail = detail_from_args(&args, session_detail);
            let format = format_from_args(&args, session_format);
            let results: Vec<Value> = out
                .page
                .into_iter()
                .map(|s| symbol_json(root.as_str(), &s, detail, format))
                .collect();
            emit_value(
                root.as_str(),
                json!({
                    "kind": kind_str,
                    "results": results,
                    "total": out.total,
                    "limit": limit,
                    "offset": offset,
                    "resume": out.resume,
                }),
            )
        }
        "codegraph_function_scope" => {
            let target = resolve_target(api, &args, "id", "func_name", &[]).await?;
            match target {
                Target::Ambiguous(v) => emit_value(root.as_str(), v),
                Target::Symbol(sym) => match api.function_scope(sym.id).await {
                    Some(scope) => {
                        let detail = detail_from_args(&args, session_detail);
                        let format = format_from_args(&args, session_format);
                        let parameters: Vec<Value> = scope
                            .parameters
                            .iter()
                            .map(|s| symbol_json(root.as_str(), s, detail, format))
                            .collect();
                        let locals: Vec<Value> = scope
                            .locals
                            .iter()
                            .map(|s| symbol_json(root.as_str(), s, detail, format))
                            .collect();
                        emit_value(
                            root.as_str(),
                            json!({
                                "function": symbol_json(root.as_str(), &scope.function, detail, format),
                                "parameters": parameters,
                                "locals": locals,
                            }),
                        )
                    }
                    None => emit_value(
                        root.as_str(),
                        json!({
                            "function": sym.name,
                            "parameters": [],
                            "locals": [],
                            "total": 0,
                        }),
                    ),
                },
            }
        }
        "codegraph_search_by_annotation" => {
            let annotation = arg_str(&args, "annotation")?;
            let kind = args
                .get("kind")
                .and_then(|v| v.as_str())
                .and_then(SymbolKind::parse);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let resume = args
                .get("resume")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(20000);
            let out = api
                .search_by_annotation_resumable(
                    annotation,
                    kind,
                    Pagination { limit, offset },
                    resume,
                    timeout_ms,
                )
                .await?;
            if out.timed_out {
                return Err(Error::Other(format!(
                    "codegraph_search_by_annotation timed out after {}ms (collected {} symbols so far). \
                     Retry the same call with the same arguments plus \"resume\": \"{}\" \
                     to continue the search from where it stopped.",
                    timeout_ms,
                    out.progress,
                    out.resume.as_deref().unwrap_or("")
                )));
            }
            let detail = detail_from_args(&args, session_detail);
            let format = format_from_args(&args, session_format);
            let results: Vec<Value> = out
                .page
                .into_iter()
                .map(|s| symbol_json(root.as_str(), &s, detail, format))
                .collect();
            emit_value(
                root.as_str(),
                json!({
                    "annotation": annotation,
                    "kind": kind.map(|k| k.as_str()),
                    "results": results,
                    "total": out.total,
                    "offset": offset,
                    "resume": out.resume,
                }),
            )
        }
        "codegraph_dependencies" => {
            let report = api.dependencies().await;
            emit(root.as_str(), &report)
        }
        _ => Err(Error::Invalid(format!("unknown tool: {name}"))),
    }
}

/// Kết quả resolve symbol theo `id` hoặc `name` cho các tool lấy target.
enum Target {
    /// Symbol khớp duy nhất.
    Symbol(Symbol),
    /// Trùng tên — payload JSON cho LLM retry với `id`.
    Ambiguous(Value),
}

/// Resolve target của tool: ưu tiên `id_key`, fallback `name_key`. Trùng tên →
/// `Target::Ambiguous` với toàn bộ matches (giống `codegraph_symbol`).
///
/// `prefer_kinds` khác rỗng: nếu trong matches có symbol thuộc các kind này
/// (VD: class tool muốn Class/Interface/Enum, không quan tâm constructor/field
/// trùng tên) thì chỉ xét riêng nhóm đó — tránh ambiguous giả do Java ctor hoặc
/// field cùng tên với class.
async fn resolve_target(
    api: &GraphApi,
    args: &Value,
    id_key: &str,
    name_key: &str,
    prefer_kinds: &[SymbolKind],
) -> Result<Target> {
    if let Some(id) = args.get(id_key).and_then(|v| v.as_u64()) {
        return match api.symbol_by_id(id).await {
            Some(s) => Ok(Target::Symbol(s)),
            None => Err(Error::Invalid(format!("symbol id {id} not found"))),
        };
    }
    let name = args
        .get(name_key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        return Err(Error::Invalid(format!(
            "provide '{name_key}' or '{id_key}'"
        )));
    }
    let r = api.resolve(&name, 0).await?;
    if !prefer_kinds.is_empty() {
        let preferred: Vec<Symbol> = r
            .matches
            .iter()
            .filter(|s| prefer_kinds.contains(&s.kind))
            .cloned()
            .collect();
        if preferred.len() == 1 {
            return Ok(Target::Symbol(preferred[0].clone()));
        }
        if preferred.len() > 1 {
            return Ok(Target::Ambiguous(json!({
                "ambiguous": true,
                "name": name,
                "matches": preferred,
                "hint": format!(
                    "Multiple symbols share this name. Retry with '{id_key}' ALONE (e.g. {{\"{id_key}\": <id>}}) to select the exact one."
                ),
            })));
        }
    }
    if r.ambiguous {
        return Ok(Target::Ambiguous(json!({
            "ambiguous": true,
            "name": name,
            "matches": r.matches,
            "hint": format!(
                "Multiple symbols share this name. Retry with '{id_key}' ALONE (e.g. {{\"{id_key}\": <id>}}) to select the exact one."
            ),
        })));
    }
    match r.symbol {
        Some(s) => Ok(Target::Symbol(s)),
        None => Err(Error::Invalid(format!("symbol {name:?} not found"))),
    }
}

fn arg_str<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Invalid(format!("missing string arg: {k}")))
}
fn arg_u64(v: &Value, k: &str) -> Result<u64> {
    v.get(k)
        .and_then(|x| x.as_u64())
        .ok_or_else(|| Error::Invalid(format!("missing int arg: {k}")))
}

// ── Symbol detail + path relativization ──
// List tools trả symbol theo `DetailLevel` của session (`codegraph_init
// {"detail": ...}`), ghi đè từng call bằng arg `detail`. Mọi response đi qua
// `emit_value`/`emit` để `file`/`path` relativize theo workspace root — LLM
// không cần thấy tiền tố absolute lặp lại trên từng dòng.

/// Detail level cho một tool: arg `detail` ghi đè session default.
fn detail_from_args(args: &Value, session: DetailLevel) -> DetailLevel {
    args.get("detail")
        .and_then(|v| v.as_str())
        .and_then(DetailLevel::parse)
        .unwrap_or(session)
}

/// Output style cho một tool: arg `format` ghi đè session default.
fn format_from_args(args: &Value, session: OutputStyle) -> OutputStyle {
    args.get("format")
        .and_then(|v| v.as_str())
        .and_then(OutputStyle::parse)
        .unwrap_or(session)
}

/// Symbol JSON theo `detail` + `style`. `Minimize` (mặc định) → mảng vị trí cố
/// định (order được document trong server-instructions.md; file đã relativize
/// theo root — relativize_paths chỉ chạm object key, không chạm phần tử mảng);
/// `Medium` → object giữ key (field default bị lược sau trong `omit_defaults`).
fn symbol_json(root: &str, s: &Symbol, detail: DetailLevel, style: OutputStyle) -> Value {
    match style {
        OutputStyle::Minimize => json!([
            s.id,
            s.name,
            s.kind.as_str(),
            s.scope.as_str(),
            s.scope_id,
            s.type_ref,
            s.type_name,
            strip_root_prefix(&s.file, root),
            s.line,
            s.end_line,
            s.signature,
            s.doc,
            s.annotations,
            s.language,
        ]),
        OutputStyle::Medium => match detail {
            DetailLevel::Minimal => json!({
                "id": s.id,
                "name": s.name,
                "kind": s.kind.as_str(),
                "file": s.file,
                "line": s.line,
            }),
            DetailLevel::Medium => json!({
                "id": s.id,
                "name": s.name,
                "kind": s.kind.as_str(),
                "file": s.file,
                "line": s.line,
                "signature": s.signature,
            }),
            DetailLevel::Verbose => serde_json::to_value(s).unwrap_or(Value::Null),
        },
    }
}

/// Strip `root/` prefix khỏi một path — chỉ khi root là tiền tố theo boundary
/// (`root` + `/`), tránh cắt nhầm `/root2/...`. Giữ nguyên nếu không khớp.
pub(crate) fn strip_root_prefix<'a>(path: &'a str, root: &str) -> &'a str {
    if let Some(rest) = path.strip_prefix(root) {
        if let Some(rest) = rest.strip_prefix('/') {
            return rest;
        }
    }
    path
}

/// Keys mang đường dẫn file trong response — relativize theo workspace root.
const PATH_KEYS: [&str; 3] = ["file", "path", "matched_path"];

/// Strip `root/` prefix khỏi mọi đường dẫn file trong cây JSON (in-place).
fn relativize_paths(v: &mut Value, root: &str) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if PATH_KEYS.contains(&k.as_str()) {
                    if let Some(s) = val.as_str() {
                        *val = Value::String(strip_root_prefix(s, root).to_string());
                    }
                }
                relativize_paths(val, root);
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                relativize_paths(item, root);
            }
        }
        _ => {}
    }
}

/// Serialize payload JSON kèm relativize path theo root — mọi response tool
/// đi qua đây để `file`/`path` trả về tương đối so với workspace root.
fn emit_value(root: &str, v: Value) -> Result<String> {
    let mut v = v;
    relativize_paths(&mut v, root);
    omit_defaults(&mut v);
    serde_json::to_string_pretty(&v).map_err(|e| Error::Invalid(e.to_string()))
}

/// `emit_value` cho bất kỳ type serializable nào (chuyển qua `to_value`).
fn emit<T: Serialize>(root: &str, v: &T) -> Result<String> {
    let value = serde_json::to_value(v).map_err(|e| Error::Invalid(e.to_string()))?;
    emit_value(root, value)
}

/// Keys có `0` = "absent" (sentinel) — value 0 bị lược như default. Các số khác
/// (counts/totals như `total`, `symbols`, `lines`, ...) giữ nguyên 0 vì ý nghĩa.
const ZERO_SENTINEL_KEYS: [&str; 3] = ["scope_id", "type_ref", "end_line"];

/// Value có phải "default" cần lược không (Binance-style minimal):
/// null / false / "" / [] / {} — và số 0 cho sentinel keys.
fn is_default_value(key: &str, v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        Value::Number(n) => ZERO_SENTINEL_KEYS.contains(&key) && n.as_f64() == Some(0.0),
    }
}

/// Lược bỏ key có value mặc định trong mọi OBJECT (in-place). ARRAY không bao
/// giờ bị xóa phần tử — schema mảng vị trí cố định (style `minimize`) phải giữ
/// nguyên độ dài; chỉ object con bên trong được xử lý tiếp.
///
/// Giữ thứ tự key (preserve_order): `mem::take` + rebuild — `Map::remove` là
/// swap-remove (đảo thứ tự), `shift_remove` không có sẵn trên mọi bản serde_json.
pub(crate) fn omit_defaults(v: &mut Value) {
    match v {
        Value::Object(map) => {
            let old = std::mem::take(map);
            for (k, mut child) in old {
                omit_defaults(&mut child);
                if !is_default_value(&k, &child) {
                    map.insert(k, child);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                omit_defaults(item);
            }
        }
        _ => {}
    }
}
