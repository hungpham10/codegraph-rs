---
name: codegraph-figma-to-frontend
description: Turn a Figma design into frontend code — use when the user gives a Figma file key and wants components/assets extracted, mapped onto the existing frontend codebase, and implemented.
---

# Figma → Frontend with figma-cli + CodeGraph

Two-step pipeline: **figma-cli** (in `tools/figma-cli/`) fetches the design; **CodeGraph's document graph** ingests the exported JSON so components can be queried structurally; **CodeGraph's code index** maps each component onto the existing frontend codebase.

## Prerequisites

- `FIGMA_ACCESS_TOKEN` must be set (Figma personal access token). If missing, stop and ask the user for it — do not guess.
- Install once if needed: `uv pip install -e <repo>/tools/figma-cli` (provides the `figma` command).

## Step 1 — Fetch the design

```bash
export FIGMA_ACCESS_TOKEN=...
figma export <file_key> -o design.json        # file + components + styles, one JSON
figma list-components <file_key>              # quick inventory
figma list-styles <file_key>                  # colors / text styles
figma export-assets <file_key> 1:2 3:4 -f png -s 2   # export node images
```

- Get `<file_key>` from the Figma URL (`figma.com/design/<file_key>/...`).
- Export assets into the project's asset directory (e.g. `web/src/assets/`) using node ids for the components you will implement.

## Step 2 — Ingest into the document graph

```
codegraph_graphdoc_ingest {"path": "design.json"}          # returns doc id + node count
codegraph_graphdoc_stats {}
```

The design JSON becomes a queryable document tree (MAP/ARRAY/FIELD nodes).

## Step 3 — Query the design structurally

| Question | Tool |
|---|---|
| Everything under `components` / `styles` | `codegraph_graphdoc_search {"pattern": "components"}` (dotted paths, matches at increasing depth) |
| Find nodes by value (name, color, font) | `codegraph_graphdoc_search_value {"query": "Button"}` |
| Find by structure (e.g. `MAP, FIELD, NUMBER` leaves) | `codegraph_graphdoc_search_struct {"pattern": "MAP,FIELD,NUMBER"}` |
| Characteristic/rare structures in the design | `codegraph_graphdoc_mine_patterns {}` → then `codegraph_graphdoc_list_patterns` |
| Expand one node into a small payload (path, key, value, children) | `codegraph_graphdoc_hydrate {"node_id": 123, "max_depth": 2}` |
| Paginate/large docs | `codegraph_graphdoc_hydrate` with small `max_depth` (1–3) |
| Remove a re-exported design | `codegraph_graphdoc_remove {"doc_id": <id>}` |

Use `codegraph_graphdoc_mine_patterns` first on a large design — rare patterns often reveal the component scaffolding; background structures rank last.

## Step 4 — Map onto the codebase

For each component to implement:

1. Locate existing code: `codegraph_context {"query": "<ComponentName>"}` on the workspace (bind first with `codegraph_init`).
2. If the component exists, plan the modification against its `codegraph_flow` and `codegraph_impact` (blast radius before editing).
3. If new, follow the project's frontend conventions; reuse tokens from `figma list-styles` for colors/typography.

## Step 5 — Implement and verify

- Write the component using the extracted values (sizes, colors, text) read via `graphdoc_hydrate`, not from screenshots.
- Reference exported assets by their project-relative paths.
- After editing source, `codegraph_index {}` before any further graph queries about the edited code (index is a snapshot).

## Gotchas

- The document graph and code graph are separate datasets — `codegraph_graphdoc_*` never sees source code, `codegraph_search_symbol` never sees the design.
- `figma export` output can be large; prefer `list-components` for inventory and targeted `graphdoc_search` over dumping the whole JSON into context.
- Node ids in the document graph are per-ingest; re-ingesting invalidates old ids.
