// style.function.max_parameters — limit the number of real parameters.
// Enabled via `[[rhai.rule]] use = "style.function.max_parameters" params = { max = 4 }`.
const ADVICE = "Functions normally take at most a few real parameters.";

fn describe(params) {
    "Functions normally take at most " + (params.max ?? 4).to_string() + " parameters."
}

fn real_param(seg) {
    let t = seg.trim();
    if t == "" { return false; }
    t != "self" && t != "&self" && t != "&mut self" && t != "self: &Self" && t != "self: Self"
}

fn count_params(sig) {
    let open = sig.index_of("(");
    if open < 0 { return 0; }
    let mut depth = 0;
    let mut end = -1;
    let mut i = open;
    while i < sig.len() {
        let c = sig.sub_string(i, 1);
        if c == "(" { depth += 1; }
        else if c == ")" {
            depth -= 1;
            if depth == 0 { end = i; break; }
        }
        i += 1;
    }
    if end < 0 { return 0; }
    let inner = sig.sub_string(open + 1, end - open - 1);
    if inner.trim() == "" { return 0; }
    // split on top-level commas only (commas inside (..) or <..> belong to one param)
    let mut depth = 0;
    let mut seg = "";
    let mut count = 0;
    let chars = inner.len();
    let mut j = 0;
    while j < chars {
        let c = inner.sub_string(j, 1);
        if c == "(" || c == "<" { depth += 1; seg += c; }
        else if c == ")" || c == ">" { depth -= 1; seg += c; }
        else if c == "," && depth == 0 {
            if real_param(seg) { count += 1; }
            seg = "";
        } else { seg += c; }
        j += 1;
    }
    if real_param(seg) { count += 1; }
    count
}

fn check(sym) {
    let max = params.max ?? 4;
    let n = count_params(sym.signature);
    if n > max {
        return #{
            message: "function `" + sym.name + "` takes " + n.to_string() + " parameters (max " + max.to_string() + ")",
            hint: "group parameters into a struct or options type"
        };
    }
    false
}
