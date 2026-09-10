use serde::{Deserialize, Serialize};

/// Configuration for the document graph layer (`.codegraph/config.toml [docgraph]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocConfig {
    /// Storage backend for document tries (same kind as code graph).
    pub storage: Option<StorageConfig>,
    /// Base id for document nodes.
    pub doc_base: Option<u64>,
    /// Base id for mined pattern ids.
    pub pattern_base: Option<u64>,
    /// Bloom bloom-filter cap (in tokens) for document search.
    pub bloom_cap: Option<usize>,
    /// Key normalization aliases (e.g. `instances → replicas`).
    pub aliases: Option<Vec<(String, String)>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageConfig {
    pub r#type: Option<String>,
    pub dsn: Option<String>,
}

impl DocConfig {
    pub fn doc_base(&self) -> u64 {
        self.doc_base.unwrap_or(1_000_000_000)
    }
    pub fn pattern_base(&self) -> u64 {
        self.pattern_base.unwrap_or(3_000_000_000)
    }
    pub fn bloom_cap(&self) -> usize {
        self.bloom_cap.unwrap_or(64)
    }
    pub fn storage_kind(&self) -> Option<&str> {
        self.storage.as_ref().and_then(|s| s.r#type.as_deref())
    }
    pub fn storage_dsn(&self) -> Option<&str> {
        self.storage.as_ref().and_then(|s| s.dsn.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let cfg = DocConfig::default();
        assert_eq!(cfg.doc_base(), 1_000_000_000);
        assert_eq!(cfg.bloom_cap(), 64);
    }
}
