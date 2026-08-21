# CodeGraph

[![CI](https://github.com/hungpham10/codegraph-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/hungpham10/codegraph-rs/actions/workflows/ci.yml)
[![CodSpeed Badge](https://img.shields.io/endpoint?url=https://app.codspeed.io//badge.json)](https://app.codspeed.io//hungpham10/codegraph-rs?utm_source=badge)
[![codecov](https://codecov.io/gh/hungpham10/codegraph-rs/graph/badge.svg?token=PUSFMM0CM8)](https://codecov.io/gh/hungpham10/codegraph-rs)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> Local-first code intelligence for AI agents. Built in Rust.

CodeGraph parses your codebase with tree‑sitter, builds a semantic graph where each symbol has a global ID and each function a call chain, and serves the graph to AI agents via the Model Context Protocol (MCP).

## Install

**Automatic (recommended)**

- **Linux / macOS**: `curl -fsSL https://raw.githubusercontent.com/hungpham10/codegraph-rs/main/scripts/install.sh | sh`
- **Windows (PowerShell)**: `irm https://raw.githubusercontent.com/hungpham10/codegraph-rs/main/scripts/install.ps1 | iex`

*See the full installation guide at* [docs/specs/08-installer.md](docs/specs/08-installer.md).

## Quick start

```sh
codegraph init
codegraph serve --mcp
```

## Documentation

- Architecture overview: [docs/architecture.md](docs/architecture.md)
- Detailed installation guide: [docs/specs/08-installer.md](docs/specs/08-installer.md)
- Full reference (configuration, CLI, MCP tools) – see the original README for comprehensive information.
