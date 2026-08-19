# Spec 05 — Call resolution

**Status**: ✅ done — implemented as the resolve phase of
`GraphIndex::ingest` (`crates/codegraph-graph/src/lib.rs`). The originally
planned `codegraph-resolve` crate (import resolver + 17 framework resolvers)
was not ported; resolution today is name-based over the global registry.

## Goal

Turn each chain's placeholder-`0` positions (unresolved `CallRecord`s) into
real callee symbol ids, so that chains, edges, and the call-name index are
consistent after ingest.

## Resolution order

For every `CallRecord`, ingest tries in order:

1. **Structural hint** — `target_class` / `target_method` (e.g. a Java class
   literal or receiver type captured at parse time) narrows the candidate set.
2. **Exact name match** — the full call name (`fmt.Println`,
   `orderRepository.saveOrder`) against the global name index.
3. **Short name** — the last segment of the call name (`Println`,
   `saveOrder`).
4. **Best candidate scoring** — when several symbols share the name:
   `override +5` · `has-chain +5` (the candidate itself has a chain, i.e. is a
   function) · `same-file +3`. Highest score wins.

Unresolved calls keep their placeholder `0` but **remain queryable** through
the inverted call-name index (`callers_by_call_name` — the window to the
"outside world" of libraries), and `codegraph_references` /
`codegraph_search_by_call` surface them like resolved ones.

## Derived state

After resolution, ingest builds:

- `chains_map` — func id → chain (remapped local → global ids)
- `edges: HashMap<(caller, callee), EdgeMeta>` — position, guard condition,
  effect, `is_loop_body`, `is_recursive`
- `call_names` — lowercase call name (+ type-qualified aliases) → call sites

All of it is rebuilt from chains + records on every ingest and on reopen.

## What is explicitly *not* done

- No import-graph resolution (relative/alias/bare-module), no tsconfig path
  aliases, no cargo-workspace member mapping.
- No framework route resolvers (express/laravel/rails/spring/gin/… → route
  nodes). Route/handler knowledge is left to the LLM reading `codegraph_flow`
  / `codegraph_search_by_annotation` output.

Rationale: the semgraph call chains already answer the questions agents ask
("who calls this", "what does this flow do"); a framework-specific resolver
layer added maintenance cost without changing agent behavior. If route nodes
become a requirement, they should be added as a post-ingest pass over
annotations + call patterns rather than a separate crate.
