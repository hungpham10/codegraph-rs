// security.crypto_weak_hash — weak hashes and non-CSPRNGs for security values.
const ADVICE = "Use SHA-256+ for hashes and a CSPRNG for tokens — never md5/sha1/Math.random.";

fn check_calls(sym, callees, callers) {
    let weak = callees.filter(|c| {
        let l = c.name.to_lower();
        l.contains("md5") || l.contains("sha1") || l == "sha-1"
            || l.contains("digest_md5") || l.contains("md5crypt")
    });
    let risky_rand = (callees.filter(|c| {
        let l = c.name.to_lower();
        l.contains("random") || l.contains("rand")
    }).len() > 0)
        && (sym.name.to_lower().contains("token") || sym.name.to_lower().contains("secret")
            || sym.name.to_lower().contains("nonce") || sym.name.to_lower().contains("password")
            || sym.name.to_lower().contains("session") || sym.name.to_lower().contains("key"));
    if weak.len() > 0 {
        return #{
            message: "`" + sym.name + "` uses weak hash `" + weak[0].name + "`",
            hint: "use SHA-256 or stronger"
        };
    }
    if risky_rand {
        return #{
            message: "`" + sym.name + "` uses a non-cryptographic RNG for security values",
            hint: "use a CSPRNG (e.g. crypto/rand, secrets module)"
        };
    }
    false
}
