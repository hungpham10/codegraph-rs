//! Transport-agnostic tool implementations shared by every frontend
//! (MCP server, GraphQL server, future UIs).
//!
//! Ban đầu các hàm này nằm trong `codegraph-mcp::tools`, nhưng để cả MCP và
//! GraphQL (và bất kỳ transport nào) cùng tiêu thụ chung một implementation,
//! chúng được đưa lên tầng `codegraph-api` — transport chỉ là lớp mỏng gọi
//! xuống đây. Các hàm trả `Result<String>` (JSON đã `emit`) hoặc `Result<Value>`
//! (cho passthrough qua GraphQL scalar).

use camino::{Utf8Path, Utf8PathBuf};
use codegraph_core::{is_marker, Error, Result, SymbolKind, SymbolMatch};
use codegraph_extract::Orchestrator;
use codegraph_graph::{GraphIndex, SharedGraphIndex};
use codegraph_sboxes::{compile_with_mocks, BranchPolicy, SboxConfig};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

// ==================== JSON emit (shared with MCP `dispatch_with_api`) ====================

/// Strip `root/` prefix khỏi một path — chỉ khi root là tiền tố theo boundary
/// (`root` + `/`), tránh cắt nhầm `/root2/...`. Giữ nguyên nếu không khớp.
pub fn strip_root_prefix<'a>(path: &'a str, root: &str) -> &'a str {
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
pub fn relativize_paths(v: &mut Value, root: &str) {
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
pub fn emit_value(root: &str, v: Value) -> Result<String> {
    let mut v = v;
    relativize_paths(&mut v, root);
    omit_defaults(&mut v);
    serde_json::to_string_pretty(&v).map_err(|e| Error::Invalid(e.to_string()))
}

/// `emit_value` cho bất kỳ type serializable nào (chuyển qua `to_value`).
pub fn emit<T: Serialize>(root: &str, v: &T) -> Result<String> {
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
pub fn omit_defaults(v: &mut Value) {
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

// ==================== Sandbox / diff / simulate ====================

/// Lấy arg string bắt buộc.
pub fn arg_str<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Invalid(format!("missing string arg: {k}")))
}

/// Parse các run-options dùng chung giữa `sandbox`, `diff_simulate`,
/// `origin_simulate`: `args` (i64 array), `mocks` (callee → rhai source),
/// `branch_policy`, `loop_cap`.
type SandboxRunOptions = (Vec<i64>, Vec<(String, String)>, SboxConfig);
pub fn parse_run_options(root: &Utf8Path, args: &Value) -> Result<SandboxRunOptions> {
    let mut call_args = Vec::new();
    if let Some(arr) = args.get("args").and_then(|v| v.as_array()) {
        for v in arr {
            call_args.push(
                v.as_i64()
                    .ok_or_else(|| Error::Invalid("args must be integers".into()))?,
            );
        }
    }
    let mut mocks = Vec::new();
    if let Some(obj) = args.get("mocks").and_then(|v| v.as_object()) {
        for (name, src) in obj {
            let src = src
                .as_str()
                .ok_or_else(|| Error::Invalid(format!("mock `{name}` must be a rhai string")))?;
            mocks.push((name.clone(), src.to_string()));
        }
    }
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
    Ok((call_args, mocks, config))
}

/// So sánh trace sequence giữa hai kết quả `run_sim` (origin/before vs
/// working_tree/after): liệt kê mock-call/cond-decision nào chỉ xuất hiện một
/// bên. `present:false` / `link_error` → sequence rỗng, delta vẫn có ý nghĩa.
fn sequence_delta(before: &Value, after: &Value) -> Value {
    let seq = |v: &Value| -> Vec<String> {
        v.get("sequence")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let sb = seq(before);
    let sa = seq(after);
    json!({
        "sequence_added": sa.iter().filter(|s| !sb.contains(s)).cloned().collect::<Vec<_>>(),
        "sequence_removed": sb.iter().filter(|s| !sa.contains(s)).cloned().collect::<Vec<_>>(),
    })
}

/// Chạy sandbox trên flow của entry function.
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
            .search_symbol_paged_resumable(
                q,
                None,
                SymbolMatch::Contains,
                codegraph_graph::Pagination {
                    limit: 20,
                    offset: 0,
                },
                None,
                None,
            )
            .await?
            .page;
        hits
            .into_iter()
            .find(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
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

    let (call_args, mocks, config) = parse_run_options(root, &args)?;

    let mut module = compile_with_mocks(&idx, &ids, &config, &mocks).await?;
    let (ret, trace) = module.run(&call_args);

    let group_names: Vec<String> = ids
        .iter()
        .filter_map(|id| idx.symbol_by_id(*id).map(|s| s.name))
        .collect();
    emit_value(
        root.as_str(),
        json!({
            "entry": flow.symbol.name,
            "entry_id": entry_id,
            "group": group_names,
            "args": call_args,
            "return": ret,
            "mocks": trace.mocks,
            "conds": trace.conds,
            "missing_mocks": trace.missing,
            "sequence": trace.sequence(),
        }),
    )
}

/// Phân tích unified diff (MR / patch / `git diff`) thành bản DRAFT tác động
/// lên graph. Read-only.
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
    emit(root.as_str(), &report)
}

/// Chạy sandbox trên flow của `entry_name` trong một index cụ thể.
async fn run_sim(
    idx: &GraphIndex,
    entry_name: &str,
    call_args: &[i64],
    config: &SboxConfig,
    mocks: &[(String, String)],
) -> Result<Value> {
    let Some(sym) = idx
        .search_symbol_paged_resumable(
            entry_name,
            None,
            SymbolMatch::Contains,
            codegraph_graph::Pagination {
                limit: 20,
                offset: 0,
            },
            None,
            None,
        )
        .await?
        .page
        .into_iter()
        .find(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
    else {
        return Ok(json!({ "present": false }));
    };

    let mut ids = vec![sym.id];
    let mut seen = std::collections::HashSet::from([sym.id]);
    if let Ok(flow) = idx.flow(sym.id).await {
        for &e in &flow.chain {
            if is_marker(e) {
                continue;
            }
            if e != sym.id && idx.symbol_by_id(e).is_some() && seen.insert(e) {
                ids.push(e);
            }
        }
    }
    ids.sort_unstable();

    let mut module = match compile_with_mocks(idx, &ids, config, mocks).await {
        Ok(m) => m,
        Err(e) => return Ok(json!({ "present": true, "link_error": e.to_string() })),
    };
    let (ret, trace) = module.run(call_args);
    Ok(json!({
        "present": true,
        "group": ids
            .iter()
            .filter_map(|id| idx.symbol_by_id(*id).map(|s| s.name.clone()))
            .collect::<Vec<_>>(),
        "return": ret,
        "sequence": trace.sequence(),
        "missing_mocks": trace.missing,
    }))
}

/// Build index của cây git tại `base_ref` (`git archive` → temp dir →
/// parse+ingest vào `GraphIndex::in_memory`). Luôn trả kèm tmp dir để caller
/// dọn dẹp, kể cả khi thất bại (trả `None` + `note` lý do).
async fn build_before_index(
    root: &Utf8Path,
    base_ref: &str,
) -> Result<(Option<GraphIndex>, Utf8PathBuf, String)> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let tmp = Utf8PathBuf::from_path_buf(
        std::env::temp_dir().join(format!("codegraph-sim-{}-{millis}", std::process::id())),
    )
    .map_err(|p| Error::Invalid(format!("temp path not UTF-8: {p:?}")))?;
    let tree = tmp.join("tree");
    let tar = tmp.join("tree.tar");
    if let Err(e) = std::fs::create_dir_all(&tree) {
        return Ok((None, tmp, format!("temp dir failed: {e}")));
    }

    let st = match std::process::Command::new("git")
        .args(["archive", "--format=tar"])
        .arg(base_ref)
        .arg("-o")
        .arg(&tar)
        .current_dir(root.as_std_path())
        .status()
    {
        Ok(s) => s,
        Err(e) => return Ok((None, tmp, format!("git unavailable: {e}"))),
    };
    if !st.success() {
        return Ok((None, tmp, format!("git archive `{base_ref}` failed")));
    }
    let ok = std::process::Command::new("tar")
        .arg("-xf")
        .arg(&tar)
        .arg("-C")
        .arg(&tree)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Ok((None, tmp, "tar extract failed".into()));
    }

    let mut before = GraphIndex::in_memory();
    match Orchestrator::with_registry()
        .index_all(&tree, &mut before, None)
        .await
    {
        Ok(_) => Ok((Some(before), tmp, String::new())),
        Err(e) => Ok((None, tmp, format!("before-index failed: {e}"))),
    }
}

/// Diff → simulate: chạy sandbox trên flow entry cho cả bản "trước" (git
/// archive tại `base_ref`) và bản "sau" (index hiện tại = post-MR), so sánh
/// trace. Read-only — không mutate index.
pub async fn dispatch_diff_simulate(
    root: &Utf8Path,
    shared: Arc<SharedGraphIndex>,
    args: Value,
) -> Result<String> {
    let diff = arg_str(&args, "diff")?;
    let parsed = codegraph_graph::diff::parse_unified_diff(diff)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let base_ref = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD")
        .to_string();

    let (call_args, mocks, config) = parse_run_options(root, &args)?;

    let idx = shared.ensure_fresh().await;
    let report = idx.diff_assess(&parsed, Some(root.as_std_path())).await;

    // Hàm bị diff chạm: ưu tiên flow (call-site trên dòng đổi), kèm symbol
    // Function/Method. Dedupe, giữ thứ tự.
    let mut affected: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in &report.files {
        for fl in &f.flows {
            if seen.insert(fl.name.clone()) {
                affected.push(fl.name.clone());
            }
        }
        for s in &f.symbols {
            if matches!(s.symbol.kind, SymbolKind::Function | SymbolKind::Method)
                && seen.insert(s.symbol.name.clone())
            {
                affected.push(s.symbol.name.clone());
            }
        }
    }

    let entry = match args.get("entry").and_then(|v| v.as_str()) {
        Some(e) => e.to_string(),
        None => affected.first().cloned().ok_or_else(|| {
            Error::Invalid("no function affected by the diff — pass `entry`".into())
        })?,
    };

    // Build index "trước" + tmp dir (caller dọn tmp kể cả khi thất bại).
    let (before_idx, tmp, build_note) = build_before_index(root, &base_ref).await?;

    let result = async {
        let before = match &before_idx {
            Some(b) => run_sim(b, &entry, &call_args, &config, &mocks).await?,
            None => json!({ "present": false, "reason": build_note }),
        };
        let after = run_sim(&idx, &entry, &call_args, &config, &mocks).await?;

        let delta = sequence_delta(&before, &after);
        Ok::<Value, Error>(json!({
            "draft": true,
            "tool": "codegraph_diff_simulate",
            "entry": entry,
            "args": call_args,
            "base_ref": base_ref,
            "affected_functions": affected,
            "before_index_note": build_note,
            "before": before,
            "after": after,
            "delta": delta,
            "note": "Read-only: before = index tạm từ `git archive {base_ref}`, after = index hiện tại (post-MR). Không mutate index.",
        }))
    }
    .await;

    let _ = std::fs::remove_dir_all(&tmp);
    let payload = result?;
    emit_value(root.as_str(), payload)
}

/// Ref → simulate: chạy sandbox trên flow entry trên cây git tại `ref` (index
/// tạm từ `git archive`) VÀ trên index hiện tại (working tree), so sánh trace
/// trước/sau — không cần diff, entry chọn tự do. Read-only — không mutate index.
pub async fn dispatch_origin_simulate(
    root: &Utf8Path,
    shared: Arc<SharedGraphIndex>,
    args: Value,
) -> Result<String> {
    let entry = arg_str(&args, "entry")?;
    let git_ref = args
        .get("ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD")
        .to_string();
    let (call_args, mocks, config) = parse_run_options(root, &args)?;

    let idx = shared.ensure_fresh().await;
    let (origin_idx, tmp, build_note) = build_before_index(root, &git_ref).await?;

    let result = async {
        let origin = match &origin_idx {
            Some(o) => run_sim(o, entry, &call_args, &config, &mocks).await?,
            None => json!({ "present": false, "reason": build_note }),
        };
        let working_tree = run_sim(&idx, entry, &call_args, &config, &mocks).await?;
        let delta = sequence_delta(&origin, &working_tree);
        Ok::<Value, Error>(json!({
            "draft": true,
            "tool": "codegraph_origin_simulate",
            "entry": entry,
            "args": call_args,
            "ref": git_ref,
            "origin_index_note": build_note,
            "origin": origin,
            "working_tree": working_tree,
            "delta": delta,
            "note": "Read-only: origin = index tạm từ `git archive {git_ref}`, working_tree = index hiện tại. Không mutate index.",
        }))
    }
    .await;

    let _ = std::fs::remove_dir_all(&tmp);
    let payload = result?;
    emit_value(root.as_str(), payload)
}
