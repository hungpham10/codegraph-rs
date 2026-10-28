// style.function.max_complexity — keep cyclomatic complexity bounded.
// Enabled via `[[rhai.rule]] use = "style.function.max_complexity" params = { max = 10 }`.
// Complexity = 1 + number of decision markers (if_true/if_false/loop/switch_case).
const ADVICE = "Keep functions under the team complexity budget.";

fn describe(params) {
    "Cyclomatic complexity should stay at or below " + (params.max ?? 10).to_string() + "."
}

fn check_flow(sym, markers) {
    let max = params.max ?? 10;
    let decisions = markers.filter(|m|
        m == "if_true" || m == "if_false" || m == "loop" || m == "switch_case").len();
    let complexity = decisions + 1;
    if complexity > max {
        return #{
            message: "function `" + sym.name + "` has cyclomatic complexity " + complexity.to_string() + " (max " + max.to_string() + ")",
            hint: "split the function into smaller units"
        };
    }
    false
}
