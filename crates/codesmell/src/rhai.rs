//! Rhai rule engine: load rule scripts, build configured instances, run them.
//!
//! Every CodeSmell rule is a rhai script. Builtin scripts ship inside the
//! binary (see [`builtin_scripts`]); custom scripts live in rule dirs
//! (default `.codesmell/rules/`) and override builtins of the same id. A script
//! only runs when an `[[rhai.rule]]` entry in the policy references it, and the
//! entry's `params` are injected as the script's `params` map (template
//! pattern).
//!
//! A script may define up to four hooks:
//! - `check(sym)` — for any symbol (function, method, class, variable, ...).
//! - `check_calls(sym, callees, callers)` — functions/methods only; `callees`
//!   and `callers` are arrays of `{ name, file }` maps.
//! - `check_flow(sym, markers)` — functions/methods only; `markers` is the
//!   ordered flow as an array of lowercase marker names (`"loop"`, `"if_true"`,
//!   ...).
//! - `describe(params)` — optional; produces the human line for `codesmell guide`.
//!
//! A hook returns `false`/`()` to pass, a `string` for a default violation, or
//! a `{ message, hint, severity }` map for a full one. `const ADVICE` (set at
//! rule-definition time) documents the rule for the LLM guide.

use codegraph_core::{marker_name, Symbol, SymbolKind};
use codegraph_graph::GraphIndex;
use rhai::{Array, Dynamic, Engine, Map, Scope, AST};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use crate::engine::{collect_symbols, rel_path, CheckScope, Violation};
use crate::glob::GlobSet;
use crate::policy::{Policy, Severity};

const CHECK: &str = "check";
const CHECK_CALLS: &str = "check_calls";
const CHECK_FLOW: &str = "check_flow";
const DESCRIBE: &str = "describe";

/// Symbol kinds handed to `check` (code-like decls; parameters, modules, files
/// and config are excluded as noise).
pub const RULE_SYMBOL_KINDS: &[SymbolKind] = &[
    SymbolKind::Function,
    SymbolKind::Method,
    SymbolKind::Class,
    SymbolKind::Interface,
    SymbolKind::Enum,
    SymbolKind::Variable,
    SymbolKind::Constant,
    SymbolKind::Field,
];

/// Symbol kinds for `check_calls` / `check_flow` (only symbols with a call graph).
const FN_KINDS: &[SymbolKind] = &[SymbolKind::Function, SymbolKind::Method];

#[derive(Default, Clone, Copy)]
struct HookSet {
    check: bool,
    check_calls: bool,
    check_flow: bool,
    describe: bool,
}

/// A compiled rule script (by id).
pub struct RhaiRule {
    ast: AST,
    hooks: HookSet,
    /// `const ADVICE` captured at load.
    advice: String,
}

/// One enabled, configured rule: a compiled script + the policy entry's params,
/// scoping and severity.
pub struct RuleInstance {
    pub rule_id: String,
    pub use_script: String,
    pub advice: String,
    params: Map,
    paths: GlobSet,
    exclude: GlobSet,
    pub severity: Option<Severity>,
    hooks: HookSet,
}

/// Loaded set of rule scripts (builtins + user dirs) sharing one engine with
/// the `glob` / `regex_match` helper functions registered.
pub struct RhaiRuleLib {
    engine: Engine,
    rules: HashMap<String, RhaiRule>,
}

impl RhaiRuleLib {
    /// Load builtin scripts plus every `*.rhai` under `dirs` (relative to
    /// `root`); a user script overrides a builtin of the same id. A script that
    /// fails to compile aborts the whole lint — a silently inactive rule is a
    /// security hole, not a warning.
    pub fn load(root: &Path, dirs: &[String]) -> anyhow::Result<Self> {
        let engine = build_engine();
        let mut rules: HashMap<String, RhaiRule> = HashMap::new();
        let mut errors: Vec<String> = Vec::new();

        for (id, src) in builtin_scripts() {
            match compile_rule(&engine, id, src) {
                Ok(r) => {
                    rules.insert(id.to_string(), r);
                }
                Err(e) => errors.push(format!("builtin rule `{id}`: {e}")),
            }
        }
        for dir in dirs {
            let abs = root.join(dir);
            let Ok(entries) = std::fs::read_dir(&abs) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|x| x.to_str()) != Some("rhai") {
                    continue;
                }
                let Some(id) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                match compile_rule(&engine, &id, &src) {
                    Ok(r) => {
                        rules.insert(id.clone(), r);
                    }
                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                }
            }
        }

        if !errors.is_empty() {
            anyhow::bail!("failed to compile rule script(s):\n{}", errors.join("\n"));
        }
        Ok(RhaiRuleLib { engine, rules })
    }

    /// Build the enabled instances from the policy. Entries whose `use` does
    /// not resolve to a loaded script are reported loudly (not silently) so a
    /// typo'd security rule cannot hide.
    pub fn instances(&self, policy: &Policy) -> Vec<RuleInstance> {
        let mut out = Vec::new();
        for e in &policy.rhai.rules {
            if e.use_script.is_empty() {
                eprintln!("codesmell: warning: [[rhai.rule]] with empty `use` — skipped");
                continue;
            }
            match self.rules.get(&e.use_script) {
                None => eprintln!(
                    "codesmell: warning: [[rhai.rule]] use = \"{}\" — no such rule in rule dirs; skipped",
                    e.use_script
                ),
                Some(r) => {
                    let params = e.params.as_ref().map(toml_to_map).unwrap_or_default();
                    out.push(RuleInstance {
                        rule_id: e.id.clone().unwrap_or_else(|| e.use_script.clone()),
                        use_script: e.use_script.clone(),
                        advice: r.advice.clone(),
                        params,
                        paths: GlobSet::new(&e.paths),
                        exclude: GlobSet::new(&e.exclude),
                        severity: e.severity,
                        hooks: r.hooks,
                    });
                }
            }
        }
        out
    }

    /// Human-facing description for `codesmell guide` (from `describe(params)`),
    /// if the script defines one; `None` falls back to `advice`.
    pub fn describe(&self, inst: &RuleInstance) -> Option<String> {
        if !inst.hooks.describe {
            return None;
        }
        let mut scope = Scope::new();
        self.engine
            .call_fn::<String>(
                &mut scope,
                &self.rules[&inst.use_script].ast,
                DESCRIBE,
                (inst.params.clone(),),
            )
            .ok()
    }

    /// Evaluate `instance`'s `hook` with `args` (each arg is a `Dynamic`).
    /// Returns `None` to pass (or on a runtime error, reported to stderr);
    /// otherwise the returned value the script flagged with.
    pub(crate) fn invoke(
        &self,
        inst: &RuleInstance,
        hook: &str,
        label: &str,
        args: impl rhai::FuncArgs,
    ) -> Option<Dynamic> {
        let mut scope = Scope::new();
        scope.push_constant("params", inst.params.clone());
        match self.engine.call_fn::<Dynamic>(
            &mut scope,
            &self.rules[&inst.use_script].ast,
            hook,
            args,
        ) {
            Ok(d) => {
                if d.is_unit() || matches!(d.clone().try_cast::<bool>(), Some(false)) {
                    None
                } else {
                    Some(d)
                }
            }
            Err(e) => {
                eprintln!(
                    "codesmell: warning: rule `{}` failed on `{}`: {e}",
                    inst.rule_id, label
                );
                None
            }
        }
    }
}

// ==================== Engine + helpers ====================

fn build_engine() -> Engine {
    let mut engine = Engine::new();
    // Map literals use rhai's `#{ ... }` syntax; keep expression nesting unlimited
    // so realistic rule bodies (long string concatenations, nested `if`) compile.
    engine.set_max_expr_depths(0, 0);
    engine.register_fn("glob", glob_match);
    engine.register_fn("regex_match", regex_match);
    engine
}

thread_local! {
    static RX: RefCell<HashMap<String, Option<regex::Regex>>> = RefCell::new(HashMap::new());
}

/// `regex_match(pattern, text)` — cached per pattern.
fn regex_match(pattern: &str, text: &str) -> bool {
    RX.with(|c| {
        let mut cache = c.borrow_mut();
        let re = cache
            .entry(pattern.to_string())
            .or_insert_with(|| regex::Regex::new(pattern).ok());
        re.as_ref().map(|r| r.is_match(text)).unwrap_or(false)
    })
}

/// `glob(pattern, text)` — glob match (wraps the crate `glob` helper).
fn glob_match(pattern: &str, text: &str) -> bool {
    crate::glob::glob_matches(pattern, text)
}

fn compile_rule(engine: &Engine, _id: &str, src: &str) -> anyhow::Result<RhaiRule> {
    let ast = engine.compile(src).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Run top-level statements so `const ADVICE` is defined in the scope;
    // function definitions are collected into the AST library regardless.
    let mut scope = Scope::new();
    engine
        .run_ast_with_scope(&mut scope, &ast)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let advice = scope
        .get("ADVICE")
        .and_then(|d| d.clone().try_cast::<String>())
        .unwrap_or_default();
    let mut hooks = HookSet::default();
    for f in ast.iter_functions() {
        match &*f.name {
            CHECK => hooks.check = true,
            CHECK_CALLS => hooks.check_calls = true,
            CHECK_FLOW => hooks.check_flow = true,
            DESCRIBE => hooks.describe = true,
            _ => {}
        }
    }
    Ok(RhaiRule { ast, hooks, advice })
}

// ==================== Symbol → rhai value conversion ====================

/// Build the `sym` map passed to `check` hooks. `pub(crate)` so tests can craft
/// synthetic symbols without a real index.
pub(crate) fn symbol_map(s: &Symbol, root: &Path) -> Map {
    let mut m = Map::new();
    m.insert("name".into(), Dynamic::from(s.name.clone()));
    m.insert("kind".into(), Dynamic::from(s.kind.as_str().to_string()));
    m.insert("scope".into(), Dynamic::from(s.scope.as_str().to_string()));
    m.insert("file".into(), Dynamic::from(rel_path(&s.file, root)));
    m.insert("line".into(), Dynamic::from(s.line as i64));
    m.insert("end_line".into(), Dynamic::from(s.end_line as i64));
    m.insert(
        "signature".into(),
        Dynamic::from(s.signature.clone().unwrap_or_default()),
    );
    m.insert(
        "doc".into(),
        Dynamic::from(s.doc.clone().unwrap_or_default()),
    );
    m.insert("language".into(), Dynamic::from(s.language.clone()));
    let anns: Array = s
        .annotations
        .iter()
        .map(|a| {
            let mut am = Map::new();
            am.insert("name".into(), Dynamic::from(a.name.clone()));
            am.insert("line".into(), Dynamic::from(a.line as i64));
            Dynamic::from(am)
        })
        .collect();
    m.insert("annotations".into(), Dynamic::from(anns));
    m
}

fn callee_maps(callees: &[Symbol], root: &Path) -> Array {
    callees
        .iter()
        .map(|c| {
            let mut m = Map::new();
            m.insert("name".into(), Dynamic::from(c.name.clone()));
            m.insert("file".into(), Dynamic::from(rel_path(&c.file, root)));
            Dynamic::from(m)
        })
        .collect()
}

/// Lowercase marker names from a flow chain — the `markers` array passed to
/// `check_flow`.
fn marker_names(chain: &[u64]) -> Array {
    chain
        .iter()
        .filter_map(|&id| marker_name(id).map(|n| Dynamic::from(n.to_lowercase())))
        .collect()
}

// ==================== Result interpretation ====================

/// Interpret a flagged value into `(message, hint, severity-from-map)`.
fn interpret_result(
    d: &Dynamic,
    sym_name: &str,
    rule_id: &str,
) -> (String, String, Option<Severity>) {
    if let Some(s) = d.clone().try_cast::<String>() {
        return (s, default_hint(sym_name, rule_id), None);
    }
    if let Some(m) = d.clone().try_cast::<Map>() {
        let msg = map_str(&m, "message");
        let hint = map_str(&m, "hint");
        let sev = m
            .get("severity")
            .and_then(|v| v.clone().try_cast::<String>())
            .and_then(|s| Severity::parse_label(&s));
        let msg = if msg.is_empty() {
            format!("rule `{rule_id}` flagged `{sym_name}`")
        } else {
            msg
        };
        return (
            msg,
            if hint.is_empty() {
                default_hint(sym_name, rule_id)
            } else {
                hint
            },
            sev,
        );
    }
    (
        format!("rule `{rule_id}` flagged `{sym_name}`"),
        default_hint(sym_name, rule_id),
        None,
    )
}

fn default_hint(sym_name: &str, rule_id: &str) -> String {
    format!("adjust `{sym_name}` to satisfy rule `{rule_id}`")
}

fn map_str(m: &Map, k: &str) -> String {
    m.get(k)
        .and_then(|v| v.clone().try_cast::<String>())
        .unwrap_or_default()
}

// ==================== Run ====================

/// Evaluate every enabled rule instance over the repository and return the
/// violations. Returns an empty report (and no work) when the policy enables no
/// rules.
pub async fn run(
    index: &GraphIndex,
    scope: &CheckScope,
    policy: &Policy,
    root: &Path,
) -> anyhow::Result<Vec<Violation>> {
    if policy.rhai.rules.is_empty() {
        return Ok(Vec::new());
    }
    let lib = RhaiRuleLib::load(root, &policy.rhai.rule_dirs)?;
    let insts = lib.instances(policy);
    if insts.is_empty() {
        return Ok(Vec::new());
    }

    let needs_all = insts.iter().any(|i| i.hooks.check);
    let needs_calls = insts.iter().any(|i| i.hooks.check_calls);
    let needs_flow = insts.iter().any(|i| i.hooks.check_flow);

    let mut violations: Vec<Violation> = Vec::new();

    if needs_all {
        for s in collect_symbols(index, RULE_SYMBOL_KINDS, scope, root) {
            let sym = symbol_map(&s, root);
            for inst in &insts {
                if !inst.hooks.check || !in_scope(inst, &s, root) {
                    continue;
                }
                if let Some(d) = lib.invoke(inst, CHECK, &s.name, (sym.clone(),)) {
                    emit(&mut violations, inst, &s, policy, &d);
                }
            }
        }
    }

    if needs_calls || needs_flow {
        for s in collect_symbols(index, FN_KINDS, scope, root) {
            let mut callees: Option<Vec<Symbol>> = None;
            let mut callers: Option<Vec<Symbol>> = None;
            let mut markers: Option<Array> = None;
            for inst in &insts {
                if !in_scope(inst, &s, root) {
                    continue;
                }
                if inst.hooks.check_calls {
                    if callees.is_none() {
                        callees = Some(index.callees(s.id).await.unwrap_or_default());
                    }
                    if callers.is_none() {
                        callers = Some(index.callers(s.id, 1).await.unwrap_or_default());
                    }
                    let args = (
                        symbol_map(&s, root),
                        callee_maps(callees.as_ref().unwrap(), root),
                        callee_maps(callers.as_ref().unwrap(), root),
                    );
                    if let Some(d) = lib.invoke(inst, CHECK_CALLS, &s.name, args) {
                        emit(&mut violations, inst, &s, policy, &d);
                    }
                }
                if inst.hooks.check_flow {
                    if markers.is_none() {
                        markers = Some(match index.flow(s.id).await {
                            Ok(f) => marker_names(&f.chain),
                            Err(_) => Vec::new(),
                        });
                    }
                    let args = (symbol_map(&s, root), markers.clone().unwrap());
                    if let Some(d) = lib.invoke(inst, CHECK_FLOW, &s.name, args) {
                        emit(&mut violations, inst, &s, policy, &d);
                    }
                }
            }
        }
    }

    Ok(violations)
}

fn in_scope(inst: &RuleInstance, s: &Symbol, root: &Path) -> bool {
    let rel = rel_path(&s.file, root);
    if inst.exclude.matches(&rel) {
        return false;
    }
    inst.paths.is_empty() || inst.paths.matches(&rel)
}

fn emit(
    violations: &mut Vec<Violation>,
    inst: &RuleInstance,
    s: &Symbol,
    policy: &Policy,
    d: &Dynamic,
) {
    let (message, hint, result_sev) = interpret_result(d, &s.name, &inst.rule_id);
    let severity = result_sev
        .or(inst.severity)
        .unwrap_or_else(|| policy.severity_of(&inst.rule_id));
    violations.push(Violation {
        rule: inst.rule_id.clone(),
        severity,
        file: s.file.clone(),
        line: s.line,
        symbol: s.name.clone(),
        message,
        fix_hint: hint,
    });
}

// ==================== toml::Value → rhai Map ====================

fn toml_to_map(v: &toml::Value) -> Map {
    let mut m = Map::new();
    if let Some(t) = v.as_table() {
        for (k, val) in t {
            m.insert(k.clone().into(), toml_to_dynamic(val));
        }
    }
    m
}

fn toml_to_dynamic(v: &toml::Value) -> Dynamic {
    match v {
        toml::Value::String(s) => Dynamic::from(s.clone()),
        toml::Value::Integer(i) => Dynamic::from(*i),
        toml::Value::Float(f) => Dynamic::from(*f),
        toml::Value::Boolean(b) => Dynamic::from(*b),
        toml::Value::Array(a) => Dynamic::from(a.iter().map(toml_to_dynamic).collect::<Array>()),
        toml::Value::Table(t) => Dynamic::from(toml_to_map_table(t)),
        _ => Dynamic::from(()),
    }
}

fn toml_to_map_table(t: &toml::map::Map<String, toml::Value>) -> Map {
    let mut m = Map::new();
    for (k, val) in t {
        m.insert(k.clone().into(), toml_to_dynamic(val));
    }
    m
}

// ==================== Builtin rule scripts ====================

/// Builtin rule scripts shipped in the binary (rule templates). A user script
/// with the same id overrides these. Ids equal the legacy rule ids so the
/// `[severity]` map and existing docs keep working.
pub fn builtin_scripts() -> &'static [(&'static str, &'static str)] {
    &[
        (
            crate::policy::RULE_MAX_LINES,
            include_str!("../rules_builtin/style.function.max_lines.rhai"),
        ),
        (
            crate::policy::RULE_MAX_PARAMS,
            include_str!("../rules_builtin/style.function.max_parameters.rhai"),
        ),
        (
            crate::policy::RULE_MAX_NESTING,
            include_str!("../rules_builtin/style.function.max_nesting.rhai"),
        ),
        (
            crate::policy::RULE_MAX_COMPLEXITY,
            include_str!("../rules_builtin/style.function.max_complexity.rhai"),
        ),
        (
            crate::policy::RULE_NAMING,
            include_str!("../rules_builtin/style.naming.rhai"),
        ),
        (
            crate::policy::RULE_BOUNDARY,
            include_str!("../rules_builtin/architecture.boundary.rhai"),
        ),
        (
            crate::policy::RULE_MISSING_TEST,
            include_str!("../rules_builtin/testing.missing_test.rhai"),
        ),
        (
            crate::policy::RULE_DENY_CALL,
            include_str!("../rules_builtin/security.deny_call.rhai"),
        ),
        (
            crate::policy::RULE_DENY_SYMBOL,
            include_str!("../rules_builtin/security.deny_symbol.rhai"),
        ),
    ]
}

#[allow(dead_code)]
fn _assert_path_exists() {
    // Compile-time guard that the builtin script files are reachable.
    const _: &str = include_str!("../rules_builtin/style.function.max_lines.rhai");
}

/// Test helper: build a minimal `RuleInstance` for a builtin script with params.
#[cfg(test)]
pub(crate) fn test_instance(use_script: &str, params: Map) -> RuleInstance {
    let lib = RhaiRuleLib::load(Path::new("."), &[]).unwrap();
    let r = lib.rules.get(use_script).expect("builtin script exists");
    RuleInstance {
        rule_id: use_script.to_string(),
        use_script: use_script.to_string(),
        advice: r.advice.clone(),
        params,
        paths: GlobSet::new(&[]),
        exclude: GlobSet::new(&[]),
        severity: None,
        hooks: r.hooks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{RULE_DENY_CALL, RULE_DENY_SYMBOL, RULE_MAX_LINES, RULE_MAX_PARAMS};
    use codegraph_core::ScopeLevel;

    fn fake_sym(name: &str, kind: SymbolKind, line: u32, end_line: u32, sig: &str) -> Symbol {
        Symbol {
            id: 0,
            name: name.to_string(),
            kind,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "src/x.rs".to_string(),
            line,
            end_line,
            signature: Some(sig.to_string()),
            doc: None,
            annotations: vec![],
            language: "rust".to_string(),
        }
    }

    fn param_map(entries: &[(&str, i64)]) -> Map {
        let mut m = Map::new();
        for (k, v) in entries {
            m.insert((*k).into(), Dynamic::from(*v));
        }
        m
    }

    #[test]
    fn builtin_scripts_compile_and_are_unique() {
        let lib = RhaiRuleLib::load(Path::new("."), &[]).unwrap();
        assert_eq!(builtin_scripts().len(), lib.rules.len());
        let ids: std::collections::HashSet<&str> =
            builtin_scripts().iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), builtin_scripts().len(), "duplicate builtin ids");
    }

    #[test]
    fn max_lines_flags_over_limit() {
        let inst = test_instance(RULE_MAX_LINES, param_map(&[("max", 5)]));
        let lib = RhaiRuleLib::load(Path::new("."), &[]).unwrap();
        let s = fake_sym("big", SymbolKind::Function, 1, 10, "fn big() {}");
        let sym = symbol_map(&s, Path::new("."));
        let d = lib.invoke(&inst, CHECK, &s.name, (sym,)).unwrap();
        let (msg, _, _) = interpret_result(&d, &s.name, RULE_MAX_LINES);
        assert!(msg.contains("lines"), "msg was: {msg}");
    }

    #[test]
    fn max_parameters_counts_real_params_and_skips_self() {
        let inst = test_instance(RULE_MAX_PARAMS, param_map(&[("max", 3)]));
        let lib = RhaiRuleLib::load(Path::new("."), &[]).unwrap();
        // 4 real params (self excluded) → violation; nested parens in a default value must not miscount
        let s = fake_sym(
            "f",
            SymbolKind::Method,
            1,
            2,
            "pub fn f(&self, a: i32, b: i32, c: Option<(i32, i32)>, d: i32) -> i32",
        );
        let sym = symbol_map(&s, Path::new("."));
        let d = lib.invoke(&inst, CHECK, &s.name, (sym,)).unwrap();
        let (msg, _, _) = interpret_result(&d, &s.name, RULE_MAX_PARAMS);
        assert!(msg.contains("parameters"), "msg was: {msg}");
    }

    #[test]
    fn params_default_via_null_coalesce() {
        // No max in params → script default (60) applies; a 5-line fn passes.
        let inst = test_instance(RULE_MAX_LINES, Map::new());
        let lib = RhaiRuleLib::load(Path::new("."), &[]).unwrap();
        let s = fake_sym("small", SymbolKind::Function, 1, 5, "fn small() {}");
        let sym = symbol_map(&s, Path::new("."));
        assert!(lib.invoke(&inst, CHECK, &s.name, (sym,)).is_none());
    }

    #[test]
    fn deny_call_flags_dangerous_sink() {
        let mut params = Map::new();
        params.insert("deny".into(), Dynamic::from(vec!["eval".to_string()]));
        let inst = test_instance(RULE_DENY_CALL, params);
        let lib = RhaiRuleLib::load(Path::new("."), &[]).unwrap();
        let s = fake_sym("run", SymbolKind::Function, 1, 2, "fn run() {}");
        let callees = vec![fake_sym("eval", SymbolKind::Function, 1, 1, "fn eval() {}")];
        let args = (
            symbol_map(&s, Path::new(".")),
            callee_maps(&callees, Path::new(".")),
            Array::new(),
        );
        let d = lib.invoke(&inst, CHECK_CALLS, &s.name, args).unwrap();
        let (msg, _, _) = interpret_result(&d, &s.name, RULE_DENY_CALL);
        assert!(msg.contains("eval"), "msg was: {msg}");
    }

    #[test]
    fn deny_symbol_flags_secret_const() {
        let mut params = Map::new();
        params.insert("kind".into(), Dynamic::from("constant".to_string()));
        params.insert(
            "name_re".into(),
            Dynamic::from(vec!["(?i)^(PASSWORD|SECRET)$".to_string()]),
        );
        let inst = test_instance(RULE_DENY_SYMBOL, params);
        let lib = RhaiRuleLib::load(Path::new("."), &[]).unwrap();
        let s = fake_sym("PASSWORD", SymbolKind::Constant, 1, 1, "PASSWORD = \"x\"");
        let sym = symbol_map(&s, Path::new("."));
        let d = lib.invoke(&inst, CHECK, &s.name, (sym,)).unwrap();
        let (msg, _, _) = interpret_result(&d, &s.name, RULE_DENY_SYMBOL);
        assert!(msg.contains("PASSWORD"), "msg was: {msg}");
    }

    #[test]
    fn deny_call_symbol_filter_uses_glob_helper() {
        // script uses `glob()` to match the caller name; "do_eval" matches *Async? no,
        // but it proves the glob helper is reachable inside a script.
        let mut params = Map::new();
        params.insert("deny".into(), Dynamic::from(vec!["eval".to_string()]));
        params.insert("symbols".into(), Dynamic::from(vec!["do_*".to_string()])); // glob filter
        let inst = test_instance(RULE_DENY_CALL, params);
        let lib = RhaiRuleLib::load(Path::new("."), &[]).unwrap();
        // caller "do_eval" matches symbols glob → flagged
        let s = fake_sym("do_eval", SymbolKind::Function, 1, 2, "fn do_eval() {}");
        let callees = vec![fake_sym("eval", SymbolKind::Function, 1, 1, "fn eval() {}")];
        let args = (
            symbol_map(&s, Path::new(".")),
            callee_maps(&callees, Path::new(".")),
            Array::new(),
        );
        assert!(lib.invoke(&inst, CHECK_CALLS, &s.name, args).is_some());
        // caller "other" does NOT match symbols glob → not flagged
        let s2 = fake_sym("other", SymbolKind::Function, 1, 2, "fn other() {}");
        let args2 = (
            symbol_map(&s2, Path::new(".")),
            callee_maps(&callees, Path::new(".")),
            Array::new(),
        );
        assert!(lib.invoke(&inst, CHECK_CALLS, &s2.name, args2).is_none());
    }
}
