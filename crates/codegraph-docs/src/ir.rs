use serde::{Deserialize, Serialize};

/// Byte offset span in the original source document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ByteSpan {
    pub start: u64,
    pub end: u64,
}

/// The kind of a document node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Root,
    Map,
    Array,
    Field,
    Index,
    String,
    Number,
    Bool,
    Null,
    Reference,
}

/// Scalar value stored on a leaf node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Scalar {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

/// A single node in the document graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    pub kind: Kind,
    pub parent: Option<u64>,
    /// Field name / key label (present for `Field` and `Index`).
    pub key: Option<String>,
    /// Array slot index (present for `Index`).
    pub index: Option<u32>,
    pub span: ByteSpan,
    pub value: Option<Scalar>,
    pub children: Vec<u64>,
    /// Owning document.
    pub doc: u64,
}

/// A parsed structured document (YAML / JSON / TOML).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: u64,
    pub path: String,
    pub format: String,
    pub root: u64,
    pub nodes: Vec<Node>,
}
