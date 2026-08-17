// security.dangerous_exec — dynamic execution sinks (exec/system/popen/spawn).
// Skips files matching `params.skip_files_containing` (e.g. tests that shell out on purpose).
const ADVICE = "Never shell out via exec/system/popen with untrusted input — use explicit APIs.";

fn check_calls(sym, callees, callers) {
    let skip = params.skip_files_containing ?? [];
    for s in skip {
        if sym.file.contains(s) { return false; }
    }
    let sinks = ["exec", "system", "popen", "spawn", "shell_exec", "run_command"];
    let hit = callees.filter(|c| {
        let l = c.name.to_lower();
        let found = false;
        for k in sinks {
            if l.contains(k) { found = true; break; }
        }
        found
    });
    if hit.len() > 0 {
        return #{
            message: "`" + sym.name + "` calls `" + hit[0].name + "` (dynamic execution sink)",
            hint: "replace with an explicit API call or a proper parser"
        };
    }
    false
}
