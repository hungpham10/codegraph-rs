// security.deny_call — forbid functions from calling denied callees.
// Enabled via `[[rhai.rule]] use = "security.deny_call" params = {
//   deny = ["eval", "exec", "system"],
//   deny_re = ['(?i)^(os\.system|child_process\.exec)$'],
//   symbols = ["process_*"], symbols_re = ['(?i)^handle_.*webhook$'],
//   message = "dynamic code execution",
//   fix_hint = "parse the input explicitly instead of executing it",
// }`.
// (Entry-level `paths` / `exclude` scope which callers are checked.)
const ADVICE = "Avoid calling the denied functions.";

fn describe(_params) { "Calls to specific dangerous functions are denied." }

fn match_any(globs, regexes, text) {
    for g in (globs ?? []) {
        if glob(g, text) { return true; }
    }
    for r in (regexes ?? []) {
        if regex_match(r, text) { return true; }
    }
    false
}

fn check_calls(sym, callees, callers) {
    let deny = params.deny ?? [];
    let deny_re = params.deny_re ?? [];
    let symbolsf = params.symbols ?? [];
    let symbols_re = params.symbols_re ?? [];
    if symbolsf.len() > 0 || symbols_re.len() > 0 {
        if !match_any(symbolsf, symbols_re, sym.name) { return false; }
    }
    for callee in callees {
        if match_any(deny, deny_re, callee.name) {
            return #{
                message: params.message ?? ("call to `" + callee.name + "` is denied"),
                hint: params.fix_hint ?? ("remove or replace the call to `" + callee.name + "`")
            };
        }
    }
    false
}
