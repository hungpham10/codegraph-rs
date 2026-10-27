//! Astro — parser hybrid: grammar host parse markup + frontmatter,
//! `frontmatter_js_block` (TS) và `script_element > raw_text` re-parse bằng
//! JS/TS (`languages::embedded`).

use crate::languages::embedded;
use codegraph_graph::ParseResult;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_astro_next::LANGUAGE.into()
}

fn is_chunk(node: &Node) -> bool {
    // Frontmatter (luôn TS) hoặc `<script>` tag (không nhầm `<style>`).
    node.kind() == "frontmatter_js_block" || embedded::is_script_raw_text(node)
}

fn detect_ts(node: &Node, src: &[u8]) -> bool {
    if node.kind() == "frontmatter_js_block" {
        return true;
    }
    node.parent()
        .map(|p| embedded::script_tag_is_ts(&p, src))
        .unwrap_or(false)
}

pub struct AstroParser;

impl AstroParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AstroParser {
    fn default() -> Self {
        Self
    }
}

impl crate::LangParser for AstroParser {
    fn name(&self) -> &'static str {
        "astro"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["astro"]
    }
    fn ts_language(&self) -> tree_sitter::Language {
        ts_language()
    }
    fn parse_file(&self, path: &str, source: &str) -> codegraph_core::Result<ParseResult> {
        embedded::parse_embedded(ts_language(), is_chunk, detect_ts, path, "astro", source)
    }
}
