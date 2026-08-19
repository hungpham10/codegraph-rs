// team.no_panic — library code should not panic.
// A reusable rule template: `params.max_panics` lets a team allow a few panics
// in legacy code while forbidding them in new code.
const ADVICE = "Avoid panics; return a Result instead.";

fn describe(params) {
    "Functions should not panic (limit " + (params.max_panics ?? 0).to_string() + ")."
}

fn check_calls(sym, callees, callers) {
    let max = params.max_panics ?? 0;
    let hits = callees.filter(|c| {
        let l = c.name.to_lower();
        l == "panic" || l.contains("panic")
    });
    if hits.len() > max {
        return #{
            message: "`" + sym.name + "` can panic via `" + hits[0].name + "`",
            hint: "return a Result and propagate the error"
        };
    }
    false
}
