# Comparison — CodeGraph vs Alternatives

This document provides a detailed comparison of CodeGraph with other tools in the code intelligence and AI agent tooling space.

## Quick Comparison Table

| Tool | Category | Local-First | Semantic Graph | MCP Native | Multi-Storage | Binary Size | Best For |
|------|----------|-------------|----------------|------------|---------------|-------------|----------|
| **CodeGraph** | Code graph + MCP | ✅ | ✅ (tree-sitter semgraph) | ✅ Built-in | ✅ 6 backends | ~58 MB | Local-first AI agents needing full semantic graph |
| **Aider RepoMap** | Repo map generator | ✅ | ❌ (ctags-based) | ❌ | ❌ | N/A | Aider users wanting quick repo overview |
| **Sourcegraph Cody** | Cloud code search | ❌ (self-host option) | ✅ (CodeQL) | Via extension | ❌ | N/A | Enterprise multi-repo search |
| **Bloop** | Code indexer | ✅ | ❌ (search only) | ❌ | ❌ | ~30 MB | Fast local code search |
| **CodeQL** | Semantic analysis | ✅/Cloud | ✅ (QL queries) | ❌ | ❌ | Heavy | Security auditing, variant analysis |
| **Kythe** | Code graph | ✅ | ✅ | ❌ | ❌ | Complex setup | Large-scale build-integrated graphs |
| **LSP servers** (rust-analyzer, clangd, etc.) | Per-language IDE | ✅ | Per-lang only | ❌ | ❌ | Per-lang | IDE integration per language |
| **ast-grep** | Structural search | ✅ | ❌ (pattern match) | ❌ | ❌ | ~10 MB | AST pattern matching/replace |
| **context7** | Docs MCP | ❌ | N/A | ✅ | ❌ | N/A | API/documentation context |

---

## Detailed Breakdown

### CodeGraph (This Project)

**What it is**: A local-first semantic code graph built on tree-sitter that serves AI agents via the Model Context Protocol (MCP).

**Strengths**:
- **True semantic graph**: Symbols have global IDs; call chains capture control flow (LOOP, IF_TRUE, RETURN, etc.)
- **MCP-native**: 24 tools exposed directly — no wrapper needed
- **Multi-storage**: SQLite (default), LMDB, Redis, Postgres, MySQL, in-memory
- **Single binary**: ~58 MB with all backends + embedding runtime bundled
- **14 languages** with full extraction: TypeScript, TSX, JavaScript, Python, Go, Rust, Java, C, C++, C#, Ruby, PHP, Scala, Swift, Lua
- **Optional semantic search**: fastembed (BGE-small) for hybrid KNN + keyword search
- **Behavior sandbox**: JIT compile functions + run against Rhai mocks
- **Full re-index always**: Simpler, no stale state; 139 files in ~190 ms

**Trade-offs**:
- No incremental sync (by design — full re-index on change)
- Requires MCP-compatible agent (Claude Code, Cursor, Codex, opencode, Hermes, Antigravity)
- Postgres/MySQL schema applied manually (no auto-migrations)

---

### Aider RepoMap

**What it is**: A repository map generator integrated into Aider (AI pair programmer). Uses ctags + tree-sitter to create a condensed representation of the codebase for LLM context.

**Strengths**:
- Tightly integrated with Aider's editing workflow
- Fast, lightweight
- Works with 50+ languages via ctags

**Weaknesses vs CodeGraph**:
- Not a persistent semantic graph — regenerates per session
- No global symbol IDs or call chains
- No MCP server — only works within Aider
- No storage backends, no semantic search
- Ctags-based (less precise than tree-sitter extraction)

**When to choose**: You use Aider exclusively and want zero-setup repo context.

---

### Sourcegraph Cody

**What it is**: Enterprise code search + AI assistant. Uses CodeQL for semantic analysis, offers cloud and self-hosted options.

**Strengths**:
- Cross-repository search at scale
- CodeQL-powered semantic queries
- Enterprise features (RBAC, audit logs, compliance)
- IDE integrations (VS Code, JetBrains)

**Weaknesses vs CodeGraph**:
- Cloud-first (self-host is complex)
- Heavy infrastructure (PostgreSQL, Redis, Kafka, etc.)
- Not a local-first single binary
- No native MCP server (uses proprietary protocol)
- Expensive for teams

**When to choose**: Enterprise needing multi-repo search, compliance, and can invest in infrastructure.

---

### Bloop

**What it is**: Fast Rust-based code indexer and search tool. Focuses on regex/keyword search with some semantic awareness.

**Strengths**:
- Very fast indexing and search
- Rust-based, single binary (~30 MB)
- Local-first

**Weaknesses vs CodeGraph**:
- No semantic graph — no global IDs, no call chains
- No MCP server
- No semantic search (embeddings)
- Limited language extraction depth

**When to choose**: You only need fast code search, not semantic graph for AI agents.

---

### CodeQL

**What it is**: Semantic code analysis engine from GitHub. Uses a query language (QL) to find vulnerabilities and patterns.

**Strengths**:
- Deep semantic analysis via QL
- Industry standard for security research
- Variant analysis (find similar bugs)
- GitHub Advanced Security integration

**Weaknesses vs CodeGraph**:
- Query-based, not graph-native — you write QL, don't traverse a graph
- Heavy (Java-based, large download)
- No MCP server
- No persistent graph storage for agent queries
- Steep learning curve (QL language)

**When to choose**: Security auditing, variant analysis, compliance — not for AI agent context.

---

### Kythe

**What it is**: Google's language-agnostic code graph platform. Extracts facts from builds, stores in a graph.

**Strengths**:
- Language-agnostic (supports 15+ languages)
- Build-system integrated (Bazel, Gradle, etc.)
- Scales to massive codebases (Google-scale)

**Weaknesses vs CodeGraph**:
- Complex setup (requires build integration)
- No MCP server
- No single binary — distributed services
- Not designed for local AI agent use
- Steep operational overhead

**When to choose**: Large org with build infrastructure wanting cross-language code graph.

---

### LSP Servers (rust-analyzer, clangd, pyright, etc.)

**What it is**: Language Server Protocol implementations per language. Provide IDE-grade semantic analysis.

**Strengths**:
- Best-in-class per-language semantics
- IDE integration (completion, goto definition, refactor)
- Local-first

**Weaknesses vs CodeGraph**:
- Per-language only — no cross-language graph
- No unified symbol IDs across languages
- No MCP server (though some bridges exist)
- No persistent graph storage
- Not designed for AI agent consumption

**When to choose**: IDE development — not for AI agent context.

---

### ast-grep

**What it is**: Structural search and replace using tree-sitter patterns. Like grep but AST-aware.

**Strengths**:
- 30+ languages via tree-sitter
- Pattern matching on AST nodes
- Fast, single binary (~10 MB)
- Local-first

**Weaknesses vs CodeGraph**:
- No persistent graph — ephemeral pattern matching
- No global symbol IDs or call chains
- No MCP server
- No semantic search (embeddings)
- Not a queryable index

**When to choose**: One-off structural search/replace, codemods — not for persistent AI context.

---

### context7 (Upstash)

**What it is**: MCP server for documentation and API context. Not a code graph.

**Strengths**:
- MCP-native
- Good for API/docs lookup

**Weaknesses vs CodeGraph**:
- No code analysis whatsoever
- Cloud-only
- Different use case entirely

**When to choose**: Agents need API documentation context, not codebase understanding.

---

## Decision Matrix

| Your Need | Recommended Tool |
|-----------|------------------|
| Local AI agent + semantic graph + MCP | **CodeGraph** |
| Aider user, quick repo map | Aider RepoMap |
| Enterprise multi-repo search + compliance | Sourcegraph Cody |
| Fast local code search only | Bloop |
| Security auditing, variant analysis | CodeQL |
| Massive monorepo with build integration | Kythe |
| IDE development (completion, refactor) | LSP servers |
| Structural search/replace, codemods | ast-grep |
| API documentation for agents | context7 |

---

## Methodology

This comparison is based on:
- Public documentation and GitHub repos (as of 2026)
- Feature matrices from project READMEs
- Architecture descriptions (local vs cloud, graph vs search vs pattern-match)
- No hands-on benchmarking — performance claims are from respective projects

**Missing from this table**: Greptile (cloud PR review), Continue.dev (IDE extension), Cursor indexing (IDE-tied), Glean (enterprise search), semgrep (linting), bito/DeepSource (cloud review). These serve different primary use cases.

---

## See Also

- [README](../README.md) — Quick start and overview
- [Architecture](architecture.md) — How CodeGraph works internally
- [Configuration](configuration.md) — All config options
- [Storage Backends](storage-backends.md) — SQLite, LMDB, Redis, Postgres, MySQL deep-dive
- [Semantic Search](semantic-search.md) — Embedding setup and hybrid search