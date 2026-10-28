// style.naming — enforce naming patterns per symbol kind.
// Enabled via `[[rhai.rule]] use = "style.naming" params = { rules = [
//   { kind = "method", pattern = "*Async", signature_contains = "async" },
//   { kind = "class", pattern = "*Service" },
// ] }`.
const ADVICE = "Follow the team naming conventions.";

fn describe(_params) { "Naming conventions are enforced." }

fn check(sym) {
    let rules = params.rules ?? [];
    for r in rules {
        let k = r.kind ?? "";
        if k != "" && sym.kind != k { continue; }
        let sigc = r.signature_contains ?? "";
        if sigc != "" && !sym.signature.contains(sigc) { continue; }
        let paths = r.paths ?? [];
        if paths.len() > 0 {
            let hit = false;
            for p in paths {
                if glob(p, sym.file) { hit = true; break; }
            }
            if !hit { continue; }
        }
        let ok = glob(r.pattern, sym.name);
        if !ok {
            return #{
                message: "`" + sym.name + "` should match naming pattern `" + r.pattern + "`",
                hint: "rename to follow the team naming convention"
            };
        }
    }
    false
}
