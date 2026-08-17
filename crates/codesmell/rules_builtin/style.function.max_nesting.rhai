// style.function.max_nesting — avoid deeply nested control flow.
// Enabled via `[[rhai.rule]] use = "style.function.max_nesting" params = { max = 4 }`.
const ADVICE = "Avoid deeply nested control flow.";

fn describe(params) {
    "Avoid nesting deeper than " + (params.max ?? 4).to_string() + " levels."
}

fn check_flow(sym, markers) {
    let max = params.max ?? 4;
    let mut depth = 0;
    let mut mx = 0;
    for m in markers {
        if m == "loop" || m == "if_true" || m == "if_false" || m == "switch_case" {
            depth += 1;
            if depth > mx { mx = depth; }
        } else if m == "branch_end" || m == "loop_back" || m == "switch_end" || m == "break" || m == "continue" {
            depth -= 1;
        }
    }
    if mx > max {
        return #{
            message: "function `" + sym.name + "` nesting depth is " + mx.to_string() + " (max " + max.to_string() + ")",
            hint: "flatten early returns and extract nested blocks"
        };
    }
    false
}
