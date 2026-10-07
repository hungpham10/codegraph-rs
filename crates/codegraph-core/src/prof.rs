//! Sink cho log đo `[CGPROF]` — chọn nơi ghi bằng env, không hardcode stderr.
//!
//! Vì sao không `eprintln!` thẳng: `codegraph serve --mcp` nói JSON-RPC qua
//! **stdout**, còn **stderr do MCP host sở hữu** (Claude Code, Codex… pipe nó
//! của tiến trình con để đưa vào log riêng). Chạy tay trong terminal thì thấy
//! `[CGPROF]`, chạy qua host thì im lặng — dễ tưởng profiler hỏng. Sink cho
//! phép ghi ra file để vẫn đọc/tail được trong cả hai trường hợp.
//!
//! Cấu hình (mặc định **tắt** để server im lặng):
//! - `CODEGRAPH_PROFILE=stderr` (hoặc `1`) → ghi stderr.
//! - `CODEGRAPH_PROFILE_FILE=<path>` → append vào file, `tail -f` được.
//! - `CODEGRAPH_PROFILE=<path>` → tắc dụng như `CODEGRAPH_PROFILE_FILE`.
//! - `0` / `off` / `false` (hoặc unset) → tắt.

use std::fmt::Arguments;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

/// Nơi ghi log `[CGPROF]`, đọc 1 lần từ env rồi cache cho cả process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sink {
    Off,
    Stderr,
    File(String),
}

fn sink() -> &'static Sink {
    static SINK: OnceLock<Sink> = OnceLock::new();
    SINK.get_or_init(|| {
        if let Some(path) = env_value("CODEGRAPH_PROFILE_FILE") {
            return Sink::File(path);
        }
        match env_value("CODEGRAPH_PROFILE") {
            None => Sink::Off,
            Some(v) if is_off(&v) => Sink::Off,
            Some(v) if is_stderr(&v) => Sink::Stderr,
            // Giá trị còn lại coi như path — tiện khi không muốn gõ biến thứ hai.
            Some(path) => Sink::File(path),
        }
    })
}

fn env_value(name: &str) -> Option<String> {
    let v = std::env::var(name).ok()?;
    let v = v.trim().to_string();
    (!v.is_empty()).then_some(v)
}

fn is_off(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "0" | "off" | "false" | "none"
    )
}

fn is_stderr(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "stderr"
    )
}

/// Profiling đang bật hay không — call site có thể check để né format.
pub fn enabled() -> bool {
    *sink() != Sink::Off
}

/// Mở đo một stage. Trả [`Instant`] **luôn** (kể cả khi tắt) để call site
/// `stage_done` không cần branch.
pub fn stage(label: &str) -> Instant {
    let started = Instant::now();
    if enabled() {
        write(format_args!("[CGPROF] {label} start"));
    }
    started
}

/// Đóng stage mở bằng [`stage`] với `started`.
pub fn stage_done(label: &str, started: Instant) {
    if enabled() {
        write(format_args!(
            "[CGPROF] {label} took {:?}",
            started.elapsed()
        ));
    }
}

fn write(args: Arguments<'_>) {
    match sink() {
        Sink::Off => {}
        Sink::Stderr => {
            let _ = writeln!(std::io::stderr(), "{args}");
        }
        // Mở file lỗi (path sai, không quyền ghi) → im lặng, không được làm
        // hỏng request đang serve.
        Sink::File(path) => {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{args}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_values_recognized() {
        assert!(is_off("0"));
        assert!(is_off("OFF"));
        assert!(is_off("false"));
        assert!(!is_off("stderr"));
        assert!(is_stderr("1"));
        assert!(is_stderr("STDERR"));
        assert!(!is_stderr("off"));
    }
}
