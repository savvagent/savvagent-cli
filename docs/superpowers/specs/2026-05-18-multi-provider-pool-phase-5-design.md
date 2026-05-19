# Multi-provider pool — Phase 5 (user routing rules) — design

Date: 2026-05-18
Status: pending review
Roadmap: ships as part of the multi-provider rollup (see [[project_multi_provider_release.md]] in user memory); per-phase `release(0.X.0)` commits are scaffolding, not tags
Supersedes: nothing
Parent spec: `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`
Predecessor phase: `docs/superpowers/plans/2026-05-18-multi-provider-pool-phase-4.md` (shipped as `release(0.18.0)`)
Successor phase: Phase 6 — heuristic classifier (separate spec, not yet drafted)
Related memories: [[feedback_release_notes]] · [[feedback_release_docs]] · [[feedback_ui_via_plugins]] · [[feedback_streaming_test_permissions]] · [[feedback_test_locale_isolation]] · [[feedback_drive_pr_series_to_completion]] · [[feedback_verify_ci_after_push]]

## Brief

> Phase 5 of `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`.

Phase 5 in the parent spec's "Phasing" list is:

> **User rules from `routing.toml`.** Most flexible, fully debuggable since the user owns the policy. Adds `/route reload` and `/route show`.

The parent spec's "Routing config" section defines the user-facing TOML shape; the "Approach" section defines the layer (Layer 3) and its position in the router (between Modality and Heuristic/Default); the parent spec's `RequiredModalities` struct (shipped in Phase 4) already exposes `has_image` / `has_pdf` / `has_audio` fields the predicates need to bind against.

## Assumptions

Every choice made without asking lives here. The developer's review of this spec is the review of these assumptions.

1. **Config file location is `~/.savvagent/routing.toml`** (verbatim from the parent spec). Mirrors `sandbox.toml`, `models.toml`, `plugins.toml`, `language.toml` — the established pattern is one file per concern under `~/.savvagent/`.
2. **Loader and module live in `savvagent-host`, not the TUI.** Same crate as `Router::pick`. `RoutingRules` is loaded once by `Host::start` from `HostConfig::routing_rules_path`, then stored on `Host` behind a single `Arc<RwLock<RoutingRules>>` field (parallels how `Host` owns `pool`, `active_provider`, `current_model` today). `/route reload` mutates the same handle.
3. **`HostConfig::routing_rules_path: Option<PathBuf>`** — `None` means "don't load any rules; treat as empty." The TUI populates it with the resolved `~/.savvagent/routing.toml` path; the headless example and tests can pass `None`. This mirrors `HostConfig::policy` and `HostConfig::sandbox`'s "None = build a sensible default" convention.
4. **Schema version field, like sandbox.toml.** `version = 1` at the top of the file; loader rejects unknown future versions with a styled warning + empty fallback, identical to `SandboxConfig`'s pattern. Going schema-first now is cheaper than retro-fitting when we add predicates in Phase 6+.
5. **`default` field on `routing.toml` is read but treated as a hint, not a hard override.** The parent spec says "Default model. The current `/model` selection, generalized to identify both a provider and a model. Mirrors today's `SAVVAGENT_MODEL` precedence." Phase 5 wires the `default` field from `routing.toml` into the existing `legacy_model.rs` resolver chain at one position: **after** `SAVVAGENT_MODEL` and **before** `~/.savvagent/models.toml`. That is, env beats routing.toml beats models.toml beats `provider.default_model`. Documented in the parent spec's "Legacy environment compatibility" precedence section by extension. If the developer prefers `default` in `routing.toml` to outrank `models.toml` in a different order, this is the place to flag it.
6. **`heuristics = true` in the file is parsed and stored but not consumed yet** — Phase 6 owns the classifier. Phase 5 records the field on `RoutingRules` and re-routes the data through Phase 6 without re-parsing. This avoids the alternative ("ignore `heuristics` entirely until Phase 6") which would silently swallow a user-typed flag.
7. **Predicates land exactly as the parent spec lists them**, no additions:
   - `has_image: bool` (already detected by Phase 4's `RequiredModalities`)
   - `has_pdf: bool` (always `false` until protocol gains a PDF block; documented as reserved)
   - `has_audio: bool` (same — reserved)
   - `keywords: Vec<String>` — case-insensitive substring match against the latest user message's concatenated `Text` blocks (image-only turns have empty text → no keyword can match)
   - `max_input_chars: usize` — inclusive upper bound on the latest user message's text length
   - `min_input_chars: usize` — inclusive lower bound
   Multiple predicate keys within one `match` table compose with **AND** (per parent spec). The lack of OR / NOT / regex is intentional and additive — adding them later is non-breaking because the predicate set is `#[non_exhaustive]` on the type.
8. **Rules evaluate top-to-bottom, first match wins.** Parent spec is explicit. Stable order = TOML array order. No priority field; if a user wants priority, they reorder.
9. **`use = "provider/model"` is the only `use` form.** Parent spec example uses `provider/model` consistently. A bare model id without a provider is rejected at parse time with a styled note (same pattern as `legacy_model.rs`'s ambiguity warning) because Phase 3 already proved that bare model ids are ambiguous across providers.
10. **A rule whose `use` provider is not currently connected is treated like a stale `@`-override:** the rule is *skipped silently* and the next rule is tried. Logging at `info!` level so debuggers can see the skip. This mirrors `Router::pick`'s existing "stale override falls through" behavior; the alternative (failing the turn or surfacing a per-turn warning) felt too punitive for what is normally a transient state (user disconnected the provider mid-session, file mentions it, will reconnect later).
11. **A rule whose `use` model is unknown to the named provider falls back to that provider's default model + a one-shot styled warning** (logged once per `RoutingRules` load — re-logged after `/route reload`). Parallels the `legacy_model.rs` behavior ("provider exists but model is unknown" → use default + warn).
12. **`/route` is a *plugin*, not a host-direct slash dispatch.** Per [[feedback_ui_via_plugins]], new TUI features must be plugins. `/route reload` and `/route show` ship as one plugin (`internal:route`) with a single `route` SlashSpec and an args-driven subcommand parser inside `handle_slash`. Same pattern as the plugin runtime already uses for plugins with subcommands.
13. **`/route show` output renders inline as styled notes**, not in a new screen. Each rule becomes one line: `[N] <name> — match: <key=value, …> → <provider>/<model> [skipped: provider not connected]`. The active default + active heuristics flag print first. The last `RoutingDecision` from this conversation, if any, prints last. Lower-cost than a screen, easier to share via `/save`. Future iteration: a `route-view` screen plugin if dumping inline gets too noisy.
14. **`Router::pick` gains one new parameter: `rules: &RoutingRules`.** Stateless pure function as today. Existing callers (only `session.rs`) update; the new param is read from the host's `Arc<RwLock<RoutingRules>>` snapshot under the same lock-then-clone pattern used for `active_provider` (snapshot before any `.await`).
15. **`RoutingReason::Rule { name: String }`** is the new variant. The transcript badge renders `Rule(name-of-rule)`. `#[non_exhaustive]` already enabled in Phase 3 so this is additive.
16. **No state file changes.** `~/.savvagent/state.toml` is untouched. Routing rules live in their own file because the parent spec is explicit ("Rules are edited in a file; the TUI offers `/route reload` and `/route show` only").
17. **Per-conversation overrides remain out of scope** (parent spec: "Per-conversation default model override … Orthogonal to multi-provider; can land independently"). Phase 5 ships only the file-driven layer.
18. **`heuristics` and `default` keys are at the top level of the TOML;** `[[rule]]` is the only array-of-tables. This matches the parent spec's example verbatim.
19. **Loader is sync I/O** (`std::fs::read_to_string`) at startup, same as `SandboxConfig::load`. `/route reload` is async and runs the same loader on a `tokio::task::spawn_blocking` if the file is large — though in practice routing.toml is <8 KB so it's just a sync call inside an async function.
20. **i18n strings for the plugin (`slash.route-summary`, the per-line labels, error messages) are added across all four locale files (en/es/pt/hi)**, mirroring how every other slash plugin handles it. en.toml is the canonical source; non-English locales get TODO placeholders if Phase 5 ships before the locale translators see it (per past practice — the existing locale files have TODOs for newer strings already).
21. **Release version: `release(0.19.0)`** in-tree (consistent with the per-phase scaffolding pattern); the actual tagged release rolls up Phases 1-N once the initiative is done, per [[feedback_phase_release_rollup.md]] and [[project_multi_provider_release.md]] in memory.
22. **CHANGELOG, README, release notes** updated in the same commit as the version bump per [[feedback_release_notes]] and [[feedback_release_docs]].
23. **The dead-code rule** ([[feedback_dead_code_in_binary_crate.md]]): any public item the TUI adds for `/route` must be consumed by non-test TUI code OR carry `#[allow(dead_code)]` with a TODO. The plugin pattern already used by `save`/`connect` keeps every new item consumed by the plugin's own `handle_slash` so this is a no-op.

## Goal & Success Criteria

Ship Layer 3 of the parent spec's router stack: user-edited rules in `~/.savvagent/routing.toml` that map per-turn predicates (`has_image`, `keywords`, `max_input_chars` / `min_input_chars`, etc.) to specific `provider/model` picks, with a `/route reload` slash command to re-read the file at runtime and `/route show` to inspect the active rule set. Layer 3 sits between Modality (Phase 4) and Default; `@`-override and Modality still win when they apply.

Measurable success criteria:

1. With a routing.toml whose first rule matches `keywords = ["refactor"]` and `use = "anthropic/claude-opus-4-7"`, a turn whose user message contains "refactor this function" routes to `anthropic/claude-opus-4-7` with `reason = Rule("<name>")`, even when the active provider is Gemini and modality/override don't apply.
2. `/route show` lists every parsed rule, marks rules whose target provider isn't connected as `[skipped: provider not connected]`, and prints the active `default` plus the last `RoutingDecision` (if any). Output is locale-aware and themed (Muted for skipped, Default for active).
3. `/route reload` re-reads `~/.savvagent/routing.toml`, swaps the host's stored `RoutingRules`, and prints a one-line styled note with the rule count. Parse errors fall back to "no user rules" + a styled warning naming the line and column (TOML parser's native error), and the prior rule set is *not* discarded — same recovery pattern as `models.toml`'s `model-pref-save-failed`.
4. A turn while `/route reload` is running cannot observe a partial rule set: the rule list swap is atomic at the `Arc<RwLock<RoutingRules>>` boundary. (Verified by a tokio test that runs `Router::pick` in a tight loop against an in-memory `Host` while another task calls `Host::reload_routing_rules` repeatedly; both succeed without panics.)
5. Workspace + per-crate `cargo test --workspace` is green, including the new `crates/savvagent-host/src/router/rules.rs` unit tests, a new `crates/savvagent-host/tests/route_rules_e2e.rs` integration test, and the `/route` plugin's test module.
6. README and CHANGELOG updated in the version-bump commit; `release(0.19.0)` commit message follows the project's existing `release(0.X.0): <one-line>` convention.

## Scope

### In

- New `crates/savvagent-host/src/router/rules.rs` module: `RoutingRules` parser + evaluator, `RuleMatch` predicate type, `RoutingRulesError`.
- Loader: `RoutingRules::load_from_path(&Path)` and `RoutingRules::empty()`; both sync.
- New `HostConfig::routing_rules_path: Option<PathBuf>` field.
- `Host` gains `routing_rules: Arc<RwLock<RoutingRules>>` and a `Host::reload_routing_rules() -> Result<usize, RoutingRulesError>` method (return value: rule count after reload).
- `Router::pick` gains a `rules: &RoutingRules` parameter; new layer evaluates rules between Modality and Default. Rule that points at a disconnected provider is silently skipped (next rule tried, then Default).
- `RoutingReason::Rule { name: String }` variant + Display impl.
- Wiring in `session.rs` (`run_turn_inner`) to pass the rules snapshot into `Router::pick`.
- New `crates/savvagent/src/plugin/builtin/route/` plugin: `RoutePlugin`, `handle_slash` with subcommands `reload` and `show`. Registered in the built-in plugin set alongside `SavePlugin`, `ConnectPlugin`, etc.
- New `Effect::ReloadRoutingRules` and `Effect::ShowRoutingRules` variants — or, simpler, a single new `Effect::Stack(vec![PushNote{...}])` flow that the runtime applies after the plugin reads from the host directly. Decision below in Architecture; current lean is to add **one** new `Effect::ReloadRoutingRules` effect (which `apply_effects` handles by calling `host.reload_routing_rules().await` then emitting a `PushNote`), and have the plugin compute the `/route show` output itself by reading the host snapshot, returning a `Vec<PushNote>` directly. This minimizes Effect-enum churn.
- `legacy_model.rs` resolver gets one new step in its precedence chain to consult `routing.toml`'s `default` field. Existing tests + behavior preserved.
- i18n catalog updates across en/es/pt/hi (placeholder text for non-English locales if not translated in time).
- New integration test `crates/savvagent-host/tests/route_rules_e2e.rs`.
- README user-facing section: a one-paragraph note on `~/.savvagent/routing.toml` with a sample config.
- CHANGELOG `## 0.19.0 - 2026-05-18` entry.
- Workspace version bump to `0.19.0` (mirrored in `[workspace.dependencies]` literals per [[feedback_semver]]).

### Out (deferred)

- Heuristic classifier (`heuristics = true`) — Phase 6, separate spec.
- `regex` predicate support, `OR` / `NOT` predicate combinators — additive, future predicate types under `#[non_exhaustive]` `RuleMatch`.
- Per-conversation overrides (the spec's "Open question — Phase 3 fate of `/use`") — orthogonal.
- A `route-view` screen plugin (`/route show` keeps inline rendering for v1).
- Hot-reload via filesystem watcher — explicit `/route reload` is the only re-read trigger.
- Editing rules from the TUI (`/route add`, `/route delete`) — file-only per parent spec.
- Cross-provider modality fallback. Phase 4 explicitly left this to "user rules" — Phase 5 enables it because a user can now write a rule `match = { has_image = true }, use = "gemini/gemini-2.0-flash-vision"` that crosses the billing boundary by choice. Phase 5 does not *automate* the cross-provider hop, but it does let the user *configure* it.

## Architecture

### Data types

`crates/savvagent-host/src/router/rules.rs` — new module. Public types:

```rust
pub const ROUTING_RULES_SCHEMA_VERSION: u32 = 1;

/// In-memory representation of `~/.savvagent/routing.toml`.
#[derive(Debug, Clone, Default)]
pub struct RoutingRules {
    /// Default `provider/model` from the file's `default = "..."` entry,
    /// if present. Empty string means "key absent". Used by the legacy-model
    /// resolver chain.
    pub default: Option<DefaultPick>,
    /// Whether the user opted in to the Phase 6 heuristic classifier.
    /// Parsed in Phase 5; consumed in Phase 6.
    pub heuristics: bool,
    /// Rules in TOML order. First match wins during evaluation.
    pub rules: Vec<RoutingRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultPick {
    pub provider: ProviderId,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingRule {
    /// Human-readable name from the TOML `name = "…"` field.
    pub name: String,
    pub match_: RuleMatch,
    pub use_: DefaultPick, // `provider/model`
}

#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleMatch {
    pub has_image: Option<bool>,
    pub has_pdf: Option<bool>,
    pub has_audio: Option<bool>,
    pub keywords: Vec<String>, // lowercased at parse time
    pub max_input_chars: Option<usize>,
    pub min_input_chars: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RoutingRulesError {
    #[error("routing.toml schema version {found} not supported (max {max})")]
    UnsupportedVersion { found: u32, max: u32 },
    #[error("routing.toml at {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },
    #[error("routing.toml at {path}: rule {index} `{name}`: `use` must be `provider/model`, got `{got}`")]
    BadUseSyntax { path: PathBuf, index: usize, name: String, got: String },
    #[error("routing.toml at {path}: io error: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

impl RoutingRules {
    /// Empty rules — Phase 4-compatible behavior (no rule ever matches).
    pub fn empty() -> Self { Self::default() }

    /// Load and parse a routing.toml. File-absent → `Ok(Self::empty())`
    /// (matches the parent spec's "parse errors fall back to no user rules
    /// + a styled note" rule, but file-absent is a non-error case).
    pub fn load_from_path(path: &Path) -> Result<Self, RoutingRulesError>;

    /// Evaluate against a turn's signals. Returns `Some((rule_name,
    /// DefaultPick))` on first match, else `None`. Skips rules whose
    /// target provider isn't in `connected`.
    pub fn evaluate(
        &self,
        signals: &RuleSignals<'_>,
        connected: &[&ProviderId],
    ) -> Option<(String, DefaultPick)>;
}

/// Per-turn inputs the rules layer evaluates against. Built by the host
/// once per turn from the latest user message + `RequiredModalities`.
pub struct RuleSignals<'a> {
    pub required: RequiredModalities,
    pub user_text: &'a str, // concatenated Text blocks of the latest user message, lowercased on demand
}
```

### Wire shape (`~/.savvagent/routing.toml`)

Matches the parent spec verbatim, plus a `version` field for schema gating:

```toml
version = 1
default = "anthropic/claude-opus-4-7"
heuristics = false

[[rule]]
name = "vision-for-images"
match = { has_image = true }
use = "gemini/gemini-2.0-flash-vision"

[[rule]]
name = "haiku-for-shortform"
match = { max_input_chars = 400 }
use = "anthropic/claude-haiku-4-5"

[[rule]]
name = "deep-reasoning"
match = { keywords = ["refactor", "design", "architect", "investigate"] }
use = "anthropic/claude-opus-4-7"
```

### Router integration

`Router::pick` now takes one extra argument:

```rust
pub fn pick(
    override_: Option<RoutingOverride>,
    providers: &[ProviderView<'_>],
    active_provider: &ProviderId,
    active_model: &str,
    required: RequiredModalities,
    rules: &RoutingRules,
    user_text: &str,
) -> RoutingDecision
```

Layer order (first match wins):

1. **Override** — `@provider:model` from the user's first text block. As Phase 3+4.
2. **Modality** — `RequiredModalities` + same-provider vision redirect. As Phase 4.
3. **Rules** — new in Phase 5. Build `RuleSignals { required, user_text }`; call `rules.evaluate(&signals, &connected_provider_ids)`. On `Some((name, pick))`, return `RoutingDecision { provider_id: pick.provider, model_id: pick.model, reason: RoutingReason::Rule { name } }`.
4. **Default** — `(active_provider, active_model)` as today.

(Phase 6 will insert Heuristic between Rules and Default.)

### Host integration

`Host` gains `routing_rules: Arc<RwLock<RoutingRules>>`. Construction reads `HostConfig::routing_rules_path` once at `Host::start`; absent path → `RoutingRules::empty()`. New method:

```rust
impl Host {
    /// Re-read `routing_rules_path` and atomically swap the in-memory
    /// rules. Returns the new rule count on success. On parse error,
    /// leaves the existing rules untouched and returns the error so the
    /// caller (the `/route` plugin) can surface it.
    pub async fn reload_routing_rules(&self) -> Result<usize, RoutingRulesError>;

    /// Snapshot of the current rules (clone). Used by `/route show` to
    /// build its output without holding the RwLock.
    pub async fn routing_rules_snapshot(&self) -> RoutingRules;
}
```

The snapshot pattern matches the existing `active_provider` / `current_model` reads in `run_turn_inner`: `let rules = self.routing_rules.read().await.clone();` before any `.await` that runs `complete`.

### TUI plugin (`internal:route`)

New `crates/savvagent/src/plugin/builtin/route/`:

```
route/
├── mod.rs          // RoutePlugin, Manifest, SlashSpec, handle_slash
└── (tests inline in mod.rs)
```

`handle_slash("/route", args)` parses the first positional arg:

- `args == []` → treat as `show` (parity with `/sandbox` showing status when called bare).
- `args == ["reload"]` → emit `Effect::ReloadRoutingRules`.
- `args == ["show"]` → emit `Effect::ShowRoutingRules`.
- Anything else → `Effect::PushNote` with a usage hint.

Two new effects in `crates/savvagent-plugin/src/effect.rs`:

```rust
/// Re-read ~/.savvagent/routing.toml and swap the host's stored rules.
ReloadRoutingRules,
/// Print the active routing rules and the most recent decision as styled notes.
ShowRoutingRules,
```

Both handled in `crates/savvagent/src/plugin/effects.rs::apply_effects`:

- `ReloadRoutingRules` → `host.reload_routing_rules().await`; append a `PushNote` with the rule count or the parse error.
- `ShowRoutingRules` → read `host.routing_rules_snapshot().await` and `host.last_routing_decision().await` (new method on `Host`; see below), format as styled lines, push them.

`Host::last_routing_decision()` is a new field `Arc<RwLock<Option<RoutingDecision>>>` set inside `run_turn_inner` right after `Router::pick`. Cheap, always overwritten, no eviction needed.

### Data flow for one turn (Phase 5 deltas)

```
session.rs::run_turn_inner
  ├─ snapshot active_provider, active_model, routing_rules (all clones)
  ├─ build messages, parse @-prefix override
  ├─ required = required_modalities(&messages)
  ├─ user_text = concat Text blocks of the latest user message
  ├─ rules_snapshot = host.routing_rules.read().await.clone()    // NEW
  ├─ decision = Router::pick(
  │      override_,
  │      &views,
  │      &active_id,
  │      &active_model,
  │      required,
  │      &rules_snapshot,                                          // NEW
  │      &user_text,                                               // NEW
  │  )
  ├─ host.set_last_routing_decision(decision.clone()).await        // NEW
  ├─ emit TurnEvent::RouteSelected { … reason might be Rule(name) }
  ├─ … rest of turn loop unchanged …
```

### Legacy-model resolver chain (the only change to `legacy_model.rs`)

Today (after Phase 4): `SAVVAGENT_MODEL` env → `~/.savvagent/models.toml` → `provider.default_model`.

Phase 5 inserts `routing.toml`'s `default` between env and `models.toml`:

`SAVVAGENT_MODEL` env → `routing.toml#default` → `~/.savvagent/models.toml` → `provider.default_model`.

Rationale: routing.toml is the explicit per-user routing policy file; if the user puts a `default` there, it's their stated preference and should beat the per-provider `models.toml` recall. Env always wins so test harnesses and one-off CLI overrides still work.

The resolver gets one new optional parameter (`routing_default: Option<&DefaultPick>`) and chooses it before consulting `models.toml`. Pure function, easy to unit-test.

### i18n keys (added to en.toml; placeholders in es/pt/hi)

```
[slash]
route-summary    = "Manage routing rules"

[routing]
reloaded         = "Reloaded routing.toml — %{count} rule(s) active."
reload-failed    = "Couldn't reload routing.toml: %{err}"
show-header      = "Active routing rules (in order):"
show-rule-line   = "[%{index}] %{name} — %{match} → %{provider}/%{model}"
show-rule-skipped = "[%{index}] %{name} — %{match} → %{provider}/%{model}  (skipped: provider not connected)"
show-no-rules    = "No routing rules. Edit ~/.savvagent/routing.toml and run /route reload."
show-default     = "Default: %{provider}/%{model}"
show-no-default  = "Default: (using /model selection)"
show-last        = "Last decision: %{provider}/%{model} — %{reason}"
show-no-last     = "No turns this session yet."
route-usage      = "Usage: /route [show | reload]"
```

## Error handling & edge cases

- **File absent** → `Ok(RoutingRules::empty())`. Treated as the steady state for users who haven't created the file. No log, no warning. The user sees `/route show` print "No routing rules. Edit ~/.savvagent/routing.toml…".
- **File parse error** (bad TOML, schema mismatch, bad `use` syntax) → loader returns `Err(RoutingRulesError::*)`. At startup, `Host::start` logs `tracing::warn!` and continues with `RoutingRules::empty()`. At `/route reload`, the plugin renders the parse error as a styled note via `routing.reload-failed`; existing in-memory rules are preserved.
- **Rule's `use` provider not connected** → rule is skipped during evaluation; `/route show` marks the row with the "skipped" label so the user sees why their rule never fires. No per-turn warning (would be noisy for users with rules that target multi-provider setups they don't always connect).
- **Rule's `use` model not in provider's `ProviderCapabilities`** → fall back to that provider's `default_model_id()` and log a one-shot warning at load time (re-logged on every `/route reload`). Same fallback shape as `legacy_model.rs` provider-known-model-unknown branch.
- **Empty `match` table** → matches every turn. Valid (a catch-all "send everything here" rule). `/route show` prints `match: <any>` for clarity.
- **`max_input_chars < min_input_chars`** → loader rejects with `RoutingRulesError::Parse` (validation step after TOML parse). Detected at load time so the user sees one consolidated error.
- **`keywords = []`** in TOML → predicate is *unset* (vec is empty); the rule still matches if other predicates pass. (Alternative: treat empty as "match nothing." Rejected — empty list is the natural way to say "no keyword constraint on this rule.")
- **Concurrent reload mid-turn** → `Router::pick` operates on a `RoutingRules` *snapshot* (clone) taken before any `.await`, so a `reload_routing_rules` swap mid-turn cannot change the rules an in-flight `Router::pick` is reading. The very next turn picks up the new rules.
- **Routing.toml that resolves `default` to a not-connected provider** → at evaluation time, the legacy-model resolver falls through to `models.toml`'s value (same as today's "stale `default`" behavior). Logged once on load.
- **`/route reload` with no host (pool empty / pre-connect)** → still works (rules live on the host but `Host::reload_routing_rules` doesn't need a connected provider). If the host hasn't been constructed yet (very early startup window), the slash command is rejected with `routing.reload-failed` carrying a "no host yet" message — same defense as `/save` uses today.
- **Locale set to a language without translations for the new keys** → rust_i18n falls back to en (project default) automatically.

## Testing approach

Per [[feedback_streaming_test_permissions]] and [[feedback_test_locale_isolation]], the streaming tests pre-register `Allow` rules and reset locale to "en" inside `HOME_LOCK`.

### Pure unit tests (in `crates/savvagent-host/src/router/rules.rs`)

- Empty file → `RoutingRules::empty()`.
- Schema version 1 parses; version 2 (future) returns `UnsupportedVersion`.
- All predicate types parse round-trip.
- `use = "anthropic"` (no slash) → `BadUseSyntax`.
- `max_input_chars = 100, min_input_chars = 500` → `Parse` (validation rejects).
- Evaluator: AND semantics across predicates within one rule.
- Evaluator: first-match-wins ordering.
- Evaluator: skip rule whose `use.provider` not in `connected`.
- Evaluator: empty `match` table matches any turn.
- `legacy_model.rs`: `routing.toml#default` beats `models.toml` but loses to env.

### Router unit tests (in `crates/savvagent-host/src/router/router.rs`)

- `Router::pick` with `rules.empty()` and no override/modality → Default (regression).
- `Router::pick` with a rule that matches → `RoutingReason::Rule { name }`.
- Override beats matching rule.
- Modality redirect beats matching rule (rules run after modality per the layer order).
- A rule whose provider isn't in `providers` is silently skipped; falls through to Default.
- A rule whose `use.model` isn't in the provider's caps → router still returns the provider's default model id (assumes loader-side fallback already applied).

### Plugin unit tests (in `crates/savvagent/src/plugin/builtin/route/mod.rs`)

- `handle_slash("route", [])` → emits `ShowRoutingRules`.
- `handle_slash("route", ["show"])` → emits `ShowRoutingRules`.
- `handle_slash("route", ["reload"])` → emits `ReloadRoutingRules`.
- `handle_slash("route", ["wat"])` → emits `PushNote { route-usage }`.

### Effects-handler tests (in `crates/savvagent/src/plugin/effects.rs`)

- `ReloadRoutingRules` happy path: host gets the new rules; PushNote shows count.
- `ReloadRoutingRules` parse error: existing rules preserved; PushNote shows the error.
- `ShowRoutingRules` empty rules: prints `show-no-rules`.
- `ShowRoutingRules` with rules: prints header + one line per rule + default + last-decision.

### Integration tests

- `crates/savvagent-host/tests/route_rules_e2e.rs` — host with two stub providers (anthropic, gemini), routing.toml writes a `keywords = ["refactor"]` → `gemini/...` rule, run a streaming turn whose user text matches, assert `TurnEvent::RouteSelected { reason: Rule(...) }` and that the provider invoked was Gemini. Uses the existing `HOME_LOCK` per [[feedback_test_locale_isolation]] and pre-registered tool allows per [[feedback_streaming_test_permissions]] (none needed for a tool-free turn, but include the helper so the pattern is consistent for future authors).
- A second case: two rules, both could match; first wins.
- A third case: a rule whose provider was just disconnected — turn falls through to Default and emits no warning event.
- Reload-mid-turn race: tokio test with two tasks (one running `Router::pick` in a loop, the other calling `reload_routing_rules` in a loop). Asserts no deadlock / no panic / final rule count is what the last reload wrote.

### CI parity check

Per [[feedback_match_ci_toolchain_locally]] and [[feedback_verify_ci_after_push]]: run `rustup run stable cargo fmt`, `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` locally before pushing, and watch `gh run watch` for the PR's SHA before claiming green.

## Risks & Open Questions

1. **`Effect::ShowRoutingRules` vs reading host directly from the plugin.** The plugin-system pattern is that plugins return `Vec<Effect>` and the runtime applies them; plugins don't reach into the host. But `/route show`'s output is dynamic enough that an `Effect::PushNote` per line built from a host snapshot is wasteful round-trip. The chosen design adds a single `Effect::ShowRoutingRules` and has `apply_effects` do the snapshot + render. Alternative: add a generic `Effect::PushNotes(Vec<StyledLine>)` and have the plugin's `handle_slash` get a `&Host` snapshot via a trait. The current design is the minimum new surface; the alternative is more general but adds a trait dependency that the plugin system hasn't required so far. Flagging for the reviewer.

2. **`default` precedence vs `models.toml`.** Spec assumption #5 places `routing.toml#default` ahead of `models.toml`. The other defensible position is the inverse: `models.toml` is the per-provider preference the user just clicked through in the `/model` picker (very explicit), so it should win over a possibly-stale `routing.toml#default`. I picked routing.toml because it's the file the user *edited by hand* — picker-driven prefs feel more disposable. Open for revision.

3. **Empty `keywords = []` semantics.** Treated as "no keyword constraint." The alternative ("match nothing") is also defensible but more error-prone (typo deletes the list → silent rule). Documented in the parser; called out in `/route show`.

4. **Reload error visibility.** A parse error from `/route reload` shows the toml parser's native error text. That text can be cryptic for users who don't read Rust crate docs. Phase 5 ships the raw error; a follow-up could add a friendlier formatter (line+col context with the offending key). Tracking as "nice to have" — current behavior matches `models.toml` and `sandbox.toml`.

5. **`heuristics = true` while Phase 6 isn't shipped.** Phase 5 parses it but doesn't use it. A user who sets `heuristics = true` in 0.19.0 will *not* see classifier behavior until 0.20.0 lands. Add a one-line note on `/route show` ("heuristics: enabled — classifier ships in a future release") so the user isn't surprised. Worth doing in Phase 5 even though it's borderline-Phase-6 work.

6. **`RuleMatch` is `#[non_exhaustive]` from day one.** This makes the type's external semver more permissive but means consumers (Phase 6, follow-up predicates) can grow the struct without breaking changes. Cost: pattern-matchers must use `..`. Net positive — the project has used `#[non_exhaustive]` on `RoutingReason`, `PoolError`, etc. with no friction.

7. **`/route show` output length** for users with many rules (>50) gets dumped inline. The parent spec calls this "an A/B" between inline and on-demand; this design picks inline (cheap, matches `/sandbox`-style status dumps). If usage shows long-rules-list users, a Phase 6 follow-up adds pagination or a screen plugin. Not blocking Phase 5.

8. **Concurrent-reload-during-turn race coverage.** The atomic-swap design depends on snapshotting the rules clone *before* any `.await` in `run_turn_inner`. This is the same pattern the rest of `session.rs` uses (project_tui_design), so the discipline is established — but a regression here would silently produce inconsistent routing decisions. The integration test for the race condition exists specifically to guard against that.

9. **Cross-provider rule activation without warning.** Phase 4's modality layer refuses to silently cross provider boundaries; Phase 5's user-rules layer *does* cross boundaries by design (that's the whole point — `[[rule]] match = { has_image = true }, use = "gemini/..."` is the documented way to enable cross-provider vision). The transcript badge already shows the `Rule(<name>)` reason, so the user can always see the rule that did the routing. No additional consent UI ships. Risk: a user copies a routing.toml from a tutorial and finds turns going to a provider they didn't intend. Mitigation: `/route show` makes the file's effect inspectable in one command, and the badge identifies the routing source for every turn.

10. **Locale string drift.** Adding new i18n keys means es/pt/hi files get placeholder strings or English fallbacks. Past phases have shipped with TODOs; that's the project's current bar. A future "i18n catch-up" task closes the gap.
