//! Wrapper quanh r2pipe: phiên `r2 -q0` persistent để query binary.

use codegraph_core::Error;
use r2pipe::{R2Pipe, R2PipeSpawnOptions};
use serde_json::Value as Json;
use std::path::Path;
use tracing::debug;

/// Session r2 — spawn một process `r2 -q0` và giữ kết nối stdin/stdout.
pub struct R2Session {
    inner: R2Pipe,
}

impl R2Session {
    /// Mở session với binary tại `path`. Spawn `r2 -q0 <path>`.
    ///
    /// Lỗi nếu `r2` không có trong PATH — thông báo cài đặt cụ thể.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let path_str = path
            .to_str()
            .ok_or_else(|| Error::Parse(format!("path không phải UTF-8: {}", path.display())))?;
        let opts = R2PipeSpawnOptions {
            exepath: "r2".to_string(),
            args: vec!["-N", "-e", "scr.color=0", "-e", "scr.utf8=0"],
        };
        let inner = R2Pipe::spawn(path_str, Some(opts))
            .map_err(|e| Error::Parse(format!("không thể spawn r2 cho {}: {e}. Hãy cài radare2: brew install radare2 / apt install radare2", path.display())))?;
        debug!("r2 session opened for {}", path.display());
        Ok(Self { inner })
    }

    /// Gửi lệnh thô, trả về chuỗi response (đã strip NUL).
    pub fn cmd(&mut self, cmd: &str) -> Result<String, Error> {
        self.inner
            .cmd(cmd)
            .map_err(|e| Error::Parse(format!("r2 cmd `{cmd}` failed: {e}")))
    }

    /// Gửi lệnh, parse JSON response.
    pub fn cmdj(&mut self, cmd: &str) -> Result<Json, Error> {
        self.inner
            .cmdj(cmd)
            .map_err(|e| Error::Parse(format!("r2 cmdj `{cmd}` failed: {e}")))
    }

    /// Phân tích binary theo `depth` (chỉ gọi 1 lần trong đời session).
    pub fn analyze(&mut self, depth: crate::config::AnalysisDepth) -> Result<(), Error> {
        let cmd = depth.command();
        debug!("running r2 analysis: {cmd}");
        self.cmd(cmd)?;
        Ok(())
    }
}

/// Kiểm tra `r2` có trong PATH không (chạy `r2 -v`).
pub fn r2_available() -> bool {
    std::process::Command::new("r2")
        .arg("-v")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Version của r2 (chuỗi từ `r2 -v`), nếu có.
pub fn r2_version() -> Option<String> {
    let out = std::process::Command::new("r2").arg("-v").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.lines().next().unwrap_or_default().to_string())
}
