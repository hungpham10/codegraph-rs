// style.function.max_lines — keep functions short and focused.
// Enabled via `[[rhai.rule]] use = "style.function.max_lines" params = { max = 60 }`.
const ADVICE = "Keep functions short and focused.";

fn describe(params) {
    "Functions normally stay below " + (params.max ?? 60).to_string() + " lines."
}

fn check(sym) {
    let max = params.max ?? 60;
    let loc = sym.end_line - sym.line + 1;
    if loc > max {
        return #{
            message: "function `" + sym.name + "` is " + loc.to_string() + " lines (max " + max.to_string() + ")",
            hint: "split `" + sym.name + "` into smaller functions"
        };
    }
    false
}
