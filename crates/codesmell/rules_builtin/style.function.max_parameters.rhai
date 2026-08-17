// style.function.max_parameters — limit the number of real parameters.
// Enabled via `[[rhai.rule]] use = "style.function.max_parameters" params = { max = 4 }`.
const ADVICE = "Functions normally take at most a few real parameters.";

fn describe(params) {
    "Functions normally take at most " + (params.max ?? 4).to_string() + " parameters."
}

// Strip leading/trailing ASCII spaces (rhai's `trim` is unreliable here).
fn strip(s) {
    let n = s.len();
    let start = 0;
    while start < n && s.sub_string(start, 1) == " " { start += 1; }
    let end = n;
    while end > start && s.sub_string(end - 1, 1) == " " { end -= 1; }
    s.sub_string(start, end - start)
}

fn real_param(seg) {
    let t = strip(seg);
    if t == "" { return false; }
    if t == "self" || t == "&self" || t == "&mut self" { return false; }
    if t.starts_with("self:") { return false; }
    true
}

fn count_params(sig) {
    let open = sig.index_of("(");
    if open < 0 { return 0; }
    let depth = 0;
    let end = -1;
    let i = open;
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
    if strip(inner) == "" { return 0; }
    // split on top-level commas only (commas inside (..) or <..> belong to one param)
    let depth = 0;
    let seg = "";
    let count = 0;
    let chars = inner.len();
    let j = 0;
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
    if sym.kind != "function" && sym.kind != "method" { return false; }
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
