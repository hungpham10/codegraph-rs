//! Built-in policy packs, installed by `codesmell pack add`.
//!
//! A pack is a set of rule scripts plus a policy fragment (the `[[rhai.rule]]`
//! entries that enable + configure them). Both are embedded in the binary;
//! [`add_pack`] copies them into the repository — scripts into
//! `.codesmell/rules/` and the fragment into `.codesmell/packs/<name>.policy.toml`
//! — so they can be edited or removed like any local config.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::policy::{merge_fragment_text, Policy};
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
            eprintln!("codesmell: `{file_name}` already exists; not overwriting.",);
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

// ==================== Registry (path or git URL, cached) ====================

/// A resolved registry source that holds pack directories.
#[derive(Clone)]
pub enum Registry {
    /// A local directory containing one subdirectory per pack.
    Path(PathBuf),
    /// A git repository; packs are read from a per-machine clone in `cache`.
    Git { url: String, cache: PathBuf },
}

impl Registry {
    /// Human-readable source description for listings.
    pub fn describe(&self) -> String {
        match self {
            Registry::Path(p) => p.display().to_string(),
            Registry::Git { url, .. } => url.clone(),
        }
    }

    /// Build a local-path registry.
    pub fn path(p: PathBuf) -> Self {
        Registry::Path(p)
    }

    /// Build a git registry with an explicit cache directory.
    pub fn git(url: impl Into<String>, cache: PathBuf) -> Self {
        Registry::Git {
            url: url.into(),
            cache,
        }
    }
}

/// Resolve the registry source (priority: explicit flag > `CODESMELL_REGISTRY`
/// env > `~/.config/codesmell/config.toml`). Errors if nothing is configured.
pub fn resolve_registry(explicit: Option<&str>) -> anyhow::Result<Registry> {
    let raw = explicit
        .map(|s| s.to_string())
        .or_else(|| std::env::var("CODESMELL_REGISTRY").ok())
        .or_else(config_registry)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no codesmell registry configured. Set --registry, the CODESMELL_REGISTRY \
                 environment variable, or `registry = \"...\"` in ~/.config/codesmell/config.toml"
            )
        })?;
    classify_registry(raw.trim())
}

/// Like [`resolve_registry`] but returns `None` when no registry is configured
/// (used by check/guide, which only need a registry if a `[[include]]` actually
/// requires one).
pub fn resolve_registry_opt(explicit: Option<&str>) -> Option<Registry> {
    resolve_registry(explicit).ok()
}

fn classify_registry(raw: &str) -> anyhow::Result<Registry> {
    if is_git_url(raw) {
        Ok(Registry::Git {
            url: raw.to_string(),
            cache: registry_cache_dir(raw)?,
        })
    } else {
        Ok(Registry::Path(PathBuf::from(raw)))
    }
}

fn is_git_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("git@")
        || s.starts_with("ssh://")
        || s.starts_with("file://")
        || s.contains("://")
        || s.ends_with(".git")
}

fn config_registry() -> Option<String> {
    let dir = dirs::config_dir()?;
    let path = dir.join("codesmell").join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    let val: toml::Value = toml::from_str(&text).ok()?;
    val.get("registry")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn registry_cache_dir(url: &str) -> anyhow::Result<PathBuf> {
    let cache =
        dirs::cache_dir().ok_or_else(|| anyhow::anyhow!("cannot determine cache directory"))?;
    let key: String = url
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(cache.join("codesmell").join("registry").join(key))
}

/// Resolve a pack's directory from a registry (cloning the git cache on first
/// use if needed).
pub fn resolve_pack_dir(registry: &Registry, name: &str) -> anyhow::Result<PathBuf> {
    match registry {
        Registry::Path(base) => {
            let dir = base.join(name);
            if dir.is_dir() {
                Ok(dir)
            } else {
                anyhow::bail!(
                    "pack `{name}` not found in registry path {}",
                    base.display()
                )
            }
        }
        Registry::Git { url, cache } => {
            ensure_cloned(url, cache)?;
            let dir = cache.join(name);
            if dir.is_dir() {
                Ok(dir)
            } else {
                anyhow::bail!("pack `{name}` not found in registry {url}")
            }
        }
    }
}

fn ensure_cloned(url: &str, cache: &Path) -> anyhow::Result<()> {
    if cache.join(".git").exists() {
        return Ok(());
    }
    if let Some(parent) = cache.parent() {
        std::fs::create_dir_all(parent)?;
    }
    run_git(&[
        "clone".into(),
        "--depth".into(),
        "1".into(),
        url.into(),
        cache.to_string_lossy().into_owned(),
    ])
}

fn run_git(args: &[String]) -> anyhow::Result<()> {
    let status = Command::new("git")
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run `git`: {e} (is git installed?)"))?;
    if !status.success() {
        anyhow::bail!("git {} failed", args.join(" "));
    }
    Ok(())
}

/// Resolve every `[[include]]` in `policy`: merge each referenced pack's
/// fragment into the policy and register its `rules/` directory for the rhai
/// engine (appended to `policy.rhai.rule_dirs`). Pack files are NOT copied into
/// the repo — they are read directly from the (possibly cached) registry.
pub fn expand_includes(
    policy: &mut Policy,
    root: &Path,
    registry: Option<&Registry>,
) -> anyhow::Result<()> {
    if policy.includes.is_empty() {
        return Ok(());
    }
    let includes = std::mem::take(&mut policy.includes);
    for inc in includes {
        let pack_dir = if let Some(p) = &inc.path {
            let dir = if Path::new(p).is_absolute() {
                PathBuf::from(p)
            } else {
                root.join(p)
            };
            if !dir.is_dir() {
                anyhow::bail!("include path `{}` is not a directory", p);
            }
            dir
        } else if let Some(name) = &inc.name {
            let reg = match &inc.registry {
                Some(r) => classify_registry(r.trim())?,
                None => registry
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "include `{name}` needs a registry; pass --registry, set CODESMELL_REGISTRY, \
                             or add `registry = \"...\"` to ~/.config/codesmell/config.toml"
                        )
                    })?
                    .clone(),
            };
            resolve_pack_dir(&reg, name)?
        } else {
            anyhow::bail!("[[include]] needs `name` or `path`");
        };

        let frag = pack_dir.join("policy.fragment.toml");
        if frag.exists() {
            let text = std::fs::read_to_string(&frag).map_err(|e| {
                anyhow::anyhow!("cannot read pack fragment {}: {e}", frag.display())
            })?;
            merge_fragment_text(policy, &text);
        } else {
            eprintln!(
                "codesmell: warning: include pack at {} has no policy.fragment.toml; skipping its rules",
                pack_dir.display()
            );
        }

        let rules = pack_dir.join("rules");
        if rules.is_dir() {
            policy
                .rhai
                .rule_dirs
                .push(rules.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

/// Metadata for a pack available in a registry.
#[derive(Debug, Clone)]
pub struct PackInfo {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackManifest {
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
}

/// List packs available in a registry (cloning a git registry on first use).
pub fn list_registry_packs(registry: &Registry) -> anyhow::Result<Vec<PackInfo>> {
    let base = match registry {
        Registry::Path(p) => p.clone(),
        Registry::Git { url, cache } => {
            ensure_cloned(url, cache)?;
            cache.clone()
        }
    };
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Ok(out);
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let mut info = PackInfo {
            name: name.clone(),
            description: "—".to_string(),
            version: None,
        };
        if let Some(meta) = read_pack_manifest(&dir) {
            if let Some(n) = meta.name {
                info.name = n;
            }
            if let Some(d) = meta.description {
                info.description = d;
            }
            info.version = meta.version;
        }
        out.push(info);
    }
    Ok(out)
}

fn read_pack_manifest(dir: &Path) -> Option<PackManifest> {
    let text = std::fs::read_to_string(dir.join("pack.toml")).ok()?;
    toml::from_str::<PackManifest>(&text).ok()
}

/// Refresh a git registry (no-op for a local path registry).
pub fn update_registry(registry: &Registry) -> anyhow::Result<()> {
    match registry {
        Registry::Path(_) => {
            println!("codesmell: registry is a local path; nothing to update.");
            Ok(())
        }
        Registry::Git { url, cache } => {
            if cache.join(".git").exists() {
                run_git(&[
                    "-C".into(),
                    cache.to_string_lossy().into_owned(),
                    "pull".into(),
                    "--ff-only".into(),
                ])?;
            } else {
                if let Some(parent) = cache.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                run_git(&[
                    "clone".into(),
                    "--depth".into(),
                    "1".into(),
                    url.clone(),
                    cache.to_string_lossy().into_owned(),
                ])?;
            }
            Ok(())
        }
    }
}

/// Pre-fetch a pack into the local cache (useful for offline CI). No files are
/// copied into the repository; the pack stays in the registry cache.
pub fn pull_pack(registry: &Registry, name: &str) -> anyhow::Result<PathBuf> {
    let dir = resolve_pack_dir(registry, name)?;
    println!("codesmell: pack `{}` available at {}", name, dir.display());
    Ok(dir)
}
