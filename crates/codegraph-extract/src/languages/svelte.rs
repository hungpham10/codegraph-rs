//! Svelte — parser hybrid: grammar host parse markup, script block
//! (`script_element > raw_text`) re-parse bằng JS/TS (`languages::embedded`).
//! `<script context="module">` và `<script lang="ts">` đều qua đường này.

use crate::languages::embedded;
use codegraph_graph::ParseResult;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_svelte_next::LANGUAGE.into()
}

fn detect_ts(node: &Node, src: &[u8]) -> bool {
    node.parent()
        .map(|p| embedded::script_tag_is_ts(&p, src))
        .unwrap_or(false)
}

pub struct SvelteParser;

impl SvelteParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SvelteParser {
    fn default() -> Self {
        Self
    }
}

impl crate::LangParser for SvelteParser {
    fn name(&self) -> &'static str {
        "svelte"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["svelte"]
    }
    fn ts_language(&self) -> tree_sitter::Language {
        ts_language()
    }
    fn parse_file(&self, path: &str, source: &str) -> codegraph_core::Result<ParseResult> {
        embedded::parse_embedded(
            ts_language(),
            embedded::is_script_raw_text,
            detect_ts,
            path,
            "svelte",
            source,
        )
    }
}
