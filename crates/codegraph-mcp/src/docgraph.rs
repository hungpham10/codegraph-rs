//! Doc graph dùng chung — nay sống ở `codegraph_extract::docgraph` để cả MCP
//! và GraphQL dùng lại. Re-export để giữ API nội bộ ổn định.

pub use codegraph_extract::SharedDocGraph;

