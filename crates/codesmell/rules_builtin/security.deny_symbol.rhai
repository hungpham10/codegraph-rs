// security.deny_symbol — forbid symbol declarations matching denied patterns.
// Enabled via `[[rhai.rule]] use = "security.deny_symbol" params = {
//   kind = "constant",                       # or "*" for any kind
//   name = ["*Manager"],
//   name_re = ['(?i)^(PASSWORD|API_KEY|SECRET|TOKEN)$'],
//   message = "hard-coded secret-like name",
//   fix_hint = "load from env vars / a secret manager",
// }`.
const ADVICE = "Avoid declaring symbols that match the denied patterns.";

fn describe(_params) { "Certain symbol names/kinds are disallowed." }

fn check(sym) {
    let kind = params.kind ?? "*";
    if kind != "*" && sym.kind != kind { return false; }
    let hit = false;
    for g in (params.name ?? []) {
        if glob(g, sym.name) { hit = true; break; }
    }
    if !hit {
        for r in (params.name_re ?? []) {
            if regex_match(r, sym.name) { hit = true; break; }
        }
    }
    if hit {
        return #{
            message: params.message ?? ("symbol `" + sym.name + "` matches a denied pattern"),
            hint: params.fix_hint ?? "rename the symbol"
        };
    }
    false
}
