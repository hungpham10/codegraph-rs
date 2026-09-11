use crate::ir::{ByteSpan, Document, Kind, Node, Scalar};
use anyhow::Result;
use std::collections::HashMap;

/// Generic document parser: turns a raw source file into the normalized
/// `Document` IR (no format-specific graph).
pub trait DocParser: Send + Sync {
    /// Format name (e.g. `"yaml"`, `"json"`, `"toml"`).
    fn format(&self) -> &'static str;
    /// Parse `source` into `Document`. `id` is assigned by the caller.
    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document>;
}

/// Recursive representation of a parsed value (used by all format parsers).
#[derive(Debug, Clone)]
pub enum RecursiveNode {
    Map(Vec<(String, RecursiveNode, ByteSpan)>),
    Array(Vec<(RecursiveNode, ByteSpan)>),
    String(String, ByteSpan),
    Number(f64, ByteSpan),
    Bool(bool, ByteSpan),
    Null(ByteSpan),
}

/// Build a `Document` from `RecursiveNode`, assigning global node ids and
/// wiring parent/children.
pub fn build_document(path: String, format: String, id: u64, root: RecursiveNode) -> Document {
    let mut builder = DocBuilder {
        doc_id: id,
        nodes: HashMap::new(),
        order: Vec::new(),
        next_id: 2, // root = 1
    };
    let root_id = 1;
    builder.walk(root_id, None, None, None, &root, ByteSpan { start: 0, end: 0 });
    let nodes = builder.order;
    Document {
        id,
        path,
        format,
        root: root_id,
        nodes,
    }
}

struct DocBuilder {
    doc_id: u64,
    nodes: HashMap<u64, Node>,
    order: Vec<Node>,
    next_id: u64,
}

impl DocBuilder {
    fn walk(
        &mut self,
        id: u64,
        parent: Option<u64>,
        key: Option<String>,
        index: Option<u32>,
        node: &RecursiveNode,
        span: ByteSpan,
    ) -> Node {
        let (kind, value) = match node {
            RecursiveNode::Map(_) => (Kind::Map, None),
            RecursiveNode::Array(_) => (Kind::Array, None),
            RecursiveNode::String(s, _) => (Kind::String, Some(Scalar::String(s.clone()))),
            RecursiveNode::Number(n, _) => (Kind::Number, Some(Scalar::Number(*n))),
            RecursiveNode::Bool(b, _) => (Kind::Bool, Some(Scalar::Bool(*b))),
            RecursiveNode::Null(_) => (Kind::Null, Some(Scalar::Null)),
        };
        let built_node = Node {
            id,
            kind,
            parent,
            key,
            index,
            span,
            value,
            children: Vec::new(),
            doc: self.doc_id,
        };
        self.order.push(built_node.clone());
        self.nodes.insert(id, built_node.clone());
        // Link parent → child.
        if let Some(pid) = parent {
            if let Some(p) = self.nodes.get_mut(&pid) {
                p.children.push(id);
            }
        }
        // Recurse.
        match node {
            RecursiveNode::Map(entries) => {
                for (k, child, child_span) in entries {
                    let child_id = self.next_id;
                    self.next_id += 1;
                    let child_node = self.walk(
                        child_id,
                        Some(id),
                        Some(k.clone()),
                        None,
                        child,
                        *child_span,
                    );
                    self.nodes.insert(child_id, child_node);
                }
            }
            RecursiveNode::Array(items) => {
                for (i, (child, child_span)) in items.iter().enumerate() {
                    let child_id = self.next_id;
                    self.next_id += 1;
                    let child_node = self.walk(
                        child_id,
                        Some(id),
                        None,
                        Some(i as u32),
                        child,
                        *child_span,
                    );
                    self.nodes.insert(child_id, child_node);
                }
            }
            _ => {}
        }
        // Return a clone of the built node (children already filled in `order`).
        self.nodes.get(&id).cloned().unwrap_or(built_node)
    }
}

// ── YAML parser ──────────────────────────────────────────────────────────

pub struct YamlParser;

impl DocParser for YamlParser {
    fn format(&self) -> &'static str { "yaml" }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let value: serde_yaml::Value = serde_yaml::from_str(source)?;
        let root = convert_yaml_value(&value, ByteSpan { start: 0, end: source.len() as u64 });
        Ok(build_document(path.to_string(), self.format().to_string(), id, root))
    }
}

fn convert_yaml_value(value: &serde_yaml::Value, span: ByteSpan) -> RecursiveNode {
    match value {
        serde_yaml::Value::Mapping(map) => {
            let entries = map
                .iter()
                .map(|(k, v)| {
                    let key = k.as_str().map(|s| s.to_string()).unwrap_or_default();
                    let child_span = ByteSpan { start: 0, end: 0 };
                    (key, convert_yaml_value(v, child_span), child_span)
                })
                .collect();
            RecursiveNode::Map(entries)
        }
        serde_yaml::Value::Sequence(seq) => {
            let items = seq
                .iter()
                .map(|v| (convert_yaml_value(v, ByteSpan { start: 0, end: 0 }), ByteSpan { start: 0, end: 0 }))
                .collect();
            RecursiveNode::Array(items)
        }
        serde_yaml::Value::String(s) => RecursiveNode::String(s.clone(), span),
        serde_yaml::Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            RecursiveNode::Number(f, span)
        }
        serde_yaml::Value::Bool(b) => RecursiveNode::Bool(*b, span),
        serde_yaml::Value::Null => RecursiveNode::Null(span),
        _ => RecursiveNode::Null(span),
    }
}

// ── JSON parser ──────────────────────────────────────────────────────────

pub struct JsonParser;

impl DocParser for JsonParser {
    fn format(&self) -> &'static str { "json" }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let value: serde_json::Value = serde_json::from_str(source)?;
        let root = convert_json_value(&value, ByteSpan { start: 0, end: source.len() as u64 });
        Ok(build_document(path.to_string(), self.format().to_string(), id, root))
    }
}

fn convert_json_value(value: &serde_json::Value, span: ByteSpan) -> RecursiveNode {
    match value {
        serde_json::Value::Object(map) => {
            let entries = map
                .iter()
                .map(|(k, v)| {
                    let key = k.clone();
                    let child_span = ByteSpan { start: 0, end: 0 };
                    (key, convert_json_value(v, child_span), child_span)
                })
                .collect();
            RecursiveNode::Map(entries)
        }
        serde_json::Value::Array(seq) => {
            let items = seq
                .iter()
                .map(|v| (convert_json_value(v, ByteSpan { start: 0, end: 0 }), ByteSpan { start: 0, end: 0 }))
                .collect();
            RecursiveNode::Array(items)
        }
        serde_json::Value::String(s) => RecursiveNode::String(s.clone(), span),
        serde_json::Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            RecursiveNode::Number(f, span)
        }
        serde_json::Value::Bool(b) => RecursiveNode::Bool(*b, span),
        serde_json::Value::Null => RecursiveNode::Null(span),
    }
}

// ── TOML parser ──────────────────────────────────────────────────────────

pub struct TomlParser;

impl DocParser for TomlParser {
    fn format(&self) -> &'static str { "toml" }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let doc: toml::Value = toml::from_str(source)?;
        let root = convert_toml_value(&doc, ByteSpan { start: 0, end: source.len() as u64 });
        Ok(build_document(path.to_string(), self.format().to_string(), id, root))
    }
}

fn convert_toml_value(value: &toml::Value, span: ByteSpan) -> RecursiveNode {
    match value {
        toml::Value::Table(map) => {
            let entries = map
                .iter()
                .map(|(k, v)| {
                    let key = k.clone();
                    let child_span = ByteSpan { start: 0, end: 0 };
                    (key, convert_toml_value(v, child_span), child_span)
                })
                .collect();
            RecursiveNode::Map(entries)
        }
        toml::Value::Array(seq) => {
            let items = seq
                .iter()
                .map(|v| (convert_toml_value(v, ByteSpan { start: 0, end: 0 }), ByteSpan { start: 0, end: 0 }))
                .collect();
            RecursiveNode::Array(items)
        }
        toml::Value::String(s) => RecursiveNode::String(s.clone(), span),
        toml::Value::Integer(n) => RecursiveNode::Number(*n as f64, span),
        toml::Value::Float(n) => RecursiveNode::Number(*n, span),
        toml::Value::Boolean(b) => RecursiveNode::Bool(*b, span),
        toml::Value::Datetime(_) => RecursiveNode::Null(span),
    }
}

// ── HCL parser ──────────────────────────────────────────────────────────
pub struct HclParser;

impl DocParser for HclParser {
    fn format(&self) -> &'static str { "hcl" }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let value: hcl::Value = hcl::from_str(source)?;
        let root = convert_hcl_value(&value, ByteSpan { start: 0, end: source.len() as u64 });
        Ok(build_document(path.to_string(), self.format().to_string(), id, root))
    }
}

fn convert_hcl_value(value: &hcl::Value, span: ByteSpan) -> RecursiveNode {
    match value {
        hcl::Value::Object(map) => {
            let entries = map
                .iter()
                .map(|(k, v)| {
                    let key = k.clone();
                    let child_span = ByteSpan { start: 0, end: 0 };
                    (key, convert_hcl_value(v, child_span), child_span)
                })
                .collect();
            RecursiveNode::Map(entries)
        }
        hcl::Value::Array(seq) => {
            let items = seq
                .iter()
                .map(|v| (convert_hcl_value(v, ByteSpan { start: 0, end: 0 }), ByteSpan { start: 0, end: 0 }))
                .collect();
            RecursiveNode::Array(items)
        }
        hcl::Value::String(s) => RecursiveNode::String(s.clone(), span),
        hcl::Value::Number(n) => RecursiveNode::Number(n.as_f64().unwrap_or(0.0), span),
        hcl::Value::Bool(b) => RecursiveNode::Bool(*b, span),
        hcl::Value::Null => RecursiveNode::Null(span),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_parser() {
        let src = r#"
service:
  name: api
  replicas: 3
"#;
        let doc = YamlParser.parse("/tmp/a.yaml", src, 1).unwrap();
        assert_eq!(doc.nodes.len(), 4); // root, service, name, replicas
    }
}