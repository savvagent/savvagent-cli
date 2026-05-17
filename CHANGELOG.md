# Changelog

All notable changes to savvagent are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(pre-1.0: `0.MINOR.PATCH`, where MINOR captures features + breaking
boundary changes and PATCH captures fixes).

## 0.17.0 - 2026-05-17

### Added

- **`@provider:model` (and `@provider`, `@alias`) prefix.** Users can now
  route an individual turn to a specific provider/model by prefixing
  their message with `@anthropic:claude-opus-4-7`, `@gemini`, `@opus`,
  etc. Unknown `@`-tokens are NOT consumed — the message goes through
  verbatim and the next turn routes to the active provider as usual. To
  start a message with a literal `@`, prefix with `@@`.
- **Per-turn routing badge.** Each assistant turn now renders a muted
  `▸ provider/model — Reason` line above its response so it's always
  obvious which provider handled the turn and why
  (Override / Default; modality / rules / heuristics arrive in later
  phases).
- **Built-in model aliases.** `@opus`, `@sonnet`, `@haiku` map to
  Anthropic; `@flash`, `@pro` map to Gemini; `@gpt`, `@gpt-4o` map to
  OpenAI. Ambiguous aliases (same short name across providers) fall
  through with a styled note rather than picking one silently.

### Changed

- **Cross-provider history is now safe.** `/use <provider>` no longer
  clears the conversation when switching the active provider. The host
  namespaces every `tool_use_id` with the issuing provider at insertion
  time (`<provider_id>:<original_id>`) and strips the receiver's own
  prefix back off before each request; foreign-prefixed ids flow through
  every translator as opaque strings, validated by the Phase 2
  cross-vendor gate (v0.16.0).
- **`/model` picker shows every connected provider's models.** Selecting
  a model from a different provider updates both the active provider and
  the default model in one step.

### Internal

- Phase 3 of the multi-provider-pool roadmap (see
  `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`).
  New `crates/savvagent-host/src/router/{prefix,router,namespace}.rs`
  modules; `Host::run_turn_inner` now parses the `@`-prefix, invokes
  `Router::pick`, emits `TurnEvent::RouteSelected`, and namespaces ids
  on append / strips on egress.
- `TurnEvent::RouteSelected { provider_id, model_id, reason }` added.
  Existing `TurnEvent` consumers that match the enum exhaustively need a
  new arm (the TUI's `apply_turn_event` handles it; downstream consumers
  outside this repo may need to update).
- `PoolEntry` gains an `aliases` field carrying every `ModelAlias` the
  provider's `ProviderRegistration` declared; `PoolEntry::new` takes a
  new `aliases: Vec<ModelAlias>` argument.

## 0.16.0 - 2026-05-16

### CI

- **Cross-vendor `tool_use_id` compatibility gate.** New
  `crates/savvagent-host/tests/cross_vendor_history.rs` integration test
  validates that every `(sender_provider, receiver_provider)` pair across
  the three shipping vendors (Anthropic, Gemini, OpenAI) accepts SPP
  history whose `tool_use_id` is prefixed with the originating provider
  (e.g. `"anthropic:toolu_xyz"`). Nine pair tests run in PR CI against
  axum-backed mock vendor servers via the dedicated `cross-vendor-gate`
  job with `--no-fail-fast`, so any regression surfaces every failing
  pair. `#[ignore]`-marked live-vendor twins are runnable manually via
  `cargo test -p savvagent-host --test cross_vendor_history -- --ignored`
  with `ANTHROPIC_API_KEY` / `GEMINI_API_KEY` / `OPENAI_API_KEY` set.

### Internal

- Phase 2 of the multi-provider-pool roadmap (see
  `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`).
  No user-visible runtime behavior changes; this release establishes the
  release-gate Phase 3 (cross-provider routing with `@provider:model`
  overrides) depends on. A live-vendor nightly workflow is intentionally
  deferred to a follow-up.

## v0.15.0 — Multi-provider connection pool (2026-05-15)

### Added

- **Multi-provider connection pool.** `/connect <provider>` is now silent when
  the keyring already has a stored key; the API-key modal only opens when a key
  is missing or `--rekey` is passed. Multiple providers can be connected
  simultaneously; the pool is an `Arc`-held `HashMap<ProviderId, PoolEntry>` in
  the host.
- **`/disconnect <provider> [--force]`** removes a provider from the pool. Drain
  mode (default) waits for any in-flight turn to finish; Force mode signals
  cooperative cancel, waits 500 ms, then aborts. `TurnEvent::Cancelled { reason }`
  and `TurnEvent::AbortedAfterGrace { reason }` (with
  `CancellationReason::ProviderDisconnected` and `UserAbort`) emit during
  force-disconnect.
- **`/use <provider>`** switches the active provider and clears the conversation
  thread. Phase 1 invariant: a conversation runs on one active provider
  end-to-end; cross-provider routing within a conversation lands in Phase 3+
  once the cross-vendor compatibility gate (Phase 2) is green.
- **`~/.savvagent/config.toml`** for startup connection policy and per-provider
  connect timeout. Schema:

  ```toml
  [startup]
  # "opt-in" (default), "all", "last-used", "none"
  policy = "opt-in"
  startup_providers = ["anthropic"]
  connect_timeout_ms = 3000

  [migration]
  v1_done = true
  ```

- **First-launch migration picker.** Users upgrading with multiple stored keys
  see a one-time picker that selects which providers to connect on startup; the
  selection is persisted to `~/.savvagent/config.toml` under `startup_providers`.
  Single-key users see no UI change.
- **`Alt-Enter` on the `/connect` picker** re-enters the API key for the focused
  provider (equivalent to typing `/connect <id> --rekey`).
- **Status bar active-provider marker.** The active provider in the footer is
  prefixed with `▸ `; pool members that are connected but inactive are listed
  without the marker.

### Changed

- **`/connect <provider>`** no longer replaces the active host or clears the
  conversation. The new provider joins the pool additively; use `/use <provider>`
  to switch the active turn context.
- **`/model`** lists only the active provider's models. Switching to a different
  provider's model requires `/use <provider>` first.
- **`SAVVAGENT_MODEL`** accepts both legacy bare-model form
  (`claude-opus-4-7`) and new `provider/model` form
  (`anthropic/claude-opus-4-7`); ambiguous bare forms log a warning and
  fall back to the active provider's default.

### Migration notes

- Pre-existing users with multiple stored keys see a one-time picker on first
  launch; the selection writes `startup_providers` to
  `~/.savvagent/config.toml`. Single-key users see no UI change beyond the
  silent re-connect behavior.
- Users who relied on `/connect <other>` to switch providers should now use
  `/use <other>` (which also clears conversation history).

### Internal

- `ProviderId` moved from `savvagent-plugin` to `savvagent-protocol`;
  re-exported from `savvagent-plugin` for backwards compatibility.
- New types in `savvagent-host` and `savvagent-protocol`:
  `ProviderCapabilities`, `ModelCapabilities`, `ModelAlias`, `CostTier`,
  `ProviderRegistration`, `StartupConnectPolicy`, `PoolEntry`, `ProviderLease`,
  `DisconnectMode`, `PoolError`, `LegacyModelResolution`.
- `Host` gains: `add_provider`, `remove_provider`, `set_active_provider`,
  `active_capabilities`, `is_connected`.
- `TurnEvent::Cancelled { reason }` and `TurnEvent::AbortedAfterGrace { reason }`
  with `CancellationReason::ProviderDisconnected` and `UserAbort`.
- `HostEvent::ActiveProviderChanged` (and matching `HookKind`) fires on `/use`
  and startup so plugin slot rendering stays in sync.

## v0.14.3 — Self-update plugin re-checks GitHub Releases every 2 hours (2026-05-15)

### Fixed

- **Long-running TUI sessions now notice new releases.** Previously,
  the `internal:self-update` plugin only consulted the GitHub Releases
  API when the `HostStarting` hook fired (i.e., at TUI launch), so a
  session that stayed open for days would never observe a release
  published mid-session. The spawned check task now runs on a
  `tokio::time::interval` with a 2-hour cadence: the first tick
  preserves today's startup behavior (and the 24h on-disk cache at
  `~/.savvagent/update-check.json`), while subsequent ticks bypass the
  cache and re-query GitHub. New releases auto-install in exactly the
  same way as the startup path; the banner shows the progression and
  the existing restart hint fires on exit.

  The 2-hour interval is fixed (no env var override yet — file an
  issue if you need one). `MissedTickBehavior::Delay` is set so a
  suspended laptop or a long install never produces a burst of
  catch-up network calls when the system resumes.

  Skip rules per tick:
  - `Disabled` / `Updated` end the loop (opt-out, or binary already
    swapped and awaiting restart).
  - `Installing` skips the tick — covers the case where the user
    typed `/update` and the dispatcher task is parked on the
    installer.
  - `InstallFailed { latest: T }` re-runs the check on every tick to
    pick up any *newer* release GitHub publishes, but skips the
    install when the live check still resolves to `T`. This avoids
    hammering a known-broken release while still recovering
    automatically once a new release lands.

  Closes #78.

## v0.14.2 — Gemini tool calls no longer abort with "stop_reason=end_turn but tool_use block(s) present" (2026-05-15)

### Fixed

- **Gemini tool calls now actually dispatch.** Any Gemini-routed turn
  that asked the model to invoke a tool (e.g. *"Do we have any issues
  on GitHub?"* on top of a shell tool) was aborting the turn with
  `Error: malformed assistant response: stop_reason=end_turn but 1
  tool_use block(s) present`. Root cause was a two-layer mismatch:
  Gemini's `generateContent` API has no distinct `TOOL_USE` finish
  reason and emits `finishReason="STOP"` even when the candidate carries
  a `functionCall` part, but the host's `Host::run_turn_streaming` loop
  treated `stop_reason=EndTurn` alongside `tool_use` content blocks as a
  hard `HostError::MalformedResponse` and dropped the turn. Three fixes
  ship together:
  - The Gemini translator (`translate.rs::response_from_gemini`) and
    streaming accumulator (`stream.rs`) now force `StopReason::ToolUse`
    whenever the assistant content carries any `ToolUse` block,
    regardless of the upstream `finishReason`. The override is applied
    in every accumulator entry point (`consume_chunk`, `flush`, `finish`)
    via a new `Accumulator::coerce_tool_use_stop_reason` helper so it
    survives Gemini's occasional split-chunk streaming order (function
    call in one chunk, `finishReason="STOP"` in a later chunk). When
    overriding away from a `Refusal`/`Other` finish reason the provider
    now `tracing::warn!`s with the original reason so safety / malformed
    signals aren't silently lost.
  - `Host::run_turn_streaming` now treats the presence of `tool_use`
    blocks as authoritative: when any are present the host runs them
    and continues the tool-use loop regardless of the provider-reported
    `stop_reason`. Defensive against other providers with the same
    `finishReason` conflation.
  - When a turn terminates with a non-`EndTurn` stop reason and no
    tool calls (`MaxTokens`, `Refusal`, `StopSequence`, `Other`), the
    host now `tracing::warn!`s instead of swallowing the anomaly at
    `debug!`. The clearest hazard is a `MaxTokens` cutoff committing a
    truncated assistant turn to session state — at least it's loud
    now. Surfacing this in `TurnOutcome` for the TUI is a separate
    follow-up.

  Closes [#76](https://github.com/robhicks/savvagent-rs/issues/76).

### Deprecated

- `savvagent_host::HostError::MalformedResponse` is no longer
  constructed. The variant is preserved for the public API surface and
  marked `#[deprecated]` (slated for removal in 0.15.0).

## v0.14.1 — self-update cache invalidation on out-of-band upgrade (2026-05-14)

### Fixed

- **`/update` no longer reports "Already on the latest version" when a
  newer release is genuinely available.** The 24-hour update-check cache
  at `~/.savvagent/update-check.json` was being trusted on every launch
  inside its TTL, even when the running binary had moved *past* the
  cached `latest_tag`. A user who launched while on `v0.12.0` (cache
  recorded `latest_tag: v0.12.0`), then upgraded out-of-band to `v0.13.0`
  via `cargo install` / a downloaded tarball / a package manager, would
  start `v0.13.0` and have the plugin classify `0.13.0 > v0.12.0` as
  `Ahead → UpToDate` — silently hiding any newer release (`v0.14.0` in
  this case) that GitHub had since published. The plugin now treats a
  cache whose tag is older than (or unparseable against) the running
  binary as a cache miss and re-fetches, restoring the auto-install +
  banner + `/update` path. Unparseable cached tags are also re-fetched
  rather than swallowed as `CheckFailed`.

## v0.14.0 — default system prompt + tool-description trust boundary (2026-05-14)

### Added

- **Default system prompt.** The host now builds and attaches a dynamic
  system prompt at every session start, introducing Savvagent's
  identity, behavior expectations, the names of wired tools, the
  environment (OS, project root, git presence, version), and output
  conventions. Closes the long-standing failure mode where the model
  would claim "I cannot access GitHub" despite having a shell tool with
  network access available — the prompt now explicitly translates
  "shell wired" into "gh / curl / git / rg / package managers are
  available, use them."
- `HostConfig::with_app_version` lets embedders pass the binary's
  `CARGO_PKG_VERSION` so the prompt's Environment section shows the
  installed version (not the `savvagent-host` crate version). The TUI
  wires this automatically; library embedders can pass any
  `impl Into<String>`.
- `HostConfig::with_default_prompt_disabled` lets embedders that want
  to fully own the system message suppress the built-in default layer.

### Changed

- `project::system_prompt` removed; replaced by the 3-layer
  `project::layered_prompt` (default → embedder override →
  `SAVVAGENT.md` body). All three layers compose with H1 section
  headers; whitespace-only layers are silently dropped; non-empty
  layers render verbatim so code fences and indentation in
  `SAVVAGENT.md` survive.

### Breaking for embedders

- Default-installed embedders now receive a non-empty system prompt
  even when they did not set `HostConfig::system_prompt`. To preserve
  the previous "empty system field" behavior, call
  `HostConfig::with_default_prompt_disabled()` AND leave
  `system_prompt` unset AND point `project_root` at a directory with
  no `SAVVAGENT.md`.

### Security

- Tool descriptions supplied by MCP tool servers are no longer
  promoted into the system prompt. Names are listed under an
  explicitly-framed informational block as code spans; descriptions
  still flow to the model via the request's typed `tools` field.
  Third-party MCP tool servers can no longer inject policy-conflicting
  instructions through their `description` strings.
- Tool names themselves are defensively sanitized at render time:
  ASCII control characters (including `\n`, `\r`, `\t`), Unicode
  LINE SEPARATOR (`U+2028`), and PARAGRAPH SEPARATOR (`U+2029`) are
  replaced with `?`; backticks are replaced with `'` so the wrapping
  code span cannot be escaped. MCP enforces no charset on tool names,
  so this is a defensive measure against future third-party servers.
- The shell-capability paragraph is gated on a trusted
  `ToolRegistry::bash_available()` signal sourced from the embedder's
  `tool-bash`-marker endpoint configuration — not from matching tool
  names like `"run"`, which a third-party tool could spoof.

## v0.13.0 — /changelog viewer + auto-install all binaries (2026-05-14)

Two threads ship together:

1. **`/changelog` opens an in-TUI viewer.** A new built-in plugin
   (`internal:changelog`) fetches
   `https://raw.githubusercontent.com/robhicks/savvagent-rs/master/CHANGELOG.md`
   and renders it through `tui-markdown` on a dedicated screen, so users
   can read release history without leaving the TUI. Keybindings:
   `j`/`k` and `↑`/`↓` for line scroll, `PageUp`/`PageDown`,
   `g`/`G` for page-top/bottom, `r` to retry on fetch failure,
   `Esc`/`q` to close. No auto-open after self-update — it's an
   on-demand command. (`tui-markdown` is pinned to `0.3` with
   `default-features = false` to skip the `highlight-code` stack —
   syntect + ansi-to-tui — that the changelog doesn't need.)
2. **`/update` now swaps every binary in the release archive, and
   does it automatically on launch.** The v0.12.1 "Known limitations"
   are resolved: previously `/update` only replaced the main
   `savvagent` binary, leaving the six helpers
   (`savvagent-anthropic`, `savvagent-gemini`, `savvagent-openai`,
   `savvagent-tool-fs`, `savvagent-tool-bash`, `savvagent-tool-grep`)
   pinned to whatever version was already on disk. v0.13.0 introduces
   a `CargoDistInstaller` that pipes the per-release
   `savvagent-installer.sh` (or `.ps1` on Windows) through the shell,
   replacing every binary in the archive via the same trusted path
   used for fresh installs. Update detection is also no longer
   gated on user action: the TUI runs the install in the background
   on startup and `/update` is now a retry/force-now command rather
   than the primary trigger. Two new `UpdateState` variants —
   `Installing` and `InstallFailed` — track in-flight installs and
   surface failures in the status banner.

### Added

- `internal:changelog` built-in plugin and `/changelog` slash command
  (PR #70, closes #68). Locale strings added in en/es/hi/pt.

### Changed

- `internal:self-update` runs the cargo-dist installer on startup so
  every binary in the release archive is replaced on the next
  restart. `/update` becomes a retry/force-now affordance. (PR #69,
  closes #67.) New `UpdateState::Installing` and
  `UpdateState::InstallFailed` entries cover the new lifecycle.

### Upgrade notes

- Users running v0.11.0, v0.12.0, or v0.12.1 must re-run the install
  script once to land on v0.13.0 — those releases shipped an
  incorrect `bin_path_in_archive` and cannot self-upgrade. From
  v0.13.0 onwards the auto-install path takes over and no manual
  re-runs are needed.

## v0.12.1 — self-update cache hygiene (2026-05-13)

### Fixed

- `internal:self-update` test suite no longer poisons the developer's
  real `~/.savvagent/update-check.json`. Two `#[tokio::test]` cases
  (`host_starting_spawns_check_that_updates_state`,
  `other_events_are_ignored`) previously invoked the plugin's
  production `on_event` path with a stub fetcher returning
  `v99.99.99`; that path resolved the cache file via the live `$HOME`
  and wrote the stub tag to disk, which the installed binary then
  served for the full 24h TTL — causing `/update` to advertise
  `v99.99.99` and 404 on download. The plugin now accepts a test-only
  `cache_path_override`, the affected tests redirect to a tempdir,
  and a positive regression assertion verifies the override is wired
  through to `cache::save`. Field installs were unaffected — only
  contributors who ran `cargo test` saw the symptom. If your local
  `/update` is wedged on `v99.99.99`, delete
  `~/.savvagent/update-check.json` once and relaunch.

## v0.12.1 — `/update` fixes (2026-05-13)

### Fixed

- **`/update` could not find the binary inside the release archive.**
  v0.11.0 and v0.12.0 configured `self_update` with the default
  `bin_path_in_archive` (`{{ bin }}`, archive root), but cargo-dist
  nests every Unix binary under a top-level `savvagent-{target}/`
  directory in the tarball. The apply path therefore failed on
  Linux/macOS with
  `Could not find the required path in the archive: "savvagent"`.
  `bin_path_in_archive` is now set to `savvagent-{{ target }}/{{ bin }}`
  on Unix and `{{ bin }}` on Windows (the Windows zip ships flat). The
  fix takes effect for users running v0.12.1 or later; v0.11.0 and
  v0.12.0 binaries already in the field cannot self-upgrade through
  this bug — re-run the install script
  (`curl -LsSf https://github.com/robhicks/savvagent-rs/releases/latest/download/savvagent-installer.sh | sh`)
  to get to v0.12.1 the first time.
- **Test suite no longer poisons the developer's real
  `~/.savvagent/update-check.json`.** Two `#[tokio::test]` cases in
  `internal:self-update` invoked the production `on_event` path with
  a stub fetcher returning `v99.99.99`. That path resolved the cache
  file via the live `$HOME` and persisted the stub tag to disk, so an
  installed binary launched after `cargo test` would see `v99.99.99`
  as the latest release for the full 24h TTL and fail download with
  a 404 (the tag doesn't exist on GitHub). The plugin now accepts a
  test-only `cache_path_override` and a positive regression assertion
  confirms the override is wired through to `cache::save`. Field
  installs were unaffected — only contributors who ran the suite saw
  the symptom. If your local `/update` is wedged on `v99.99.99`,
  delete `~/.savvagent/update-check.json` once and relaunch.

### Known limitations (not fixed in 0.12.1)

- `/update` only swaps the main `savvagent` binary; the six helper
  binaries (`savvagent-anthropic`, `savvagent-gemini`,
  `savvagent-openai`, `savvagent-tool-fs`, `savvagent-tool-bash`,
  `savvagent-tool-grep`) shipped in the same archive stay at the
  prior version on disk. They still work because SPP and the MCP tool
  protocol are stable across patch boundaries, but a future
  release will broaden the swap to cover the full archive.

## v0.12.0 — Gemini polish, model picker, TUI integrity (2026-05-13)

Five threads ship together:

1. **Gemini connectivity fixed end-to-end.** Tool schemas (JSON Schema
   from `schemars`) are now sanitized into the OpenAPI subset Gemini's
   protobuf parser accepts, and the retired `gemini-1.5-flash` default
   is replaced with `gemini-2.5-flash`. Gemini also gains `list_models`
   support so the new picker works there.
2. **/model is an interactive picker.** No-args `/model` opens a list
   of the active provider's models with ↑/↓ navigation and Enter to
   switch. The choice is persisted per provider to
   `~/.savvagent/models.toml` and re-applied on reconnect.
3. **TUI rendering integrity.** Tool subprocesses (`tool-fs`,
   `tool-bash`, `tool-grep`) and the host's own tracing no longer
   bleed onto ratatui's alternate screen. All `tracing` output now
   lands in `~/.savvagent/logs/`.
4. **Keybindings help modals.** New `/prompt-keybindings` and
   `/editor-keybindings` slash commands open scrollable, sectioned
   help screens (chrome shared via a new `keybindings_view` module).
5. **Editor syntax theme.** `view-file` / `edit-file` now syntax-color
   code using a theme derived from the active TUI palette — switching
   themes re-themes the editor.

### New features

- `/model` picker — `Effect::SetActiveModel` and
  `ScreenArgs::ModelPicker` added to the plugin contract. The
  `internal:model` plugin owns the picker screen and emits
  `Effect::OpenScreen` for no-args invocations; typed-arg
  invocations (`/model <id>`) still apply directly.
- Per-provider model persistence at `~/.savvagent/models.toml`
  (`schema_version = 1`). Precedence on connect: `SAVVAGENT_MODEL` env
  var > persisted file > provider default.
- Gemini `list_models` — queries `v1beta/models`, filters to entries
  whose `supportedGenerationMethods` includes `generateContent`, and
  surfaces `gemini-2.5-flash` as the default when present.
- `/prompt-keybindings` — modal listing the keybindings active in the
  main prompt input.
- `/editor-keybindings` — modal listing the ratatui-code-editor
  keybindings active in `view-file` / `edit-file`.

### Fixes

- **Gemini tool schemas** — a new sanitizer
  (`provider-gemini::schema`) rewrites incoming JSON Schemas into
  Gemini's OpenAPI subset before they hit the wire. It inlines
  `$ref`/`$defs`, drops `$schema`/`$id`/`$comment`/
  `additionalProperties`/`unevaluatedProperties`/`patternProperties`,
  converts `type: ["X", "null"]` to `type: "X"` + `nullable: true`,
  rewrites `const: X` as `enum: [X]`, renames `oneOf` to `anyOf`, and
  lifts bare `{"type": "null"}` members out of `anyOf` (collapsing
  single-remaining-member `anyOf` into the parent). Resolves the
  `Unknown name "$schema"` / `Cannot find field "const"` /
  `Proto field is not repeating, cannot start list` cascades that
  previously made every Gemini turn fail at request validation.
- **Default Gemini model** bumped to `gemini-2.5-flash`; the
  retired `gemini-1.5-flash` was returning `ModelNotFound`.
- **TUI alt-screen integrity** — each tool subprocess's stderr is
  redirected to `~/.savvagent/logs/tools/<binary>.log` after sandbox
  wrapping; the host's own tracing writes to
  `~/.savvagent/logs/savvagent.log`. Tool crates' default log level
  dropped from `info` to `warn`. `RUST_LOG` still overrides for
  debugging.

### Plugin SDK changes

- `Effect::SetActiveModel { id, persist }` — runtime resolves the
  active provider, rebuilds its in-process host with `id`, and
  optionally writes to `~/.savvagent/models.toml`.
- `ScreenArgs::ModelPicker { current_id, models: Vec<ModelEntry> }`
  — picker args; `apply_effects::open_screen` patches the variant
  from `App::cached_models` (refreshed after every connect and
  model change).
- `ModelEntry { id, display_name }` — new type, exported from the
  plugin crate root.

### Dependencies

No new external dependencies.

## v0.11.0 — TUI Self Update (2026-05-13)

In-band self-update. On launch the TUI asynchronously checks the GitHub
Releases API for `robhicks/savvagent-rs` and, if a newer release is
available, surfaces a one-line banner above the existing tips row. A new
`/update` slash command downloads the matching cargo-dist tarball for
the running target triple and atomically replaces the running binary.

### New features

- `home.banner` slot — new render slot above `home.tips`. The plugin
  paints "Update available: vX → vY  (run /update)" when a newer
  release is detected, or "Updated to vY. Restart savvagent to apply."
  after a successful `/update`. Empty/blank when there is no update.
- `/update` — download and install the latest release. Two
  `Effect::PushNote` notes are emitted: a "Downloading vY…" line, then
  a success or failure line. On failure the banner stays in the
  "Update available" state so the user can retry.
- 24-hour cache for the version check, persisted to
  `~/.savvagent/update-check.json`. Subsequent launches within the TTL
  skip the network call entirely.
- Opt-out: `SAVVAGENT_NO_UPDATE_CHECK=1` env var or `--no-update-check`
  CLI flag. Either signal disables both the check and `/update`.
- Dev builds (binary running from `target/{debug,release}/`) are
  detected automatically and short-circuit to `UpdateState::Disabled`
  with no network call.
- On-quit stderr hint: after `/update` succeeds, the TUI prints a
  one-liner to stderr after the alt-screen tears down, so the user
  sees "savvagent: installed v0.11.0 (was v0.10.0). Restart to use
  the new version." even after closing the TUI.

### Plugin SDK changes

None — the feature is implemented entirely inside the savvagent crate
as a new built-in plugin (`internal:self-update`). The `Plugin` trait
surface is unchanged.

### Release infrastructure

- `cargo-dist` `unix-archive` switched from `.tar.xz` to `.tar.gz` so
  the `self_update` crate can extract Unix release artifacts using
  gzip support shipped with the crate (xz extraction would require an
  additional native dependency). v0.10.x users upgrade by re-running
  the curl|sh installer once; from v0.11.0 onwards `/update` handles
  subsequent upgrades.

### Dependencies

- `semver = "1"` — version comparisons in `internal:self-update`.
- `self_update = "=0.42.0"` — atomic binary replacement. Pinned to
  0.42 because 0.43.1 has type-inference failures against rustc 1.85
  + edition 2024.

### Known limitations

- The actual binary swap is not unit-tested end-to-end — the plugin's
  orchestration is covered by a `BinarySwapper` stub, but the
  production `self_update::backends::github::Update` path requires a
  real release artifact to exercise. End-to-end verification happens
  post-v0.11.0 once a v0.11.x release exists to update to.
- `~/.savvagent/config.toml` opt-out (mentioned in the original issue)
  is deferred. Env var + CLI flag are sufficient for v0.11.0;
  introducing a config.toml for one boolean was over-scope.
- `cargo install --force savvagent` is not supported — savvagent is
  not yet published to crates.io. cargo-dist tarballs are the only
  distribution channel.

### Migration notes

No external API or wire-protocol changes. Existing v0.10.x users
upgrade by re-running the curl|sh installer. Plugin authors are
unaffected.

## v0.10.1 — TUI polish (2026-05-13)

Render-path fixes for theme legibility, layout breathing room, and
command-palette alignment. No API, plugin, or wire-format changes.

### Fixes

- TUI padding: removed the outer terminal-edge inset and added
  interior padding to each bordered widget (header, conversation
  log, input, popups, screen-stack modals). Content now sits inside
  the borders with breathing room instead of the entire app being
  inset from the terminal edges.
- Block titles ("Conversation", popup titles, screen-stack modal
  titles) now render in `palette.fg` instead of inheriting the
  border color, so they stay legible on upstream themes whose
  `border`/`selection` color is a pale chrome accent.
- Upstream themes' `muted` color is now blended 50% toward `fg`,
  so command descriptions, footer chrome, and conversation notes
  remain readable across Solarized Light, Catppuccin Latte, Tokyo
  Night Day, etc. Built-in themes are unchanged.
- Command palette: description column aligns across rows even when
  command names exceed 12 characters (`/connect anthropic`,
  `/connect gemini`, …). Width is now computed from the longest
  filtered name with a 12-char floor and 2-col gutter.
- Footer right slot widened from 33% to 50% so the working-directory
  path and version string no longer clip the SemVer patch level
  (e.g., `v0.10.0` rendering as `v0.10.`).

## v0.10.0 — Localize TUI (2026-05-13)

Internationalization for the TUI. The savvagent crate now ships
`rust-i18n` catalogs for English, Spanish, Portuguese, and Hindi. A
new `internal:language` built-in plugin contributes a `/language`
slash command and a centered-modal picker (mirroring the existing
`internal:themes` plugin).

### New features

- `/language` — open the language picker. Arrow-key navigation,
  type-to-filter, Enter to apply + persist, Esc to cancel.
- `/language <code>` — directly switch to a supported locale
  (`en`, `es`, `pt`, `hi`).
- Boot-time locale detection: `~/.savvagent/language.toml` > `LC_ALL`
  > `LC_MESSAGES` > `LANG` > `en`.
- Live preview during picker navigation; Esc reverts.

### Plugin SDK changes

- `Effect::SetActiveLocale { code, persist }` — additive variant on
  the `#[non_exhaustive]` Effect enum.
- `ScreenArgs::LanguagePicker { current_code }` — additive variant on
  the `#[non_exhaustive]` ScreenArgs enum.
- `savvagent-plugin` crate version: 0.9.0 → 0.10.0.

### Known limitations

- Slash-command summaries in the command palette are captured at the
  boot locale; changing language mid-session requires a restart to
  refresh the summary column. Modal titles, picker rows, status-bar
  text, and pushed notes all re-resolve every frame.
- Hindi rendering depends on a terminal font that includes Devanagari
  glyphs. Without one, rows fall back to replacement boxes. The
  language code column (`hi`) is always ASCII, so the user can still
  filter and select.
- `LANGUAGE=` (glibc compound-locale env var) is not honored — only
  POSIX `LC_ALL` / `LC_MESSAGES` / `LANG`.
- A small number of user-facing literals were deferred during PR 6's
  string sweep (ui.rs header, plugin manifest `name` fields, a handful
  of `app.rs` notes). The catalog parity test continues to enforce
  structural correctness on what IS in the catalog; these will be
  cleaned up in a follow-up.

### Migration notes

No external API changes for non-plugin consumers. Plugin authors:
`SetActiveLocale` and `LanguagePicker` are additive on
`#[non_exhaustive]` enums; existing match arms continue to compile
unchanged. The runtime applies `SetActiveLocale` automatically; no
plugin code needs to call `rust_i18n::set_locale` directly.

## [0.9.0] - 2026-05-12

### v0.9.0 — Plugin system

The TUI and host are now routed through a typed `Plugin` trait. Eighteen
built-in plugins compose the entire UI surface — chrome (footer, tips),
splash, command palette, modal screens (themes, plugins manager, connect,
resume, file viewer/editor), slash commands (`/clear`, `/save`, `/model`,
`/connect`, `/resume`, `/quit`), and provider plugins for Anthropic,
OpenAI, Gemini, and the local Ollama backend. A new screen-stack runtime
replaces the v0.8 `InputMode` state machine with consistent open / close /
back semantics across every modal. Plugin enable / disable state persists
to `~/.savvagent/plugins.toml`. The trait surface is intentionally
WIT-portable so the same plugin contract can drive a future WASM
Component-Model loader without churn.

### Plugin runtime (`savvagent-plugin` crate)

- **New leaf crate `savvagent-plugin`** carrying owned types + trait
  definitions only. No `&str` returns, no callbacks, no host references
  — every method takes / returns owned data so the same contract can
  cross a WASM Component-Model boundary verbatim.
- **`Plugin` trait surface:** `manifest()`, `handle_slash()`,
  `on_event()`, `render_slot()`, and `create_screen()`. A plugin
  implements only the methods relevant to its contributions; defaults
  return `Vec::new()` / `None`.
- **Effect enum** (`Effect`) describes every action a plugin can request:
  `PushNote`, `OpenScreen`, `CloseScreen`, `Stack`, `RunSlash`,
  `SetActiveTheme`, `RegisterProvider`, `SaveTranscript`, `ClearLog`,
  `TogglePlugin`, `Quit`, `PrefillInput`. The host applies effects in
  the order returned; `Stack` composes effects without per-plugin
  recursion.
- **Concrete enums for everything that crosses the boundary** —
  `PluginKind`, `Slot`, `ScreenLayout`, `ThemeColor`, `HostEvent`,
  `Effect` — instead of `Box<dyn Trait>` or string tags. WIT export is
  mechanical: no design work pending.

### Screen stack replaces `InputMode`

- The v0.8 `InputMode` enum (Normal, SelectingTheme, ConnectPicker, …)
  is gone. A single `screen_stack: Vec<ActiveScreen>` field on `App`
  now holds whichever screens are open; the textarea is the focus
  target when the stack is empty.
- **`ScreenLayout` variants** — `CenteredModal { width, height }`,
  `Fullscreen`, `BottomSheet { height }` — clear and repaint their
  region every frame so modals never bleed through onto each other.
- **Open / close / back semantics are uniform.** Esc pops the top
  screen, Enter commits, Ctrl-C closes everything. Splash, command
  palette, themes picker, plugins manager, connect picker, resume
  picker, and the file viewer / editor all participate.

### 18 built-in plugins

Grouped by category. Every entry is shipped enabled by default unless
noted; Core plugins cannot be disabled.

- **Chrome (Core):**
  - `internal:home-footer` — three-segment status bar (provider
    badges left, turn state center, `working_dir · ~N ctx · $0.00 · vX.Y.Z`
    right).
  - `internal:home-tips` — bottom-of-screen muted hint line.
- **Splash (Core):** `internal:splash` — startup splash screen.
- **Modals (Core):**
  - `internal:command-palette` — Ctrl-P / `/` palette.
  - `internal:themes` — `/theme` picker (replaces the v0.8 dedicated
    modal; now a `Screen` plugin like everything else).
  - `internal:plugins-manager` — `/plugins` enable / disable manager.
- **File screens (Optional):**
  - `internal:view-file` — `/view` centered modal.
  - `internal:edit-file` — `/edit` centered modal.
- **Slash-command plugins:**
  - `internal:clear` (Core), `internal:save` (Core), `internal:model`
    (Core), `internal:connect` (Core), `internal:resume` (Core),
    `internal:quit` (Core).
- **Provider plugins (Optional):**
  - `internal:provider-anthropic`, `internal:provider-openai`,
    `internal:provider-gemini`, `internal:provider-local`.

### Plugin manager + persistence

- **`/plugins` opens the manager modal** listing every plugin with its
  kind, version, contribution summary (which slash commands / slots /
  screens it owns), and an on / off toggle. Core plugins render greyed
  out — selecting them is a no-op.
- **Toggles persist atomically** to `~/.savvagent/plugins.toml`
  (schema `version = 1`). Writes go through a tempfile + rename so a
  crash mid-write never leaves a half-baked file.
- **File permissions:** 0o600 on the file, 0o700 on the
  `~/.savvagent/` directory (Unix). Missing file = all defaults
  (additive — never a hard failure on first launch).
- **Startup re-applies persisted overrides** before building the slash
  / slot / screen indexes, so a disabled plugin contributes nothing
  from frame one.

### Theme system (v0.8 work preserved + extended)

- **All v0.8 themes still present** — the 3 hand-rolled built-ins
  (`default`, `dark-mono`, `pastel`) plus the 15 upstream
  `ratatui-themes` slugs (`dracula`, `nord`, `tokyo-night`,
  `catppuccin-mocha`, `catppuccin-latte`, `gruvbox-dark`,
  `gruvbox-light`, `solarized-dark`, `solarized-light`,
  `one-dark-pro`, `monokai-pro`, `rose-pine`, `kanagawa`,
  `everforest`, `cyberpunk`).
- **Picker is now a `Screen` plugin** (`internal:themes`) — same
  open / close semantics as every other modal. `/theme` opens the
  picker; `/theme <slug>` still applies + persists directly without
  opening the modal.
- **Semantic `ThemeColor` variants** — `Fg`, `Bg`, `Accent`, `Muted`,
  `Error`, `Warning`, `Success`, `Secondary`, `Border` — so plugin
  chrome resolves through the active theme rather than hard-coding
  Crossterm colors. Switch themes and every plugin's rendering
  follows.

### Event-hook dispatch

- **`HostEvent` lifecycle events** flow through a `HookDispatcher`:
  `HostStarting`, `Connect`, `Disconnect`, `TurnStart`, `TurnEnd`,
  `ToolCallStart`, `ToolCallEnd`, `PromptSubmitted`, `TranscriptSaved`,
  `ProviderRegistered`, `ContextSizeChanged`.
- **Per-subscriber error isolation.** A panicking or errant plugin
  doesn't take down the dispatcher or other subscribers; its error is
  logged + skipped.
- **Shared `MAX_DISPATCH_DEPTH` cap** with `RunSlash` re-entry — a
  plugin that emits `RunSlash` in response to an event still respects
  the same recursion limit as plugin-emitted slash dispatch, so
  feedback loops fail fast.

### Multi-region home layout

- **Three-segment footer** below the textarea:
  - **Left:** provider badges, contributed by whichever provider
    plugins are enabled + connected.
  - **Center:** turn state ("ready", "thinking…", tool-call summary),
    contributed by `home-footer`.
  - **Right:** `working_dir · ~N ctx · $0.00 · vX.Y.Z`, contributed by
    `home-footer`.
- **1-row vertical / 2-col horizontal frame margin** around the content
  area for breathing room.
- **`$0.00` is a placeholder** — see _Out of scope_ below for the
  deferred cost-tracking work.

### UX polish (v0.9 hotfix, shipped pre-tag)

These landed on master between PR 8 and the release branch to fix
issues caught during manual smoke-testing:

- **Command palette is driven by the live slash index.** Disabled
  plugins' slashes don't appear; newly added plugins show up without
  touching a static list anywhere.
- **`/view` and `/edit` open as centered modals** (popup, not
  full-bleed) and strip the `@` file-picker prefix from path args so
  `/view @src/main.rs` works as expected.
- **`/edit` and `/view` from the palette prefill the textarea**
  (`/view ` / `/edit ` with a trailing space) so the user can finish
  the path via the `@` file picker before submitting.
- **Ctrl-P opens the palette** (matching v0.8 muscle memory).
- **`/quit` is a plugin again** (`internal:quit`, Core) — restored as
  a first-class plugin contribution rather than an `App::handle_command`
  arm.
- **Disabling a plugin actually disables its slashes.** Legacy
  `App::handle_command` arms for `/clear`, `/save`, `/view`, `/edit`,
  and `/quit` are removed — those commands now route exclusively
  through the slash index, so toggling the owning plugin off removes
  the command from the surface.

### Behavior changes (potentially breaking)

- **New config file `~/.savvagent/plugins.toml`** (schema v1).
  Additive: missing-file = all defaults; existing installs upgrade
  without touching anything on disk until the user toggles something.
- **Splash + theme persistence files unchanged** (`splash.toml`,
  `theme.toml`).
- **`InputMode::SelectingTheme` (and the rest of `InputMode`) deleted.**
  Any out-of-tree consumer reading `App` internals would notice — no
  public API impact otherwise.

### Internal architecture

- **New crate:** `crates/savvagent-plugin/` (leaf, no host deps; only
  WIT-portable types + the `Plugin` / `Screen` traits).
- **Consolidation:** the v0.8 `crates/savvagent/src/{splash, palette,
  theme, providers}.rs` modules collapsed into
  `crates/savvagent/src/plugin/builtin/` per-plugin directories.
- **`App::handle_command` slimmed** to just the legacy `/connect` arm
  (every other slash routes through the slash index now).
- **New runtime modules under `crates/savvagent/src/plugin/`:**
  `registry`, `manifests` (slash / slot / screen indexes), `effects`
  (`apply_effects` + `dispatch_host_event`), `hooks`
  (`HookDispatcher`), `slash`, `keybindings`, `screen_stack`, `slots`,
  `convert`.

### Out of scope (deferred)

These are deliberately not in v0.9 and have follow-up issues:

- WASM Component-Model loader / `.wit` file (the trait surface is
  WIT-portable; no loader yet).
- Third-party plugin discovery + signing.
- Sidebar UI.
- Streaming-delta hooks (per-token `on_text_delta` etc.).
- Hot-reload of disabled plugins (toggle takes effect on next launch
  for some surfaces).
- Real session-wide token-usage tracking + cost. The `$0.00` in the
  status line is a placeholder until `TurnOutcome.usage` accumulation
  and a per-model pricing table land.
- `HostEvent::Disconnect` emission — the variant exists and dispatches
  correctly, but no current code path fires it.
- Ollama health-check before `/connect local` — currently builds the
  client unconditionally; PR 7 punted the check to a follow-up.

[0.9.0]: https://github.com/robhicks/savvagent-rs/releases/tag/v0.9.0
