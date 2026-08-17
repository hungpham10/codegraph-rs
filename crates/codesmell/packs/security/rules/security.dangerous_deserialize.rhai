// security.dangerous_deserialize — unsafe native deserialization.
const ADVICE = "Never deserialize untrusted data with native formats (pickle/yaml.load/Marshal/readObject).";

fn check_calls(sym, callees, callers) {
    let sinks = ["pickle.loads", "pickle.load", "yaml.load", "marshal.load", "unserialize", "read_object", "objectinputstream"];
    let hit = callees.filter(|c| {
        let l = c.name.to_lower();
        let mut found = false;
        for k in sinks {
            if l.contains(k) { found = true; break; }
        }
        found
    });
    if hit.len() > 0 {
        return #{
            message: "`" + sym.name + "` deserializes with `" + hit[0].name + "`",
            hint: "use a data-only format (JSON) with schema validation"
        };
    }
    false
}
