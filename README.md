# Savvagent

A fast, MCP-first terminal coding agent written end-to-end in Rust.

The product vision and rationale live in [`PRD.md`](PRD.md). This README is
for developers working on the repo: how to build it, how it's laid out, and
how to extend it. End users wanting to install Savvagent can skip to
[Install](#install) below.

## Install

Precompiled binaries for Linux (x86_64 / aarch64), macOS (Apple Silicon),
and Windows (x86_64) are published to GitHub Releases on every tag. Each
release ships one archive per platform containing seven binaries — the
`savvagent` TUI plus the three bundled tool servers (`savvagent-tool-fs`,
`savvagent-tool-bash`, `savvagent-tool-grep`) and three standalone
provider MCP servers (`savvagent-anthropic`, `savvagent-gemini`,
`savvagent-openai`) — installed to your Cargo bin directory. Local
(Ollama) is linked into the TUI and has no standalone shim.

**Linux / macOS** (one-liner):

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/robhicks/savvagent-rs/releases/latest/download/savvagent-installer.sh | sh
```

**Windows** (PowerShell):

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/robhicks/savvagent-rs/releases/latest/download/savvagent-installer.ps1 | iex"
```

**Manual:** download the matching `savvagent-<target>.tar.gz` (or `.zip`
on Windows) from the [Releases page](https://github.com/robhicks/savvagent-rs/releases),
unpack it, and put the binaries on your `$PATH`. Each archive ships with a
`.sha256` next to it for verification.

After installing, run `savvagent` in your project, `/connect` once to store
an API key in the OS keyring, and you're done. The TUI checks for updates
on launch and installs them automatically in the background — all binaries
in the release archive are replaced in place, not just the main `savvagent`
executable. The banner above the prompt reports progress; restart savvagent
to use the new version. (`/update` becomes a retry/force-now command;
v0.11.0 through v0.12.1 only swapped the main binary and required a manual
installer re-run, so users on those versions must re-run the install script
once to land on v0.13.0 — auto-install takes over from there.)

## Repository layout

The workspace is a small set of focused crates:

| Crate | Purpose |
|---|---|
| [`crates/savvagent`](crates/savvagent) | All seven shipping binaries (`savvagent` TUI plus the `savvagent-tool-{fs,bash,grep}` tool shims and the `savvagent-{anthropic,gemini,openai}` provider shims). Owns `/connect`, file picker, transcript persistence, the plugin runtime. |
| [`crates/savvagent-host`](crates/savvagent-host) | Agent engine consumed as a library. Drives the tool-use loop, manages provider/tool sessions, owns the OS-level sandbox and per-tool stderr capture, exposes `Host::run_turn` and `run_turn_streaming`. |
| [`crates/savvagent-protocol`](crates/savvagent-protocol) | Pure-types crate: `CompleteRequest`, `CompleteResponse`, `StreamEvent`, content blocks, `ListModelsResponse`. SPP wire spec in [`SPEC.md`](crates/savvagent-protocol/SPEC.md). |
| [`crates/savvagent-mcp`](crates/savvagent-mcp) | The `ProviderClient` / `ProviderHandler` traits and the `InProcessProviderClient` bridge that makes provider crates linkable as libraries. |
| [`crates/savvagent-plugin`](crates/savvagent-plugin) | The `Plugin` / `Screen` traits and the `Effect` vocabulary every TUI feature (slash commands, modals, themes, language, providers) is expressed in. WIT-portable surface — see PRD §plugins. |
| [`crates/provider-anthropic`](crates/provider-anthropic) | Anthropic Messages API as a `ProviderHandler` library plus `provider_anthropic::run` (the entry point the `savvagent-anthropic` shim calls). |
| [`crates/provider-gemini`](crates/provider-gemini) | Google Gemini, same shape. Includes a JSON-Schema → OpenAPI-subset sanitizer for tool params. |
| [`crates/provider-openai`](crates/provider-openai) | OpenAI Chat Completions, same shape. |
| [`crates/provider-local`](crates/provider-local) | Ollama (local) over its native HTTP API. Keyless; linked into the TUI only — no standalone shim. |
| [`crates/tool-fs`](crates/tool-fs) | `read_file` / `write_file` / `list_dir` / `glob` / `insert` / `replace` / `multi_edit` library plus `tool_fs::run` (the entry point the `savvagent-tool-fs` shim calls). |
| [`crates/tool-bash`](crates/tool-bash) | Sandboxed `bash` execution. The tool with the trickiest spawn lifecycle — see `savvagent-host::tools` for the lazy-spawn + `allow_net` resolver. |
| [`crates/tool-grep`](crates/tool-grep) | `ripgrep`-style search. |

Every provider and every tool is "just" an MCP-shaped library that *can* be
wrapped in a binary. The TUI links providers in-process by default and
spawns the three tool servers (`tool-fs`, `tool-bash`, `tool-grep`) as
stdio children, optionally wrapped in an OS sandbox (`bwrap` on Linux,
`sandbox-exec` on macOS).

## Prerequisites

- Rust 1.85+ (workspace pins `rust-version = "1.85"`).
- Linux: a running freedesktop Secret Service for the keyring (GNOME Keyring,
  KeePassXC, or KWallet — any of them works). The crate falls back to a
  no-op when none is present, but `/connect` will fail to persist keys.
- macOS / Windows: nothing extra; the keyring uses the platform store.

## Quick start

```bash
# Build everything once. Important: the TUI doesn't depend on the tool-fs
# crate at compile time, but it spawns `savvagent-tool-fs` at runtime — a
# workspace build is the easy way to make that binary exist.
cargo build

# Run the TUI. With nothing configured, it boots disconnected.
cargo run -p savvagent
```

If the bundled tool servers (`savvagent-tool-fs`, `savvagent-tool-bash`,
`savvagent-tool-grep`) aren't on `$PATH` and aren't sitting next to the
TUI binary, the TUI still boots — affected tools are just disabled.
Re-run `cargo build` or set `SAVVAGENT_TOOL_{FS,BASH,GREP}_BIN` to point
at a specific path.

Inside the TUI:

1. Press <kbd>Ctrl-P</kbd> *or* type `/` on an empty prompt to open the
   command palette. Keep typing to filter (e.g. `/co` narrows to
   `/connect`); <kbd>↑</kbd>/<kbd>↓</kbd> move, <kbd>Enter</kbd> selects,
   <kbd>Esc</kbd> cancels. You can also just type the full command
   (`/connect`) and press <kbd>Enter</kbd>.
2. Pick a provider with <kbd>↑</kbd>/<kbd>↓</kbd>, hit <kbd>Enter</kbd>.
3. Paste your API key (input is masked) and <kbd>Enter</kbd>.

The key is stashed in the OS keyring under service `savvagent`, account
`<provider id>`. On the next launch the TUI auto-connects to whichever
provider has a key on file.

### Other slash commands

| Command | What it does |
|---|---|
| `/connect [<provider>] [--rekey]` | Add a provider to the connection pool. Silent when the keyring already has a stored key — the API-key modal only opens when a key is missing or `--rekey` is passed. Multiple providers can be connected simultaneously; switch with `/use <provider>`. |
| `/disconnect <provider> [--force]` | Remove a provider from the pool. Default (drain) mode waits for any in-flight turn to finish. `--force` signals a cooperative cancel, waits 500 ms, then aborts. |
| `/use <provider>` | Switch the active provider. As of v0.17.0 the conversation history is preserved across the switch — `tool_use_id`s are provider-namespaced so subsequent turns on the new provider can safely see prior tool calls. For one-off routing without changing the active provider, use the `@<provider>` prefix. |
| `/model` | Open the model picker (no args), or switch directly: `/model gemini-2.5-pro`. As of v0.17.0 the picker lists every connected provider's models; selecting a model from a different provider switches the active provider too. Selection persists per provider to `~/.savvagent/models.toml`. |
| `/theme` | Open the theme picker (no args), or switch directly: `/theme tokyo-night`. Persists to `~/.savvagent/theme.toml`. |
| `/language` | Open the locale picker. Persists to `~/.savvagent/language.toml`. Ships with en / es / pt / hi; falls back to en for missing keys. |
| `/plugins` | Open the plugin manager — toggle optional plugins on/off; core plugins can't be disabled. Persists to `~/.savvagent/plugins.toml`. |
| `/update` | Re-run the latest-release install. The TUI checks for new releases on launch AND re-checks every 2 hours while the TUI is open, auto-installing any newer release (the banner above the prompt reports progress). `/update` is only needed to retry after a failed install or to force the install before the next 2-hour tick. Replaces every binary in the release archive — `savvagent` plus the six helpers. Opt out with `SAVVAGENT_NO_UPDATE_CHECK=1` or `--no-update-check`. |
| `/save` | Write the current transcript to `~/.savvagent/transcripts/<unix>.json`. |
| `/resume` | Re-open a previously-saved transcript and continue from where it ended. With no args opens a picker; takes an absolute path or a bare basename relative to `~/.savvagent/transcripts/`. |
| `/clear` | Reset the conversation history (and the visible log). |
| `/view <path>` | Open a file in the read-only popup editor. `@<path>` in the prompt also works as an inline shortcut. |
| `/edit <path>` | Open a file for editing (Ctrl-S saves, Esc closes). Syntax highlighting follows the active theme. |
| `/tools` | List the tools registered with the current host, with their permission verdict. |
| `/bash <cmd>` | Run a shell command through `tool-bash`. `--net` / `--no-net` toggle network access for that single call. |
| `/sandbox` | Show or change OS-level sandbox settings; `/sandbox on` / `/sandbox off` persist to `~/.savvagent/sandbox.toml`. |
| `/prompt-keybindings` | Modal listing the keybindings active in the main prompt input. |
| `/editor-keybindings` | Modal listing the keybindings active inside `view-file` / `edit-file`. |
| `/quit` | Exit. |

`@` opens a file picker that inserts `@path` into the prompt.

### Routing turns to a specific provider/model

Prefix any message with `@<provider>:<model>` (or `@<provider>`, or
`@<alias>`) to route that single turn to a specific destination
regardless of the active provider:

- `@anthropic:claude-opus-4-7 design this` — explicit provider + model
- `@gemini explain this` — bare provider, picks Gemini's default model
- `@opus refactor this` — alias, resolves to Anthropic's claude-opus-4-7
- `@@team look here` — literal `@team` (strips one `@`)

Unknown `@`-tokens are not consumed: the message goes through verbatim
and the next turn routes to whichever provider `/use` last selected.
Each assistant turn shows a muted `▸ provider/model — Reason` line above
its response so the routing decision is always visible.

### Multi-provider pool (Phase 1)

As of v0.15.0 Savvagent maintains a *connection pool* — you can `/connect`
multiple providers and they all hold active keyring sessions. The currently
active provider drives the turn loop; everything else sits connected but idle.

**Phase 1 invariant:** a conversation thread runs on one active provider
end-to-end. Switching providers with `/use <provider>` starts a fresh
conversation (history is cleared). Cross-provider routing within a single
conversation — auto-routing by cost/capability, `@provider:model` mid-turn
overrides — is deferred to Phase 3+ and depends on the cross-vendor
compatibility gate passing in Phase 2.

**Startup policy** is configured in `~/.savvagent/config.toml`:

```toml
[startup]
# Which providers to connect automatically when the TUI starts.
# "opt-in"   — only the providers listed in startup_providers (default)
# "all"      — every provider that has a key in the keyring
# "last-used"— reconnect to whichever provider was active at last exit
# "none"     — boot disconnected; use /connect manually
policy = "opt-in"
startup_providers = ["anthropic"]
connect_timeout_ms = 3000

[migration]
# Set to true after the first-launch migration picker has run.
v1_done = true
```

First-time users with multiple keys already in the keyring see a one-time
picker on launch that initializes `startup_providers`. Single-key users see
no UI change.

### Default behavior

Savvagent attaches a dynamic system prompt at the start of every
session, even when no `SAVVAGENT.md` is present and no override is
configured. The prompt covers:

- **Identity** — who Savvagent is and how it runs.
- **Behavior expectations** — use available tools proactively; don't
  claim a limitation without checking the tools first.
- **Tool affordances** — the *names* of the tools wired for this
  session (descriptions reach the model through the typed `tools`
  field, not via the system prompt). When the shell tool is wired,
  an explicit paragraph reminds the model that `gh`, `curl`, `git`,
  `rg`, package managers, and any installed CLI are reachable.
- **Environment** — OS, project root, git presence, Savvagent version.
- **Conventions** — `path/to/file.rs:42` link format; brief edit
  summaries.

To extend the prompt for your project, add a `SAVVAGENT.md` to the
project root. Its body is appended after the default and after any
embedder-supplied override, so your project guidance wins on
ambiguous points.

To suppress the default layer (embedders only):

```rust
let config = HostConfig::new(provider, model)
    .with_default_prompt_disabled();
```

This affects only the built-in default layer — the embedder
`system_prompt` override and `SAVVAGENT.md` body still compose if
present.

## Development workflow

```bash
# Continuous type-checking on save.
bacon                # default job is `cargo check`
bacon clippy-all     # clippy across the workspace

# Tests.
cargo test --workspace

# Specific crate.
cargo test -p savvagent-host

# The headless host smoke-test (needs a running provider — see below).
cargo run -p savvagent-host --example headless -- "list my Cargo.toml"
```

`bacon.toml` defines several jobs (`check`, `check-all`, `clippy`,
`clippy-all`, `test`, `doc`, `run`); pick whichever matches what you're
iterating on.

### Cross-vendor compatibility gate

`crates/savvagent-host/tests/cross_vendor_history.rs` exercises every
sender/receiver pair across the shipping providers to ensure foreign
`tool_use_id`s round-trip through each vendor's translator. PR CI runs
the offline (mocked) matrix as a dedicated `cross-vendor-gate` job. Live
variants are `#[ignore]`-marked; run them manually with the appropriate
`*_API_KEY` env vars:

```bash
ANTHROPIC_API_KEY=sk-… GEMINI_API_KEY=AIza… OPENAI_API_KEY=sk-… \
    cargo test -p savvagent-host --test cross_vendor_history -- --ignored
```

### Running the TUI in watch mode

There is no built-in watch mode for the TUI itself — bacon's `run` job
captures stdout, which doesn't play nicely with an interactive terminal
UI. For an actual restart-on-change loop, use `cargo-watch` in its own
terminal so the TUI gets a real TTY:

```bash
cargo install cargo-watch   # one-time
cargo watch -c -x 'run -p savvagent'
```

`tool-fs` is spawned at runtime, so make sure a workspace `cargo build`
has produced `savvagent-tool-fs`. If you want both steps explicit:

```bash
cargo watch -c -x build -x 'run -p savvagent'
```

For pure type-checking / clippy / test feedback while you edit, keep
`bacon` running in a separate pane.

### Running providers as standalone MCP servers

The default in-process path is the easy one. Sometimes you want the binary
form — e.g., when iterating on the wire format or running the `headless`
example. The standalone provider servers ship as bins on the `savvagent`
crate; each takes its API key via env and listens on loopback:

```bash
# Anthropic — defaults to 127.0.0.1:8787
ANTHROPIC_API_KEY=sk-ant-… cargo run -p savvagent --bin savvagent-anthropic

# Gemini — defaults to 127.0.0.1:8788
GEMINI_API_KEY=…           cargo run -p savvagent --bin savvagent-gemini

# OpenAI — defaults to 127.0.0.1:8789
OPENAI_API_KEY=…           cargo run -p savvagent --bin savvagent-openai
```

Ollama (local) only runs as an in-process provider; there's no
`savvagent-local` shim because the upstream `ollama serve` already speaks
HTTP on `OLLAMA_HOST` (default `127.0.0.1:11434`).

Then point the TUI (or `savvagent-host` example) at it:

```bash
SAVVAGENT_PROVIDER_URL=http://127.0.0.1:8787/mcp cargo run -p savvagent
```

When `SAVVAGENT_PROVIDER_URL` is set the TUI uses the MCP client path
instead of the in-process bridge — useful for debugging the wire protocol
or pointing at a third-party MCP provider.

## Architecture in five sentences

`Host` (in `savvagent-host`) owns a `ToolRegistry` and a
*connected-provider pool* — a `HashMap<ProviderId, PoolEntry>` with one
entry marked active. Each `PoolEntry` wraps an `Arc<dyn ProviderClient>`
that is either an in-process bridge over a `ProviderHandler` (default) or an
`rmcp` Streamable HTTP client connected to a remote provider binary (opt-in).
Each user turn runs through `Host::run_turn_streaming`, which pulls the
active entry from the pool and loops `provider.complete` →
`tool_registry.call` until the model emits `end_turn`, forwarding stream
events to the TUI as it goes. The TUI keeps the host in
`Arc<RwLock<Option<Arc<Host>>>>` so per-turn tasks can snapshot it without
holding a lock across awaits, and `/connect` adds to the pool atomically.
Tool servers are stdio children, owned by the registry and reaped on
shutdown.

## Adding a new provider

1. Create `crates/provider-foo/` with the standard layout (mirror
   `provider-gemini`).
2. Implement `savvagent_mcp::ProviderHandler` for your `FooProvider` —
   translate to/from the upstream API, deal with streaming via
   `StreamEmitter`. The Anthropic and Gemini crates are the reference.
3. Expose a `FooProvider::builder().api_key(...).build()` constructor.
4. Append one entry to [`crates/savvagent/src/providers.rs::PROVIDERS`]:
   ```rust
   ProviderSpec {
       id: "foo",
       display_name: "Foo Models",
       api_key_env: "FOO_API_KEY",
       default_model: "foo-latest",
       api_key_required: true,
       build: build_foo,
       health_check: None,
   }
   ```
   …and a `build_foo` factory next to the existing four. Set
   `api_key_required: false` for keyless providers (the Local/Ollama
   entry is the reference); set `health_check: Some(check_foo)` if
   the provider has a reachability probe (also see Local/Ollama).
5. Wire `provider-foo` into `Cargo.toml` (workspace deps + savvagent crate
   deps).
6. Optional: ship a standalone `savvagent-foo` MCP server. Add a
   `pub async fn run()` to `provider_foo`'s lib (mirror `provider_anthropic::run`),
   then add a `[[bin]]` entry in `crates/savvagent/Cargo.toml` pointing at a
   3-line shim in `crates/savvagent/src/bin/savvagent-foo.rs` that just calls
   `provider_foo::run().await`. The release archive picks it up automatically.

That's the whole touch surface. There's no provider registry to update in
the host, no tool dispatch table — the host doesn't know about providers
beyond the `ProviderClient` trait.

## Adding a new tool

Tools are stdio MCP servers. Mirror `crates/tool-fs`:

1. Implement your tool methods using `rmcp`'s server primitives.
2. Build a binary that calls `serve_server` on stdin/stdout.
3. The host config takes a `ToolEndpoint::Stdio { command, args }` —
   `savvagent` currently bakes one in (`SAVVAGENT_TOOL_FS_BIN`); for
   additional tools you can extend `HostConfig::with_tool` calls in
   `crates/savvagent/src/main.rs`.

## Environment variables

| Var | Where read | Default | Notes |
|---|---|---|---|
| `SAVVAGENT_PROVIDER_URL` | `savvagent` | (unset) | When set, skips in-process bridge; uses MCP HTTP. |
| `SAVVAGENT_MODEL` | `savvagent` | per-provider | Overrides both `~/.savvagent/models.toml` and `ProviderSpec::default_model`. Accepts `provider/model` form (`anthropic/claude-opus-4-7`) or bare model name (`claude-opus-4-7`; ambiguous bare names log a warning and fall back to the active provider). Precedence on connect: env > persisted > default. |
| `SAVVAGENT_TOOL_FS_BIN` | `savvagent` | `savvagent-tool-fs` (PATH) | Path to the fs tool binary. |
| `SAVVAGENT_TOOL_BASH_BIN` | `savvagent` | `savvagent-tool-bash` (PATH) | Path to the bash tool binary. |
| `SAVVAGENT_TOOL_GREP_BIN` | `savvagent` | `savvagent-tool-grep` (PATH) | Path to the grep tool binary. |
| `SAVVAGENT_NO_UPDATE_CHECK` | `savvagent` | (unset) | When set, disables the launch-time and periodic (2-hour) version check, and the `/update` slash command. CLI equivalent: `--no-update-check`. |
| `ANTHROPIC_API_KEY` | `savvagent-anthropic` | — | Read at server start. In-process flow gets it from `/connect`. |
| `ANTHROPIC_BASE_URL` | `savvagent-anthropic` | `https://api.anthropic.com` | For local mocks. |
| `SAVVAGENT_ANTHROPIC_LISTEN` | `savvagent-anthropic` | `127.0.0.1:8787` | Bind address. |
| `GEMINI_API_KEY` / `GOOGLE_API_KEY` | `savvagent-gemini` | — | Same idea. |
| `GEMINI_BASE_URL` | `savvagent-gemini` | `https://generativelanguage.googleapis.com` | |
| `SAVVAGENT_GEMINI_LISTEN` | `savvagent-gemini` | `127.0.0.1:8788` | |
| `OPENAI_API_KEY` | `savvagent-openai` | — | Same idea. |
| `OPENAI_BASE_URL` | `savvagent-openai` | `https://api.openai.com` | For local mocks. |
| `SAVVAGENT_OPENAI_LISTEN` | `savvagent-openai` | `127.0.0.1:8789` | Bind address. |
| `OLLAMA_HOST` | `savvagent` (Local provider) | `http://127.0.0.1:11434` | URL of the local Ollama HTTP server. |
| `RUST_LOG` | all binaries | `warn` (TUI), `warn` (tool servers) | Standard `tracing-subscriber` env filter. Tool servers' stderr is captured to `~/.savvagent/logs/tools/`, the TUI's tracing lands in `~/.savvagent/logs/savvagent.log`. |

`.env` and `.env.local` at the repo root are auto-loaded on startup.

## Persistence on disk

| Path | Owner | Contents |
|---|---|---|
| `~/.savvagent/transcripts/<unix_secs>.json` | TUI | One pretty-printed `Vec<spp::Message>` per save (auto on `TurnComplete`, manual on `/save`). |
| `~/.savvagent/config.toml` | TUI startup | Startup connection policy (`opt-in` / `all` / `last-used` / `none`), `startup_providers` list, per-provider `connect_timeout_ms`, and one-time migration flag. Created automatically on first launch when multiple keyring entries are found. |
| `~/.savvagent/models.toml` | `/model` | `{ providers: { id = model } }`. Re-applied at `/connect`. |
| `~/.savvagent/theme.toml` | `/theme` | Selected theme slug. |
| `~/.savvagent/language.toml` | `/language` | Selected locale code. |
| `~/.savvagent/plugins.toml` | `/plugins` | Optional plugin enabled-set. Core plugins ignore this file. |
| `~/.savvagent/sandbox.toml` | `/sandbox` | Sandbox mode + per-tool `allow_net` overrides. |
| `~/.savvagent/permissions.toml` | host | Per-tool / per-pattern permission verdicts. |
| `~/.savvagent/update-check.json` | `internal:self-update` | 24-hour cache of the GitHub Releases version probe. |
| `~/.savvagent/logs/savvagent.log` | TUI | All `tracing` output from the TUI process. |
| `~/.savvagent/logs/tools/<binary>.log` | host | Captured stderr from each spawned tool subprocess. |
| OS keyring (`service=savvagent`, `account=<provider id>`) | `/connect` | Provider API keys. Never written to disk in plaintext. |

## Project context: `SAVVAGENT.md`

If a `SAVVAGENT.md` file exists at the project root the host reads it as
the system prompt. See `crates/savvagent-host/src/project.rs` and the
"Project context" section of the PRD.

## Reference docs

- [`PRD.md`](PRD.md) — vision, scope, milestones.
- [`crates/savvagent-protocol/SPEC.md`](crates/savvagent-protocol/SPEC.md) —
  Savvagent Provider Protocol (SPP) wire format.
- [`docs/`](docs) — architecture diagrams and design notes.

## License

Licensed under the GNU Affero General Public License v3.0 or later
(`AGPL-3.0-or-later`). See [`LICENSE`](LICENSE) for the full text.
