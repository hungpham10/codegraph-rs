//! Policy model + loading + pack-fragment merge.
//!
//! Policy lives in `.codesmell/policy.toml` (TOML, to match the codegraph
//! ecosystem). Every rule is a rhai script — builtin scripts ship inside the
//! binary, custom scripts live in rule dirs (default `.codesmell/rules/`).
//! A script only runs when an `[[rhai.rule]]` entry references it; `params`
//! parameterizes the script, so a script is a reusable rule template and the
//! policy file is its configuration.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Severity of a violation, ordered least → most serious.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    #[default]
    Required,
    Blocking,
}

impl Severity {
    /// Human-facing label used in `warning[...]` / `error[...]` output.
    pub fn as_label(self) -> &'static str {
        match self {
            Severity::Info => "note",
            Severity::Warning => "warning",
            Severity::Required => "error",
            Severity::Blocking => "error",
        }
    }

    /// Parse a severity label (case-insensitive) — used by rhai rule results.
    pub fn parse_label(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "info" => Severity::Info,
            "warning" => Severity::Warning,
            "required" => Severity::Required,
            "blocking" => Severity::Blocking,
            _ => return None,
        })
    }
}

/// Ids of the rule scripts embedded in the binary (`rules_builtin/*.rhai`).
/// Kept as constants because tests, docs and the `[severity]` map refer to
/// them; a script's id is simply its file stem.
pub const RULE_MAX_LINES: &str = "style.function.max_lines";
pub const RULE_MAX_PARAMS: &str = "style.function.max_parameters";
pub const RULE_MAX_NESTING: &str = "style.function.max_nesting";
pub const RULE_MAX_COMPLEXITY: &str = "style.function.max_complexity";
pub const RULE_NAMING: &str = "style.naming";
pub const RULE_BOUNDARY: &str = "architecture.boundary";
pub const RULE_MISSING_TEST: &str = "testing.missing_test";
pub const RULE_DENY_CALL: &str = "security.deny_call";
pub const RULE_DENY_SYMBOL: &str = "security.deny_symbol";

// ==================== Rule entries ====================

/// One `[[rhai.rule]]` entry: enables + configures a rhai rule script.
///
/// `use` names the script (its id / file stem — builtin or from a rule dir).
/// The same script may be referenced several times with different `params`
/// (e.g. a stricter limit for new code); `id` optionally renames the resulting
/// violations so entries can be told apart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleEntry {
    /// Script id to enable (builtin id or `.rhai` file stem).
    #[serde(rename = "use")]
    pub use_script: String,
    /// Display rule id for violations (defaults to `use`).
    pub id: Option<String>,
    /// Free-form parameters injected into the script as the `params` map.
    pub params: Option<toml::Value>,
    /// Only apply to symbols whose file matches one of these globs
    /// (repo-relative; empty = every file).
    pub paths: Vec<String>,
    /// Exclude symbols whose file matches any of these globs (wins over
    /// `paths`).
    pub exclude: Vec<String>,
    /// Severity override for violations from this entry.
    pub severity: Option<Severity>,
}

fn default_rule_dirs() -> Vec<String> {
    vec![".codesmell/rules".to_string()]
}

/// The `[rhai]` section: where rule scripts are found + which are enabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RhaiSection {
    /// Directories (relative to the repo root) scanned for `*.rhai` rule
    /// scripts. A user script with the same id as a builtin overrides it.
    #[serde(default = "default_rule_dirs")]
    pub rule_dirs: Vec<String>,
    /// Enabled rule entries; a script runs only when referenced here.
    #[serde(default, rename = "rule")]
    pub rules: Vec<RuleEntry>,
}

impl Default for RhaiSection {
    fn default() -> Self {
        RhaiSection {
            rule_dirs: default_rule_dirs(),
            rules: Vec::new(),
        }
    }
}

// ==================== Policy ====================

/// One `[[include]]` entry in `policy.toml`: pulls a pack into the policy by
/// reference. Its `policy.fragment.toml` is merged into the policy and its
/// `rules/` directory is scanned for rhai scripts — without copying the pack's
/// files into the repository.
///
/// Set exactly one of `name` (resolved against a registry) or `path` (a local
/// pack directory, absolute or relative to the repo root).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IncludeEntry {
    /// Pack name in the registry (resolved via `--registry` / `CODESMELL_REGISTRY`
    /// / `~/.config/codesmell/config.toml`).
    pub name: Option<String>,
    /// Direct path to a pack directory (absolute, or relative to the repo root).
    pub path: Option<String>,
    /// Override the registry source for this include only.
    pub registry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub version: u8,
    #[serde(default)]
    pub rhai: RhaiSection,
    /// Per-rule-id severity overrides (win over each entry's default).
    pub severity: HashMap<String, Severity>,
    /// Packs pulled in by reference (see [`IncludeEntry`]); serialized as the
    /// `[[include]]` table in `policy.toml`.
    #[serde(default, rename = "include")]
    pub includes: Vec<IncludeEntry>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            version: 1,
            rhai: RhaiSection::default(),
            severity: HashMap::new(),
            includes: Vec::new(),
        }
    }
}

impl Policy {
    /// Severity for a rule id, falling back to the category default.
    pub fn severity_of(&self, rule_id: &str) -> Severity {
        self.severity
            .get(rule_id)
            .copied()
            .unwrap_or_else(|| default_severity(rule_id))
    }
}

/// Default severity per rule id (doc §11). Security rules fail the build by
/// default; everything else is a warning unless configured otherwise.
pub fn default_severity(rule_id: &str) -> Severity {
    match rule_id {
        RULE_BOUNDARY => Severity::Blocking,
        RULE_MISSING_TEST => Severity::Required,
        _ if rule_id.starts_with("security.") => Severity::Required,
        _ => Severity::Warning,
    }
}

// ==================== Loading ====================

/// Load `.codesmell/policy.toml` by walking up from `start`, then merge every
/// `.codesmell/packs/*.policy.toml` fragment installed next to it (see
/// `codesmell pack add`). Fragment rule entries are appended and their
/// `[severity]` wins; scalars of the main policy are never overwritten.
///
/// Returns `(policy, found_path)`; on missing/absent file a default policy is
/// returned (with `found_path = None`).
pub fn load_policy(start: &Path) -> (Policy, Option<PathBuf>) {
    let mut cur = Some(start.to_path_buf());
    while let Some(dir) = cur {
        let candidate = dir.join(".codesmell").join("policy.toml");
        if candidate.exists() {
            let mut policy = match std::fs::read_to_string(&candidate) {
                Ok(text) => match toml::from_str::<Policy>(&text) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!(
                            "codesmell: warning: failed to parse {}: {e}; using defaults",
                            candidate.display()
                        );
                        Policy::default()
                    }
                },
                Err(e) => {
                    eprintln!(
                        "codesmell: warning: cannot read {}: {e}",
                        candidate.display()
                    );
                    Policy::default()
                }
            };
            merge_pack_fragments(&mut policy, &dir.join(".codesmell").join("packs"));
            return (policy, Some(candidate));
        }
        cur = dir.parent().map(|p| p.to_path_buf());
    }
    (Policy::default(), None)
}

/// Merge a single pack fragment (TOML text) into `policy`: append its
/// `[[rhai.rule]]` entries and let its `[severity]` overrides win. A fragment
/// that fails to parse is reported loudly and skipped — a silently inactive
/// security pack is a hole.
pub(crate) fn merge_fragment_text(policy: &mut Policy, text: &str) -> bool {
    match toml::from_str::<Policy>(text) {
        Ok(frag) => {
            policy.rhai.rules.extend(frag.rhai.rules);
            for (k, v) in frag.severity {
                policy.severity.insert(k, v);
            }
            true
        }
        Err(e) => {
            eprintln!("codesmell: warning: failed to parse pack fragment: {e}; fragment skipped");
            false
        }
    }
}

/// Merge every `*.policy.toml` fragment under `packs_dir` into `policy`
/// (sorted by file name for determinism).
fn merge_pack_fragments(policy: &mut Policy, packs_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(packs_dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".policy.toml"))
        })
        .collect();
    files.sort();
    for file in files {
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "codesmell: warning: cannot read pack fragment {}: {e}",
                    file.display()
                );
                continue;
            }
        };
        merge_fragment_text(policy, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_entries_load_from_toml() {
        let toml = r#"
version = 1

[[rhai.rule]]
use = "style.function.max_lines"
params = { max = 60 }

[[rhai.rule]]
use = "style.function.max_lines"
id = "style.legacy_max_lines"
params = { max = 120 }
paths = ["legacy/**"]

[[rhai.rule]]
use = "security.no_eval"
severity = "blocking"

[severity]
"style.function.max_lines" = "info"
"#;
        let p: Policy = toml::from_str(toml).unwrap();
        assert_eq!(p.rhai.rules.len(), 3);
        assert_eq!(p.rhai.rules[0].use_script, "style.function.max_lines");
        assert_eq!(p.rhai.rule_dirs, vec![".codesmell/rules".to_string()]);
        // entry severity override
        assert_eq!(p.rhai.rules[2].severity, Some(Severity::Blocking));
        // [severity] map override + defaults
        assert_eq!(p.severity_of(RULE_MAX_LINES), Severity::Info);
        assert_eq!(p.severity_of(RULE_BOUNDARY), Severity::Blocking);
        assert_eq!(p.severity_of("security.custom"), Severity::Required);
        assert_eq!(p.severity_of(RULE_MAX_PARAMS), Severity::Warning);
    }

    #[test]
    fn custom_rule_dirs_are_respected() {
        let p: Policy = toml::from_str("[rhai]\nrule_dirs = [\"policies\"]").unwrap();
        assert_eq!(p.rhai.rule_dirs, vec!["policies".to_string()]);
        assert!(p.rhai.rules.is_empty());
    }

    #[test]
    fn pack_fragments_append_rules_and_win_severity() {
        let dir = tempfile::tempdir().unwrap();
        let packs = dir.path().join("packs");
        std::fs::create_dir_all(&packs).unwrap();
        std::fs::write(
            packs.join("a.policy.toml"),
            "[[rhai.rule]]\nuse = \"security.no_eval\"\n\n[severity]\n\"security.no_eval\" = \"blocking\"",
        )
        .unwrap();
        let mut p = Policy::default();
        p.rhai.rules.push(RuleEntry {
            use_script: RULE_MAX_LINES.into(),
            ..Default::default()
        });
        merge_pack_fragments(&mut p, &packs);
        assert_eq!(p.rhai.rules.len(), 2);
        assert_eq!(p.severity_of("security.no_eval"), Severity::Blocking);

        // A broken fragment is skipped loudly, not merged.
        std::fs::write(packs.join("b.policy.toml"), "not [ valid toml").unwrap();
        let mut p2 = Policy::default();
        merge_pack_fragments(&mut p2, &packs);
        assert_eq!(p2.rhai.rules.len(), 1); // only a.policy.toml's entry
    }
}
