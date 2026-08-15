//! StorageRoute — vị trí lưu trữ của một repository, dùng chung giữa
//! `codegraph-extract` (đọc config → route), `codegraph-graph` (mở index) và
//! `codegraph-mcp` (session). Tách khỏi chuỗi DSN để route RDBMS sharded có thể
//! mang theo `repo_id` — không nhét vào query param của DSN connect.

/// Hướng mở storage của một repository.
///
/// `PartialEq` dùng để session/MCP so sánh route hiện tại với route mới khi root
/// đổi (`ensure_ready` swap index nếu khác).
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Default)]
pub enum StorageRoute {
    /// In-memory (test/dev, không persist).
    #[default]
    Memory,
    /// Backend local single-process: `sqlite://<path>`, `lmdb://<path>`,
    /// `redis://<url>` — chuỗi dsn gốc.
    Local(String),
    /// RDBMS sharded: N pool, mỗi DSN = 1 shard server (cùng schema 001+002).
    /// Shard thật của repo được tra từ bảng `repos` (mapping repo_id → shard)
    /// thay vì recompute `repo_id % N` mỗi lần — đổi số lượng DSN không làm
    /// repo dịch server.
    Sharded {
        /// Các DSN connect — mỗi phần tử = 1 shard server. Thứ tự = index shard.
        dsns: Vec<String>,
        /// repo_id (số u64) của repository. `None` khi config chưa ghi —
        /// resolver sẽ adopt theo `root` (bảng `repos`) hoặc sinh mới + self-heal
        /// ghi lại config.toml.
        repo_id: Option<u64>,
        /// Root path chuẩn — lookup ngược trong bảng `repos` để cùng root path
        /// (clone/máy khác) dùng chung repo_id → chung partition.
        root: Option<String>,
    },
}

impl StorageRoute {
    /// Shard mục tiêu khi chỉ tính bằng `repo_id % N` — dùng làm **điểm tra
    /// mapping** trong bảng `repos` (bản sao nằm trên mọi shard, nên đọc ở bất
    /// kỳ shard nào cũng tìm được) và làm shard gán cho repo CHƯA đăng ký.
    ///
    /// `None` khi route không phải `Sharded` hoặc `dsns` rỗng (config lỗi).
    pub fn shard_of(&self, repo_id: u64) -> Option<usize> {
        match self {
            StorageRoute::Sharded { dsns, .. } if !dsns.is_empty() => {
                Some((repo_id % dsns.len() as u64) as usize)
            }
            _ => None,
        }
    }

    /// repo_id hiện có trong route — `None` nếu không phải `Sharded` hoặc config
    /// chưa ghi (cần resolver sinh/adopt).
    pub fn repo_id(&self) -> Option<u64> {
        match self {
            StorageRoute::Sharded { repo_id, .. } => *repo_id,
            _ => None,
        }
    }

    /// Root path chuẩn hiện có trong route (`None` nếu không phải `Sharded`).
    pub fn root(&self) -> Option<&str> {
        match self {
            StorageRoute::Sharded { root, .. } => root.as_deref(),
            _ => None,
        }
    }
}

