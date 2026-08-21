# Storage Backends

Deep dive on CodeGraph's pluggable storage backends.

## Overview

CodeGraph's `GraphIndex` uses a pluggable storage abstraction. The backend is selected via `[storage] type` in `config.toml`.

| Backend | Type | Persistence | Concurrency | Best For |
|---------|------|-------------|-------------|----------|
| SQLite | Embedded SQL | Single file (WAL) | Single-writer, multi-reader | Default, local projects |
| LMDB | Embedded KV (mmap) | Directory | Multi-reader, single-writer | Large indexes, mmap-friendly |
| Redis | Client-server | Remote | Multi-writer | Shared index, multi-process |
| Memory | In-process | None | N/A | Testing, ephemeral |
| PostgreSQL | Client-server (sharded) | Remote | Multi-writer | Multi-tenant, production |
| MySQL | Client-server (sharded) | Remote | Multi-writer | Multi-tenant, production |

---

## SQLite (Default)

**Config**:
```toml
[storage]
type = "sqlite"
# dsn = "sqlite:///absolute/path/to/db.sqlite"  # optional override
```

**Characteristics**:
- Single file: `.codegraph/db.sqlite` (WAL mode)
- Entities + radix streams stored in tables
- No external dependencies (bundled `rusqlite` with `bundled` feature)
- WAL mode allows concurrent readers during write
- **Default and recommended** for most local use

**Performance** (from `crates/codegraph-bench/STORAGE_PERF.md` on `crates/` corpus):
- Open + ingest (median): ~12–14 µs (in-memory baseline), ~40–43 ms (SQLite on disk)
- On-disk size: ~590–690 KB for `crates/` workspace

**Limitations**:
- Single-writer — not suitable for concurrent multi-process writes
- File-based — not network-accessible

---

## LMDB

**Config**:
```toml
[storage]
type = "lmdb"
# dsn = "lmdb:///absolute/path/to/db.lmdb"  # optional override
```

**Characteristics**:
- Memory-mapped KV store (`.codegraph/db.lmdb/` directory)
- Bundled C library (`lmdb-rkv`) — no system dependency
- Zero-copy reads via mmap — excellent for read-heavy workloads
- Single-writer, multi-reader (like SQLite)
- **Smaller on-disk footprint** than SQLite (~2.2× smaller per benchmarks)

**Performance** (same corpus):
- Open + ingest (median): ~16–28 ms
- On-disk size: ~270 KB for `crates/` workspace

**When to choose**:
- Very large indexes where mmap helps
- Read-heavy workloads
- You want smaller disk usage

**Limitations**:
- Single-writer
- Directory-based (not a single file)
- Map size must be configured for very large DBs (handled automatically)

---

## Redis

**Config**:
```toml
[storage]
type = "redis"
dsn = "redis://localhost:6379"  # REQUIRED
```

**Characteristics**:
- Client-server — requires running Redis instance
- Supports multi-process / multi-machine access
- Uses Redis hashes/streams for entities and indexes
- Connection pooling via `redis` crate with `tokio-comp`

**When to choose**:
- Multiple processes sharing one index
- Index lives on a separate server
- Need pub/sub for cache invalidation (future)

**Limitations**:
- Network latency on every operation
- Requires Redis server management
- No embedded mode

---

## Memory (Ephemeral)

**Config**:
```toml
[storage]
type = "memory"
```

**Characteristics**:
- Pure in-process `DashMap` + in-memory engines
- Nothing persisted — index lost on exit
- Fastest for benchmarks/testing

**When to choose**:
- Unit tests
- Ephemeral indexing (CI, scripting)
- Benchmarking storage overhead

---

## PostgreSQL (Multi-Tenant, Sharded)

**Config**:
```toml
[storage]
type = "postgres"
dsns = [
  "postgres://user:pass@db1:5432/codegraph",
  "postgres://user:pass@db2:5432/codegraph",
]
# repo_id auto-generated and written to config
# repo_id = 14028493579208694412
```

**Architecture**:
- Every table partitioned by leading `repo_id` (`u64`)
- Each project root (`.codegraph/`) → its own `repo_id`
- Sharding: `shard = repo_id % len(dsns)`
- Re-indexing/deleting one repo never touches another

**Schema** (manual apply required):
```bash
# Run against EVERY shard
psql "$DSN" -f sql/postgres/001-initial-schema.sql
psql "$DSN" -f sql/postgres/002-add-repos-registry.sql
```

**Tables** (per shard):
- `repos` — registry of `repo_id` → root path
- `entities` — symbols (partitioned by `repo_id`)
- `chains` — call chains (partitioned)
- `call_records` — resolved calls (partitioned)
- `edges` — derived edges (partitioned)
- `vectors` — embeddings (partitioned, if enabled)

**Build**: Requires `rdbms` feature (on by default for `codegraph` binary):
```bash
cargo build --features rdbms
cargo build -p codegraph-mcp --features rdbms
```

**When to choose**:
- Multi-tenant SaaS (each customer = one repo_id)
- Shared infrastructure, isolated data
- Need SQL tooling for analytics

**Limitations**:
- Manual schema management
- Network latency
- More complex ops

---

## MySQL (Multi-Tenant, Sharded)

**Config**:
```toml
[storage]
type = "mysql"
dsns = [
  "mysql://user:pass@db1:3306/codegraph",
  "mysql://user:pass@db2:3306/codegraph",
]
# repo_id auto-generated
```

**Schema** (manual apply):
```bash
mysql "$DB" < sql/mysql/001-initial-schema.sql
mysql "$DB" < sql/mysql/002-add-repos-registry.sql
```

Same architecture as Postgres — partitioned by `repo_id`, sharded by `repo_id % N`.

**When to choose**: Same as Postgres, but MySQL preferred.

---

## Backend Selection Guide

| Scenario | Recommended |
|----------|-------------|
| Local development, single project | `sqlite` (default) |
| Large local index, read-heavy | `lmdb` |
| Multiple agents/processes same machine | `redis` or `lmdb` |
| Team shared index (LAN) | `redis` |
| Multi-tenant SaaS | `postgres` or `mysql` |
| CI/testing | `memory` |
| Production with SQL tooling needs | `postgres` |

---

## Switching Backends

1. Update `config.toml` `[storage] type = "..."`
2. Run `codegraph init` (or `codegraph_index` via MCP) — full re-index
3. Old index files remain but are unused (safe to delete `.codegraph/db.*`)

**Note**: No migration between backends — always full re-index from source.

---

## Performance Notes

From `crates/codegraph-bench/STORAGE_PERF.md` (local `crates/` corpus, 3 runs median):

| Backend | Open+Ingest | On-Disk Size | Query Latency (200 ops) |
|---------|-------------|--------------|-------------------------|
| `in_memory` | ~12 µs | N/A | ~84–90 ns/op |
| `sqlite` | ~40–43 ms | ~590–690 KB | ~84–90 ns/op |
| `lmdb` | ~16–28 ms | ~270 KB | ~84–90 ns/op |

- Query latency dominated by in-memory engines (radix + chain search), not storage
- High variance noted in SQLite/LMDB ingest ("measurement machine was loaded")
- LMDB ~1.4–2.1× faster ingest than SQLite; ~2.2× smaller on disk

---

## Related Docs

- [Configuration](configuration.md) — Full config.toml reference
- [Architecture](architecture.md) — GraphIndex and storage abstraction
- [SQL Schema](sql/README.md) — Postgres/MySQL schema details
- [README](../README.md) — Quick start