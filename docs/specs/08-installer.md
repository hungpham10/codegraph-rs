# Spec 08 — Multi-agent installer

**Status**: ✅ done — implemented in `crates/codegraph-installer`
(`src/targets/`, `instructions-template.md` embedded via `include_str!`).

## Goal

Configure MCP clients (Claude Code, Cursor, Codex CLI, opencode) in one
command, idempotently, without breaking existing config.

## Targets

| Target | Config written | Notes |
|---|---|---|
| Claude Code | `~/.claude/settings.json` → `mcpServers.codegraph` (+ agent instructions) | JSON surgical edit |
| Cursor | `.cursor/mcp.json` | **Quirk**: wrong cwd — always inject an absolute `--path` |
| Codex | `~/.codex/config.toml` → `[mcp_servers.codegraph]` | TOML edits preserve sibling tables |
| opencode | `opencode.jsonc` / `.json` | Comment-preserving edits via `jsonc-parser` |

Each target lives in `src/targets/{id}.rs` behind a common trait
(id/label/detect/install/uninstall).

## Agent instructions

`INSTRUCTIONS_MD` (`src/instructions-template.md`) is the agent-agnostic
instruction block written next to the client config. The richer, always
current guide is [docs/codegraph.md](../codegraph.md) — also the file
embedded into the MCP server itself, so installer text and server
instructions no longer drift apart.

## Idempotence contract

- Installing twice → second run reports unchanged; file byte-equal after the
  first.
- Sibling entries (`mcpServers.other`, `[mcp_servers.other]`) survive
  untouched; comments in `.jsonc` survive.
- Uninstall removes only the `codegraph` entry.

Validated in `crates/codegraph-installer/tests/install.rs`.

## Entry point

Exposed as the `install` subcommand of the `codegraph` binary (interactive
multi-select of detected agents; `--all` for non-interactive). The original
"Hermes" target was dropped when its config format was never stabilized.
