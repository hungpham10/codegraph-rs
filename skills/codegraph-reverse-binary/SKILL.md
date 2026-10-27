---
name: codegraph-reverse-binary
description: Analyze compiled binaries with CodeGraph's binary graph — use when asked to reverse-engineer, enumerate entrypoints/imports/JNI symbols, or look up what lives at a binary address.
---

# Reverse Engineering Binaries with CodeGraph

CodeGraph maintains a separate **binary graph** dataset (extracted via radare2 during indexing, feature `binary`). Query it with the `codegraph_graphbin_*` tools — these search the binary dataset, independent from source-code symbols.

## Prerequisite: the binary must be indexed

Binaries are discovered and extracted when the workspace is indexed (orchestrator scans for binaries and runs radare2; results land in the `BinaryGraph` dataset). So:

1. `codegraph_init {"path": "<workspace containing the binary>"}`
2. `codegraph_index {}` — this is where extraction actually runs (radare2 must be on PATH).
3. Confirm with `codegraph_graphbin_stats` — it reports symbol/entrypoint/import/export counts and which binaries were indexed. If all zeros, the binary wasn't scanned (check extension, location, and that the build has feature `binary`).

## Anchoring: start from entrypoints, not grep

Do NOT grep the binary for strings as the first step. Anchor at symbols:

```
codegraph_graphbin_list {}                                  # all symbols, paginated
codegraph_graphbin_list {"flag": "entrypoint"}              # entrypoints — your starting anchors
codegraph_graphbin_list {"flag": "export"}                  # exported functions
codegraph_graphbin_list {"flag": "import"}                  # imported library calls (API surface)
codegraph_graphbin_list {"flag": "jni"}                     # JNI symbols (Android/Java interop)
codegraph_graphbin_list {"flag": "import", "kind": "Function", "limit": 100, "offset": 0}
codegraph_graphbin_addr {"addr": 4198400}                   # O(1) lookup at a virtual address
codegraph_graphbin_addr {"path": "/path/to/binary"}         # entrypoints of one binary
codegraph_graphbin_stats {}                                 # overview: what got indexed
```

- Results come back in two sections: `code` (source index hits) and `binary` (binary graph hits) when searching with `codegraph_search_symbol` and `source: "all"`. Use `source: "binary"` to restrict to binary symbols only.
- `codegraph_graphbin_addr` gives exact symbol-at-address lookups — use it to name addresses found in disassembly or crash logs.

## Typical analysis flow

1. `codegraph_graphbin_stats` → what binaries and symbol classes exist.
2. `codegraph_graphbin_list {flag: "entrypoint"}` → anchor list.
3. From an entrypoint symbol, switch to the source-side tools (`codegraph_flow`, `codegraph_callers`) when the binary was built from indexed sources — the shared symbol space lets you trace chains across both datasets.
4. `codegraph_graphbin_addr` to resolve any address you encounter.

## Notes

- Requires `radare2` on PATH for extraction.
- Results are a snapshot, same as the source index — re-run `codegraph_index` after adding/replacing a binary.
- `codegraph_graphbin_list` is paginated: use `offset`/`order` (`name` | `addr` | `id`) for large binaries.
