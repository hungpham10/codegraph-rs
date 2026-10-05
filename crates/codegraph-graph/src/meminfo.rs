//! Đo RAM của process — **std-only**, không thêm dependency.
//!
//! Mục tiêu: đo được memory thực của `GraphIndex` (in-memory-first nên RAM là
//! tài nguyên khan hiếm), phục vụ hai việc: benchmark tối ưu bộ nhớ, và hiển thị
//! trong `codegraph_status`.
//!
//! - **Linux/BSD**: đọc `/proc/self/statm` (RSS hiện tại, × page size) và
//!   `/proc/self/status` `VmHWM` (RSS đỉnh).
//! - **macOS**: `/proc` không có → gọi `ps -o rss= -p <pid>` (KB). Chỉ có RSS
//!   hiện tại; đỉnh để `None` ( caller tự track bằng [`MemTracker`]).
//! - **Platform khác**: `None` — caller phải xử lý, KHÔNG panic.
//!
//! ```no_run
//! use codegraph_graph::meminfo::{MemTracker, rss_bytes};
//!
//! let mut t = MemTracker::new();
//! t.mark("baseline");
//! // … mở index / ingest …
//! let peak = t.mark("sau ingest"); // max RSS thấy từ lần mark trước
//! println!("peak = {}", peak);
//! # let _ = rss_bytes();
//! ```

/// Page size mặc định khi không đọc được từ hệ thống (Linux arm64/x86_64 đều
/// 4096). Sai số này chỉ ảnh hưởng con số báo cáo, không ảnh hưởng logic.
const FALLBACK_PAGE_SIZE: u64 = 4096;

/// RSS hiện tại của process (bytes). `None` nếu platform không hỗ trợ.
pub fn rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        // statm: size resident shared text lib data dt — tính bằng **page**.
        // resident là field thứ 2 (index 1).
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        return Some(resident_pages.saturating_mul(page_size()));
    }
    #[cfg(not(target_os = "linux"))]
    {
        rss_via_ps()
    }
}

/// RSS đỉnh từ lúc process bắt đầu (bytes).
///
/// Chỉ có trên Linux (`VmHWM`). macOS trả `None` — dùng [`MemTracker`] để tự
/// lấy mẫu theo phase.
pub fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb.saturating_mul(1024));
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Định dạng byte cho log/bench: `12.3 MiB`.
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Theo dõi RSS theo từng phase — [`rss_bytes`] không có sẵn trên mọi nền tảng
/// (đỉnh), nên benchmark tự lấy mẫu tại các mốc rồi giữ max.
///
/// ```no_run
/// # use codegraph_graph::meminfo::MemTracker;
/// let mut t = MemTracker::new();
/// t.mark("open");
/// t.mark("ingest");
/// for (label, bytes) in t.samples() { println!("{label}: {bytes}"); }
/// ```
#[derive(Debug, Clone)]
pub struct MemTracker {
    samples: Vec<(String, u64)>,
    peak: u64,
}

impl Default for MemTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl MemTracker {
    pub fn new() -> Self {
        Self {
            samples: Vec::new(),
            peak: 0,
        }
    }

    /// Lấy mẫu RSS ở mốc `label`. RSS không đọc được → ghi `0` và **không** cập
    /// nhật peak, để không báo nhầm số 0 là "không tốn RAM".
    pub fn mark(&mut self, label: impl Into<String>) -> u64 {
        let rss = rss_bytes().unwrap_or(0);
        if rss > self.peak {
            self.peak = rss;
        }
        self.samples.push((label.into(), rss));
        rss
    }

    /// RSS đỉnh trong các mốc đã đánh dấu.
    pub fn peak(&self) -> u64 {
        self.peak
    }

    /// Toàn bộ mẫu theo thứ tự thời gian.
    pub fn samples(&self) -> &[(String, u64)] {
        &self.samples
    }
}

// ── Platform helpers ──

#[cfg(target_os = "linux")]
fn page_size() -> u64 {
    // `getconf PAGESIZE` không portable qua std; đọc từ `/proc/self/smaps` quá
    // nặng. Linux hầu hết là 4096 — chấp nhận sai số nhỏ thay vì thêm libc.
    FALLBACK_PAGE_SIZE
}

#[cfg(not(target_os = "linux"))]
fn rss_via_ps() -> Option<u64> {
    // `ps -o rss= -p <pid>` in ra RSS **tính bằng KiB**. Cần pid: đọc
    // `/proc/self` không có trên macOS nên dùng `std::process::id()`.
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kb.saturating_mul(1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_is_plausible_when_supported() {
        // Chỉ assert khi platform đọc được — CI chạy Linux nên có, nhưng để
        // chấp nhận `None` ở nền tảng khác.
        if let Some(rss) = rss_bytes() {
            assert!(rss > 0, "RSS phải > 0, got {rss}");
            // Một process Rust không thể dưới vài MB.
            assert!(rss > 512 * 1024, "RSS nhỏ bất thường: {rss}");
        }
    }

    #[test]
    fn peak_ge_current_on_linux() {
        if let Some(peak) = peak_rss_bytes()
            && let Some(rss) = rss_bytes()
        {
            assert!(peak >= rss, "VmHWM ({peak}) phải >= RSS ({rss})");
        }
    }

    #[test]
    fn fmt_bytes_scales_units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024), "1.0 KiB");
        assert_eq!(fmt_bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
    }

    #[test]
    fn tracker_keeps_max() {
        let mut t = MemTracker::new();
        t.mark("a");
        let first = t.samples()[0].1;
        t.mark("b");
        assert_eq!(t.samples().len(), 2);
        assert!(t.peak() >= first);
        assert!(t.samples()[0].0 == "a");
    }
}
