//! Render the conventions pack an LLM reads before writing code, plus the
//! starter `policy.toml` template emitted by `codesmell init`.

use crate::policy::Policy;

/// Human/LLM-readable conventions pack (doc §9).
pub fn render_guide(policy: &Policy) -> String {
    let mut lines = vec![
        "# Repository conventions (CodeSmell)".to_string(),
        String::new(),
        "Before writing or modifying code, follow these conventions:".to_string(),
        String::new(),
    ];
    let mut n: u32 = 1;
    let s = &policy.style;

    if let Some(m) = s.function.max_lines {
        lines.push(format!("{n}. Functions normally stay below {m} lines."));
        n += 1;
    }
    if let Some(m) = s.function.max_parameters {
        lines.push(format!(
            "{n}. Functions normally take at most {m} parameters."
        ));
        n += 1;
    }
    if let Some(m) = s.function.max_nesting {
        lines.push(format!("{n}. Avoid nesting deeper than {m} levels."));
        n += 1;
    }
    for nr in &s.naming.rules {
        if let Some(sig) = &nr.signature_contains {
            lines.push(format!(
                "{n}. `{sig}` symbols must match the naming pattern `{pattern}`.",
                pattern = nr.pattern
            ));
        } else {
            lines.push(format!(
                "{n}. `{kind}` symbols should match the naming pattern `{pattern}`.",
                kind = nr.kind,
                pattern = nr.pattern
            ));
        }
        n += 1;
    }
    for b in &policy.architecture.boundary {
        for d in &b.deny {
            lines.push(format!("{n}. Layer boundary denied: `{d}`."));
            n += 1;
        }
    }
    if policy.testing.require_tests_for_changed_logic {
        lines.push(format!(
            "{n}. New or changed business logic requires a unit test."
        ));
        n += 1;
    }
    if !policy.testing.test_paths.is_empty() {
        lines.push(format!(
            "{n}. Tests live in: {}.",
            policy.testing.test_paths.join(", ")
        ));
        n += 1;
    }
    if n == 1 {
        lines.push("No team conventions are configured yet. Run `codesmell init` to start.".into());
    }
    lines.join("\n")
}

/// Starter `.codesmell/policy.toml` written by `codesmell init`.
pub const STARTER_POLICY: &str = r#"# CodeSmell policy — team engineering conventions.
# Run `codesmell guide` to print the conventions pack for an LLM.
version = 1

[style.function]
# max_lines = 60
# max_parameters = 4
# max_nesting = 4

# [[style.naming.rule]]
# kind = "class"
# pattern = "*Service"
#
# [[style.naming.rule]]
# kind = "method"
# pattern = "*Async"
# signature_contains = "async"

# [[architecture.layer]]
# name = "controller"
# paths = ["src/controllers/**", "**/*Controller.java"]
#
# [[architecture.layer]]
# name = "service"
# paths = ["src/services/**", "**/*Service.java"]
#
# [[architecture.layer]]
# name = "repository"
# paths = ["src/repositories/**", "**/*Repository.java"]
#
# [[architecture.boundary]]
# deny = ["controller -> repository"]
# allow = ["controller -> service", "service -> repository"]

[testing]
# require_tests_for_changed_logic = true
# test_paths = ["tests/**", "**/*_test.go", "**/*_test.rs", "**/test_*.py", "**/*Test.java"]
# logic_selectors = [{ layers = ["service"] }, { min_lines = 20 }]

# [testing.coverage]   # reserved; not enforced in MVP
# line = 80

# Per-area overrides (doc §3): file → directory → module → repository.
# [[override]]
# paths = ["legacy/**"]
# [override.style.function]
# max_lines = 120
"#;

/// Suggested AGENTS.md / CLAUDE.md snippet.
pub const AGENTS_SNIPPET: &str = r#"## CodeSmell conventions
Run `codesmell guide` before writing or modifying code, and `codesmell check` (or
`codesmell check --diff -`) after, then fix every violation by severity.
"#;
