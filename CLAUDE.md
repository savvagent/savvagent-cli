# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Savvagent — a Rust-only, MCP-first terminal coding agent. Vision and scope live in `PRD.md`; the SPP wire format is in `crates/savvagent-protocol/SPEC.md`. Developer-facing details (slash commands, env vars, on-disk paths) are in `README.md`.

## Common commands

```bash
# Build everything. Required even for TUI-only work, because the TUI spawns
# `savvagent-tool-fs` at runtime and needs that binary to exist.
cargo build

# Run the TUI (default workspace member).
cargo run -p savvagent

# Tests.
cargo test --workspace
cargo test -p savvagent-host                     # single crate
cargo test -p savvagent-host -- name::of::test   # single test

# Continuous check / clippy via bacon.
bacon                # cargo check (default)
bacon clippy-all     # clippy across the workspace
bacon test           # cargo test

# Headless smoke-test (needs a provider — see README "Running providers as
# standalone MCP servers"):
cargo run -p savvagent-host --example headless -- "list my Cargo.toml"
```

`.env` and `.env.local` at repo root are auto-loaded on startup.

## Architecture (the parts you can't see by reading one file)

The core abstraction is "everything is MCP-shaped." A provider is just a `ProviderHandler` from `savvagent-mcp`; a tool is just a stdio MCP server. Each can be wrapped in a binary (`provider-anthropic` ships `savvagent-anthropic`, `tool-fs` ships `savvagent-tool-fs`), but providers are linked **in-process by default** via `InProcessProviderClient` — the binary form exists for wire-protocol debugging.

### Turn loop

`Host` (in `savvagent-host`) holds a `Box<dyn ProviderClient>` plus a `ToolRegistry`. `Host::run_turn_streaming` loops `provider.complete` → `tool_registry.call` until the model emits `end_turn`, forwarding `StreamEvent`s out as it goes. The tool-use loop, session state, and project-context loading (`SAVVAGENT.md` if present) all live here — the TUI is a thin shell on top.

### Host swap

The TUI keeps the active host as `Arc<RwLock<Option<Arc<Host>>>>`. Per-turn worker tasks **clone the `Arc<Host>` under a brief read lock** and then drop the guard before any `.await` — never hold the `RwLock` across awaits. `/connect` swaps the slot atomically. See `crates/savvagent/src/app.rs` and `tui.rs`.

### Provider transport split

- **In-process (default):** `InProcessProviderClient` wraps a `ProviderHandler` directly — no HTTP, no serialization round-trip.
- **MCP HTTP (opt-in):** when `SAVVAGENT_PROVIDER_URL` is set, the TUI connects to a remote provider over `rmcp`'s Streamable HTTP transport instead.

The `Host` only sees `Box<dyn ProviderClient>` and doesn't know which path is in use. There is **no provider registry inside the host**.

### Tool transport

Tools are always stdio child processes owned by `ToolRegistry`. They're reaped on shutdown. The TUI bakes in one (`savvagent-tool-fs`, locatable via `$PATH` or `SAVVAGENT_TOOL_FS_BIN`); additional tools can be added via `HostConfig::with_tool` in `crates/savvagent/src/main.rs`.

### `rmcp` ProgressDispatcher gotcha

`subscriber.next()` from `rmcp`'s `ProgressDispatcher` does **not** auto-close when the RPC completes. Forwarder tasks that pump progress notifications must `JoinHandle::abort()` after the request future resolves, or the caller's mpsc waiter will deadlock. This pattern is used in `provider-anthropic`/`provider-gemini` streaming paths.

## Workspace map (for navigation)

| Crate | Owns |
|---|---|
| `crates/savvagent` | TUI binary, `/connect`, file picker, transcript persistence, the `PROVIDERS` registry. |
| `crates/savvagent-host` | `Host`, `ToolRegistry`, session state, project context (`SAVVAGENT.md`). |
| `crates/savvagent-protocol` | Pure types: `CompleteRequest`, `CompleteResponse`, `StreamEvent`, content blocks. |
| `crates/savvagent-mcp` | `ProviderClient` / `ProviderHandler` traits and the `InProcessProviderClient` bridge. |
| `crates/provider-anthropic`, `crates/provider-gemini` | Provider impls (libraries) + thin `savvagent-<vendor>` MCP-server binaries. |
| `crates/tool-fs` | `read_file` / `write_file` / `list_dir` / `glob` as a stdio MCP server. |

## Extending

The README has step-by-step recipes — the short version:

- **New provider:** mirror `provider-gemini`, implement `ProviderHandler`, then append a `ProviderSpec` entry (and `build_*` factory) in `crates/savvagent/src/providers.rs::PROVIDERS`. The host needs no changes.
- **New tool:** mirror `crates/tool-fs` (stdio MCP server) and register it via `HostConfig::with_tool` in `crates/savvagent/src/main.rs`.

## Persistence

- Transcripts: `~/.savvagent/transcripts/<unix>.json` (auto on `TurnComplete`, manual on `/save`).
- API keys: OS keyring under service `savvagent`, account `<provider id>`. Never written to disk in plaintext — `/connect` is the only writer.

## Claude Code skills

This repo's own committed skill location is `.github/skills/` (Copilot CLI's skill format, e.g. `savvagent-development`). `.claude/skills/<name>/SKILL.md` is the equivalent location for Claude-Code-compatible skills and is **committed**, not gitignored — `.claude/skills/rust-engineer/SKILL.md` is the existing precedent. Only repo-authored, project-specific skills belong there (mirroring or complementing `.github/skills/`, e.g. `rust-engineer`, `tui-engineer`).

Generic or personal Claude Code skills pulled from `~/.claude/skills/` (e.g. a symlinked `creating-tickets`) are **not** added under this repo's `.claude/skills/` — they stay personal and machine-local in the contributor's home directory, never checked into this tree. Nothing needs to be gitignored as a result, since a correctly-scoped personal skill is never placed under the repo in the first place.

Any tracker-related skill (ticket creation, issue triage, etc.) used while working in this repo must defer to this repo's actual tracker — **GitHub Issues**, via `gh issue`/`gh pr` and the conventions in `.github/skills/savvagent-development` — never a JIRA-first or other generic tracker abstraction a personal skill might assume.

If a Claude Code skill and a `.github/skills/` skill overlap in purpose (e.g. a personal ticket-creation skill vs. `savvagent-development`'s own ticket conventions), this repo's own `.github/skills/` skill governs while working in this repo; the personal skill's generic guidance yields.
