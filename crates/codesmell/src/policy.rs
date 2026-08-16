//! Policy model + loading + scope-aware override resolution.
//!
//! Policy lives in `.codesmell/policy.toml` (TOML, to match the codegraph
//! ecosystem). When a symbol is checked, [`Policy::effective_for`] returns a
//! policy with any matching `[[override]]` blocks merged in — implementing the
//! file → directory → module → repository resolution order from the design doc.

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
}

pub const RULE_MAX_LINES: &str = "style.function.max_lines";
pub const RULE_MAX_PARAMS: &str = "style.function.max_parameters";
pub const RULE_MAX_NESTING: &str = "style.function.max_nesting";
pub const RULE_NAMING: &str = "style.naming";
pub const RULE_BOUNDARY: &str = "architecture.boundary";
pub const RULE_MISSING_TEST: &str = "testing.missing_test";

// ==================== Style ====================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StyleFunction {
    pub max_lines: Option<u32>,
    pub max_parameters: Option<u32>,
    pub max_nesting: Option<u32>,
}

/// A naming convention: symbols of `kind` (optionally whose signature contains
/// `signature_contains`) must have a name matching `pattern` (a glob).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NamingRule {
    pub kind: String,
    pub pattern: String,
    pub signature_contains: Option<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StyleNaming {
    #[serde(rename = "rule")]
    pub rules: Vec<NamingRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Style {
    pub function: StyleFunction,
    pub naming: StyleNaming,
}

// ==================== Architecture ====================

/// A logical layer, identified by file paths (glob) rather than by naming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub paths: Vec<String>,
}

/// A boundary rule. `deny` edges are forbidden; `allow` documents permitted
/// edges (informational in MVP — only `deny` is enforced).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Boundary {
    pub deny: Vec<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub severity: Option<Severity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Architecture {
    #[serde(rename = "layer")]
    pub layers: Vec<Layer>,
    pub boundary: Vec<Boundary>,
}

// ==================== Testing ====================

/// Selector for "business logic" that must be tested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicSelector {
    #[serde(default)]
    pub layers: Vec<String>,
    #[serde(default)]
    pub min_lines: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Coverage {
    pub line: Option<u32>,
    pub branch: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Testing {
    #[serde(default)]
    pub require_tests_for_changed_logic: bool,
    #[serde(default)]
    pub test_paths: Vec<String>,
    #[serde(default)]
    pub logic_selectors: Vec<LogicSelector>,
    #[serde(default)]
    pub coverage: Coverage,
}

// ==================== Override + Policy ====================

/// A scoped policy override (doc §3). Applied to symbols whose file matches any
/// `paths` glob. Override is a shallow merge: scalars replace, rule/list fields
/// are appended.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Override {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub architecture: Architecture,
    #[serde(default)]
    pub testing: Testing,
    #[serde(default)]
    pub severity: HashMap<String, Severity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    #[serde(default)]
    pub version: u8,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub architecture: Architecture,
    #[serde(default)]
    pub testing: Testing,
    #[serde(default)]
    pub severity: HashMap<String, Severity>,
    #[serde(default)]
    pub overrides: Vec<Override>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            version: 1,
            style: Style::default(),
            architecture: Architecture::default(),
            testing: Testing::default(),
            severity: HashMap::new(),
            overrides: Vec::new(),
        }
    }
}

impl Policy {
    /// Resolve the policy effective for a given file (relative to repo root),
    /// merging every matching `[[override]]` block.
    pub fn effective_for(&self, rel_file: &str) -> Policy {
        let mut eff = Policy {
            version: self.version,
            style: self.style.clone(),
            architecture: self.architecture.clone(),
            testing: self.testing.clone(),
            severity: self.severity.clone(),
            overrides: Vec::new(),
        };
        for ov in &self.overrides {
            let hits = ov
                .paths
                .iter()
                .any(|p| crate::glob::glob_matches(p, rel_file));
            if !hits {
                continue;
            }
            if let Some(v) = ov.style.function.max_lines {
                eff.style.function.max_lines = Some(v);
            }
            if let Some(v) = ov.style.function.max_parameters {
                eff.style.function.max_parameters = Some(v);
            }
            if let Some(v) = ov.style.function.max_nesting {
                eff.style.function.max_nesting = Some(v);
            }
            eff.style.naming.rules.extend(ov.style.naming.rules.clone());
            eff.architecture
                .layers
                .extend(ov.architecture.layers.clone());
            eff.architecture
                .boundary
                .extend(ov.architecture.boundary.clone());
            if ov.testing.require_tests_for_changed_logic {
                eff.testing.require_tests_for_changed_logic = true;
            }
            eff.testing.test_paths.extend(ov.testing.test_paths.clone());
            eff.testing
                .logic_selectors
                .extend(ov.testing.logic_selectors.clone());
            for (k, v) in &ov.severity {
                eff.severity.insert(k.clone(), *v);
            }
        }
        eff
    }

    /// Severity for a rule id, falling back to the category default.
    pub fn severity_of(&self, rule_id: &str) -> Severity {
        self.severity
            .get(rule_id)
            .copied()
            .unwrap_or_else(|| default_severity(rule_id))
    }
}

/// Default severity per rule category (doc §11).
pub fn default_severity(rule_id: &str) -> Severity {
    match rule_id {
        RULE_BOUNDARY => Severity::Blocking,
        RULE_MISSING_TEST => Severity::Required,
        _ => Severity::Warning,
    }
}

/// Load `.codesmell/policy.toml` by walking up from `start`.
/// Returns `(policy, found_path)`; on missing/absent file a default policy is
/// returned (with `found_path = None`).
pub fn load_policy(start: &Path) -> (Policy, Option<PathBuf>) {
    let mut cur = Some(start.to_path_buf());
    while let Some(dir) = cur {
        let candidate = dir.join(".codesmell").join("policy.toml");
        if candidate.exists() {
            match std::fs::read_to_string(&candidate) {
                Ok(text) => match toml::from_str::<Policy>(&text) {
                    Ok(p) => return (p, Some(candidate)),
                    Err(e) => {
                        eprintln!(
                            "codesmell: warning: failed to parse {}: {e}; using defaults",
                            candidate.display()
                        );
                        return (Policy::default(), Some(candidate));
                    }
                },
                Err(e) => {
                    eprintln!(
                        "codesmell: warning: cannot read {}: {e}",
                        candidate.display()
                    );
                    return (Policy::default(), Some(candidate));
                }
            }
        }
        cur = dir.parent().map(|p| p.to_path_buf());
    }
    (Policy::default(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Policy {
        let toml = r#"
version = 1
[style.function]
max_lines = 60
max_parameters = 4

[[style.naming.rule]]
kind = "method"
pattern = "*Async"
signature_contains = "async"

[[architecture.layer]]
name = "controller"
paths = ["src/controllers/**"]

[[architecture.boundary]]
deny = ["controller -> repository"]

[testing]
require_tests_for_changed_logic = true
test_paths = ["tests/**"]
"#;
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn naming_rule_loads_from_toml_array() {
        let p = base();
        assert_eq!(p.style.naming.rules.len(), 1);
        assert_eq!(p.style.naming.rules[0].pattern, "*Async");
        assert_eq!(p.architecture.layers.len(), 1);
        assert_eq!(p.architecture.boundary.len(), 1);
    }

    #[test]
    fn effective_for_merges_override_scalars_and_rules() {
        let mut p = base();
        p.overrides.push(Override {
            paths: vec!["legacy/**".into()],
            style: Style {
                function: StyleFunction {
                    max_lines: Some(120),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        });
        // Outside legacy: base limits win.
        let eff = p.effective_for("src/services/order.rs");
        assert_eq!(eff.style.function.max_lines, Some(60));
        // Inside legacy: override wins, other base fields preserved.
        let eff = p.effective_for("legacy/order.rs");
        assert_eq!(eff.style.function.max_lines, Some(120));
        assert_eq!(eff.style.function.max_parameters, Some(4));
        assert_eq!(eff.style.naming.rules.len(), 1);
    }

    #[test]
    fn severity_defaults_by_category() {
        assert_eq!(default_severity(RULE_BOUNDARY), Severity::Blocking);
        assert_eq!(default_severity(RULE_MISSING_TEST), Severity::Required);
        assert_eq!(default_severity(RULE_MAX_LINES), Severity::Warning);
    }

    #[test]
    fn severity_override_is_respected() {
        let mut p = base();
        p.severity
            .insert(RULE_MAX_LINES.to_string(), Severity::Blocking);
        assert_eq!(p.severity_of(RULE_MAX_LINES), Severity::Blocking);
        // unspecified rule keeps its category default
        assert_eq!(p.severity_of(RULE_BOUNDARY), Severity::Blocking);
    }
}
