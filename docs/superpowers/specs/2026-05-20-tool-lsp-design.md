# tool-lsp — design

Date: 2026-05-20
Status: pending review
Roadmap: depends on `2026-05-20-host-mcp-resources-design.md` (host
prereq) — tool-lsp ships in the release immediately after.
Related: PRD lists "LSP / ACP IDE integration" as a v0.6+ item; this is
that work, scoped to the LSP half.

## Problem

Savvagent has no language intelligence. The model has to grep, read,
and reason about code without semantic information — where is this
defined, who calls it, what types does this expression have, what
diagnostics does the compiler raise after my edit. LSP servers
(`rust-analyzer`, `typescript-language-server`, `pyright`, `gopls`,
…) already provide this; we just don't speak to them.

The MCP-tool seam is the natural place to plug LSP in: a stdio MCP
server (`tool-lsp`) wraps one or more LSP children, translates LSP
requests/responses into a small MCP tool surface, and surfaces
diagnostics through the new resource-push plumbing.

## Approach

A single new crate, `crates/tool-lsp`, that:

1. **Reads a generic config**, `~/.savvagent/lsp.toml` (global) merged
   with `<repo>/.savvagent/lsp.toml` (per-repo override), describing
   language → command mappings. No language is hardcoded.
2. **Resolves workspace roots on every call** by walking parents from
   the call's `path` argument until one of the configured
   `root_markers` is found. The model never has to know the root.
3. **Spawns LSP children lazily**, keyed by `(language_id,
   workspace_root)`, and reuses them across calls. Idle pool eviction
   keeps memory bounded.
4. **Exposes one MCP tool per LSP operation** — `lsp_definition`,
   `lsp_references`, `lsp_hover`, `lsp_document_symbols`,
   `lsp_workspace_symbols`, `lsp_rename`, `lsp_code_actions` — each
   with a tight JSON schema.
5. **Publishes diagnostics as MCP resources** (`lsp://diagnostics/<absolute-path>`)
   using the host's new resource-push surface. Each
   `publishDiagnostics` from any active LSP server fires a
   `notifications/resources/updated`; the host injects the URI into the
   conversation and the model can pull contents via `read_resource`.

The model never applies edits directly. `lsp_rename` and
`lsp_code_actions` return edit descriptors as plain JSON
(`[{path, edits: [{range, new_text}]}]`); the model then issues
`tool-fs::write_file` calls to apply them. This keeps every mutation
inside the existing permission system.

## Crate layout

```
crates/tool-lsp/
├── Cargo.toml
├── src/
│   ├── main.rs         # stdio MCP server entrypoint
│   ├── config.rs       # lsp.toml load + global+repo merge
│   ├── language.rs     # extension → language id, root_markers walk
│   ├── pool.rs         # LspPool: HashMap<(LanguageId, PathBuf), Arc<LspSession>>
│   ├── session.rs      # LspSession: child + JSON-RPC client + initialize handshake
│   ├── convert.rs      # LSP types ↔ MCP-friendly JSON
│   ├── tools/
│   │   ├── mod.rs
│   │   ├── definition.rs
│   │   ├── references.rs
│   │   ├── hover.rs
│   │   ├── document_symbols.rs
│   │   ├── workspace_symbols.rs
│   │   ├── rename.rs
│   │   └── code_actions.rs
│   └── resources/
│       └── diagnostics.rs   # publish lsp://diagnostics/<path>
└── tests/
    └── fixtures/
        └── fake-lsp/         # canned JSON-RPC server for integration tests
```

The `tools/` and `resources/` submodules each own one MCP-visible
surface. Crossing them — e.g. `definition` reading diagnostics — is
disallowed; if a tool needs information another module owns, it
queries `LspPool`, not the sibling.

## Configuration

`lsp.toml` schema (one example entry shown):

```toml
[[language]]
id = "rust"
extensions = ["rs"]
root_markers = ["Cargo.toml", "rust-project.json"]
command = "rust-analyzer"
args = []
# Optional: extra env vars passed to the LSP child.
env = { RUST_LOG = "warn" }

[[language]]
id = "typescript"
extensions = ["ts", "tsx", "mts", "cts"]
root_markers = ["tsconfig.json", "package.json"]
command = "typescript-language-server"
args = ["--stdio"]
```

**Merge semantics.** The global file
(`~/.savvagent/lsp.toml`) is loaded first; per-repo
(`<repo>/.savvagent/lsp.toml`, if present) overrides by `language.id`.
A repo-level entry fully replaces the global entry for that language
— we don't deep-merge fields, because partial merges over `args` and
`env` create surprising results.

**No defaults shipped.** v1 does not bundle a built-in language list.
The first time a user runs Savvagent in a Rust project with no config,
they get nothing from tool-lsp; we document copy-pasteable entries in
the README for `rust-analyzer`, `typescript-language-server`,
`pyright-langserver`, and `gopls`. A future PR can ship a default
config that picks up servers found on `$PATH`.

## Workspace resolution

Every tool call takes a relative-to-host-cwd `path` (never an explicit
workspace_root). Internally:

1. Resolve `path` against `SAVVAGENT_TOOL_LSP_ROOT` (mirrors the
   pattern from `tool-fs` — `ToolRegistry::connect` already sets that
   env on every tool spawn; we add `SAVVAGENT_TOOL_LSP_ROOT` alongside
   `_FS_ROOT`/`_BASH_ROOT`/`_GREP_ROOT`).
2. Map the file extension to a `LanguageId` via the loaded config.
   Unknown extension → tool returns
   `{"error": "no LSP configured for .<ext> files"}` as a tool-error
   outcome. The model can recover; the host doesn't fail.
3. Walk parents from the file's directory; the first directory
   containing **any** of that language's `root_markers` is the
   workspace root. Stop at filesystem root; if no marker is found,
   tool returns
   `{"error": "no workspace root for <path> (looked for: Cargo.toml, …)"}`.
4. Acquire-or-spawn `LspSession` for `(language_id, root)`.
5. Ensure the target file is `didOpen` in that session (pulling
   bytes from disk on first open).
6. Issue the LSP request, translate via `convert.rs`, return.

## Pool & lifecycle

```rust
pub struct LspPool {
    sessions: Mutex<HashMap<(LanguageId, PathBuf), Arc<LspSession>>>,
    config: Arc<LspConfig>,
}

pub struct LspSession {
    child: tokio::process::Child,
    rpc: JsonRpcClient,           // tower-lsp-server's client adapter
    initialized: tokio::sync::Notify,
    open_files: Mutex<HashSet<Url>>,
    diagnostics: Mutex<HashMap<Url, Vec<Diagnostic>>>,
    last_activity: AtomicU64,     // unix seconds; updated on every request
}
```

- **Spawn:** lazy, on first call that needs the session. The spawn
  performs the LSP `initialize` handshake; subsequent calls await the
  `initialized` `Notify` before issuing any request.
- **Reuse:** every cache hit on `(language, root)` returns the same
  `Arc<LspSession>`; calls run concurrently against the same child
  (LSP allows interleaved requests).
- **Eviction:** a background task in `LspPool` ticks every 60s and
  drops sessions where `now - last_activity > 600s` (10 min). On
  drop, we send `shutdown` then `exit` per the LSP spec, then `kill()`
  if the child hasn't exited in 2s.
- **Process exit:** the `main.rs` entrypoint installs a tokio
  shutdown signal handler; on `SIGTERM`/`SIGINT` it loops over all
  sessions, sends `shutdown`/`exit`, and waits up to 5s before
  `kill()`ing stragglers. Matches how `tool-bash` is reaped.

## Tool surface

| Tool | Inputs | Output |
|---|---|---|
| `lsp_definition` | `{path, line, character}` | `[{path, range}]` |
| `lsp_references` | `{path, line, character, include_declaration?}` | `[{path, range, preview}]` |
| `lsp_hover` | `{path, line, character}` | `{contents: markdown, range?}` |
| `lsp_document_symbols` | `{path}` | tree of `{name, kind, range, children}` |
| `lsp_workspace_symbols` | `{query, root?}` | `[{name, kind, path, range}]` |
| `lsp_rename` | `{path, line, character, new_name}` | `[{path, edits: [{range, new_text}]}]` |
| `lsp_code_actions` | `{path, range, only?}` | `[{title, kind, edit?}]` |

`range` is always `{start: {line, character}, end: {line, character}}`,
0-indexed (matches LSP). `path` in outputs is always relative to
`SAVVAGENT_TOOL_LSP_ROOT`. `preview` on references is a one-line
excerpt of the source — we pull it from `tool-fs`'s view of the file
or read it directly from disk on the tool-lsp side.

### WorkspaceEdit restrictions

`lsp_rename` and `lsp_code_actions` can receive `WorkspaceEdit`s that
include rename-file, create-file, delete-file, and version-tagged
text edits. v1 returns an error outcome for any of those:

```
{"error": "WorkspaceEdit includes file rename/create/delete which is
 not supported in tool-lsp v1; please perform the change manually
 with tool-fs"}
```

The model falls back to manual edits. Lifting this restriction is a
v2 task and requires careful interaction with the permission system.

## Diagnostics as MCP resources

Each `LspSession` runs a notification handler for
`textDocument/publishDiagnostics`. On receipt:

1. Update `session.diagnostics[uri]` to the new list.
2. Translate `uri` → `lsp://diagnostics/<absolute-path>`.
3. Send `notifications/resources/updated { uri }` over MCP.

The host (per the resource-push spec) injects
`[resource updated: lsp://diagnostics/<path>]` into the next
iteration's history. The model decides whether to read it via
`read_resource`. The `read_resource` dispatch routes back to
`tool-lsp`, which serves `resources/read` for any
`lsp://diagnostics/*` URI by serializing
`session.diagnostics[uri]` as:

```json
[
  {
    "range": {"start": {...}, "end": {...}},
    "severity": "error" | "warning" | "info" | "hint",
    "source": "rustc" | "rust-analyzer" | …,
    "code": "E0308",
    "message": "mismatched types\nexpected `u32`, found `i64`"
  }
]
```

Empty diagnostics for a URI mean "no problems" — we still publish
the empty list rather than removing the URI, because the model needs
a way to see "you fixed it."

## `initialize` handshake & progress

LSP `initialize` and the subsequent indexing pass (especially for
rust-analyzer) can take 10-60 seconds on cold projects. During this
window the session is unusable. tool-lsp emits MCP `progress`
notifications during the wait:

- `tool-lsp: starting rust-analyzer for /path/to/repo`
- `tool-lsp: indexing… 1234 files`
- `tool-lsp: ready`

These already flow through the host's existing rmcp progress channel
(`ProgressDispatcher`) and the TUI surfaces them as status lines.
The well-known
[rmcp ProgressDispatcher gotcha](../../memory/...) (subscriber.next()
not auto-closing) applies — tool-lsp's progress forwarder MUST
`JoinHandle::abort()` after the request future resolves.

## Permissions

Every `lsp_*` tool is read-only against the workspace (it spawns
processes and reads files, but does not mutate). The default
permission policy treats them like other read tools — same tier as
`tool-fs::read_file` and `tool-fs::list_dir`.

`SAVVAGENT_TOOL_LSP_ROOT` is enforced inside tool-lsp the same way
`SAVVAGENT_TOOL_FS_ROOT` is enforced inside tool-fs: any `path` that
resolves outside the root via `..` traversal is rejected with a
tool-error outcome.

Sandbox: when the host sandbox is on (`bwrap` / `sandbox-exec`),
tool-lsp inherits the wrapper. The child LSP servers it spawns inherit
the sandbox transitively — `rust-analyzer` and friends run with the
same fs/network rules as the tool itself. We do NOT punch holes in
the sandbox per LSP server; if a language server needs network
(e.g. for fetching crates.io index data) the user must enable
network access at the tool-lsp level via the existing
`tool_overrides` map.

## Testing

- **Unit (config.rs).** Parse a representative `lsp.toml`; verify
  merge precedence for global vs per-repo entries.
- **Unit (language.rs).** Extension → language lookup; root-marker
  walk with synthetic temp directories (`tempfile`).
- **Unit (convert.rs).** Round-trip LSP `Location` ↔ our JSON; rename
  WorkspaceEdit rejection cases.
- **Integration (`tests/fixtures/fake-lsp/`).** A small JSON-RPC
  server that returns canned responses for `initialize`,
  `textDocument/definition`, `textDocument/references`, and emits one
  `publishDiagnostics`. The integration test boots tool-lsp pointed at
  `fake-lsp`, drives each MCP tool, asserts the translated output, and
  verifies the resource notification fires.
- **No real LSP servers in CI.** Too heavy, too flaky. Devs run
  smoke tests locally against `rust-analyzer` before tagging a
  release.

## Workspace wiring

`tool-lsp` is added to the workspace's default tool set in
`crates/savvagent/src/main.rs`:

```rust
config
    .with_tool(ToolEndpoint::Stdio {
        command: tool_lsp_path(),
        args: vec![],
    })
    .with_tool(/* tool-fs */)
    .with_tool(/* tool-grep */)
    .with_tool(/* tool-bash */);
```

`tool_lsp_path()` mirrors `tool_fs_path()`: `$PATH`-resolve
`savvagent-tool-lsp`, then fall back to
`SAVVAGENT_TOOL_LSP_BIN`, then to the workspace `target/` location.

## PR slicing

(Each PR builds on the host-resource PRs and a green CI of the prior
tool-lsp PR.)

1. **PR 1 — crate scaffold.** New `crates/tool-lsp` with `Cargo.toml`,
   `main.rs` stdio MCP server skeleton that advertises zero tools and
   exits cleanly. Wired into workspace `Cargo.toml` and the default
   tool list. CI green.
2. **PR 2 — config + language resolution.** `config.rs`, `language.rs`,
   with unit tests. Tool surface still empty; tool-lsp serves a single
   placeholder tool `lsp_languages` that lists configured languages
   for debugging.
3. **PR 3 — pool + session + first tool.** `pool.rs`, `session.rs`,
   `convert.rs`, plus `lsp_definition`. Real LSP `initialize`
   handshake. Integration test against `fake-lsp`. The placeholder
   `lsp_languages` from PR 2 is removed.
4. **PR 4 — read tools.** `lsp_references`, `lsp_hover`,
   `lsp_document_symbols`, `lsp_workspace_symbols`. Each ships with a
   focused integration test in `fake-lsp`.
5. **PR 5 — edit-descriptor tools.** `lsp_rename`, `lsp_code_actions`
   with WorkspaceEdit restrictions enforced.
6. **PR 6 — diagnostics resource.** `resources/diagnostics.rs`.
   Publishes `lsp://diagnostics/<path>` on every `publishDiagnostics`.
   Depends on host PRs 1-4 being live.
7. **PR 7 — release.** Workspace bump to the next minor after the
   host release. README gains an "LSP" section with copy-pasteable
   `lsp.toml` examples. CHANGELOG entry.

## What's explicitly out of scope

- **Auto-apply of WorkspaceEdits.** Always model-mediated via tool-fs.
- **`textDocument/completion`.** The model already generates code; we
  don't need LSP autocomplete to feed it.
- **`textDocument/signatureHelp`.** Same reason.
- **`textDocument/formatting`.** Use `cargo fmt` / `prettier` via
  `tool-bash`.
- **`callHierarchy` and `typeHierarchy`.** Useful but niche; defer
  until a concrete model task needs them.
- **`semanticTokens`.** Visualization-oriented; not needed for an
  agent.
- **DAP (Debug Adapter Protocol).** Different protocol, different
  scope. A separate `tool-dap` crate would be a future addition.
- **A built-in default config that auto-detects servers on `$PATH`.**
  Defer to v2; v1 ships with documented examples only.

## Migration / compatibility

Adding `tool-lsp` to the default tool list adds eight new tools
(`lsp_definition` … `lsp_code_actions`) to every host that runs the
TUI's `main.rs`. Embedders that build their own `HostConfig` and don't
add the endpoint see no change. No SPP wire change. No CLI flag
change.
