// architecture.boundary — forbid calls that cross denied layer edges.
// Enabled via `[[rhai.rule]] use = "architecture.boundary" params = {
//   layers = [{ name = "controller", paths = ["src/controllers/**"] },
//             { name = "repository", paths = ["src/repositories/**"] }],
//   deny = ["controller -> repository"],
// }`.
const ADVICE = "Respect the architecture layer boundaries.";

fn describe(_params) { "Architecture layer boundaries are enforced." }

fn layer_of(layers, file) {
    for l in layers {
        for p in (l.paths ?? []) {
            if glob(p, file) { return l.name; }
        }
    }
    ""
}

fn check_calls(sym, callees, callers) {
    let layers = params.layers ?? [];
    if layers.len() == 0 { return false; }
    let deny = params.deny ?? [];
    if deny.len() == 0 { return false; }
    let caller_layer = layer_of(layers, sym.file);
    if caller_layer == "" { return false; }
    for callee in callees {
        let callee_layer = layer_of(layers, callee.file);
        if callee_layer == "" { continue; }
        let edge = caller_layer + " -> " + callee_layer;
        for d in deny {
            if d == edge || d == (caller_layer + " -> *") {
                return #{
                    message: "`" + sym.name + "` (" + caller_layer + ") calls `" + callee.name + "` (" + callee_layer + "): edge `" + edge + "` is denied",
                    hint: "route `" + sym.name + "` through an allowed layer"
                };
            }
        }
    }
    false
}
