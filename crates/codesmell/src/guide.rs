//! Render the conventions pack an LLM reads before writing code, plus the
//! starter `policy.toml` template emitted by `codesmell init`.

use crate::policy::Policy;
use crate::rhai::RhaiRuleLib;

/// Human/LLM-readable conventions pack (doc §9).
///
/// One line per enabled rule: `describe(params)` if the script defines it,
/// else the script's `ADVICE` constant, else a generic pointer to the entry.
/// This is the PREVENT side of the secure-vibe model (rules for the agent).
pub fn render_guide(policy: &Policy, lib: Option<&RhaiRuleLib>) -> String {
    let mut lines = vec![
        "# Repository conventions (CodeSmell)".to_string(),
        String::new(),
        "Before writing or modifying code, follow these conventions:".to_string(),
        String::new(),
    ];
    let mut n: u32 = 1;

    match lib {
        Some(lib) => {
            for inst in lib.instances(policy) {
                let line = lib
                    .describe(&inst)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        if inst.advice.is_empty() {
                            format!(
                                "Rule `{}` is enabled (see [[rhai.rule]] use = \"{}\").",
                                inst.rule_id, inst.use_script
                            )
                        } else {
                            inst.advice.clone()
                        }
                    });
                lines.push(format!("{n}. {line}"));
                n += 1;
            }
        }
        None => {
            for e in &policy.rhai.rules {
                if !e.use_script.is_empty() {
                    lines.push(format!("{n}. Rule `{}` is enabled.", e.use_script));
                    n += 1;
                }
            }
        }
    }

    if n == 1 {
        lines.push("No team conventions are configured yet. Run `codesmell init` to start.".into());
    }
    lines.join("\n")
}

/// Starter `.codesmell/policy.toml` written by `codesmell init`.
pub const STARTER_POLICY: &str = r#"# CodeSmell policy — team engineering conventions.
# Run `codesmell guide` to print the conventions pack for the LLM.
#
# Every rule is a rhai script (builtin, or your own under .codesmell/rules/).
# `[[rhai.rule]]` enables + configures one; `params` is the script's input.

version = 1

[rhai]
rule_dirs = [".codesmell/rules"]

# --- style ---
[[rhai.rule]]
use = "style.function.max_lines"
params = { max = 60 }

[[rhai.rule]]
use = "style.function.max_parameters"
params = { max = 4 }

[[rhai.rule]]
use = "style.function.max_nesting"
params = { max = 4 }

[[rhai.rule]]
use = "style.function.max_complexity"
params = { max = 10 }

[[rhai.rule]]
use = "style.naming"
params = { rules = [{ kind = "method", pattern = "*Async", signature_contains = "async" }, { kind = "class", pattern = "*Service" }] }

# --- architecture ---
[[rhai.rule]]
use = "architecture.boundary"
params = { layers = [{ name = "controller", paths = ["src/controllers/**", "**/*Controller.java"] }, { name = "service", paths = ["src/services/**", "**/*Service.java"] }, { name = "repository", paths = ["src/repositories/**", "**/*Repository.java"] }], deny = ["controller -> repository"] }

# --- testing ---
[[rhai.rule]]
use = "testing.missing_test"
params = { require = true, test_paths = ["tests/**", "**/*_test.rs", "**/*_test.go", "**/test_*.py"], selectors = [{ layers = ["service"] }, { min_lines = 20 }] }

# --- security (see `codesmell pack add security`) ---
# [[rhai.rule]]
# use = "security.deny_call"
# params = { deny = ["eval", "exec", "system"], message = "dynamic code execution" }

[severity]                     # per-rule-id severity overrides
"style.function.max_lines" = "warning"
"architecture.boundary" = "blocking"
"testing.missing_test" = "required"
"#;

/// Suggested AGENTS.md / CLAUDE.md snippet.
pub const AGENTS_SNIPPET: &str = r#"## CodeSmell conventions
Run `codesmell guide` before writing or modifying code, and `codesmell check` (or
`codesmell check --diff -`) after, then fix every violation by severity.
"#;
