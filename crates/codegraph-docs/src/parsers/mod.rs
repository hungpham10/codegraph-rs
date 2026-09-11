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
    builder.walk(
        root_id,
        None,
        None,
        None,
        &root,
        ByteSpan { start: 0, end: 0 },
    );
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
        if let Some(pid) = parent
            && let Some(p) = self.nodes.get_mut(&pid)
        {
            p.children.push(id);
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
                    let child_node =
                        self.walk(child_id, Some(id), None, Some(i as u32), child, *child_span);
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
    fn format(&self) -> &'static str {
        "yaml"
    }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let value: serde_yaml::Value = serde_yaml::from_str(source)?;
        let root = convert_yaml_value(
            &value,
            ByteSpan {
                start: 0,
                end: source.len() as u64,
            },
        );
        Ok(build_document(
            path.to_string(),
            self.format().to_string(),
            id,
            root,
        ))
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
                .map(|v| {
                    (
                        convert_yaml_value(v, ByteSpan { start: 0, end: 0 }),
                        ByteSpan { start: 0, end: 0 },
                    )
                })
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
    fn format(&self) -> &'static str {
        "json"
    }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let value: serde_json::Value = serde_json::from_str(source)?;
        let root = convert_json_value(
            &value,
            ByteSpan {
                start: 0,
                end: source.len() as u64,
            },
        );
        Ok(build_document(
            path.to_string(),
            self.format().to_string(),
            id,
            root,
        ))
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
                .map(|v| {
                    (
                        convert_json_value(v, ByteSpan { start: 0, end: 0 }),
                        ByteSpan { start: 0, end: 0 },
                    )
                })
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
    fn format(&self) -> &'static str {
        "toml"
    }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let doc: toml::Value = toml::from_str(source)?;
        let root = convert_toml_value(
            &doc,
            ByteSpan {
                start: 0,
                end: source.len() as u64,
            },
        );
        Ok(build_document(
            path.to_string(),
            self.format().to_string(),
            id,
            root,
        ))
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
                .map(|v| {
                    (
                        convert_toml_value(v, ByteSpan { start: 0, end: 0 }),
                        ByteSpan { start: 0, end: 0 },
                    )
                })
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
    fn format(&self) -> &'static str {
        "hcl"
    }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let value: hcl::Value = hcl::from_str(source)?;
        let root = convert_hcl_value(
            &value,
            ByteSpan {
                start: 0,
                end: source.len() as u64,
            },
        );
        Ok(build_document(
            path.to_string(),
            self.format().to_string(),
            id,
            root,
        ))
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
                .map(|v| {
                    (
                        convert_hcl_value(v, ByteSpan { start: 0, end: 0 }),
                        ByteSpan { start: 0, end: 0 },
                    )
                })
                .collect();
            RecursiveNode::Array(items)
        }
        hcl::Value::String(s) => RecursiveNode::String(s.clone(), span),
        hcl::Value::Number(n) => RecursiveNode::Number(n.as_f64().unwrap_or(0.0), span),
        hcl::Value::Bool(b) => RecursiveNode::Bool(*b, span),
        hcl::Value::Null => RecursiveNode::Null(span),
    }
}

// ── nginx parser ──────────────────────────────────────────────────────────

pub struct NginxParser;

impl DocParser for NginxParser {
    fn format(&self) -> &'static str {
        "nginx"
    }

    fn parse(&self, path: &str, source: &str, id: u64) -> Result<Document> {
        let root = parse_nginx(source)?;
        Ok(build_document(
            path.to_string(),
            self.format().to_string(),
            id,
            root,
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum NginxToken {
    /// Từ khoá/giá trị (nội dung chuỗi đã bỏ quote).
    Word(String, u32),
    /// Nội dung thô của `_by_lua_block` — giữ nguyên, không parse như nginx.
    LuaCode(String, u32),
    LBrace(u32),
    RBrace(u32),
    Semi(u32),
}

/// Tokenize theo behavior của lexer gonginx (tham khảo `nginx/parser/lexer.go`):
/// - `#` đến cuối dòng là comment (bỏ qua).
/// - Quote `"`, `'`, `` ` `` → 1 word, hỗ trợ escape `\"`; unquote khi tạo value.
/// - `${...}` là variable reference trong word — `{`/`}` bên trong không phải
///   block delimiter (Issue 17: `set $x $a${uri}index.html;`).
/// - Word kết thúc bằng `_by_lua_block` → scan code thô đến `}` đóng (đếm
///   depth, bỏ qua `{`/`}` trong `#` comment) thành `LuaCode`.
fn tokenize_nginx(source: &str) -> Result<Vec<NginxToken>> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut last_word = String::new();
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '#' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '{' => {
                tokens.push(NginxToken::LBrace(line));
                last_word.clear();
                i += 1;
            }
            '}' => {
                tokens.push(NginxToken::RBrace(line));
                last_word.clear();
                i += 1;
            }
            ';' => {
                tokens.push(NginxToken::Semi(line));
                last_word.clear();
                i += 1;
            }
            q @ ('"' | '\'' | '`') => {
                i += 1;
                let mut word = String::new();
                loop {
                    match chars.get(i) {
                        None | Some('\n') => {
                            anyhow::bail!(
                                "unexpected end of file while scanning quoted string at line {line}"
                            );
                        }
                        Some('\\') if chars.get(i + 1) == Some(&q) => {
                            word.push(q);
                            i += 2;
                        }
                        Some(c2) if *c2 == q => {
                            i += 1;
                            break;
                        }
                        Some(c2) => {
                            word.push(*c2);
                            i += 1;
                        }
                    }
                }
                tokens.push(NginxToken::Word(word.clone(), line));
                last_word = word;
            }
            _ => {
                let mut word = String::new();
                let mut in_var_ref = false;
                let mut prev = '\0';
                loop {
                    let Some(&c2) = chars.get(i) else { break };
                    if in_var_ref {
                        if c2 == '}' {
                            in_var_ref = false;
                        }
                        word.push(c2);
                        prev = c2;
                        i += 1;
                        continue;
                    }
                    if c2.is_whitespace() || matches!(c2, ';' | '\n') {
                        break;
                    }
                    if c2 == '{' {
                        if prev == '$' {
                            in_var_ref = true;
                            word.push('{');
                            prev = c2;
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    if c2 == '}' {
                        break;
                    }
                    word.push(c2);
                    prev = c2;
                    i += 1;
                }
                tokens.push(NginxToken::Word(word.clone(), line));
                last_word = word;
            }
        }
        // `_by_lua_block {` → nội dung tiếp theo là code thô đến `}` đóng.
        if last_word.ends_with("_by_lua_block")
            && chars.get(i) == Some(&'{')
        {
            i += 1;
            let mut code = String::new();
            let mut depth = 0usize;
            loop {
                let Some(&c2) = chars.get(i) else {
                    anyhow::bail!(
                        "unexpected end of file while scanning lua code starting at line {line}"
                    );
                };
                if c2 == '#' {
                    // Comment trong lua: giữ nguyên đến cuối dòng, `{`/`}` trong
                    // comment không đổi depth.
                    while i < chars.len() && chars[i] != '\n' {
                        code.push(chars[i]);
                        i += 1;
                    }
                    continue;
                }
                match c2 {
                    '{' => depth += 1,
                    '}' if depth == 0 => break,
                    '}' => depth -= 1,
                    '\n' => line += 1,
                    _ => {}
                }
                code.push(c2);
                i += 1;
            }
            tokens.push(NginxToken::LuaCode(code, line));
            last_word.clear();
        }
    }
    Ok(tokens)
}

fn parse_nginx(source: &str) -> Result<RecursiveNode> {
    let tokens = tokenize_nginx(source)?;
    let mut pos = 0;
    let entries = parse_nginx_entries(&tokens, &mut pos, true)?;
    Ok(RecursiveNode::Map(entries))
}

/// Parse một scope: directive `name args...;` hoặc block `name args... { ... }`.
/// Với scope lồng (top=false) dừng và tiêu thụ `}` đóng; scope top yêu cầu
/// hết token và không được gặp `}` lạc.
fn parse_nginx_entries(
    tokens: &[NginxToken],
    pos: &mut usize,
    top: bool,
) -> Result<Vec<(String, RecursiveNode, ByteSpan)>> {
    let mut entries: Vec<(String, RecursiveNode, ByteSpan)> = Vec::new();
    while let Some(tok) = tokens.get(*pos) {
        match tok {
            NginxToken::Word(name, line) => {
                *pos += 1;
                let mut args: Vec<String> = Vec::new();
                let (key, value) = loop {
                    match tokens.get(*pos) {
                        Some(NginxToken::Word(arg, _)) => {
                            args.push(arg.clone());
                            *pos += 1;
                        }
                        Some(NginxToken::Semi(_)) => {
                            *pos += 1;
                            let value = match args.len() {
                                0 => RecursiveNode::Null(ByteSpan { start: 0, end: 0 }),
                                1 => RecursiveNode::String(args.remove(0), ByteSpan { start: 0, end: 0 }),
                                _ => RecursiveNode::Array(
                                    args.drain(..)
                                        .map(|a| {
                                            (
                                                RecursiveNode::String(a, ByteSpan { start: 0, end: 0 }),
                                                ByteSpan { start: 0, end: 0 },
                                            )
                                        })
                                        .collect(),
                                ),
                            };
                            break (name.clone(), value);
                        }
                        Some(NginxToken::LuaCode(code, l)) => {
                            // `_by_lua_block { ... }` — tokenizer đã tiêu thụ `{`
                            // và gói code thô thành LuaCode; chỉ còn chờ `}`.
                            *pos += 1;
                            match tokens.get(*pos) {
                                Some(NginxToken::RBrace(_)) => {
                                    *pos += 1;
                                }
                                other => {
                                    let l2 = match other {
                                        Some(
                                            NginxToken::Word(_, l2)
                                            | NginxToken::LuaCode(_, l2)
                                            | NginxToken::LBrace(l2)
                                            | NginxToken::RBrace(l2)
                                            | NginxToken::Semi(l2),
                                        ) => *l2,
                                        None => *l,
                                    };
                                    anyhow::bail!(
                                        "expected '}}' after lua code of \"{name}\" at line {l2}"
                                    );
                                }
                            }
                            break (
                                name.clone(),
                                RecursiveNode::String(code.clone(), ByteSpan { start: 0, end: 0 }),
                            );
                        }
                        Some(NginxToken::LBrace(_)) => {
                            *pos += 1;
                            let inner = parse_nginx_entries(tokens, pos, false)?;
                            let key = if args.is_empty() {
                                name.clone()
                            } else {
                                format!("{name} {}", args.join(" "))
                            };
                            break (key, RecursiveNode::Map(inner));
                        }
                        Some(NginxToken::RBrace(l)) => {
                            anyhow::bail!("expected ';' or '{{' after \"{name}\" at line {l}");
                        }
                        None => {
                            anyhow::bail!("expected ';' or '{{' after \"{name}\" at line {line}");
                        }
                    }
                };
                push_nginx_entry(&mut entries, key, value);
            }
            NginxToken::RBrace(line) if !top => {
                *pos += 1;
                return Ok(entries);
            }
            NginxToken::RBrace(line) => {
                anyhow::bail!("unexpected '}}' at line {line}");
            }
            NginxToken::Semi(line) => {
                anyhow::bail!("unexpected ';' at line {line}");
            }
            NginxToken::LBrace(line) => {
                anyhow::bail!("unexpected '{{' at line {line}");
            }
            NginxToken::LuaCode(_, line) => {
                anyhow::bail!("unexpected lua code outside block at line {line}");
            }
        }
    }
    if !top {
        anyhow::bail!("missing '}}' at end of file");
    }
    Ok(entries)
}

/// Thêm entry vào scope; key trùng (nhiều `server {}`, nhiều `add_header;`)
/// gộp thành `Array`.
fn push_nginx_entry(
    entries: &mut Vec<(String, RecursiveNode, ByteSpan)>,
    key: String,
    value: RecursiveNode,
) {
    let span = ByteSpan { start: 0, end: 0 };
    if let Some(slot) = entries.iter_mut().find(|(k, _, _)| *k == key) {
        match &mut slot.1 {
            RecursiveNode::Array(items) => items.push((value, span)),
            old => {
                let prev = old.clone();
                *old = RecursiveNode::Array(vec![(prev, span), (value, span)]);
            }
        }
    } else {
        entries.push((key, value, span));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocConfig, DocumentGraph};
    use codegraph_graph::InMemoryStorage;
    use std::sync::Arc;
    use tokio::sync::RwLock as TokioRwLock;

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

    #[test]
    fn nginx_parser_nested_blocks_and_directives() {
        let src = r#"
# global comment
worker_processes auto;

http {
    include mime.types;
    server {
        listen 8080;
        server_name example.com;
        location /api {
            proxy_pass http://backend;
            add_header X-A 1;
        }
    }
    upstream backend {
        server 10.0.0.1:8080;
        server 10.0.0.2:8080;
    }
}
"#;
        let doc = NginxParser.parse("/etc/nginx/nginx.conf", src, 1).unwrap();
        let find = |key: &str| doc.nodes.iter().find(|n| n.key.as_deref() == Some(key));
        // root, worker_processes, http, include, server, listen, server_name,
        // "location /api", proxy_pass, add_header (Array + 2 strings),
        // "upstream backend", server trùng (Array + 2 strings) = 16 node.
        assert_eq!(doc.nodes.len(), 16);
        // Block có args → key gồm cả args.
        assert!(find("location /api").is_some());
        assert!(find("upstream backend").is_some());
        // Directive nhiều args.
        assert!(find("worker_processes").is_some());
        // Trùng key trong upstream gộp thành 1 entry Array với 2 con.
        let ups = find("upstream backend").unwrap();
        let servers: Vec<_> = doc
            .nodes
            .iter()
            .filter(|n| n.parent == Some(ups.id) && n.key.as_deref() == Some("server"))
            .collect();
        assert_eq!(servers.len(), 1);
        // Con của entry Array: 2 server theo thứ tự khai báo.
        let kids: Vec<_> = doc
            .nodes
            .iter()
            .filter(|n| n.parent == Some(servers[0].id))
            .collect();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].value, Some(Scalar::String("10.0.0.1:8080".to_string())));
        assert_eq!(kids[1].value, Some(Scalar::String("10.0.0.2:8080".to_string())));
    }

    #[test]
    fn nginx_parser_syntax_errors() {
        // Thiếu ';' trước '{' lạc.
        assert!(NginxParser.parse("a.conf", "foo bar }", 1).is_err());
        // Thiếu '}' cuối file.
        assert!(NginxParser.parse("a.conf", "http { server { listen 80;", 1).is_err());
        // Dấu ';' đứng một mình.
        assert!(NginxParser.parse("a.conf", ";", 1).is_err());
    }

    #[test]
    fn nginx_parser_variables_quoted_and_lua() {
        // Issue 17: `${uri}` trong value — `{`/`}` trong var-ref không phải block.
        let doc = NginxParser
            .parse("a.conf", "location / {\n set $serve_URL $fullurl${uri}index.html;\n}", 1)
            .unwrap();
        let set = doc
            .nodes
            .iter()
            .find(|n| n.key.as_deref() == Some("set"))
            .unwrap();
        // 2 args → Array; `${uri}` giữ nguyên trong arg thứ 2.
        let last = doc
            .nodes
            .iter()
            .filter(|n| n.parent == Some(set.id))
            .last()
            .unwrap();
        assert_eq!(last.value, Some(Scalar::String("$fullurl${uri}index.html".to_string())));

        // Issue 65: quoted string chứa `{`/`}` — không đếm là block delimiter.
        let doc = NginxParser
            .parse(
                "a.conf",
                "log_format main '{' '\"msec\": \"$msec\" ' '}';\nerror_log off;",
                1,
            )
            .unwrap();
        assert!(doc.nodes.iter().any(|n| n.key.as_deref() == Some("error_log")));

        // Quoted string unquote + escape `\"`.
        let doc = NginxParser
            .parse("a.conf", r#"directive "with a quoted \" good.";"#, 1)
            .unwrap();
        let d = doc.nodes.iter().find(|n| n.key.as_deref() == Some("directive")).unwrap();
        assert_eq!(d.value, Some(Scalar::String("with a quoted \" good.".to_string())));

        // `_by_lua_block` — code thô giữ nguyên, `{`/`}` trong comment không đổi depth.
        let doc = NginxParser
            .parse(
                "a.conf",
                "location = /foo {\n rewrite_by_lua_block {\n  t = { key=\"foo\" } # comment { unexpect\n }\n}\n",
                1,
            )
            .unwrap();
        let loc = doc
            .nodes
            .iter()
            .find(|n| n.key.as_deref() == Some("location = /foo"))
            .unwrap();
        let lua = doc
            .nodes
            .iter()
            .find(|n| n.parent == Some(loc.id) && n.key.as_deref() == Some("rewrite_by_lua_block"))
            .unwrap();
        assert!(matches!(lua.value, Some(Scalar::String(ref s)) if s.contains("t = { key=\"foo\" }")));

        // Unclosed quote → lỗi có số dòng.
        let err = NginxParser.parse("a.conf", "server {\n set $a \"unterminated\n}", 1);
        assert!(err.is_err());
    }

    #[test]
    fn nginx_detect_format() {
        assert_eq!(detect_format("conf/nginx.conf").unwrap(), "nginx");
        assert_eq!(detect_format("a.CONF").unwrap(), "nginx");
    }

    #[tokio::test]
    async fn nginx_ingest_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nginx.conf");
        std::fs::write(&p, "events { worker_connections 1024; }\n").unwrap();
        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage, DocConfig::default());
        let _doc_id = graph.ingest_file(p.to_str().unwrap(), None).await.unwrap();
        // root + events + worker_connections = 3.
        assert_eq!(graph.stats().nodes, 3);
        assert_eq!(graph.stats().docs, 1);
    }
}

/// Detect document format từ extension: `tf`/`hcl` → hcl, `yaml`/`yml`,
/// `json`, `toml`. Lỗi khi extension không nhận diện được.
pub fn detect_format(path: &str) -> Result<String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "tf" | "hcl" => Ok("hcl".to_string()),
        "yaml" | "yml" => Ok("yaml".to_string()),
        "json" => Ok("json".to_string()),
        "toml" => Ok("toml".to_string()),
        "conf" | "nginx" => Ok("nginx".to_string()),
        _ => Err(anyhow::anyhow!(
            "unknown format for extension .{ext}; specify --format to override"
        )),
    }
}

/// Chọn parser theo format name (`"hcl"`, `"yaml"`, `"json"`, `"toml"`).
pub fn parser_for(format: &str) -> Result<Box<dyn DocParser>> {
    match format {
        "hcl" => Ok(Box::new(HclParser)),
        "yaml" => Ok(Box::new(YamlParser)),
        "json" => Ok(Box::new(JsonParser)),
        "toml" => Ok(Box::new(TomlParser)),
        "nginx" => Ok(Box::new(NginxParser)),
        _ => Err(anyhow::anyhow!("unsupported document format: {format}")),
    }
}
