//! Built-in policy packs, installed by `codesmell pack add`.
//!
//! A pack is a set of rule scripts plus a policy fragment (the `[[rhai.rule]]`
//! entries that enable + configure them). Both are embedded in the binary;
//! [`add_pack`] copies them into the repository — scripts into
//! `.codesmell/rules/` and the fragment into `.codesmell/packs/<name>.policy.toml`
//! — so they can be edited or removed like any local config.

use std::path::Path;

use anyhow::Context;

/// A policy pack: rule scripts + a TOML fragment that enables them.
pub struct Pack {
    pub name: &'static str,
    pub description: &'static str,
    /// TOML fragment to merge (appended to `.codesmell/packs/<name>.policy.toml`).
    pub fragment: &'static str,
    /// `(file_name, source)` pairs copied into `.codesmell/rules/`.
    pub rules: &'static [(&'static str, &'static str)],
}

/// All built-in packs.
pub fn builtin_packs() -> &'static [Pack] {
    &[SECURITY_PACK]
}

/// Demo security pack: dangerous calls, weak crypto, unsafe deserialization.
/// Domain coverage is intentionally small to prove the mechanism; extend by
/// adding scripts + entries here.
pub const SECURITY_PACK: Pack = Pack {
    name: "security",
    description: "Dangerous calls, weak crypto, unsafe deserialization (demo pack).",
    fragment: include_str!("../packs/security/policy.fragment.toml"),
    rules: &[
        (
            "security.dangerous_exec.rhai",
            include_str!("../packs/security/rules/security.dangerous_exec.rhai"),
        ),
        (
            "security.crypto_weak_hash.rhai",
            include_str!("../packs/security/rules/security.crypto_weak_hash.rhai"),
        ),
        (
            "security.dangerous_deserialize.rhai",
            include_str!("../packs/security/rules/security.dangerous_deserialize.rhai"),
        ),
    ],
};

/// Copy a pack's scripts + fragment into `root` (existing files are left
/// untouched so local edits are never silently overwritten).
pub fn add_pack(root: &Path, pack: &Pack) -> anyhow::Result<()> {
    let rules_dir = root.join(".codesmell").join("rules");
    std::fs::create_dir_all(&rules_dir)
        .with_context(|| format!("creating {}", rules_dir.display()))?;
    for (file_name, src) in pack.rules {
        let dest = rules_dir.join(file_name);
        if dest.exists() {
            eprintln!(
                "codesmell: `{file_name}` already exists; not overwriting.",
            );
        } else {
            std::fs::write(&dest, src).with_context(|| format!("writing {}", dest.display()))?;
            println!("codesmell: wrote {}", dest.display());
        }
    }

    let packs_dir = root.join(".codesmell").join("packs");
    std::fs::create_dir_all(&packs_dir)
        .with_context(|| format!("creating {}", packs_dir.display()))?;
    let frag_dest = packs_dir.join(format!("{}.policy.toml", pack.name));
    if frag_dest.exists() {
        eprintln!(
            "codesmell: `{}.policy.toml` already exists; not overwriting.",
            pack.name
        );
    } else {
        std::fs::write(&frag_dest, pack.fragment)
            .with_context(|| format!("writing {}", frag_dest.display()))?;
        println!("codesmell: wrote {}", frag_dest.display());
    }
    println!(
        "\nRun `codesmell policy` to see the merged policy, or edit the files under `.codesmell/`."
    );
    Ok(())
}
