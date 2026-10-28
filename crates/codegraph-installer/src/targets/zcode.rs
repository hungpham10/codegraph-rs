//! ZCode CLI — MCP config at `~/.zcode/cli/config.json` under `mcp.servers`
//! (project: `<root>/.zcode/config.json`, cùng cấu trúc). Skills dùng thư mục
//! `AGENTS` chung: `~/.agents/skills` (global) / `<root>/.agents/skills`
//! (project). Entry MCP đã tồn tại hợp lệ (`serve --mcp`) được giữ nguyên
//! — không đè extra args (vd `--mermaid`) mà user đã tự chỉnh.

use crate::{targets::jsonutil, AgentTarget, DetectStatus, InstallOpts, InstallReport};
use anyhow::Result;
use camino::Utf8PathBuf;
use serde_json::{json, Value};

pub struct ZcodeTarget;

impl ZcodeTarget {
    fn config_path(&self, opts: &InstallOpts) -> Option<Utf8PathBuf> {
        if opts.global {
            let home = opts.home_dir()?;
            Utf8PathBuf::from_path_buf(home.join(".zcode").join("cli").join("config.json")).ok()
        } else {
            opts.project_root
                .as_ref()
                .map(|r| r.join(".zcode").join("config.json"))
        }
    }

    /// `codegraph` entry hợp lệ đã có sẵn trong `mcp.servers` (command trỏ
    /// codegraph và chạy `serve --mcp`) — khi đó không ghi đè.
    fn existing_entry(v: &Value) -> Option<Value> {
        let entry = v.pointer("/mcp/servers/codegraph")?;
        let command = entry.get("command").and_then(|c| c.as_str())?;
        let args = entry.get("args").and_then(|a| a.as_array())?;
        let args: Vec<&str> = args.iter().filter_map(|a| a.as_str()).collect();
        if command.ends_with("codegraph") && args.len() >= 2 && args[..2] == ["serve", "--mcp"] {
            Some(entry.clone())
        } else {
            None
        }
    }
}

impl AgentTarget for ZcodeTarget {
    fn id(&self) -> &'static str {
        "zcode"
    }
    fn label(&self) -> &'static str {
        "ZCode CLI"
    }

    fn detect(&self, opts: &InstallOpts) -> DetectStatus {
        let Some(config) = self.config_path(opts) else {
            return DetectStatus::NotFound;
        };
        // Marker presence: thư mục chứa config phải tồn tại.
        let Some(parent) = config.parent() else {
            return DetectStatus::NotFound;
        };
        if !parent.exists() {
            return DetectStatus::NotFound;
        }
        if !config.exists() {
            return DetectStatus::Found;
        }
        let Ok(v) = jsonutil::read_or_default(&config) else {
            return DetectStatus::Found;
        };
        if Self::existing_entry(&v).is_some() {
            DetectStatus::AlreadyConfigured
        } else {
            DetectStatus::Found
        }
    }

    fn install(&self, opts: &InstallOpts) -> Result<InstallReport> {
        let config = self
            .config_path(opts)
            .ok_or_else(|| anyhow::anyhow!("no zcode config path"))?;
        let v = if config.exists() {
            jsonutil::read_or_default(&config)?
        } else {
            Value::Object(Default::default())
        };
        if Self::existing_entry(&v).is_some() {
            return Ok(InstallReport::Unchanged);
        }

        let mut args = vec![Value::String("serve".into()), Value::String("--mcp".into())];
        if let Some(root) = &opts.project_root {
            args.push(Value::String("--path".into()));
            args.push(Value::String(root.to_string()));
        }
        let entry = json!({
            "type": "stdio",
            "command": opts.binary_path.as_str(),
            "args": args,
        });

        let mut v = v;
        let obj = v
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("zcode config not an object"))?;
        let mcp = obj
            .entry("mcp")
            .or_insert_with(|| Value::Object(Default::default()));
        let mcp = mcp
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcp not an object"))?;
        let servers = mcp
            .entry("servers")
            .or_insert_with(|| Value::Object(Default::default()));
        let servers = servers
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcp.servers not an object"))?;
        servers.insert("codegraph".into(), entry);

        if let Some(parent) = config.parent() {
            std::fs::create_dir_all(parent.as_std_path())?;
        }
        jsonutil::write_pretty(&config, &v)?;
        Ok(InstallReport::Installed(vec![config]))
    }

    fn uninstall(&self, opts: &InstallOpts) -> Result<InstallReport> {
        let Some(config) = self.config_path(opts) else {
            return Ok(InstallReport::Unchanged);
        };
        if !config.exists() {
            return Ok(InstallReport::Unchanged);
        }
        let mut v = jsonutil::read_or_default(&config)?;
        let mut changed = false;
        if let Some(servers) = v
            .pointer_mut("/mcp/servers")
            .and_then(|s| s.as_object_mut())
        {
            if servers.remove("codegraph").is_some() {
                changed = true;
            }
        }
        if changed {
            jsonutil::write_pretty(&config, &v)?;
            Ok(InstallReport::Updated(vec![config]))
        } else {
            Ok(InstallReport::Unchanged)
        }
    }

    fn skills_dir(&self, opts: &InstallOpts) -> Option<Utf8PathBuf> {
        if opts.global {
            let home = opts.home_dir()?;
            Utf8PathBuf::from_path_buf(home.join(".agents").join("skills")).ok()
        } else {
            opts.project_root
                .as_ref()
                .map(|r| r.join(".agents").join("skills"))
        }
    }
}
