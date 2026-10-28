//! `codegraph init --agent <id>`: sinh file hướng dẫn riêng cho agent với một
//! workflow viết code tối giản dựa trên CodeGraph (mô hình hoá theo AGENTS.md
//! của chính repo codegraph: MCP-first trước grep, verify có thứ tự).

use camino::Utf8PathBuf;

/// Nội dung AGENTS.md cho workflow viết code. Marker đầu file để installer/init
/// nhận ra nội dung do mình sinh (idempotent update).
pub const WORKFLOW_MD: &str = r#"<!-- codegraph:workflow v1 -->

# CodeGraph — Coding Workflow

This project has a CodeGraph MCP server (`.codegraph/` index). CodeGraph gives
sub-millisecond structural answers (symbols, call chains, impact) from a full
AST parse. **Always query CodeGraph before grep/read** when the question is
about code structure — grep only for literal text.

## Workflow for any code change

1. **Understand** — before editing, ground the task in the graph:
   - `codegraph_context {query: "<feature>"}` — packed context, one call.
   - `codegraph_impact {node}` on hot symbols — what breaks downstream.
2. **Edit** — write code that reads like the surrounding code. Read the
   target file before editing; don't edit from search snippets alone.
3. **Verify** — in order, after every meaningful change:
   - formatter (`cargo fmt` / project equivalent)
   - linter (`cargo clippy` / project equivalent)
   - the tests covering the changed area first, then the full suite
   - never claim "verified" from a subset of passing tests
4. **Report** — cite `file:line` for every claim; state what was run and what
   failed, plainly. No hedging on green results.

## Notes

- The index is a **snapshot**. After editing code, re-index (`codegraph_index {}`)
  or accept stale results; read the file to confirm current state.
- Duplicate symbol names return `ambiguous: true` — retry with the numeric `id`.
- If tools refuse: the session is unbound. Call `codegraph_init {"path": ...}`
  then `codegraph_index {}`.
"#;

/// Các target được hỗ trợ bởi `codegraph init --agent`.
pub const KNOWN_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "zcode",
    "cursor",
    "hermes",
    "antigravity",
];

/// Frontmatter riêng cho từng agent (`.mdc` của Cursor, Antigravity cần
/// trigger header). `None` = ghi plain markdown.
fn frontmatter(target: &str) -> Option<&'static str> {
    match target {
        "cursor" => Some("---\ndescription: CodeGraph coding workflow\nalwaysApply: true\n---\n\n"),
        "antigravity" => Some(
            "---\nname: codegraph-workflow\ndescription: Coding workflow via CodeGraph MCP\ntrigger: auto\n---\n\n",
        ),
        _ => None,
    }
}

/// Đường dẫn file hướng dẫn cho target ở project scope. Trả `None` với
/// target lạ hoặc target không có file tương ứng.
pub fn project_md_path(target: &str, root: &Utf8PathBuf) -> Option<Utf8PathBuf> {
    let p = match target {
        "claude" => root.join("CLAUDE.md"),
        "codex" | "opencode" | "zcode" => root.join("AGENTS.md"),
        "cursor" => root.join(".cursor").join("rules").join("codegraph.mdc"),
        "hermes" => root.join(".hermes").join("AGENTS.md"),
        "antigravity" => root.join(".agents").join("AGENTS.md"),
        _ => return None,
    };
    Some(p)
}

/// Nội dung workflow hoàn chỉnh cho target (kèm frontmatter nếu cần).
/// `None` nếu target không hợp lệ.
pub fn workflow_md(target: &str) -> Option<String> {
    if !KNOWN_AGENTS.contains(&target) {
        return None;
    }
    Some(frontmatter(target).unwrap_or_default().to_string() + WORKFLOW_MD)
}

/// Ghi workflow vào project (create_dir_all). Nếu file đã tồn tại với nội dung
/// khác (user tự viết) thì KHÔNG đè — trả `Ok(None)`.
/// Trả về path đã ghi, hoặc `Some(path)` khi đã khớp sẵn (no-op).
pub fn write_workflow(
    target: &str,
    root: &Utf8PathBuf,
) -> Result<Option<Utf8PathBuf>, anyhow::Error> {
    use anyhow::Context;
    let Some(path) = project_md_path(target, root) else {
        anyhow::bail!(
            "unknown agent '{target}' (known: {})",
            KNOWN_AGENTS.join(", ")
        );
    };
    let want = workflow_md(target).expect("path implies content");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent.as_std_path())
            .with_context(|| format!("create {}", parent))?;
    }
    let existing = std::fs::read_to_string(path.as_std_path()).ok();
    if existing.as_deref() == Some(want.as_str()) {
        return Ok(Some(path));
    }
    if existing.is_some() {
        // File đã có nội dung user — không đè.
        return Ok(None);
    }
    std::fs::write(path.as_std_path(), want)?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_per_target() {
        let root = Utf8PathBuf::from("/proj");
        assert_eq!(
            project_md_path("claude", &root).unwrap(),
            root.join("CLAUDE.md")
        );
        assert_eq!(
            project_md_path("codex", &root).unwrap(),
            root.join("AGENTS.md")
        );
        assert_eq!(
            project_md_path("zcode", &root).unwrap(),
            root.join("AGENTS.md")
        );
        assert_eq!(
            project_md_path("cursor", &root).unwrap(),
            root.join(".cursor/rules/codegraph.mdc")
        );
        assert!(project_md_path("nope", &root).is_none());
    }

    #[test]
    fn cursor_has_frontmatter_others_dont() {
        let c = workflow_md("cursor").unwrap();
        assert!(c.starts_with("---\n"));
        assert!(workflow_md("codex")
            .unwrap()
            .starts_with("<!-- codegraph:workflow"));
    }

    #[test]
    fn write_workflow_roundtrip_and_no_clobber() {
        let d = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
        let path = write_workflow("codex", &root).unwrap().unwrap();
        assert!(path.as_std_path().exists());
        // Idempotent.
        assert_eq!(write_workflow("codex", &root).unwrap().unwrap(), path);
        // File có sẵn của user không bị đè.
        let _ = std::fs::remove_file(path.as_std_path());
        std::fs::write(path.as_std_path(), "# my own agents\n").unwrap();
        assert!(write_workflow("codex", &root).unwrap().is_none());
        assert_eq!(
            std::fs::read_to_string(path.as_std_path()).unwrap(),
            "# my own agents\n"
        );
    }
}
