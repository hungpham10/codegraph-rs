//! Re-export `Session` từ `codegraph-api` (đã được đưa lên tầng shared).
//! Giữ file này để `codegraph-mcp` không vỡ — mọi định nghĩa giờ nằm ở
//! `codegraph_api::session`.

pub use codegraph_api::session::{stats_json, DetailLevel, InitOutcome, OutputStyle, Session};
