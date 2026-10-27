//! Parser hybrid cho ngôn ngữ "host + script nhúng" (Svelte, Astro).
//!
//! Grammar host (tree-sitter-svelte / tree-sitter-astro) chỉ parse markup —
//! nội dung `<script>`/frontmatter là `raw_text`/`frontmatter_js_block`, không
//! ra symbol function/class nào. Module này:
//!
//! 1. Parse file bằng grammar host, thu các node script chunk (matcher tùy
//!    ngôn ngữ, lọc theo parent để không nhầm `raw_text` của `<style>`).
//! 2. Re-parse từng chunk bằng LangSpec JS (`javascript::SPEC`) hoặc TS
//!    (`typescript::SPEC` khi host khai `lang="ts"` / frontmatter) — source
//!    được pad `\n` trước để **line number khớp file gốc**.
//! 3. Merge các `ParseResult`: id local shift theo số symbol đã gộp
//!    (symbol id + `scope_id` + `type_ref` + chain key/head + `caller_id`);
//!    imports gộp vào `file_deps` cho resolve call cross-file.
//!
//! Ngoài ra mỗi file được thay bằng 1 symbol `Module` tên file-stem (component).

use crate::languages::common::{run_spec, LangSpec};
use codegraph_core::{Result, ScopeLevel, Symbol, SymbolKind, SYMBOL_BASE};
use codegraph_graph::{FileDeps, ParseResult};
use tree_sitter::{Node, Parser};

/// Node là script chunk (`raw_text` trong `script_element`, frontmatter...).
pub type ChunkMatcher = fn(&Node) -> bool;

/// Chunk có TypeScript không (`lang="ts"` trên `<script>`, frontmatter Astro).
pub type TsDetector = fn(&Node, &[u8]) -> bool;

/// Kiểm tra `<script ... lang="ts">` — text của `start_tag` chứa `lang="ts"`.
pub fn script_tag_is_ts(script_element: &Node, src: &[u8]) -> bool {
    script_element
        .children(&mut script_element.walk())
        .find(|c| c.kind() == "start_tag")
        .and_then(|tag| tag.utf8_text(src).ok())
        .map(|t| {
            let head = t.split('>').next().unwrap_or(t);
            head.contains("lang=\"ts\"") || head.contains("lang='ts'")
        })
        .unwrap_or(false)
}

/// `raw_text` con trực tiếp của `script_element` — matcher chung cho Svelte/Astro
/// (loại trừ `raw_text` của `<style>`).
pub fn is_script_raw_text(node: &Node) -> bool {
    node.kind() == "raw_text" && node.parent().is_some_and(|p| p.kind() == "script_element")
}

/// Parse file host + re-parse script chunks → `ParseResult` gộp.
pub fn parse_embedded(
    host_language: tree_sitter::Language,
    is_chunk: ChunkMatcher,
    detect_ts: TsDetector,
    path: &str,
    language: &str,
    source: &str,
) -> Result<ParseResult> {
    let mut parser = Parser::new();
    parser
        .set_language(&host_language)
        .map_err(|e| crate::parse_err(format!("set_language: {e}")))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| crate::parse_err("parse failed"))?;
    let src = source.as_bytes();

    let mut acc = ParseResult {
        path: path.to_string(),
        language: language.to_string(),
        bytes: source.len() as u64,
        lines: source.lines().count() as u32,
        symbols: vec![component_symbol(path, language, source)],
        chains: Default::default(),
        calls: Vec::new(),
        fn_ptr_assigns: Vec::new(),
        file_deps: None,
    };

    // Preorder DFS giữ document order của các chunk.
    let mut chunks = Vec::new();
    collect_chunks(&tree.root_node(), is_chunk, &mut chunks);
    for chunk in chunks {
        let text = chunk.utf8_text(src).unwrap_or_default();
        if text.trim().is_empty() {
            continue;
        }
        // Pad newline để row của sub-parse khớp line của file gốc.
        let padded = format!("{}{}", "\n".repeat(chunk.start_position().row), text);
        let sub = run_spec(
            embedded_spec(detect_ts(&chunk, src)),
            path,
            language,
            &padded,
        )?;
        merge(&mut acc, sub);
    }
    Ok(acc)
}

/// Symbol `Module` đại diện component (file-stem) — component được import ở
/// file khác nên cần một identity trong graph.
fn component_symbol(path: &str, language: &str, source: &str) -> Symbol {
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string();
    Symbol {
        id: SYMBOL_BASE,
        name: stem,
        kind: SymbolKind::Module,
        scope: ScopeLevel::Global,
        scope_id: 0,
        type_ref: 0,
        type_name: None,
        file: path.to_string(),
        line: 1,
        end_line: source.lines().count() as u32,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        language: language.to_string(),
        impl_trait: None,
        sig_key: None,
    }
}

fn collect_chunks<'a>(node: &Node<'a>, is_chunk: ChunkMatcher, out: &mut Vec<Node<'a>>) {
    if is_chunk(node) {
        out.push(*node);
    }
    let mut cursor = node.walk();
    for ch in node.children(&mut cursor) {
        collect_chunks(&ch, is_chunk, out);
    }
}

/// Spec JS/TS để parse script chunk (feature `lang-svelte`/`lang-astro` imply
/// cả `lang-javascript` + `lang-typescript` nên luôn có sẵn).
fn embedded_spec(ts: bool) -> &'static LangSpec {
    #[cfg(feature = "lang-typescript")]
    {
        let ts_spec = &crate::languages::typescript::SPEC;
        if ts {
            return ts_spec;
        }
    }
    #[cfg(feature = "lang-javascript")]
    {
        return &crate::languages::javascript::SPEC;
    }
    #[allow(unreachable_code)]
    {
        let _ = ts;
        unreachable!("lang-svelte/lang-astro imply lang-javascript")
    }
}

/// Gộp sub-result vào accumulator — shift id local theo số symbol hiện có.
fn merge(acc: &mut ParseResult, add: ParseResult) {
    let off = acc.symbols.len() as u64;
    for mut s in add.symbols {
        s.id += off;
        if s.scope_id != 0 {
            s.scope_id += off;
        }
        if s.type_ref != 0 {
            s.type_ref += off;
        }
        acc.symbols.push(s);
    }
    for (k, mut chain) in add.chains {
        if let Some(head) = chain.first_mut() {
            if *head != 0 {
                *head += off;
            }
        }
        acc.chains.insert(k + off, chain);
    }
    for mut c in add.calls {
        c.caller_id += off;
        acc.calls.push(c);
    }
    acc.fn_ptr_assigns.extend(add.fn_ptr_assigns);
    match (&mut acc.file_deps, add.file_deps) {
        (None, Some(deps)) => acc.file_deps = Some(deps),
        (Some(cur), Some(deps)) => cur.imports.extend(deps.imports),
        _ => {}
    }
}

/// Ghi đè `FileDeps` (không dùng — giữ cho API đối xứng, tránh dead-code warn
/// khi ngôn ngữ mới muốn dùng).
#[allow(dead_code)]
pub fn with_file_deps(res: &mut ParseResult, deps: FileDeps) {
    res.file_deps = Some(deps);
}
