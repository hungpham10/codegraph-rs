//! Codex CLI — config at `~/.codex/config.toml` (global) hoặc
//! `<project>/.codex/config.toml` (project-scoped), instructions at
//! `AGENTS.md` cạnh đó. Uses toml_edit to preserve sibling tables and user
//! formatting.

use crate::{AgentTarget, DetectStatus, InstallOpts, InstallReport, INSTRUCTIONS_MD};
use anyhow::Result;
use camino::Utf8PathBuf;
use toml_edit::{value, Array, DocumentMut, Item, Table};

pub struct CodexTarget;

impl CodexTarget {
    /// Thư mục `.codex` cho scope hiện tại (global = trong home).
    fn codex_dir(&self, opts: &InstallOpts) -> Option<Utf8PathBuf> {
        if opts.global {
            let home = opts.home_dir()?;
            Utf8PathBuf::from_path_buf(home.join(".codex")).ok()
        } else {
            opts.project_root.as_ref().map(|r| r.join(".codex"))
        }
    }
    fn config_path(&self, opts: &InstallOpts) -> Option<Utf8PathBuf> {
        self.codex_dir(opts).map(|d| d.join("config.toml"))
    }
    /// AGENTS.md: global thì nằm trong `.codex/`, project thì ở root (dùng
    /// chung cho mọi agent đọc AGENTS.md của repo).
    fn agents_path(&self, opts: &InstallOpts) -> Option<Utf8PathBuf> {
        if opts.global {
            self.codex_dir(opts).map(|d| d.join("AGENTS.md"))
        } else {
            opts.project_root.as_ref().map(|r| r.join("AGENTS.md"))
        }
    }
}

impl AgentTarget for CodexTarget {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn label(&self) -> &'static str {
        "Codex CLI"
    }

    fn detect(&self, opts: &InstallOpts) -> DetectStatus {
        let Some(dir) = self.codex_dir(opts) else {
            return DetectStatus::NotFound;
        };
        if !dir.exists() {
            return DetectStatus::NotFound;
        }
        let Some(p) = self.config_path(opts) else {
            return DetectStatus::Found;
        };
        if !p.exists() {
            return DetectStatus::Found;
        }
        let Ok(text) = std::fs::read_to_string(p.as_std_path()) else {
            return DetectStatus::Found;
        };
        let Ok(doc) = text.parse::<DocumentMut>() else {
            return DetectStatus::Found;
        };
        if doc
            .get("mcp_servers")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("codegraph"))
            .is_some()
        {
            DetectStatus::AlreadyConfigured
        } else {
            DetectStatus::Found
        }
    }

    fn install(&self, opts: &InstallOpts) -> Result<InstallReport> {
        let config = self
            .config_path(opts)
            .ok_or_else(|| anyhow::anyhow!("no codex config path"))?;
        let text = std::fs::read_to_string(config.as_std_path()).unwrap_or_default();
        let mut doc: DocumentMut = if text.is_empty() {
            DocumentMut::new()
        } else {
            text.parse()?
        };

        let mut servers = match doc.remove("mcp_servers") {
            Some(Item::Table(t)) => t,
            _ => {
                let mut t = Table::new();
                t.set_implicit(true);
                t
            }
        };

        let mut cg = Table::new();
        cg["command"] = value(opts.binary_path.as_str());
        let mut args = Array::new();
        args.push("serve");
        args.push("--mcp");
        if let Some(root) = &opts.project_root {
            args.push("--path");
            args.push(root.as_str());
        }
        cg["args"] = value(args);

        let existing = servers.get("codegraph").map(|i| i.to_string());
        let new_str = Item::Table(cg.clone()).to_string();
        let changed = existing.as_deref() != Some(new_str.as_str());

        servers["codegraph"] = Item::Table(cg);
        doc["mcp_servers"] = Item::Table(servers);

        let mut written = Vec::new();
        if changed {
            if let Some(parent) = config.parent() {
                std::fs::create_dir_all(parent.as_std_path())?;
            }
            std::fs::write(config.as_std_path(), doc.to_string())?;
            written.push(config);
        }
        if let Some(md) = self.agents_path(opts) {
            let want = INSTRUCTIONS_MD;
            let existing = std::fs::read_to_string(md.as_std_path()).ok();
            // AGENTS.md ở project root dùng chung với các agent khác — chỉ ghi
            // khi chưa tồn tại, không đè nội dung user đang có.
            let should_write = if opts.global {
                existing.as_deref() != Some(want)
            } else {
                existing.is_none()
            };
            if should_write {
                if let Some(parent) = md.parent() {
                    std::fs::create_dir_all(parent.as_std_path())?;
                }
                std::fs::write(md.as_std_path(), want)?;
                written.push(md);
            }
        }
        if written.is_empty() {
            Ok(InstallReport::Unchanged)
        } else {
            Ok(InstallReport::Installed(written))
        }
    }

    fn uninstall(&self, opts: &InstallOpts) -> Result<InstallReport> {
        let Some(config) = self.config_path(opts) else {
            return Ok(InstallReport::Unchanged);
        };
        if !config.exists() {
            return Ok(InstallReport::Unchanged);
        }
        let text = std::fs::read_to_string(config.as_std_path())?;
        let mut doc: DocumentMut = text.parse()?;
        let mut changed = false;
        if let Some(Item::Table(servers)) = doc.get_mut("mcp_servers") {
            if servers.remove("codegraph").is_some() {
                changed = true;
            }
        }
        if changed {
            std::fs::write(config.as_std_path(), doc.to_string())?;
            Ok(InstallReport::Updated(vec![config]))
        } else {
            Ok(InstallReport::Unchanged)
        }
    }

    fn skills_dir(&self, opts: &InstallOpts) -> Option<Utf8PathBuf> {
        self.codex_dir(opts).map(|d| d.join("skills"))
    }
}
