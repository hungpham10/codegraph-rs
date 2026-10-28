//! CodeGraph skills shipped with the binary (`skills/*/SKILL.md` in the repo,
//! embedded at compile time) and helpers to install/uninstall them into an
//! agent's skills directory.

use anyhow::Result;
use camino::Utf8PathBuf;

/// `(skill_name, SKILL.md content)` for every skill shipped with codegraph.
pub const SKILLS: &[(&str, &str)] = &[
    (
        "codegraph-code-review",
        include_str!("../../../skills/codegraph-code-review/SKILL.md"),
    ),
    (
        "codegraph-development-workflow",
        include_str!("../../../skills/codegraph-development-workflow/SKILL.md"),
    ),
    (
        "codegraph-figma-to-frontend",
        include_str!("../../../skills/codegraph-figma-to-frontend/SKILL.md"),
    ),
    (
        "codegraph-reverse-binary",
        include_str!("../../../skills/codegraph-reverse-binary/SKILL.md"),
    ),
    (
        "codegraph-security-review",
        include_str!("../../../skills/codegraph-security-review/SKILL.md"),
    ),
];

/// Ghi mọi skill vào `dir/<name>/SKILL.md` (skip nếu nội dung đã khớp).
/// Trả về danh sách file thực sự ghi.
pub fn install_skills(dir: &Utf8PathBuf) -> Result<Vec<Utf8PathBuf>> {
    let mut written = Vec::new();
    for (name, content) in SKILLS {
        let skill_md = dir.join(name).join("SKILL.md");
        let existing = std::fs::read_to_string(skill_md.as_std_path()).ok();
        if existing.as_deref() == Some(content) {
            continue;
        }
        if let Some(parent) = skill_md.parent() {
            std::fs::create_dir_all(parent.as_std_path())?;
        }
        std::fs::write(skill_md.as_std_path(), content)?;
        written.push(skill_md);
    }
    Ok(written)
}

/// Xoá thư mục skill nếu SKILL.md trong đó khớp nội dung mình ship (chỉ xoá
/// những gì codegraph đã cài; skill do user tự viết thì giữ nguyên).
pub fn uninstall_skills(dir: &Utf8PathBuf) -> Result<Vec<Utf8PathBuf>> {
    let mut removed = Vec::new();
    for (name, content) in SKILLS {
        let skill_dir = dir.join(name);
        let skill_md = skill_dir.join("SKILL.md");
        let existing = std::fs::read_to_string(skill_md.as_std_path()).ok();
        if existing.as_deref() != Some(content) {
            continue;
        }
        match std::fs::remove_dir_all(skill_dir.as_std_path()) {
            Ok(()) => removed.push(skill_dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let dir = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
        assert_eq!(install_skills(&dir).unwrap().len(), SKILLS.len());
        // Idempotent.
        assert!(install_skills(&dir).unwrap().is_empty());
        assert_eq!(uninstall_skills(&dir).unwrap().len(), SKILLS.len());
        assert!(uninstall_skills(&dir).unwrap().is_empty());
        // Skill do user tự sửa thì không bị xoá.
        let custom = dir.join(SKILLS[0].0).join("SKILL.md");
        std::fs::create_dir_all(custom.parent().unwrap().as_std_path()).unwrap();
        std::fs::write(custom.as_std_path(), "# my own version\n").unwrap();
        assert!(uninstall_skills(&dir).unwrap().is_empty());
        assert!(custom.exists());
    }
}
