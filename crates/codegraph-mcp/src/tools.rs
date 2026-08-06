use camino::Utf8Path;
use codegraph_api::GraphApi;
use codegraph_context::{ContextRequest, Format};
use codegraph_core::{Error, Result, Symbol, SymbolKind, SymbolMatch, is_marker};
use codegraph_extract::{init_project, project_db_path, project_dir, ExtractStats, Orchestrator};
use codegraph_graph::{GraphIndex, SharedGraphIndex};
use codegraph_sboxes::{BranchPolicy, SboxConfig, compile_with_mocks};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "codegraph_search",
            "Search symbols by name (substring, case-insensitive).",
            json!({ "type": "object", "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer", "default": 20 }
            }, "required": ["query"] }),
        ),
        tool(
            "codegraph_symbol",
            "Look up a symbol by id or exact name. Duplicate names → ambiguous with the full match list; retry with symbol_id.",
            json!({ "type": "object", "properties": {
                "id": { "type": "integer" },
                "name": { "type": "string" }
            } }),
        ),
        tool(
            "codegraph_callers",
            "Find functions that (transitively) call the given symbol.",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer" },
                "depth": { "type": "integer", "default": 1 }
            }, "required": ["node"] }),
        ),
        tool(
            "codegraph_callees",
            "Find functions called directly by the given symbol.",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer" }
            }, "required": ["node"] }),
        ),
        tool(
            "codegraph_impact",
            "Impact radius: who transitively depends on this symbol.",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer" },
                "max_depth": { "type": "integer", "default": 3 }
            }, "required": ["node"] }),
        ),
        tool(
            "codegraph_flow",
            "Call chain of a symbol: markers (LOOP, IF_TRUE, …) + callee names + call sites with line/condition/effect.",
            json!({ "type": "object", "properties": {
                "node": { "type": "integer" }
            }, "required": ["node"] }),
        ),
        tool(
            "codegraph_search_flow",
            "Find functions whose call chain contains a pattern. Pattern = comma-separated tokens: numeric ids, marker names (LOOP, IF_TRUE, IF_FALSE, BRANCH_END, RETURN, LOOP_BACK, SWITCH_CASE, SWITCH_END, BREAK, CONTINUE, THROW) or symbol names.",
            json!({ "type": "object", "properties": {
                "pattern": { "type": "string" }
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
            "Functions that call a library call whose name contains the query (includes unresolved external calls).",
            json!({ "type": "object", "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer", "default": 20 }
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
        // ── Admin tools (init / index) — thao tác trên workspace root của server ──
        tool(
            "codegraph_init",
            "Initialize the workspace for CodeGraph (idempotent): creates .codegraph/ with .gitignore, version, and config.toml. Pass index=false to skip the full re-index that runs by default.",
            json!({ "type": "object", "properties": {
                "index": { "type": "boolean", "default": true }
            } }),
        ),
        tool(
            "codegraph_index",
            "Full re-index of the workspace into .codegraph/db.sqlite. Requires the workspace to be initialized (run codegraph_init first).",
            json!({ "type": "object", "properties": {} }),
        ),
        // ── Enhanced symbol search (semgraph_search_symbol) ──
        tool(
            "codegraph_search_symbol",
            "Search symbols by name with optional kind filter, match mode, and pagination. match: 'contains' (substring anywhere, default), 'prefix' (name starts with), 'suffix' (name ENDS with — e.g. query=\"Service\" finds every *Service class), 'exact' (exact name, case-insensitive). Use 'total' with 'offset' to fetch further pages until offset >= total.",
            json!({ "type": "object", "properties": {
                "query": { "type": "string" },
                "kind": { "type": "string", "enum": ["function", "method", "class", "interface", "enum", "variable", "constant", "parameter", "field", "module", "file"] },
                "match": { "type": "string", "enum": ["contains", "prefix", "suffix", "exact"], "default": "contains" },
                "limit": { "type": "integer", "default": 20 },
                "offset": { "type": "integer", "default": 0 }
            }, "required": ["query"] }),
        ),
        // ── Class queries (semgraph_get_class_methods / get_class / list_classes / list_interfaces) ──
        tool(
            "codegraph_class_methods",
            "Get all methods belonging to a class/interface/enum. Disambiguate duplicate class names with 'id' from codegraph_search (pass 'id' alone).",
            json!({ "type": "object", "properties": {
                "class_name": { "type": "string" },
                "id": { "type": "integer" },
                "compact": { "type": "boolean", "default": true }
            } }),
        ),
        tool(
            "codegraph_class",
            "Get class/interface/enum details with fields and methods as separate lists.",
            json!({ "type": "object", "properties": {
                "class_name": { "type": "string" },
                "id": { "type": "integer" }
            } }),
        ),
        tool(
            "codegraph_list_classes",
            "List all class symbols in the index (paginated).",
            json!({ "type": "object", "properties": {
                "limit": { "type": "integer", "default": 20 },
                "offset": { "type": "integer", "default": 0 }
            } }),
        ),
        tool(
            "codegraph_list_interfaces",
            "List all interface symbols in the index (paginated).",
            json!({ "type": "object", "properties": {
                "limit": { "type": "integer", "default": 20 },
                "offset": { "type": "integer", "default": 0 }
            } }),
        ),
        tool(
            "codegraph_function_scope",
            "Get a function's parameters and local variables. Disambiguate duplicate function names with 'id' from codegraph_search (pass 'id' alone).",
            json!({ "type": "object", "properties": {
                "func_name": { "type": "string" },
                "id": { "type": "integer" }
            } }),
        ),
        // ── Annotation / call / dependency queries ──
        tool(
            "codegraph_search_by_annotation",
            "Search symbols by annotation (e.g. @RestController, @GetMapping, @Autowired, @Override). Case-insensitive substring match. Optional kind filter.",
            json!({ "type": "object", "properties": {
                "annotation": { "type": "string" },
                "kind": { "type": "string", "enum": ["function", "method", "class", "interface", "enum", "variable", "constant", "parameter", "field", "module", "file"] },
                "limit": { "type": "integer", "default": 50 },
                "offset": { "type": "integer", "default": 0 }
            }, "required": ["annotation"] }),
        ),
        tool(
            "codegraph_search_by_call",
            "Find functions that call a given class/method name inside their bodies (e.g. \"LogManager\" or \"LogManager.getLogger\"). Matches ALL call names captured by the parser — including external library calls that don't resolve to in-repo symbols. Each result includes per-call-site context: line, surrounding condition, whether inside a loop, and the call arguments.",
            json!({ "type": "object", "properties": {
                "call_name": { "type": "string" },
                "limit": { "type": "integer", "default": 50 }
            }, "required": ["call_name"] }),
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
    ]
}

fn tool(name: &str, desc: &str, schema: Value) -> Value {
    json!({ "name": name, "description": desc, "inputSchema": schema })
}

pub async fn dispatch_with_api(api: &GraphApi, name: &str, args: Value) -> Result<String> {
    match name {
        "codegraph_search" => {
            let q = arg_str(&args, "query")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let hits = api.search(q, limit).await?;
            serde_json::to_string_pretty(&hits).map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_symbol" => {
            if let Some(id) = args.get("id").and_then(|v| v.as_u64()) {
                let s = api.symbol_by_id(id).await;
                return serde_json::to_string_pretty(&s).map_err(|e| Error::Invalid(e.to_string()));
            }
            if let Some(name) = args.get("name").and_then(|v| v.as_str()) {
                let r = api.resolve(name, 0).await?;
                if r.ambiguous {
                    // Trùng tên — trả matches để LLM retry với symbol_id.
                    return Ok(format!(
                        "ambiguous ({} matches):\n{}",
                        r.matches.len(),
                        serde_json::to_string_pretty(&r.matches)
                            .map_err(|e| Error::Invalid(e.to_string()))?
                    ));
                }
                return serde_json::to_string_pretty(&r.symbol)
                    .map_err(|e| Error::Invalid(e.to_string()));
            }
            Err(Error::Invalid("provide id or name".into()))
        }
        "codegraph_callers" => {
            let id = arg_u64(&args, "node")?;
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let hits = api.callers(id, depth).await?;
            serde_json::to_string_pretty(&hits).map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_callees" => {
            let id = arg_u64(&args, "node")?;
            let hits = api.callees(id).await?;
            serde_json::to_string_pretty(&hits).map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_impact" => {
            let id = arg_u64(&args, "node")?;
            let depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
            let report = api.impact(id, depth).await?;
            serde_json::to_string_pretty(&report).map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_flow" => {
            let id = arg_u64(&args, "node")?;
            let flow = api.flow(id).await?;
            serde_json::to_string_pretty(&flow).map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_search_flow" => {
            let pattern = arg_str(&args, "pattern")?;
            let hits = api.search_flow_pattern(pattern).await?;
            serde_json::to_string_pretty(&hits).map_err(|e| Error::Invalid(e.to_string()))
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
            };
            Ok(api.context_markdown(&req).await?)
        }
        "codegraph_references" => {
            let q = arg_str(&args, "query")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let report = api.references(q, limit).await?;
            serde_json::to_string_pretty(&report).map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_files" => {
            let prefix = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let files = api.files(prefix).await;
            serde_json::to_string_pretty(&files).map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_status" => {
            let stats = api.stats().await;
            serde_json::to_string_pretty(&stats).map_err(|e| Error::Invalid(e.to_string()))
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
            let (results, total) = api
                .search_symbol_paged(q, kind, mode, limit, offset)
                .await?;
            serde_json::to_string_pretty(&json!({
                "results": results,
                "total": total,
                "limit": limit,
                "offset": offset,
                "has_more": offset as usize + results.len() < total,
            }))
            .map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_class_methods" => {
            let target = resolve_target(
                api,
                &args,
                "id",
                "class_name",
                &[SymbolKind::Class, SymbolKind::Interface, SymbolKind::Enum],
            )
            .await?;
            match target {
                Target::Ambiguous(v) => Ok(json_str(v)),
                Target::Symbol(sym) => {
                    if !matches!(
                        sym.kind,
                        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
                    ) {
                        return Err(Error::Invalid(format!(
                            "symbol {:?} (id {}) is not a class/interface/enum",
                            sym.name, sym.id
                        )));
                    }
                    let compact = args
                        .get("compact")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let methods = api.class_methods(sym.id).await;
                    let methods: Vec<Value> = if compact {
                        methods
                            .into_iter()
                            .map(|m| {
                                json!({ "id": m.id, "name": m.name, "kind": m.kind, "line": m.line })
                            })
                            .collect()
                    } else {
                        methods
                            .into_iter()
                            .map(|m| serde_json::to_value(&m).unwrap_or(Value::Null))
                            .collect()
                    };
                    serde_json::to_string_pretty(&json!({
                        "class_name": sym.name,
                        "methods": methods,
                        "compact": compact,
                        "total": methods.len(),
                    }))
                    .map_err(|e| Error::Invalid(e.to_string()))
                }
            }
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
                Target::Ambiguous(v) => Ok(json_str(v)),
                Target::Symbol(sym) => match api.class_info(sym.id).await {
                    Some(info) => serde_json::to_string_pretty(&info)
                        .map_err(|e| Error::Invalid(e.to_string())),
                    None => Err(Error::Invalid(format!(
                        "symbol {:?} (id {}) is not a class/interface/enum",
                        sym.name, sym.id
                    ))),
                },
            }
        }
        "codegraph_list_classes" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let (results, total) = api.list_by_kind(SymbolKind::Class, limit, offset).await;
            serde_json::to_string_pretty(&json!({
                "kind": "class",
                "results": results,
                "total": total,
                "limit": limit,
                "offset": offset,
            }))
            .map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_list_interfaces" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let (results, total) = api.list_by_kind(SymbolKind::Interface, limit, offset).await;
            serde_json::to_string_pretty(&json!({
                "kind": "interface",
                "results": results,
                "total": total,
                "limit": limit,
                "offset": offset,
            }))
            .map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_function_scope" => {
            let target = resolve_target(api, &args, "id", "func_name", &[]).await?;
            match target {
                Target::Ambiguous(v) => Ok(json_str(v)),
                Target::Symbol(sym) => match api.function_scope(sym.id).await {
                    Some(scope) => serde_json::to_string_pretty(&scope)
                        .map_err(|e| Error::Invalid(e.to_string())),
                    None => Ok(serde_json::to_string_pretty(&json!({
                        "function": sym.name,
                        "parameters": [],
                        "locals": [],
                        "total": 0,
                    }))
                    .map_err(|e| Error::Invalid(e.to_string()))?),
                },
            }
        }
        "codegraph_search_by_annotation" => {
            let annotation = arg_str(&args, "annotation")?;
            let kind = args
                .get("kind")
                .and_then(|v| v.as_str())
                .and_then(SymbolKind::parse);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as u32;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let (results, total, truncated) = api
                .search_by_annotation(annotation, kind, offset, limit)
                .await;
            serde_json::to_string_pretty(&json!({
                "annotation": annotation,
                "kind": kind.map(|k| k.as_str()),
                "results": results,
                "total": total,
                "offset": offset,
                "truncated": truncated,
            }))
            .map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_search_by_call" => {
            let call_name = arg_str(&args, "call_name")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as u32;
            let hits = api.references(call_name, limit).await?;
            serde_json::to_string_pretty(&json!({
                "call_name": call_name,
                "results": hits,
                "total": hits.len(),
            }))
            .map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_dependencies" => {
            let report = api.dependencies().await;
            serde_json::to_string_pretty(&report).map_err(|e| Error::Invalid(e.to_string()))
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

fn json_str(v: Value) -> String {
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
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

// ── Admin tools (codegraph_init / codegraph_index) ──
// Cần workspace root (không qua GraphApi) — server lưu `root` và gọi hàm này.

pub async fn dispatch_admin(root: &Utf8Path, name: &str, args: Value) -> Result<String> {
    match name {
        "codegraph_init" => {
            let dir = init_project(root)?;
            let do_index = args.get("index").and_then(|v| v.as_bool()).unwrap_or(true);
            let mut out = json!({ "initialized": dir.as_str() });
            if do_index {
                match run_index(root).await {
                    Ok(stats) => {
                        out["indexed"] = stats_json(&stats);
                    }
                    Err(e) => {
                        return Err(Error::Invalid(format!(
                            "initialized {}, but indexing failed: {e}",
                            dir
                        )));
                    }
                }
            }
            serde_json::to_string_pretty(&out).map_err(|e| Error::Invalid(e.to_string()))
        }
        "codegraph_index" => {
            if !project_dir(root).exists() {
                return Err(Error::Invalid(
                    "workspace not initialized: missing .codegraph/. Run codegraph_init first."
                        .into(),
                ));
            }
            let stats = run_index(root).await?;
            serde_json::to_string_pretty(&stats_json(&stats))
                .map_err(|e| Error::Invalid(e.to_string()))
        }
        _ => Err(Error::Invalid(format!("unknown admin tool: {name}"))),
    }
}

/// Full re-index: mở sqlite → `Orchestrator::index_all` (ingest = full re-index).
/// Không progress bar — MCP transport là stdout, tránh nhiễu JSON-RPC.
async fn run_index(root: &Utf8Path) -> Result<ExtractStats> {
    let db_str = project_db_path(root).as_str().to_string();
    let mut idx = GraphIndex::open(&db_str).await?;
    Orchestrator::with_registry()
        .index_all(root, &mut idx, None)
        .await
}

fn stats_json(s: &ExtractStats) -> Value {
    json!({
        "files": s.files,
        "symbols": s.symbols,
        "chains": s.chains,
        "calls": s.calls,
        "skipped": s.skipped,
    })
}

// ── Sandbox tool (codegraph_sandbox) ──
// Cần workspace root (config.toml `[sandbox]` + mock dirs) và snapshot index,
// nên dispatch riêng qua `SharedGraphIndex` — không qua `GraphApi`.

/// Chạy sandbox trên flow của entry function.
///
/// `node` (symbol id) hoặc `name` (substring → function match đầu tiên) chọn
/// entry; group = entry + mọi callee trong flow resolve được. `mocks` là map
/// callee → Rhai source (body được wrap tự động thành `fn <name>(args)`), override
/// file mock cùng tên — mocks thiếu được ghi vào `missing_mocks`.
pub async fn dispatch_sandbox(
    root: &Utf8Path,
    shared: Arc<SharedGraphIndex>,
    args: Value,
) -> Result<String> {
    let idx = shared.ensure_fresh().await;

    // Entry: `node` id, hoặc `name` (substring, function match đầu tiên).
    let entry_id = if let Some(id) = args.get("node").and_then(|v| v.as_u64()) {
        id
    } else {
        let q = arg_str(&args, "name")?;
        let hits = idx
            .search_symbol(q, Some(SymbolKind::Function), 1)
            .await?;
        hits.first()
            .map(|s| s.id)
            .ok_or_else(|| Error::Invalid(format!("no function matching `{q}`")))?
    };

    // Group: entry + mọi callee trong flow là symbol biết tên (compile thành
    // machine code); callee không resolve → mock dispatch. Giống cmd_sandbox CLI.
    let flow = idx.flow(entry_id).await?;
    let mut ids = vec![entry_id];
    let mut seen = std::collections::HashSet::from([entry_id]);
    for &e in &flow.chain {
        if is_marker(e) {
            continue;
        }
        if e != entry_id && idx.symbol_by_id(e).is_some() && seen.insert(e) {
            ids.push(e);
        }
    }
    ids.sort_unstable();

    let mut config = SboxConfig::load(root).unwrap_or_default();
    if let Some(p) = args.get("branch_policy").and_then(|v| v.as_str()) {
        config.branch_policy = match p {
            "if_true" => BranchPolicy::IfTrue,
            "if_false" => BranchPolicy::IfFalse,
            other => {
                return Err(Error::Invalid(format!(
                    "bad branch_policy `{other}` (expected if_true/if_false)"
                )));
            }
        };
    }
    if let Some(c) = args.get("loop_cap").and_then(|v| v.as_u64()) {
        config.loop_cap = c as usize;
    }

    // Entry args — i64 array (abstract values; arena preloads arena[0..n]).
    let mut call_args = Vec::new();
    if let Some(arr) = args.get("args").and_then(|v| v.as_array()) {
        for v in arr {
            call_args.push(
                v.as_i64()
                    .ok_or_else(|| Error::Invalid("args must be integers".into()))?,
            );
        }
    }

    // Per-call inline mocks: callee name → rhai body / full fn source.
    let mut mocks = Vec::new();
    if let Some(obj) = args.get("mocks").and_then(|v| v.as_object()) {
        for (name, src) in obj {
            let src = src.as_str().ok_or_else(|| {
                Error::Invalid(format!("mock `{name}` must be a rhai string"))
            })?;
            mocks.push((name.clone(), src.to_string()));
        }
    }

    let mut module = compile_with_mocks(&idx, &ids, &config, &mocks).await?;
    let (ret, trace) = module.run(&call_args);

    let group_names: Vec<String> = ids
        .iter()
        .filter_map(|id| idx.symbol_by_id(*id).map(|s| s.name))
        .collect();
    serde_json::to_string_pretty(&json!({
        "entry": flow.symbol.name,
        "entry_id": entry_id,
        "group": group_names,
        "args": call_args,
        "return": ret,
        "mocks": trace.mocks,
        "conds": trace.conds,
        "missing_mocks": trace.missing,
        "sequence": trace.sequence(),
    }))
    .map_err(|e| Error::Invalid(e.to_string()))
}

/// Phân tích unified diff (MR / patch / `git diff`) thành bản DRAFT tác động
/// lên graph. Read-only: parse diff, đối chiếu dòng bên new với symbol + call-site
/// trong index, trả report JSON — không mutate index.
pub async fn dispatch_diff(
    root: &Utf8Path,
    shared: Arc<SharedGraphIndex>,
    args: Value,
) -> Result<String> {
    let diff = arg_str(&args, "diff")?;
    let parsed = codegraph_graph::diff::parse_unified_diff(diff)
        .map_err(|e| Error::Invalid(e.to_string()))?;

    let idx = shared.ensure_fresh().await;
    let report = idx.diff_assess(&parsed, Some(root.as_std_path())).await;
    serde_json::to_string_pretty(&report).map_err(|e| Error::Invalid(e.to_string()))
}
