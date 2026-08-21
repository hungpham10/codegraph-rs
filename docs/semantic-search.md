# Semantic Search (Embeddings)

Optional vector similarity search over symbol embeddings. Off by default.

## Quick Start

```toml
# .codegraph/config.toml
[embedding]
backend = "fastembed"
model = "bge-small-en-v1.5"
cache_dir = "~/.cache/codegraph/embeddings"
```

```bash
# Pre-download model (optional, for offline indexing)
codegraph embed --model bge-small-en-v1.5

# Re-index to generate embeddings
codegraph init
```

## How It Works

1. **Model**: BGE-small-en-v1.5 (384-dim, ONNX) via fastembed
2. **Indexing**: Each symbol's name + signature → embedding vector
3. **Storage**: Vectors persisted alongside graph (in same backend)
4. **Query**: `codegraph_search_symbol` with `match = "semantic"` or `"hybrid"`
5. **Hybrid**: Reciprocal Rank Fusion (RRF) merges substring + semantic results

## Configuration Reference

| Key | Required | Default | Description |
|-----|----------|---------|-------------|
| `backend` | Yes* | unset (off) | `"fastembed"` to enable; `"hashing"` for deterministic fallback |
| `model` | No | `"bge-small-en-v1.5"` | ONNX model name (must be 384-dim) |
| `cache_dir` | No | `"~/.cache/codegraph/embeddings"` | Global model cache |
| `vss_extension` | No | unset | SQLite-only: path to sqlite-vss for HNSW ANN |
| `execution_provider` | No | unset | `"coreml"` for macOS Apple Neural Engine/GPU |

*Required to enable — if unset, semantic search is completely disabled (no model loads).

## MCP Tool Changes

With embeddings enabled, `codegraph_search_symbol` gains:

| Match Mode | Description |
|------------|-------------|
| `contains` (default) | Substring match on lowercase names |
| `prefix` / `suffix` / `exact` | String match variants |
| `semantic` | Vector KNN — finds symbols by semantic similarity |
| `hybrid` | RRF merge of `contains` + `semantic` |

**Example**:
```json
// Semantic search
{ "query": "user authentication", "match": "semantic", "limit": 10 }

// Hybrid (recommended for best recall)
{ "query": "auth user", "match": "hybrid", "limit": 10 }
```

## SQLite + sqlite-vss (HNSW ANN)

For large indexes, exact KNN (brute-force) is slow. SQLite can use the `sqlite-vss` extension for HNSW approximate nearest neighbor.

**Setup**:
1. Install sqlite-vss (see https://github.com/asg017/sqlite-vss)
2. Point `vss_extension` to the extension directory:
```toml
[embedding]
backend = "fastembed"
vss_extension = "~/.cache/codegraph/embeddings/vss"
```
3. Re-index — vectors will be indexed in HNSW

**Trade-offs**:
- HNSW: faster queries, approximate results, extra disk space
- Brute-force: exact, slower on >100k vectors, no extra deps

## macOS Hardware Acceleration (CoreML)

On macOS, run embeddings on Apple Neural Engine / GPU via CoreML execution provider.

**Build**:
```bash
cargo build --features fastembed,apple-accel
```
*Fails on non-macOS.*

**Config**:
```toml
[embedding]
backend = "fastembed"
execution_provider = "coreml"
```

**Benefits**: 2–5× faster embedding inference on Apple Silicon.

## Model Management

**Pre-download** (offline indexing):
```bash
codegraph embed --model bge-small-en-v1.5 --cache-dir ~/.cache/codegraph/embeddings
```
- Requires binary built with `--features fastembed`
- Downloads ONNX model to cache dir
- Subsequent indexing works offline

**Cache location**: `~/.cache/codegraph/embeddings/` (configurable via `cache_dir`)

**Model files** (~50 MB):
- `model.onnx` — the quantized BGE-small model
- `tokenizer.json` — tokenizer config

## Error Handling

**Critical**: If the model fails to load (no network, missing ONNX runtime, corrupted cache), **opening the index errors out**. There is no silent fallback to lexical-only search.

This is by design — silent fallback would return misleading results.

**Troubleshooting**:
- Verify `cache_dir` exists and is writable
- Check ONNX Runtime is available (bundled in release binary)
- Run `codegraph embed` to re-download model
- Check logs: `RUST_LOG=codegraph_graph=debug codegraph init`

## Performance

| Metric | Value |
|--------|-------|
| Model size | ~50 MB (ONNX, int8 quantized) |
| Dimensions | 384 |
| Embedding latency | ~2–5 ms/symbol (CPU), ~0.5–1 ms (CoreML) |
| Index overhead | 384 × 4 bytes × num_symbols (~1.5 KB/symbol) |
| Query latency (brute-force) | O(N) — ~100k vectors = ~50 ms |
| Query latency (HNSW) | O(log N) — ~100k vectors = ~2 ms |

## When to Enable

✅ **Enable if**:
- Agents search by concept/intent ("error handling", "database connection")
- Codebase has inconsistent naming (synonyms, abbreviations)
- You want "fuzzy" symbol discovery

❌ **Skip if**:
- Strict name-based search is sufficient
- Indexing speed is critical (embeddings add ~2–5 ms/symbol)
- Disk space is constrained
- Offline-only with no pre-download opportunity

## Related Docs

- [Configuration](configuration.md) — Full config.toml reference
- [Storage Backends](storage-backends.md) — Vector storage per backend
- [MCP Tools](../README.md#mcp-tools) — `codegraph_search_symbol` reference
- [README](../README.md) — Quick start