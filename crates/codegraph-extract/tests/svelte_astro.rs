//! Integration test cho parser hybrid Svelte/Astro (`languages::embedded`):
//! symbol từ script block, line number khớp file gốc, TS detection, merge id,
//! import deps, và registry dispatch theo extension.

use codegraph_core::SymbolKind;
use codegraph_extract::LangParser;

#[test]
fn svelte_script_symbols_with_correct_lines() {
    let src = r#"<script>
  import { helper } from './helper';

  let count = 0;
  const double = () => count * 2;

  function greet(name) {
    return helper(name);
  }
</script>

<button on:click={() => (count += 1)}>{double()}</button>
"#;
    let parser = codegraph_extract::languages::svelte::SvelteParser::new();
    let res = parser.parse_file("src/App.svelte", src).unwrap();

    assert_eq!(res.language, "svelte");
    // Component Module + double + greet (count là variable_declarator Variable).
    let names: Vec<(&str, SymbolKind)> = res
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.kind))
        .collect();
    assert!(names.contains(&("App", SymbolKind::Module)), "{names:?}");
    assert!(
        names.contains(&("double", SymbolKind::Function)),
        "{names:?}"
    );
    assert!(
        names.contains(&("greet", SymbolKind::Function)),
        "{names:?}"
    );

    // Line number khớp file gốc (không phải của script content).
    let greet = res.symbols.iter().find(|s| s.name == "greet").unwrap();
    assert_eq!(greet.line, 7, "greet nằm ở dòng 7 của file .svelte");

    // Chain: greet gọi helper qua placeholder + CallRecord.
    let greet_id = greet.id;
    let chain = res.chains.get(&greet_id).expect("greet có chain");
    assert_eq!(chain[0], greet_id);
    assert!(chain.contains(&0), "greet gọi helper → placeholder 0");
    let call = res
        .calls
        .iter()
        .find(|c| c.caller_id == greet_id && c.call_name == "helper")
        .expect("call record helper");
    assert_eq!(call.line, 8);

    // Import deps từ script block → resolve cross-file.
    let deps = res.file_deps.as_ref().expect("có import");
    assert!(
        deps.imports
            .iter()
            .any(|i| i.alias == "helper" && i.path == "./helper"),
        "{:?}",
        deps.imports
    );
}

#[test]
fn svelte_ts_script_uses_typescript_spec() {
    let src = r#"<script lang="ts">
  export let name: string;
  function shout(msg: string): string {
    return msg.toUpperCase();
  }
</script>

<h1>{name}</h1>
"#;
    let parser = codegraph_extract::languages::svelte::SvelteParser::new();
    let res = parser.parse_file("src/Comp.svelte", src).unwrap();
    // TS param annotation vẫn ra Parameter (script parse bằng TS spec).
    let shout = res.symbols.iter().find(|s| s.name == "shout").unwrap();
    assert_eq!(shout.kind, SymbolKind::Function);
    assert_eq!(shout.line, 3);
}

#[test]
fn svelte_style_block_is_not_parsed_as_js() {
    let src = r#"<script>
  const x = 1;
</script>

<style>
  .btn { color: red; }
</style>
"#;
    let parser = codegraph_extract::languages::svelte::SvelteParser::new();
    let res = parser.parse_file("src/B.svelte", src).unwrap();
    // CSS không ra symbol rác; chỉ component + biến x (variable_declarator).
    let names: Vec<&str> = res.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"B"), "{names:?}");
    assert!(!names.iter().any(|n| n.contains("color")), "{names:?}");
}

#[test]
fn astro_frontmatter_and_script_tag() {
    let src = r#"---
import Card from '../components/Card.astro';
const title = 'Hello';

function buildGreeting(n: string): string {
  return `hi ${n}`;
}
---
<script>
  function clientGreet() {
    return buildGreeting('client');
  }
</script>
<html>
  <body><Card name={title} /></body>
</html>
"#;
    let parser = codegraph_extract::languages::astro::AstroParser::new();
    let res = parser.parse_file("src/pages/index.astro", src).unwrap();

    assert_eq!(res.language, "astro");
    let names: Vec<(&str, SymbolKind)> = res
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.kind))
        .collect();
    assert!(names.contains(&("index", SymbolKind::Module)), "{names:?}");
    assert!(
        names.contains(&("buildGreeting", SymbolKind::Function)),
        "{names:?}"
    );
    assert!(
        names.contains(&("clientGreet", SymbolKind::Function)),
        "{names:?}"
    );

    // Frontmatter là TS: param annotation vẫn extract.
    let bg = res
        .symbols
        .iter()
        .find(|s| s.name == "buildGreeting")
        .unwrap();
    assert_eq!(bg.line, 5);

    // clientGreet (script tag, JS) gọi buildGreeting (frontmatter) — 2 chunk
    // khác nhau, id không đè nhau.
    let cg = res
        .symbols
        .iter()
        .find(|s| s.name == "clientGreet")
        .unwrap();
    assert_ne!(cg.id, bg.id);
    let call = res
        .calls
        .iter()
        .find(|c| c.caller_id == cg.id && c.call_name == "buildGreeting")
        .expect("call buildGreeting trong clientGreet");
    assert_eq!(call.line, 11);

    let deps = res.file_deps.as_ref().expect("có import Card");
    assert!(
        deps.imports
            .iter()
            .any(|i| i.alias == "Card" && i.path == "../components/Card.astro"),
        "{:?}",
        deps.imports
    );
}

#[test]
fn registry_dispatches_by_extension() {
    use codegraph_extract::registry;
    let parsers = registry();
    let svelte = parsers
        .iter()
        .find(|p| p.name() == "svelte")
        .expect("svelte");
    assert!(svelte.extensions().contains(&"svelte"));
    let astro = parsers.iter().find(|p| p.name() == "astro").expect("astro");
    assert!(astro.extensions().contains(&"astro"));
}

#[test]
fn unique_symbol_ids_across_chunks() {
    // Nhiều chunk → id local phải unique (shift theo số symbol đã gộp).
    let src = r#"<script>
  function a() {}
</script>
<script>
  function b() {}
</script>
"#;
    let parser = codegraph_extract::languages::svelte::SvelteParser::new();
    let res = parser.parse_file("src/M.svelte", src).unwrap();
    let mut ids: Vec<u64> = res.symbols.iter().map(|s| s.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), res.symbols.len(), "id trùng: {:?}", res.symbols);
    let names: Vec<&str> = res.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&("a")), "{names:?}");
    assert!(names.contains(&("b")), "{names:?}");
}
