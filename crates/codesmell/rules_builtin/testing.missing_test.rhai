// testing.missing_test — business logic needs a unit test.
// Enabled via `[[rhai.rule]] use = "testing.missing_test" params = {
//   require = true,
//   test_paths = ["tests/**", "**/*_test.rs"],
//   layers = [{ name = "service", paths = ["src/services/**"] }],
//   selectors = [{ min_lines = 20 }],
// }`.
const ADVICE = "New or changed business logic requires a unit test.";

fn describe(params) {
    if params.require ?? false { "Business logic requires a unit test." } else { "" }
}

fn layer_of(layers, file) {
    for l in layers {
        for p in (l.paths ?? []) {
            if glob(p, file) { return l.name; }
        }
    }
    ""
}

fn is_logic(sym, layers, selectors) {
    if selectors.len() == 0 { return true; }
    let layer = layer_of(layers, sym.file);
    for sel in selectors {
        let sels = sel.layers ?? [];
        for l in sels {
            if layer == l { return true; }
        }
        let min = sel.min_lines ?? 0;
        if min > 0 && (sym.end_line - sym.line + 1) >= min { return true; }
    }
    false
}

fn check_calls(sym, callees, callers) {
    if !(params.require ?? false) { return false; }
    let test_paths = params.test_paths ?? [];
    if test_paths.len() == 0 { return false; }
    if !is_logic(sym, params.layers ?? [], params.selectors ?? []) { return false; }
    let mut tested = false;
    for caller in callers {
        for p in test_paths {
            if glob(p, caller.file) { tested = true; break; }
        }
        if tested { break; }
    }
    if !tested {
        return #{
            message: "business logic `" + sym.name + "` has no unit test",
            hint: "add a unit test that covers this logic"
        };
    }
    false
}
