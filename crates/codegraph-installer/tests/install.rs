use camino::Utf8PathBuf;
use codegraph_installer::{project_registry as registry, DetectStatus, InstallOpts, InstallReport};

fn opts(root: &Utf8PathBuf) -> InstallOpts {
    // Create presence markers for every agent so detect() returns Found/AlreadyConfigured
    // instead of NotFound in the isolated temp dir.
    for marker in &[
        ".claude",
        ".codex",
        ".cursor",
        ".hermes",
        ".gemini/antigravity-cli",
        ".config/opencode",
        ".zcode",
    ] {
        std::fs::create_dir_all(root.join(marker).as_std_path()).unwrap();
    }
    InstallOpts {
        project_root: Some(root.clone()),
        global: false,
        binary_path: Utf8PathBuf::from("/usr/local/bin/codegraph"),
        home_dir: Some(root.clone()),
    }
}

#[test]
fn install_idempotent() {
    let d = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
    let o = opts(&root);
    for target in registry() {
        assert_eq!(target.detect(&o), DetectStatus::Found);
        let r1 = target.install(&o).unwrap();
        assert!(
            matches!(r1, InstallReport::Installed(_)),
            "{} first install: {:?}",
            target.id(),
            r1
        );
        assert_eq!(target.detect(&o), DetectStatus::AlreadyConfigured);
        let r2 = target.install(&o).unwrap();
        assert!(
            matches!(r2, InstallReport::Unchanged),
            "{} re-install: {:?}",
            target.id(),
            r2
        );
    }
}

#[test]
fn uninstall_removes_mcp_entry() {
    let d = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
    let o = opts(&root);
    for target in registry() {
        target.install(&o).unwrap();
        let r = target.uninstall(&o).unwrap();
        assert!(
            matches!(r, InstallReport::Updated(_)),
            "{} uninstall: {:?}",
            target.id(),
            r
        );
        assert_eq!(
            target.detect(&o),
            DetectStatus::Found,
            "{} should remain installed-but-not-configured",
            target.id()
        );
    }
}

#[test]
fn sibling_keys_preserved() {
    let d = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
    let claude_settings = root.join(".claude").join("settings.local.json");
    std::fs::create_dir_all(claude_settings.parent().unwrap().as_std_path()).unwrap();
    std::fs::write(
        claude_settings.as_std_path(),
        r#"{"mcpServers":{"other":{"command":"foo"}},"theme":"dark"}"#,
    )
    .unwrap();
    let o = opts(&root);
    let claude = registry().into_iter().find(|t| t.id() == "claude").unwrap();
    claude.install(&o).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_settings.as_std_path()).unwrap())
            .unwrap();
    assert!(
        v.pointer("/mcpServers/other").is_some(),
        "sibling MCP entry must survive"
    );
    assert_eq!(
        v.pointer("/theme").and_then(|v| v.as_str()),
        Some("dark"),
        "sibling field must survive"
    );
    assert!(v.pointer("/mcpServers/codegraph").is_some());
}

#[test]
fn codex_project_scope_writes_project_files() {
    let d = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(root.join(".codex").as_std_path()).unwrap();
    let o = InstallOpts {
        project_root: Some(root.clone()),
        global: false,
        binary_path: Utf8PathBuf::from("/usr/local/bin/codegraph"),
        home_dir: Some(root.clone()),
    };
    let codex = registry().into_iter().find(|t| t.id() == "codex").unwrap();
    codex.install(&o).unwrap();
    let config = root.join(".codex").join("config.toml");
    let text = std::fs::read_to_string(config.as_std_path()).unwrap();
    assert!(text.contains("[mcp_servers.codegraph]"), "{text}");
    assert!(root.join("AGENTS.md").as_std_path().exists());
    assert_eq!(codex.detect(&o), DetectStatus::AlreadyConfigured);

    // AGENTS.md có sẵn của user không bị đè.
    std::fs::remove_file(config.as_std_path()).unwrap();
    std::fs::write(root.join("AGENTS.md").as_std_path(), "# my agents\n").unwrap();
    codex.install(&o).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("AGENTS.md").as_std_path()).unwrap(),
        "# my agents\n"
    );
}

#[test]
fn zcode_global_scope_uses_home_dir_and_preserves_entry() {
    let d = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
    let cli_dir = root.join(".zcode").join("cli");
    std::fs::create_dir_all(cli_dir.as_std_path()).unwrap();
    let config = cli_dir.join("config.json");
    std::fs::write(
        config.as_std_path(),
        r#"{"mcp":{"servers":{"codegraph":{"type":"stdio","command":"codegraph","args":["serve","--mcp","--mermaid"]}}}}"#,
    )
    .unwrap();
    let o = InstallOpts {
        project_root: None,
        global: true,
        binary_path: Utf8PathBuf::from("/usr/local/bin/codegraph"),
        home_dir: Some(root.clone()),
    };
    let zcode = codegraph_installer::registry()
        .into_iter()
        .find(|t| t.id() == "zcode")
        .unwrap();
    assert_eq!(zcode.detect(&o), DetectStatus::AlreadyConfigured);
    // Entry hợp lệ đã có → install không ghi đè (giữ --mermaid).
    assert!(matches!(
        zcode.install(&o).unwrap(),
        InstallReport::Unchanged
    ));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config.as_std_path()).unwrap()).unwrap();
    assert_eq!(
        v.pointer("/mcp/servers/codegraph/args/2")
            .and_then(|a| a.as_str()),
        Some("--mermaid"),
        "user's extra args must survive"
    );
    // Skills theo thư mục .agents chung.
    assert_eq!(
        zcode.skills_dir(&o).unwrap(),
        root.join(".agents").join("skills")
    );
}

#[test]
fn codex_global_scope_uses_home_dir() {
    let d = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
    let o = InstallOpts {
        project_root: None,
        global: true,
        binary_path: Utf8PathBuf::from("/usr/local/bin/codegraph"),
        home_dir: Some(root.clone()),
    };
    let codex = codegraph_installer::registry()
        .into_iter()
        .find(|t| t.id() == "codex")
        .unwrap();
    codex.install(&o).unwrap();
    assert!(root
        .join(".codex")
        .join("config.toml")
        .as_std_path()
        .exists());
    assert!(root.join(".codex").join("AGENTS.md").as_std_path().exists());
    assert_eq!(codex.detect(&o), DetectStatus::AlreadyConfigured);

    // Skills theo scope global.
    let dir = codex.skills_dir(&o).unwrap();
    assert_eq!(
        dir,
        root.join(".codex").join("skills"),
        "skills dir should follow .codex"
    );
}

#[test]
fn claude_skills_dir_follows_scope() {
    let d = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
    let project = InstallOpts {
        project_root: Some(root.clone()),
        global: false,
        binary_path: Utf8PathBuf::from("/usr/local/bin/codegraph"),
        home_dir: Some(root.clone()),
    };
    let global_opts = InstallOpts {
        project_root: None,
        global: true,
        binary_path: Utf8PathBuf::from("/usr/local/bin/codegraph"),
        home_dir: Some(root.clone()),
    };
    let claude = registry().into_iter().find(|t| t.id() == "claude").unwrap();
    assert_eq!(
        claude.skills_dir(&project).unwrap(),
        root.join(".claude").join("skills")
    );
    // Global dùng home_dir override (không đụng home thật).
    assert_eq!(
        claude.skills_dir(&global_opts).unwrap(),
        root.join(".claude").join("skills")
    );
}
