//! Git layer — đường **đọc** dữ liệu VCS (branch / diff / export tree).
//!
//! Đối xứng với [`Source`](crate::Source): tách theo nhóm thao tác để provider
//! mới chỉ implement tầng nó cần, và chỗ đã dùng `&dyn Git` không phải sửa.
//!
//! - [`GitRepo`] — nhận diện repo + liệt kê branch.
//! - [`GitDiff`] — unified diff giữa 2 ref.
//! - [`GitArchive`] — export cây tại một ref ra thư mục (cho "before index").
//!
//! `DiskGit` là impl mặc định, gọi `git` qua tiến trình ngoài — đây là chỗ duy
//! nhất trong hệ biết tới `Command::new("git")`. Provider từ xa (GitHub API,
//! object store) implement trait với I/O thật; test dùng impl giả trả diff cố
//! định mà không cần repo.

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_core::{Error, Result};

/// Tầng nền — mọi thao tác git đều biết root của nó.
pub trait GitInfo {
    /// Workspace root (thư mục chứa repo).
    fn root(&self) -> &Utf8Path;
}

/// Tầng **nhận diện + discovery**: repo nào, có branch gì.
#[allow(clippy::double_must_use)] // async_trait sinh `must_use` trùng (clippy 1.99)
#[async_trait]
pub trait GitRepo: GitInfo {
    /// Có phải git repository không (`rev-parse --is-inside-work-tree`).
    async fn is_repo(&self) -> bool;

    /// Liệt kê branch local (short names, mới nhất trước). Rỗng nếu không repo.
    async fn branches(&self) -> Result<Vec<String>>;

    /// Branch hiện tại (`HEAD` detached → `None`).
    async fn current_branch(&self) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Tầng **diff**: unified diff giữa 2 ref.
#[allow(clippy::double_must_use)] // async_trait sinh `must_use` trùng (clippy 1.99)
#[async_trait]
pub trait GitDiff: GitInfo {
    /// Unified diff `base..head` (`head = None` → so với working tree), ở dạng
    /// tương thích `parse_unified_diff`. Lỗi nếu `base`/`head` không tồn tại.
    async fn diff(&self, base: &str, head: Option<&str>) -> Result<String>;
}

/// Tầng **export tree**: lấy nội dung cây tại một ref ra đĩa (để parse lại
/// thành index "trước" khi mô phỏng/so sánh nhánh).
#[allow(clippy::double_must_use)] // async_trait sinh `must_use` trùng (clippy 1.99)
#[async_trait]
pub trait GitArchive: GitInfo {
    /// Ghi nội dung cây tại `git_ref` vào thư mục `dest` (đã tồn tại). Mặc định:
    /// lỗi — provider không hỗ trợ export không cần override.
    async fn export_tree(&self, git_ref: &str, dest: &Utf8Path) -> Result<()> {
        let _ = (git_ref, dest);
        Err(Error::Invalid(format!(
            "git provider tại {} không hỗ trợ export_tree",
            self.root()
        )))
    }
}

/// Trait gộp — phần lớn nơi chỉ cần cái này (`&dyn Git`).
pub trait Git: GitRepo + GitDiff + GitArchive + Send + Sync {}
impl<T: GitRepo + GitDiff + GitArchive + Send + Sync> Git for T {}

/// Git trên filesystem local — impl mặc định, gọi `git`/`tar` ngoài tiến trình.
pub struct DiskGit {
    root: Utf8PathBuf,
}

impl DiskGit {
    pub fn new(root: impl Into<Utf8PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Chạy một lệnh git, trả stdout; lỗi kèm stderr để dễ chẩn đoán.
    fn run(&self, args: &[&str]) -> Result<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(self.root.as_str())
            .args(args)
            .output()
            .map_err(|e| Error::Invalid(format!("git unavailable: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(Error::Invalid(format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or(""),
                err.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl GitInfo for DiskGit {
    fn root(&self) -> &Utf8Path {
        &self.root
    }
}

#[async_trait]
impl GitRepo for DiskGit {
    async fn is_repo(&self) -> bool {
        self.run(&["rev-parse", "--is-inside-work-tree"])
            .map(|s| s.trim() == "true")
            .unwrap_or(false)
    }

    async fn branches(&self) -> Result<Vec<String>> {
        let text = self.run(&["branch", "--format=%(refname:short)", "--sort=-committerdate"])?;
        Ok(text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    async fn current_branch(&self) -> Result<Option<String>> {
        let text = self.run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        let b = text.trim();
        if b.is_empty() || b == "HEAD" {
            Ok(None)
        } else {
            Ok(Some(b.to_string()))
        }
    }
}

#[async_trait]
impl GitDiff for DiskGit {
    async fn diff(&self, base: &str, head: Option<&str>) -> Result<String> {
        let mut args: Vec<&str> = vec!["diff", "--no-color", "--unified=3", base];
        if let Some(h) = head {
            args.push(h);
        }
        self.run(&args)
    }
}

#[async_trait]
impl GitArchive for DiskGit {
    async fn export_tree(&self, git_ref: &str, dest: &Utf8Path) -> Result<()> {
        // `git archive` → tar tạm → `tar -xf` vào dest (giữ hành vi cũ).
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let tar = Utf8PathBuf::from_path_buf(
            std::env::temp_dir().join(format!("codegraph-git-{}-{millis}.tar", std::process::id())),
        )
        .map_err(|p| Error::Invalid(format!("temp path not UTF-8: {p:?}")))?;

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(self.root.as_str())
            .args(["archive", "--format=tar"])
            .arg(git_ref)
            .arg("-o")
            .arg(tar.as_str())
            .status()
            .map_err(|e| Error::Invalid(format!("git unavailable: {e}")))?;
        if !status.success() {
            return Err(Error::Invalid(format!("git archive `{git_ref}` failed")));
        }

        let ok = std::process::Command::new("tar")
            .arg("-xf")
            .arg(tar.as_str())
            .arg("-C")
            .arg(dest.as_str())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let _ = std::fs::remove_file(tar.as_std_path());
        if !ok {
            return Err(Error::Invalid("tar extract failed".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trên thư mục không phải repo: `is_repo` false, các lệnh khác lỗi sạch
    /// (không panic). Đây là hợp đồng để caller xử lý "không có git".
    #[tokio::test]
    async fn non_repo_is_graceful() {
        let d = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap();
        let git = DiskGit::new(root);
        assert!(!git.is_repo().await);
        assert!(git.branches().await.is_err());
    }

    /// Export tree mặc định của provider không hỗ trợ → lỗi rõ ràng.
    #[tokio::test]
    async fn default_export_tree_errors() {
        struct NoArchive(Utf8PathBuf);
        impl GitInfo for NoArchive {
            fn root(&self) -> &Utf8Path {
                &self.0
            }
        }
        #[async_trait]
        impl GitRepo for NoArchive {
            async fn is_repo(&self) -> bool {
                false
            }
            async fn branches(&self) -> Result<Vec<String>> {
                Ok(vec![])
            }
        }
        #[async_trait]
        impl GitDiff for NoArchive {
            async fn diff(&self, _base: &str, _head: Option<&str>) -> Result<String> {
                Ok(String::new())
            }
        }
        // GitArchive dùng default.
        impl GitArchive for NoArchive {}

        let na = NoArchive(Utf8PathBuf::from("/tmp"));
        let dest = Utf8PathBuf::from("/tmp");
        let err = na.export_tree("HEAD", &dest).await.unwrap_err();
        assert!(format!("{err}").contains("không hỗ trợ"));
    }
}
