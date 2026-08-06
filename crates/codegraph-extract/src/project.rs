//! Project scaffolding: `.codegraph/` layout, paths, and `init_project`.
//!
//! Tách riêng phần init (trước đây nằm inline trong CLI `cmd_init`) để cả CLI
//! và MCP server (`codegraph_init` tool) dùng chung.

use crate::config::DEFAULT_CONFIG_TOML;
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_core::Result;

/// Thư mục `.codegraph/` trong workspace root.
pub const CODEGRAPH_DIR: &str = ".codegraph";

/// Tên file sqlite index bên trong `.codegraph/`.
const DB_FILE: &str = "db.sqlite";

/// Đường dẫn thư mục `.codegraph/` của `root`.
pub fn project_dir(root: &Utf8Path) -> Utf8PathBuf {
    root.join(CODEGRAPH_DIR)
}

/// Đường dẫn file index sqlite: `root/.codegraph/db.sqlite`.
pub fn project_db_path(root: &Utf8Path) -> Utf8PathBuf {
    project_dir(root).join(DB_FILE)
}

/// Khởi tạo `.codegraph/` trong `root` (idempotent): tạo thư mục, viết
/// `.gitignore`, `version`, và `config.toml` (chỉ khi chưa có). Trả về đường
/// dẫn thư mục `.codegraph`.
pub fn init_project(root: &Utf8Path) -> Result<Utf8PathBuf> {
    let dir = project_dir(root);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(".gitignore"), "*\n")?;
    std::fs::write(dir.join("version"), env!("CARGO_PKG_VERSION"))?;
    let config_path = dir.join("config.toml");
    if !config_path.exists() {
        std::fs::write(&config_path, DEFAULT_CONFIG_TOML)?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_project_creates_layout_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();

        let first = init_project(root).unwrap();
        assert_eq!(first, project_dir(root));
        assert!(first.join(".gitignore").is_file());
        assert!(first.join("version").is_file());
        assert!(first.join("config.toml").is_file());
        let gitignore = std::fs::read_to_string(first.join(".gitignore")).unwrap();
        assert_eq!(gitignore, "*\n");

        // Lần gọi thứ hai — không lỗi, config.toml giữ nguyên.
        let config = std::fs::read_to_string(first.join("config.toml")).unwrap();
        init_project(root).unwrap();
        assert_eq!(
            std::fs::read_to_string(first.join("config.toml")).unwrap(),
            config
        );
    }

    #[test]
    fn project_db_path_joins_under_codegraph() {
        let root = Utf8Path::new("/repo");
        assert_eq!(project_dir(root).as_str(), "/repo/.codegraph");
        assert_eq!(project_db_path(root).as_str(), "/repo/.codegraph/db.sqlite");
    }
}
