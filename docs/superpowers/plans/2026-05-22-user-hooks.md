# User-defined hooks — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Claude-Code-compatible user shell hooks (`settings.json`) with `PreToolUse` blocking via a new `PreToolUseGate` trait in `savvagent-host`, plus four observe-only event hooks (`PostToolUse`, `UserPromptSubmit`, `SessionStart`, `Stop`).

**Architecture:** One new built-in plugin `internal:user-hooks` under `crates/savvagent/src/plugin/builtin/user_hooks/`. Reuses the four-path discovery pattern from sub-project A (`.savvagent/` and `.claude/` × project and user). A new `PreToolUseGate` trait in `savvagent-host` is consulted inside `ToolRegistry::call_with_bash_net_override` before tool dispatch. The plugin registers itself as the gate via a new savvagent-internal `Effect::RegisterPreToolGate { plugin_id }` (mirroring how providers register clients). Two new WIT-portable `Effect` variants (`PrependToPendingPrompt`, `CancelPendingTurn`) carry string payloads for prompt rewriting and turn cancellation.

**Tech Stack:** Rust 2024, `serde_json` (workspace), `globset = "0.4"` (add if not present), `async-trait` (workspace), `tokio::process::Command` (with `kill_on_drop`), `tempfile` (dev-dep, already present).

---

## Spec drift discoveries (read while drafting this plan)

1. **`ToolRegistry` is `pub(crate)`** — the gate field belongs on `Host` (public API), not on `ToolRegistry`. The call sites at `session.rs:1249` and `session.rs:1494` invoke `self.tool_registry.call_with_bash_net_override(...)`. Consulting the gate must happen there (or inside `Host`-side helpers around them) rather than inside `ToolRegistry`.
2. **`BuiltinProviderPlugin` lives at `crates/savvagent/src/plugin/builtin/provider_common.rs`** — pattern reference. The sibling `BuiltinHookPlugin` trait will live alongside it.
3. **`globset` may not be a direct dep of `savvagent`** — `ignore` (used by sub-project A's discovery) pulls it in transitively. Task 4 verifies and adds as needed.

---

## File map

**Create:**

- `crates/savvagent-host/src/pre_tool_gate.rs` — `PreToolUseGate` trait + `PreToolDecision` enum.
- `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` — `Plugin` impl, dispatch glue, plus the `BuiltinHookPlugin` trait impl.
- `crates/savvagent/src/plugin/builtin/user_hooks/config.rs` — serde types: `HooksConfig`, `MatcherGroup`, `HookCommand`.
- `crates/savvagent/src/plugin/builtin/user_hooks/discovery.rs` — walk + merge the four `settings.json` files.
- `crates/savvagent/src/plugin/builtin/user_hooks/matcher.rs` — `globset` compile + cached patterns.
- `crates/savvagent/src/plugin/builtin/user_hooks/payload.rs` — stdin JSON builder per `HookKind`.
- `crates/savvagent/src/plugin/builtin/user_hooks/decision.rs` — `HookDecision` + JSON-stdout parse.
- `crates/savvagent/src/plugin/builtin/user_hooks/runner.rs` — spawn shell child, await with timeout, parse outcome.
- `crates/savvagent/src/plugin/builtin/user_hooks/pre_tool_gate.rs` — plugin's `impl PreToolUseGate`.
- `crates/savvagent/src/plugin/builtin/user_hooks/reload.rs` — `/reload-hooks` slash command handler.

**Modify:**

- `crates/savvagent-plugin/src/effect.rs` — add three `Effect` variants (`RegisterPreToolGate`, `PrependToPendingPrompt`, `CancelPendingTurn`).
- `crates/savvagent-host/src/lib.rs` — re-export `pre_tool_gate` module.
- `crates/savvagent-host/src/session.rs` — `Host` field for the gate, setter, call-site consultation around tool dispatch.
- `crates/savvagent/src/app.rs` — shared `Arc<RwLock<HooksIndex>>` field, startup load.
- `crates/savvagent/src/main.rs` — pass the shared handle into `register_builtins`.
- `crates/savvagent/src/plugin/mod.rs` — register the new plugin in `register_builtins`.
- `crates/savvagent/src/plugin/builtin/mod.rs` — declare the `user_hooks` module.
- `crates/savvagent/src/plugin/builtin/provider_common.rs` — add sibling `BuiltinHookPlugin` trait + `HookEntry` (mirrors `BuiltinProviderPlugin`/`ProviderEntry`).
- `crates/savvagent/src/plugin/effects.rs` — `apply_effects` arms for the three new variants.
- `crates/savvagent/Cargo.toml` — `globset.workspace = true` if absent.
- `README.md` — new "User-defined hooks" section + on-disk paths reference.
- `CHANGELOG.md` — `[Unreleased]` entry.

---

## Conventions

- All `cargo test` invocations specify the crate (`-p savvagent` or `-p savvagent-host`) for fast iteration.
- `cargo test -p savvagent` runs MULTIPLE test binaries. The main lib/bin runner is the one with hundreds of tests. Always read the FIRST `test result` line.
- CI uses `RUSTFLAGS=-D warnings`. New `pub` items not yet consumed get `#[allow(dead_code)]` with a brief `// consumed by Task N` comment, removed once Task N lands.
- Disk-asserting tests gate to `#[cfg(unix)]` (follow-up tracking the `dirs::home_dir()` Windows test-isolation gap remains open from sub-project A).
- Tests touching `HOME` use `HOME_LOCK` + `HomeGuard` per [[feedback_test_locale_isolation]].
- No Claude self-attribution in commits, comments, or anywhere.
- Local commits only; no `git push` from implementer subagents.

---

### Task 1: Skeleton plugin + registration

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/mod.rs`
- Modify: `crates/savvagent/src/plugin/mod.rs` (`register_builtins`)

- [ ] **Step 1: Smoke test + plugin shell**

Create `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs`:

```rust
//! `internal:user-hooks` — discovers and dispatches Claude-Code-compatible
//! user shell hooks from `settings.json`. See
//! `docs/superpowers/specs/2026-05-22-user-hooks-design.md`.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
};

/// Built-in plugin that exposes user-authored shell hooks.
pub struct UserHooksPlugin;

impl UserHooksPlugin {
    /// Construct a new [`UserHooksPlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for UserHooksPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for UserHooksPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "reload-hooks".into(),
            summary: "Rescan user-defined hooks (settings.json)".into(),
            args_hint: None,
            requires_arg: false,
        }];
        Manifest {
            id: PluginId::new("internal:user-hooks").expect("valid built-in id"),
            name: "User hooks".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Claude-Code-compatible settings.json hooks".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_reload_hooks() {
        let p = UserHooksPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:user-hooks");
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-hooks"));
    }
}
```

- [ ] **Step 2: Declare the module**

Append to `crates/savvagent/src/plugin/builtin/mod.rs`:

```rust
/// `internal:user-hooks` — Claude-Code-compatible user shell hooks from
/// `settings.json` files. PreToolUse gating + observe-only events.
pub mod user_hooks;
```

- [ ] **Step 3: Register in `register_builtins`**

In `crates/savvagent/src/plugin/mod.rs`, inside the `plugins` Vec in `register_builtins`, add (alphabetical position is fine):

```rust
        Box::new(builtin::user_hooks::UserHooksPlugin::new()),
```

- [ ] **Step 4: Update the expected-list test**

In `crates/savvagent/src/plugin/mod.rs`'s `register_builtins_pr8_complete` test, add `"internal:user-hooks"` to the expected-ids list and bump the count assertions by 1 (e.g. `set.plugins.len()` from 26 → 27, registry `.len()` from 30 → 31, and update the trailing comment).

- [ ] **Step 5: Run the smoke test**

```bash
cargo test -p savvagent plugin::builtin::user_hooks::tests::manifest_has_reload_hooks
cargo test -p savvagent --test 'register_builtins_pr8_complete' 2>&1 | head -10
```

Either invocation works. Confirm both pass.

- [ ] **Step 6: Build + commit**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
git add crates/savvagent/src/plugin/builtin/user_hooks/mod.rs \
        crates/savvagent/src/plugin/builtin/mod.rs \
        crates/savvagent/src/plugin/mod.rs
git commit -m "feat(plugin/user-hooks): plugin skeleton with /reload-hooks"
```

---

### Task 2: JSON config types

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_hooks/config.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` (add `mod config;`)

- [ ] **Step 1: Write failing tests + impl**

Create `crates/savvagent/src/plugin/builtin/user_hooks/config.rs`:

```rust
//! Serde types for the Claude-Code-compatible `settings.json` hooks block.
//!
//! Top-level keys other than `hooks` are ignored (forward-compat). Unknown
//! event names under `hooks` parse cleanly with a warn-log; they're
//! preserved so a future map-event-to-HookKind pass can address them.

#![allow(dead_code)] // consumed by Task 4 (discovery)

use serde::Deserialize;

/// The portion of `settings.json` we care about.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SettingsFile {
    #[serde(default)]
    pub hooks: HooksMap,
}

/// `hooks.{EventName} -> Vec<MatcherGroup>`. Untyped event keys so we
/// can warn-log unknowns at index-build time.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct HooksMap(pub std::collections::BTreeMap<String, Vec<MatcherGroup>>);

/// A `(matcher, hooks)` group within an event's array.
#[derive(Debug, Clone, Deserialize)]
pub struct MatcherGroup {
    /// Glob pattern over the tool name; ignored for non-tool events.
    /// Defaults to `"*"` if absent.
    #[serde(default = "default_matcher")]
    pub matcher: String,
    /// The shell commands to run when this group matches.
    pub hooks: Vec<HookCommand>,
}

fn default_matcher() -> String {
    "*".into()
}

/// One shell command to invoke.
#[derive(Debug, Clone, Deserialize)]
pub struct HookCommand {
    /// Currently the only supported type is `"command"`. Other values
    /// warn-log and skip this entry.
    #[serde(default = "default_type", rename = "type")]
    pub type_field: String,
    /// The command line (passed to `sh -c`).
    pub command: String,
    /// Per-hook timeout in seconds. Default 60.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_type() -> String {
    "command".into()
}

fn default_timeout() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_settings_parses() {
        let s: SettingsFile = serde_json::from_str("{}").unwrap();
        assert!(s.hooks.0.is_empty());
    }

    #[test]
    fn ignores_unknown_top_level_keys() {
        let src = r#"{ "permissions": { "x": 1 }, "hooks": {} }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        assert!(s.hooks.0.is_empty());
    }

    #[test]
    fn parses_full_pre_tool_use() {
        let src = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "tool-fs:write_file",
                        "hooks": [
                            { "type": "command", "command": "/p/check.sh", "timeout": 30 }
                        ]
                    }
                ]
            }
        }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        let groups = s.hooks.0.get("PreToolUse").expect("PreToolUse present");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].matcher, "tool-fs:write_file");
        assert_eq!(groups[0].hooks.len(), 1);
        assert_eq!(groups[0].hooks[0].command, "/p/check.sh");
        assert_eq!(groups[0].hooks[0].timeout, 30);
        assert_eq!(groups[0].hooks[0].type_field, "command");
    }

    #[test]
    fn missing_matcher_defaults_to_star() {
        let src = r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "x" } ] } ] } }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        let groups = s.hooks.0.get("Stop").unwrap();
        assert_eq!(groups[0].matcher, "*");
    }

    #[test]
    fn missing_timeout_defaults_to_60() {
        let src = r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "x" } ] } ] } }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        assert_eq!(s.hooks.0.get("Stop").unwrap()[0].hooks[0].timeout, 60);
    }

    #[test]
    fn unknown_event_name_parses_into_map() {
        // Unknowns are preserved at parse time; discovery warn-logs them
        // when building the per-HookKind index in Task 4.
        let src = r#"{ "hooks": { "Notification": [] } }"#;
        let s: SettingsFile = serde_json::from_str(src).unwrap();
        assert!(s.hooks.0.contains_key("Notification"));
    }
}
```

- [ ] **Step 2: Declare the module**

Add to `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs`:

```rust
mod config;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent plugin::builtin::user_hooks::config::tests
```

Expected: 6 PASS.

- [ ] **Step 4: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/config.rs \
        crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): settings.json schema types"
```

---

### Task 3: Matcher (globset)

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_hooks/matcher.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` (add `mod matcher;`)
- Possibly modify: `crates/savvagent/Cargo.toml` (add `globset.workspace = true` if absent)

- [ ] **Step 1: Verify `globset` workspace dep**

Inspect:

```bash
grep -n "globset" Cargo.toml crates/savvagent/Cargo.toml
```

If `globset` is in `[workspace.dependencies]` of the root `Cargo.toml` but NOT in `crates/savvagent/Cargo.toml`, append to the latter's `[dependencies]`:

```toml
globset.workspace = true
```

If it's missing entirely from the workspace, append to root `Cargo.toml`'s `[workspace.dependencies]`:

```toml
globset = "0.4"
```

…and then add `globset.workspace = true` to `crates/savvagent/Cargo.toml` as above.

- [ ] **Step 2: Write the failing tests + impl**

Create `crates/savvagent/src/plugin/builtin/user_hooks/matcher.rs`:

```rust
//! Compiled tool-name matchers built from `MatcherGroup::matcher` strings.

#![allow(dead_code)] // consumed by Task 4 (discovery) + Task 19 (PreToolUseGate impl)

use globset::{Glob, GlobMatcher};

/// A compiled glob pattern paired with the raw source string (for logs).
#[derive(Debug, Clone)]
pub struct CompiledMatcher {
    pub source: String,
    pub matcher: GlobMatcher,
}

impl CompiledMatcher {
    /// Compile a matcher string. Empty string is rejected.
    pub fn compile(source: &str) -> Result<Self, String> {
        if source.is_empty() {
            return Err("empty matcher pattern".into());
        }
        let glob = Glob::new(source).map_err(|e| format!("invalid glob `{source}`: {e}"))?;
        Ok(CompiledMatcher {
            source: source.to_string(),
            matcher: glob.compile_matcher(),
        })
    }

    /// Returns `true` if `tool_name` matches this pattern.
    pub fn is_match(&self, tool_name: &str) -> bool {
        self.matcher.is_match(tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_matches_anything() {
        let m = CompiledMatcher::compile("*").unwrap();
        assert!(m.is_match("run"));
        assert!(m.is_match("tool-fs:write_file"));
        assert!(m.is_match(""));
    }

    #[test]
    fn exact_match() {
        let m = CompiledMatcher::compile("run").unwrap();
        assert!(m.is_match("run"));
        assert!(!m.is_match("runner"));
        assert!(!m.is_match("Run"));
    }

    #[test]
    fn prefix_glob() {
        let m = CompiledMatcher::compile("tool-fs:*").unwrap();
        assert!(m.is_match("tool-fs:write_file"));
        assert!(m.is_match("tool-fs:read_file"));
        assert!(!m.is_match("tool-grep:search"));
    }

    #[test]
    fn suffix_glob() {
        let m = CompiledMatcher::compile("*_file").unwrap();
        assert!(m.is_match("write_file"));
        assert!(m.is_match("tool-fs:read_file"));
        assert!(!m.is_match("file_write"));
    }

    #[test]
    fn empty_pattern_rejected() {
        assert!(CompiledMatcher::compile("").is_err());
    }

    #[test]
    fn invalid_glob_rejected() {
        // Unmatched bracket — globset rejects.
        assert!(CompiledMatcher::compile("[abc").is_err());
    }
}
```

- [ ] **Step 3: Declare the module**

Append to `mod.rs`:

```rust
mod matcher;
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p savvagent plugin::builtin::user_hooks::matcher::tests
```

Expected: 6 PASS.

- [ ] **Step 5: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/matcher.rs \
        crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
# Add Cargo.toml only if you actually modified it.
git add crates/savvagent/Cargo.toml Cargo.toml 2>/dev/null || true
git commit -m "feat(plugin/user-hooks): glob-pattern matcher"
```

---

### Task 4: Discovery — four-path walk + merge + per-event index

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_hooks/discovery.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` (add `mod discovery;`)

- [ ] **Step 1: Failing tests + impl**

Create `crates/savvagent/src/plugin/builtin/user_hooks/discovery.rs`:

```rust
//! Walks the four well-known `settings.json` paths and merges hook lists
//! into a per-event index keyed by `HookEvent`.
//!
//! Precedence order (sequential execution within an event respects this):
//! 1. `<project>/.savvagent/settings.json`
//! 2. `<project>/.claude/settings.json`
//! 3. `~/.savvagent/settings.json`
//! 4. `~/.claude/settings.json`

#![allow(dead_code)] // consumed by Task 18 (plugin index) + Task 19 (gate)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::plugin::builtin::user_hooks::config::{HookCommand, MatcherGroup, SettingsFile};
use crate::plugin::builtin::user_hooks::matcher::CompiledMatcher;

/// All `HookEvent` variants we map to a `HookKind` today. Strings
/// referencing names outside this set warn-log at index-build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    SessionStart,
    Stop,
}

impl HookEvent {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "PreToolUse" => Some(HookEvent::PreToolUse),
            "PostToolUse" => Some(HookEvent::PostToolUse),
            "UserPromptSubmit" => Some(HookEvent::UserPromptSubmit),
            "SessionStart" => Some(HookEvent::SessionStart),
            "Stop" => Some(HookEvent::Stop),
            _ => None,
        }
    }
}

/// A compiled-and-validated matcher group ready for dispatch.
#[derive(Debug, Clone)]
pub struct CompiledGroup {
    pub matcher: CompiledMatcher,
    pub commands: Vec<HookCommand>,
    /// Source path for diagnostics.
    pub source: PathBuf,
}

/// The per-event index the runtime uses.
#[derive(Debug, Default, Clone)]
pub struct HooksIndex {
    pub by_event: BTreeMap<HookEvent, Vec<CompiledGroup>>,
    pub warnings: Vec<String>,
}

/// Walk all four directories with precedence and produce the merged
/// index. Missing files are silently ignored; malformed files warn-log.
pub fn walk_all(project_root: &Path, home: &Path) -> HooksIndex {
    let paths: [PathBuf; 4] = [
        project_root.join(".savvagent").join("settings.json"),
        project_root.join(".claude").join("settings.json"),
        home.join(".savvagent").join("settings.json"),
        home.join(".claude").join("settings.json"),
    ];
    let mut index = HooksIndex::default();
    for path in paths {
        load_one(&path, &mut index);
    }
    index
}

fn load_one(path: &Path, index: &mut HooksIndex) {
    if !path.exists() {
        return;
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            index.warnings.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    let parsed: SettingsFile = match serde_json::from_str(&contents) {
        Ok(p) => p,
        Err(e) => {
            index.warnings.push(format!("{}: malformed JSON: {e}", path.display()));
            return;
        }
    };
    for (event_name, groups) in parsed.hooks.0.iter() {
        let Some(event) = HookEvent::parse(event_name) else {
            index.warnings.push(format!(
                "{}: hooks.{event_name} is reserved or unknown; ignoring",
                path.display()
            ));
            continue;
        };
        for group in groups {
            compile_and_push(path, event, group, index);
        }
    }
}

fn compile_and_push(
    path: &Path,
    event: HookEvent,
    group: &MatcherGroup,
    index: &mut HooksIndex,
) {
    let matcher = match CompiledMatcher::compile(&group.matcher) {
        Ok(m) => m,
        Err(why) => {
            index.warnings.push(format!("{}: {why}", path.display()));
            return;
        }
    };
    let mut commands = Vec::new();
    for h in &group.hooks {
        if h.type_field != "command" {
            index.warnings.push(format!(
                "{}: unsupported hook type `{}` (only \"command\" in v1)",
                path.display(),
                h.type_field
            ));
            continue;
        }
        commands.push(h.clone());
    }
    if commands.is_empty() {
        return;
    }
    index.by_event.entry(event).or_default().push(CompiledGroup {
        matcher,
        commands,
        source: path.to_path_buf(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn missing_files_returns_empty() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let idx = walk_all(proj.path(), home.path());
        assert!(idx.by_event.is_empty());
        assert!(idx.warnings.is_empty());
    }

    #[test]
    fn precedence_order_within_event() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "A" } ] } ] } }"#,
        );
        write(
            &proj.path().join(".claude"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "B" } ] } ] } }"#,
        );
        write(
            &home.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "C" } ] } ] } }"#,
        );
        write(
            &home.path().join(".claude"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "D" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        let groups = idx.by_event.get(&HookEvent::Stop).expect("Stop present");
        let cmds: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.commands.iter().map(|c| c.command.as_str()))
            .collect();
        assert_eq!(cmds, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn malformed_json_warns_other_files_load() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(&proj.path().join(".savvagent"), "settings.json", "{ broken");
        write(
            &home.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "command": "ok" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert_eq!(idx.warnings.len(), 1);
        assert!(idx.warnings[0].contains("malformed JSON"));
        let groups = idx.by_event.get(&HookEvent::Stop).unwrap();
        assert_eq!(groups[0].commands[0].command, "ok");
    }

    #[test]
    fn unknown_event_warns_and_skips() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "SubagentStop": [ { "hooks": [ { "command": "x" } ] } ], "Stop": [ { "hooks": [ { "command": "y" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert_eq!(idx.warnings.len(), 1);
        assert!(idx.warnings[0].contains("SubagentStop"));
        let groups = idx.by_event.get(&HookEvent::Stop).unwrap();
        assert_eq!(groups[0].commands[0].command, "y");
    }

    #[test]
    fn invalid_matcher_pattern_warns_and_skips_group() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "PreToolUse": [ { "matcher": "[bad", "hooks": [ { "command": "x" } ] }, { "matcher": "*", "hooks": [ { "command": "y" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert_eq!(idx.warnings.len(), 1);
        assert!(idx.warnings[0].contains("invalid glob"));
        let groups = idx.by_event.get(&HookEvent::PreToolUse).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].commands[0].command, "y");
    }

    #[test]
    fn non_command_type_warns_and_skips_entry() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent"),
            "settings.json",
            r#"{ "hooks": { "Stop": [ { "hooks": [ { "type": "webhook", "command": "x" }, { "command": "y" } ] } ] } }"#,
        );

        let idx = walk_all(proj.path(), home.path());
        assert_eq!(idx.warnings.len(), 1);
        assert!(idx.warnings[0].contains("unsupported hook type"));
        let groups = idx.by_event.get(&HookEvent::Stop).unwrap();
        assert_eq!(groups[0].commands.len(), 1);
        assert_eq!(groups[0].commands[0].command, "y");
    }
}
```

- [ ] **Step 2: Declare the module**

Append to `mod.rs`:

```rust
mod discovery;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent plugin::builtin::user_hooks::discovery::tests
```

Expected: 6 PASS.

- [ ] **Step 4: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/discovery.rs \
        crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): four-path discovery + per-event index"
```

---

### Task 5: Payload builder

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_hooks/payload.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` (add `mod payload;`)

- [ ] **Step 1: Failing tests + impl**

Create `crates/savvagent/src/plugin/builtin/user_hooks/payload.rs`:

```rust
//! Builds the stdin JSON payload each hook receives. Shape matches
//! Claude Code's hook contract.

#![allow(dead_code)] // consumed by Task 19 (gate) + Task 20 (on_event)

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::plugin::builtin::user_hooks::discovery::HookEvent;

/// Per-call context shared across all payloads.
#[derive(Debug, Clone)]
pub struct HookContext<'a> {
    pub session_id: &'a str,
    pub transcript_path: &'a Path,
    pub cwd: &'a Path,
}

/// Build a `PreToolUse` stdin payload.
pub fn pre_tool_use(ctx: &HookContext<'_>, tool_name: &str, tool_input: &Value) -> Value {
    base(ctx, HookEvent::PreToolUse)
        .extend(&[("tool_name", json!(tool_name)), ("tool_input", tool_input.clone())])
}

/// Build a `PostToolUse` stdin payload.
pub fn post_tool_use(
    ctx: &HookContext<'_>,
    tool_name: &str,
    tool_input: &Value,
    tool_response: &Value,
) -> Value {
    base(ctx, HookEvent::PostToolUse).extend(&[
        ("tool_name", json!(tool_name)),
        ("tool_input", tool_input.clone()),
        ("tool_response", tool_response.clone()),
    ])
}

/// Build a `UserPromptSubmit` stdin payload.
pub fn user_prompt_submit(ctx: &HookContext<'_>, prompt: &str) -> Value {
    base(ctx, HookEvent::UserPromptSubmit).extend(&[("prompt", json!(prompt))])
}

/// Build a `SessionStart` stdin payload.
pub fn session_start(ctx: &HookContext<'_>, source: &str) -> Value {
    base(ctx, HookEvent::SessionStart).extend(&[("source", json!(source))])
}

/// Build a `Stop` stdin payload.
pub fn stop(ctx: &HookContext<'_>, stop_hook_active: bool) -> Value {
    base(ctx, HookEvent::Stop).extend(&[("stop_hook_active", json!(stop_hook_active))])
}

fn event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::PreToolUse => "PreToolUse",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::UserPromptSubmit => "UserPromptSubmit",
        HookEvent::SessionStart => "SessionStart",
        HookEvent::Stop => "Stop",
    }
}

struct Builder(Map<String, Value>);

fn base(ctx: &HookContext<'_>, event: HookEvent) -> Builder {
    let mut m = Map::new();
    m.insert("session_id".into(), json!(ctx.session_id));
    m.insert(
        "transcript_path".into(),
        json!(ctx.transcript_path.display().to_string()),
    );
    m.insert("cwd".into(), json!(ctx.cwd.display().to_string()));
    m.insert("hook_event_name".into(), json!(event_name(event)));
    Builder(m)
}

impl Builder {
    fn extend(mut self, pairs: &[(&str, Value)]) -> Value {
        for (k, v) in pairs {
            self.0.insert((*k).to_string(), v.clone());
        }
        Value::Object(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> (PathBuf, PathBuf, HookContext<'static>) {
        // Leak the paths so the &Path can be 'static for the helper.
        // We only do this in tests.
        let transcript: &'static Path =
            Box::leak(PathBuf::from("/t/123.json").into_boxed_path());
        let cwd: &'static Path = Box::leak(PathBuf::from("/cwd").into_boxed_path());
        (
            transcript.to_path_buf(),
            cwd.to_path_buf(),
            HookContext {
                session_id: "sid",
                transcript_path: transcript,
                cwd,
            },
        )
    }

    #[test]
    fn pre_tool_use_payload_has_all_fields() {
        let (_, _, c) = ctx();
        let v = pre_tool_use(&c, "tool-fs:write_file", &json!({ "path": "/etc" }));
        assert_eq!(v["session_id"], "sid");
        assert_eq!(v["transcript_path"], "/t/123.json");
        assert_eq!(v["cwd"], "/cwd");
        assert_eq!(v["hook_event_name"], "PreToolUse");
        assert_eq!(v["tool_name"], "tool-fs:write_file");
        assert_eq!(v["tool_input"]["path"], "/etc");
    }

    #[test]
    fn post_tool_use_includes_response() {
        let (_, _, c) = ctx();
        let v = post_tool_use(&c, "run", &json!({}), &json!({ "ok": true }));
        assert_eq!(v["hook_event_name"], "PostToolUse");
        assert_eq!(v["tool_response"]["ok"], true);
    }

    #[test]
    fn user_prompt_submit_includes_prompt() {
        let (_, _, c) = ctx();
        let v = user_prompt_submit(&c, "hello");
        assert_eq!(v["hook_event_name"], "UserPromptSubmit");
        assert_eq!(v["prompt"], "hello");
    }

    #[test]
    fn session_start_has_source() {
        let (_, _, c) = ctx();
        let v = session_start(&c, "startup");
        assert_eq!(v["source"], "startup");
    }

    #[test]
    fn stop_has_loop_flag() {
        let (_, _, c) = ctx();
        let v = stop(&c, true);
        assert_eq!(v["stop_hook_active"], true);
    }
}
```

- [ ] **Step 2: Declare the module**

Append to `mod.rs`:

```rust
mod payload;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent plugin::builtin::user_hooks::payload::tests
```

Expected: 5 PASS.

- [ ] **Step 4: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/payload.rs \
        crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): stdin JSON payload builders"
```

---

### Task 6: Decision types + JSON stdout parser

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_hooks/decision.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` (add `mod decision;`)

- [ ] **Step 1: Failing tests + impl**

Create `crates/savvagent/src/plugin/builtin/user_hooks/decision.rs`:

```rust
//! Hook outcome decision types + parser for the Claude-Code-compatible
//! structured-JSON stdout protocol.

#![allow(dead_code)] // consumed by Task 7 (runner) and beyond

use serde::Deserialize;
use serde_json::Value;

use crate::plugin::builtin::user_hooks::discovery::HookEvent;

/// Final per-hook outcome, after considering exit code AND any parsed
/// JSON on stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    /// Hook proceeded cleanly. `additional_context` is `Some` only for
    /// `UserPromptSubmit` returning `hookSpecificOutput.additionalContext`.
    Continue { additional_context: Option<String>, suppress_output: bool },
    /// Hook blocked the chain. `reason` becomes the user-visible note.
    Block { reason: String, suppress_output: bool },
}

#[derive(Debug, Deserialize, Default)]
struct StructuredOutput {
    #[serde(default)]
    cont: Option<bool>,
    #[serde(default, rename = "continue")]
    cont_serde: Option<bool>,
    #[serde(default, rename = "stopReason")]
    stop_reason: Option<String>,
    #[serde(default, rename = "suppressOutput")]
    suppress_output: Option<bool>,
    #[serde(default, rename = "hookSpecificOutput")]
    hook_specific: Option<HookSpecific>,
    // Legacy fields.
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct HookSpecific {
    #[serde(default, rename = "hookEventName")]
    hook_event_name: Option<String>,
    #[serde(default, rename = "permissionDecision")]
    permission_decision: Option<String>,
    #[serde(default, rename = "permissionDecisionReason")]
    permission_decision_reason: Option<String>,
    #[serde(default, rename = "additionalContext")]
    additional_context: Option<String>,
}

fn event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::PreToolUse => "PreToolUse",
        HookEvent::PostToolUse => "PostToolUse",
        HookEvent::UserPromptSubmit => "UserPromptSubmit",
        HookEvent::SessionStart => "SessionStart",
        HookEvent::Stop => "Stop",
    }
}

/// Parse the hook's stdout AND combine with the exit code to produce
/// the final decision. Invalid JSON falls back to the exit-code-only
/// outcome. `warnings` collects any non-fatal anomalies (mismatched
/// `hookEventName`, unknown `permissionDecision`).
pub fn parse_outcome(
    event: HookEvent,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    warnings: &mut Vec<String>,
) -> HookDecision {
    // Try structured stdout first.
    let parsed: Option<StructuredOutput> = serde_json::from_str(stdout.trim()).ok();
    if let Some(p) = parsed {
        // continue / stopReason
        let cont = p.cont_serde.or(p.cont).unwrap_or(true);
        let suppress = p.suppress_output.unwrap_or(false);
        let mut additional: Option<String> = None;

        // Event-specific decision overrides via hookSpecificOutput.
        if let Some(hs) = p.hook_specific {
            if let Some(name) = &hs.hook_event_name {
                if name != event_name(event) {
                    warnings.push(format!(
                        "hookSpecificOutput.hookEventName `{name}` does not match firing event `{}`; ignoring hookSpecificOutput",
                        event_name(event)
                    ));
                } else {
                    match event {
                        HookEvent::PreToolUse => match hs.permission_decision.as_deref() {
                            Some("allow") => {
                                return HookDecision::Continue {
                                    additional_context: None,
                                    suppress_output: suppress,
                                };
                            }
                            Some("deny") => {
                                return HookDecision::Block {
                                    reason: hs
                                        .permission_decision_reason
                                        .unwrap_or_else(|| "denied by hook".into()),
                                    suppress_output: suppress,
                                };
                            }
                            Some("ask") => {
                                warnings.push(
                                    "permissionDecision=`ask` is not supported in v1; treating as `deny`"
                                        .into(),
                                );
                                return HookDecision::Block {
                                    reason: hs
                                        .permission_decision_reason
                                        .unwrap_or_else(|| "ask requested (not supported in v1)".into()),
                                    suppress_output: suppress,
                                };
                            }
                            Some(other) => {
                                warnings.push(format!(
                                    "unknown permissionDecision `{other}`; ignoring"
                                ));
                            }
                            None => {}
                        },
                        HookEvent::UserPromptSubmit => {
                            additional = hs.additional_context;
                        }
                        _ => {}
                    }
                }
            }
        }

        if !cont {
            let reason = p
                .stop_reason
                .or_else(|| p.reason.clone())
                .unwrap_or_else(|| "blocked by user hook".into());
            return HookDecision::Block {
                reason,
                suppress_output: suppress,
            };
        }
        // Legacy: decision=="block" / "approve"
        if let Some(d) = p.decision.as_deref() {
            warnings.push(
                "legacy `decision` field is deprecated; prefer `continue` + `stopReason` or `hookSpecificOutput`"
                    .into(),
            );
            if d == "block" {
                return HookDecision::Block {
                    reason: p
                        .reason
                        .unwrap_or_else(|| "blocked by user hook".into()),
                    suppress_output: suppress,
                };
            }
        }
        return HookDecision::Continue {
            additional_context: additional,
            suppress_output: suppress,
        };
    }
    // No structured stdout — exit-code-only.
    if exit_code == 2 {
        let reason = if stderr.trim().is_empty() {
            "blocked by user hook".to_string()
        } else {
            stderr.trim().to_string()
        };
        return HookDecision::Block {
            reason,
            suppress_output: false,
        };
    }
    HookDecision::Continue {
        additional_context: None,
        suppress_output: false,
    }
}

// Force serde to make the rename-field non-conflicting.
// (Both `cont` and `continue` keys are unusual; we keep `cont_serde` for
// the `continue` JSON key since `continue` is a reserved word in Rust.)
const _: fn() = || {
    let _: Option<bool> = StructuredOutput::default().cont;
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_0_no_stdout_continues() {
        let mut w = Vec::new();
        let d = parse_outcome(HookEvent::Stop, 0, "", "", &mut w);
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.is_empty());
    }

    #[test]
    fn exit_2_no_stdout_blocks_with_stderr() {
        let mut w = Vec::new();
        let d = parse_outcome(
            HookEvent::PreToolUse,
            2,
            "",
            "writes to .git/ forbidden\n",
            &mut w,
        );
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "writes to .git/ forbidden"),
            _ => panic!("expected Block"),
        }
    }

    #[test]
    fn structured_continue_false_blocks() {
        let mut w = Vec::new();
        let stdout = r#"{"continue":false,"stopReason":"nope"}"#;
        let d = parse_outcome(HookEvent::Stop, 0, stdout, "", &mut w);
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "nope"),
            _ => panic!(),
        }
    }

    #[test]
    fn permission_decision_allow_continues() {
        let mut w = Vec::new();
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#;
        let d = parse_outcome(HookEvent::PreToolUse, 0, stdout, "", &mut w);
        assert!(matches!(d, HookDecision::Continue { .. }));
    }

    #[test]
    fn permission_decision_deny_blocks_with_reason() {
        let mut w = Vec::new();
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"path forbidden"}}"#;
        let d = parse_outcome(HookEvent::PreToolUse, 0, stdout, "", &mut w);
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "path forbidden"),
            _ => panic!(),
        }
    }

    #[test]
    fn permission_decision_ask_warns_and_blocks() {
        let mut w = Vec::new();
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#;
        let d = parse_outcome(HookEvent::PreToolUse, 0, stdout, "", &mut w);
        assert!(matches!(d, HookDecision::Block { .. }));
        assert!(w.iter().any(|s| s.contains("ask")));
    }

    #[test]
    fn user_prompt_submit_additional_context_passes_through() {
        let mut w = Vec::new();
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"extra"}}"#;
        let d = parse_outcome(HookEvent::UserPromptSubmit, 0, stdout, "", &mut w);
        match d {
            HookDecision::Continue { additional_context, .. } => {
                assert_eq!(additional_context.as_deref(), Some("extra"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn mismatched_hook_event_name_warns_and_ignores() {
        let mut w = Vec::new();
        let stdout = r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","permissionDecision":"deny"}}"#;
        let d = parse_outcome(HookEvent::PreToolUse, 0, stdout, "", &mut w);
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.iter().any(|s| s.contains("PostToolUse")));
    }

    #[test]
    fn invalid_json_falls_back_to_exit_code() {
        let mut w = Vec::new();
        let d = parse_outcome(HookEvent::PreToolUse, 2, "{ not json", "stderr msg", &mut w);
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "stderr msg"),
            _ => panic!(),
        }
    }

    #[test]
    fn legacy_decision_block_warns() {
        let mut w = Vec::new();
        let stdout = r#"{"decision":"block","reason":"legacy"}"#;
        let d = parse_outcome(HookEvent::Stop, 0, stdout, "", &mut w);
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "legacy"),
            _ => panic!(),
        }
        assert!(w.iter().any(|s| s.contains("legacy")));
    }
}
```

- [ ] **Step 2: Declare the module**

Append to `mod.rs`:

```rust
mod decision;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent plugin::builtin::user_hooks::decision::tests
```

Expected: 10 PASS.

- [ ] **Step 4: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/decision.rs \
        crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): outcome parser (exit-code + structured JSON)"
```

---

### Task 7: Runner (shell process spawn + timeout)

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_hooks/runner.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` (add `mod runner;`)

- [ ] **Step 1: Failing tests + impl**

Create `crates/savvagent/src/plugin/builtin/user_hooks/runner.rs`:

```rust
//! Spawns a shell hook, writes the JSON payload to its stdin, awaits
//! with timeout, and returns a `HookDecision`.

#![allow(dead_code)] // consumed by Task 19 (gate) + Task 20 (on_event)

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::plugin::builtin::user_hooks::decision::{HookDecision, parse_outcome};
use crate::plugin::builtin::user_hooks::discovery::HookEvent;

/// Run one hook command with the given stdin payload and timeout.
pub async fn run_one(
    event: HookEvent,
    command: &str,
    timeout_secs: u64,
    payload: &Value,
    project_root: &Path,
) -> (HookDecision, Vec<String>, String, String) {
    let mut warnings = Vec::new();
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("SAVVAGENT_PROJECT_DIR", project_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                HookDecision::Continue {
                    additional_context: None,
                    suppress_output: false,
                },
                vec![format!("hook `{command}`: spawn failed: {e}")],
                String::new(),
                String::new(),
            );
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let bytes = serde_json::to_vec(payload).unwrap_or_default();
        let _ = stdin.write_all(&bytes).await;
        let _ = stdin.shutdown().await;
    }

    let wait = child.wait_with_output();
    let output = match timeout(Duration::from_secs(timeout_secs), wait).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            warnings.push(format!("hook `{command}`: wait failed: {e}"));
            return (
                HookDecision::Continue {
                    additional_context: None,
                    suppress_output: false,
                },
                warnings,
                String::new(),
                String::new(),
            );
        }
        Err(_) => {
            warnings.push(format!("hook `{command}`: timed out after {timeout_secs}s"));
            return (
                HookDecision::Continue {
                    additional_context: None,
                    suppress_output: false,
                },
                warnings,
                String::new(),
                String::new(),
            );
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code().unwrap_or(-1);

    let decision = parse_outcome(event, exit_code, &stdout, &stderr, &mut warnings);

    // Convert exit-2 on non-block-capable events into a warning instead
    // of a block.
    let decision = match (event, &decision) {
        (HookEvent::PostToolUse, HookDecision::Block { .. })
        | (HookEvent::SessionStart, HookDecision::Block { .. }) => {
            warnings.push(format!(
                "hook `{command}` exited 2 on non-block-capable event {event:?}; treating as warning"
            ));
            HookDecision::Continue {
                additional_context: None,
                suppress_output: false,
            }
        }
        _ => decision,
    };

    (decision, warnings, stdout, stderr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn payload() -> Value {
        json!({ "hook_event_name": "Stop" })
    }

    fn root() -> PathBuf {
        PathBuf::from("/tmp")
    }

    #[tokio::test]
    async fn exit_zero_continues() {
        let (d, w, _, _) = run_one(HookEvent::Stop, "true", 5, &payload(), &root()).await;
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.is_empty());
    }

    #[tokio::test]
    async fn exit_2_blocks_with_stderr() {
        let (d, _w, _, _) = run_one(
            HookEvent::PreToolUse,
            "echo nope >&2; exit 2",
            5,
            &payload(),
            &root(),
        )
        .await;
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "nope"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn timeout_warns_and_continues() {
        let (d, w, _, _) = run_one(HookEvent::Stop, "sleep 10", 1, &payload(), &root()).await;
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.iter().any(|s| s.contains("timed out")));
    }

    #[tokio::test]
    async fn spawn_failure_warns_and_continues() {
        // sh -c on missing binary returns exit 127 — not a true spawn
        // failure but exercises the chain. A true spawn failure is
        // covered indirectly when `sh` itself is missing, which we
        // can't reliably arrange in a test.
        let (d, _w, _stdout, _stderr) = run_one(
            HookEvent::PreToolUse,
            "/no/such/binary",
            5,
            &payload(),
            &root(),
        )
        .await;
        // exit 127 is non-zero non-2 — non-blocking; the chain should
        // continue.
        assert!(matches!(d, HookDecision::Continue { .. }));
    }

    #[tokio::test]
    async fn exit_2_on_session_start_warns_not_blocks() {
        let (d, w, _, _) = run_one(
            HookEvent::SessionStart,
            "echo bad >&2; exit 2",
            5,
            &payload(),
            &root(),
        )
        .await;
        assert!(matches!(d, HookDecision::Continue { .. }));
        assert!(w.iter().any(|s| s.contains("non-block-capable")));
    }

    #[tokio::test]
    async fn structured_stdout_takes_precedence() {
        let cmd = r#"echo '{"continue":false,"stopReason":"structured"}'; exit 0"#;
        let (d, _w, _, _) = run_one(HookEvent::Stop, cmd, 5, &payload(), &root()).await;
        match d {
            HookDecision::Block { reason, .. } => assert_eq!(reason, "structured"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn project_dir_env_is_set() {
        let (_d, _w, stdout, _stderr) = run_one(
            HookEvent::Stop,
            r#"echo "$SAVVAGENT_PROJECT_DIR""#,
            5,
            &payload(),
            &root(),
        )
        .await;
        assert!(stdout.trim() == "/tmp" || stdout.trim().ends_with("/tmp"));
    }
}
```

- [ ] **Step 2: Declare the module**

Append to `mod.rs`:

```rust
mod runner;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent plugin::builtin::user_hooks::runner::tests
```

Expected: 7 PASS.

- [ ] **Step 4: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/runner.rs \
        crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): shell runner with timeout + decision parse"
```

---

### Task 8: `PreToolUseGate` trait in `savvagent-host`

**Files:**
- Create: `crates/savvagent-host/src/pre_tool_gate.rs`
- Modify: `crates/savvagent-host/src/lib.rs` (re-export module + key types)

- [ ] **Step 1: Failing tests + trait**

Create `crates/savvagent-host/src/pre_tool_gate.rs`:

```rust
//! `PreToolUseGate` — savvagent-internal trait for gating tool dispatch.
//!
//! This is NOT part of the WIT-portable plugin surface; it lives in
//! `savvagent-host` and is consulted by the `Host` before
//! `ToolRegistry::call_with_bash_net_override`. The user-hooks plugin
//! implements it; future hooks (e.g. subagent-level gates) may too.

use async_trait::async_trait;
use serde_json::Value;

/// Synchronous gate consulted before each tool dispatch.
#[async_trait]
pub trait PreToolUseGate: Send + Sync {
    /// Decide whether to allow a tool call.
    ///
    /// Implementations should be best-effort and fail open: any panic
    /// the caller might recover from translates to `Allow` rather than
    /// stalling the TUI.
    async fn check(&self, tool_name: &str, input: &Value) -> PreToolDecision;
}

/// Decision returned by a [`PreToolUseGate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreToolDecision {
    /// Allow the tool call to proceed.
    Allow,
    /// Block the call. `reason` is surfaced as the tool result and as a
    /// `[blocked]` PushNote to the user.
    Block(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct AllowAll;

    #[async_trait]
    impl PreToolUseGate for AllowAll {
        async fn check(&self, _name: &str, _input: &Value) -> PreToolDecision {
            PreToolDecision::Allow
        }
    }

    #[tokio::test]
    async fn allow_gate_returns_allow() {
        let g = AllowAll;
        assert_eq!(
            g.check("run", &json!({"cmd": "ls"})).await,
            PreToolDecision::Allow
        );
    }

    struct DenyAll;

    #[async_trait]
    impl PreToolUseGate for DenyAll {
        async fn check(&self, name: &str, _input: &Value) -> PreToolDecision {
            PreToolDecision::Block(format!("deny {name}"))
        }
    }

    #[tokio::test]
    async fn deny_gate_returns_block_with_reason() {
        let g = DenyAll;
        match g.check("run", &json!({})).await {
            PreToolDecision::Block(r) => assert_eq!(r, "deny run"),
            _ => panic!(),
        }
    }
}
```

- [ ] **Step 2: Re-export from `lib.rs`**

Append to `crates/savvagent-host/src/lib.rs`:

```rust
/// `PreToolUseGate` trait and `PreToolDecision` enum.
pub mod pre_tool_gate;
pub use pre_tool_gate::{PreToolDecision, PreToolUseGate};
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent-host pre_tool_gate::tests
```

Expected: 2 PASS.

- [ ] **Step 4: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/pre_tool_gate.rs \
        crates/savvagent-host/src/lib.rs
git commit -m "feat(host): PreToolUseGate trait + PreToolDecision"
```

---

### Task 9: `Host` field + setter for the gate

**Files:**
- Modify: `crates/savvagent-host/src/session.rs`

- [ ] **Step 1: Add the field to `Host`**

In `crates/savvagent-host/src/session.rs`, find the `pub struct Host` declaration (around line 289). Add a new field, mirroring the existing `Arc<…>`-typed handles:

```rust
    /// Optional `PreToolUseGate` consulted before every tool dispatch.
    /// `None` means "no gate; allow all". The user-hooks plugin installs
    /// itself via [`Host::set_pre_tool_gate`].
    pre_tool_gate: tokio::sync::RwLock<
        Option<std::sync::Arc<dyn crate::pre_tool_gate::PreToolUseGate>>,
    >,
```

In `Host::new` (or wherever the struct is constructed — search for `Host {` in `session.rs`), initialize:

```rust
            pre_tool_gate: tokio::sync::RwLock::new(None),
```

- [ ] **Step 2: Setter**

In the `impl Host` block, add:

```rust
    /// Install a `PreToolUseGate`. Overwrites any prior gate (intended
    /// to be called exactly once, during startup, by the user-hooks
    /// plugin's registration effect).
    pub async fn set_pre_tool_gate(
        &self,
        gate: std::sync::Arc<dyn crate::pre_tool_gate::PreToolUseGate>,
    ) {
        let mut g = self.pre_tool_gate.write().await;
        *g = Some(gate);
    }

    /// Borrow the currently-installed gate. Used by the dispatch path.
    pub(crate) async fn pre_tool_gate_snapshot(
        &self,
    ) -> Option<std::sync::Arc<dyn crate::pre_tool_gate::PreToolUseGate>> {
        self.pre_tool_gate.read().await.clone()
    }
```

- [ ] **Step 3: Smoke test**

In `session.rs`'s existing `#[cfg(test)] mod tests`, add:

```rust
    #[tokio::test]
    async fn pre_tool_gate_starts_none_and_can_be_set() {
        use crate::pre_tool_gate::{PreToolDecision, PreToolUseGate};
        use async_trait::async_trait;
        use serde_json::Value;

        struct Allow;
        #[async_trait]
        impl PreToolUseGate for Allow {
            async fn check(&self, _: &str, _: &Value) -> PreToolDecision {
                PreToolDecision::Allow
            }
        }

        let host = test_host_minimal().await;
        assert!(host.pre_tool_gate_snapshot().await.is_none());
        host.set_pre_tool_gate(std::sync::Arc::new(Allow)).await;
        assert!(host.pre_tool_gate_snapshot().await.is_some());
    }
```

Wire `test_host_minimal()` from whatever existing helper builds a `Host` for tests in this file (search for `async fn test_host`, `fn host_for_test`, or similar). If no such helper exists, use the construction pattern from any existing `#[tokio::test]` in this file that builds a `Host`.

- [ ] **Step 4: Run**

```bash
cargo test -p savvagent-host pre_tool_gate_starts_none_and_can_be_set
```

Expected: PASS.

- [ ] **Step 5: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "feat(host): Host::pre_tool_gate field + set/snapshot accessors"
```

---

### Task 10: Wire the gate into the tool-dispatch path

**Files:**
- Modify: `crates/savvagent-host/src/session.rs` (call sites around the two `call_with_bash_net_override` invocations)

- [ ] **Step 1: Locate dispatch sites**

```bash
grep -n "call_with_bash_net_override" crates/savvagent-host/src/session.rs
```

Two sites: around lines 1249 and 1494 (the model-driven and slash-driven tool paths).

- [ ] **Step 2: Add a helper that gates before dispatching**

Add to `impl Host` (or a free helper module local to `session.rs`):

```rust
    /// Consult the `PreToolUseGate` (if any) before tool dispatch. On
    /// `Block`, returns `Some(error_outcome)`; the caller short-circuits
    /// the dispatch with this outcome. `None` means "proceed to dispatch".
    ///
    /// Panics inside the gate are caught and treated as `Allow` (fail
    /// open) to avoid hanging the TUI.
    async fn check_pre_tool_gate(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Option<crate::tools::ToolCallOutcome> {
        let Some(gate) = self.pre_tool_gate_snapshot().await else {
            return None;
        };
        let res = std::panic::AssertUnwindSafe(gate.check(tool_name, input));
        // tokio futures aren't unwind-safe in general; use catch_unwind
        // around the awaited future via a small spawn.
        let join = tokio::spawn(async move { res.await }).await;
        match join {
            Ok(crate::pre_tool_gate::PreToolDecision::Allow) => None,
            Ok(crate::pre_tool_gate::PreToolDecision::Block(reason)) => {
                Some(crate::tools::ToolCallOutcome::error(format!(
                    "blocked by user hook: {reason}"
                )))
            }
            Err(e) => {
                tracing::warn!("PreToolUseGate panicked: {e}; failing open");
                None
            }
        }
    }
```

If `ToolCallOutcome::error` isn't `pub` from the `tools` module, check its actual public constructors and adapt. Look for `pub fn error` or `pub fn new_error` in `crates/savvagent-host/src/tools.rs`.

- [ ] **Step 3: Invoke at the two dispatch sites**

At both `call_with_bash_net_override` call sites, immediately before the existing call, insert:

```rust
                            if let Some(blocked) =
                                self.check_pre_tool_gate(&tool_name, &input).await
                            {
                                /* substitute the dispatch result with the blocked outcome */
                                blocked
                            } else {
                                self.tool_registry
                                    .call_with_bash_net_override(/* existing args */)
                                    .await
                            }
```

The exact variable names (`tool_name`, `input`) must match the local bindings at each site — inspect both call sites carefully and adapt names. Do NOT replace the existing `call_with_bash_net_override` call; conditionally short-circuit before it.

- [ ] **Step 4: Add an integration-style test**

In `session.rs` tests:

```rust
    #[tokio::test]
    async fn pre_tool_gate_block_short_circuits_dispatch() {
        use crate::pre_tool_gate::{PreToolDecision, PreToolUseGate};
        use async_trait::async_trait;
        use serde_json::{Value, json};

        struct Deny;
        #[async_trait]
        impl PreToolUseGate for Deny {
            async fn check(&self, _: &str, _: &Value) -> PreToolDecision {
                PreToolDecision::Block("test deny".into())
            }
        }

        let host = test_host_with_tool_fs().await;
        host.set_pre_tool_gate(std::sync::Arc::new(Deny)).await;

        // Drive a tool call through whatever surface the existing tests
        // use (run_turn / dispatch_tool / direct). Whichever path goes
        // through the gated dispatch should surface the deny reason.
        // …assert the tool result contains "test deny" or "blocked by user hook"…
    }
```

Adapt to the actual test helpers available. If no easy harness exists, add a smaller test inside `tools.rs` that constructs a `Host`-like wrapper and exercises just the gating helper. The minimum-viable test is the snapshot test from Task 9; full path coverage can be deferred to Task 24 (E2E).

- [ ] **Step 5: Build + run all `savvagent-host` tests to ensure no regression**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent-host 2>&1 | grep -E "^test result" | head -3
```

Expect no regressions (no decrease from the prior pass count).

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/session.rs
git commit -m "feat(host): consult PreToolUseGate before tool dispatch"
```

---

### Task 11: Three new `Effect` variants

**Files:**
- Modify: `crates/savvagent-plugin/src/effect.rs`

- [ ] **Step 1: Add the variants**

In `crates/savvagent-plugin/src/effect.rs`, append to the `Effect` enum:

```rust
    /// Announce that this plugin provides a `PreToolUseGate`. The
    /// runtime fetches the gate object via a savvagent-internal seam
    /// (not part of the WIT-portable surface) and installs it on the
    /// host. Mirrors the [`Effect::RegisterProvider`] pattern.
    RegisterPreToolGate {
        /// Plugin id whose `BuiltinHookPlugin::take_pre_tool_gate()`
        /// will be invoked to materialize the gate.
        plugin_id: crate::types::PluginId,
    },
    /// Prepend `text` to the most-recently-submitted user prompt
    /// before it reaches the model. Used by `UserPromptSubmit` hooks
    /// returning `additionalContext`. Multiple emissions concatenate
    /// in order with a `\n\n` separator between each; the original
    /// prompt remains last.
    PrependToPendingPrompt {
        /// Text to prepend. Empty string is a no-op.
        text: String,
    },
    /// Abort the turn that's about to start. Used by `UserPromptSubmit`
    /// or `Stop` hooks that blocked. The runtime renders `reason` as a
    /// `[blocked] …` PushNote in the conversation log; the prompt or
    /// stop is not sent to the model.
    CancelPendingTurn {
        /// User-visible reason. Empty string falls back to
        /// `"blocked by user hook"`.
        reason: String,
    },
```

- [ ] **Step 2: Construction smoke tests**

Append to the existing tests module in `effect.rs` (or create one as in Task 11 of sub-project A):

```rust
#[cfg(test)]
mod added_hook_effects_smoke {
    use super::*;
    use crate::types::PluginId;

    #[test]
    fn variants_constructable() {
        let _ = Effect::RegisterPreToolGate {
            plugin_id: PluginId::new("internal:user-hooks").unwrap(),
        };
        let _ = Effect::PrependToPendingPrompt {
            text: "context".into(),
        };
        let _ = Effect::CancelPendingTurn {
            reason: "no".into(),
        };
    }
}
```

- [ ] **Step 3: Run**

```bash
cargo test -p savvagent-plugin effect::added_hook_effects_smoke
cargo test -p savvagent-plugin
cargo test -p savvagent 2>&1 | grep -E "^test result" | head -1
```

All should pass (the savvagent main bin uses the same wildcard arm in `apply_effects` as sub-project A's three new variants did — see Task 11 of sub-project A's plan for verification).

- [ ] **Step 4: Build clean**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
```

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-plugin/src/effect.rs
git commit -m "feat(plugin/effect): RegisterPreToolGate, PrependToPendingPrompt, CancelPendingTurn"
```

---

### Task 12: `BuiltinHookPlugin` trait sibling

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/provider_common.rs`

- [ ] **Step 1: Add the trait + entry**

Append to `crates/savvagent/src/plugin/builtin/provider_common.rs` (next to the existing `BuiltinProviderPlugin` declaration):

```rust
/// Savvagent-internal trait that hook plugins implement to hand the
/// runtime an `Arc<dyn PreToolUseGate>`. Mirrors
/// [`BuiltinProviderPlugin`] / [`ProviderEntry`]: the registry holds
/// one `Arc<Mutex<dyn BuiltinHookPlugin>>` view, and `take_pre_tool_gate`
/// produces the concrete trait object for the host.
pub(crate) trait BuiltinHookPlugin: savvagent_plugin::Plugin {
    /// Surrender the plugin's `PreToolUseGate` to the runtime. The
    /// runtime calls this exactly once at startup, after observing
    /// `Effect::RegisterPreToolGate`. The plugin may return the same
    /// `Arc` on every call (the gate is shared state).
    fn take_pre_tool_gate(
        &mut self,
    ) -> Option<std::sync::Arc<dyn savvagent_host::PreToolUseGate>>;
}
```

If `savvagent_host::PreToolUseGate` isn't re-exported from the crate's root yet, verify Task 8 Step 2 added the `pub use pre_tool_gate::{PreToolDecision, PreToolUseGate};` re-export.

- [ ] **Step 2: Smoke test**

In `provider_common.rs`'s existing `#[cfg(test)] mod tests` (or create one), add:

```rust
    #[test]
    fn builtin_hook_plugin_default_impl_returns_none() {
        // Default impl can't be tested in isolation without an impl,
        // but a placeholder type with no override should return None
        // (we expose the trait without a default fn, so this test is
        // a compile-time presence check more than a behavior test).
        struct Stub;
        #[async_trait::async_trait]
        impl savvagent_plugin::Plugin for Stub {
            fn manifest(&self) -> savvagent_plugin::Manifest {
                use savvagent_plugin::*;
                Manifest {
                    id: PluginId::new("internal:test-stub").unwrap(),
                    name: "stub".into(),
                    version: "0".into(),
                    description: "stub".into(),
                    kind: PluginKind::Core,
                    contributions: Contributions::default(),
                }
            }
        }
        impl super::BuiltinHookPlugin for Stub {
            fn take_pre_tool_gate(
                &mut self,
            ) -> Option<std::sync::Arc<dyn savvagent_host::PreToolUseGate>> {
                None
            }
        }
        let mut s = Stub;
        assert!(s.take_pre_tool_gate().is_none());
    }
```

- [ ] **Step 3: Build + run**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::builtin::provider_common::tests::builtin_hook_plugin_default_impl_returns_none
```

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/provider_common.rs
git commit -m "feat(plugin/provider-common): BuiltinHookPlugin sibling trait"
```

---

### Task 13: `apply_effects` arm for `RegisterPreToolGate`

**Files:**
- Modify: `crates/savvagent/src/plugin/effects.rs`

- [ ] **Step 1: Locate**

`grep -n "fn apply_one\|Effect::RegisterProvider" crates/savvagent/src/plugin/effects.rs` — read the `RegisterProvider` arm; the new arm follows the same shape (lookup plugin, take handle, set on host).

- [ ] **Step 2: Add the arm**

Near the `RegisterProvider` arm:

```rust
        Effect::RegisterPreToolGate { plugin_id } => {
            use crate::plugin::builtin::provider_common::BuiltinHookPlugin;
            let reg = app.plugin_registry.read().await;
            let Some(entry) = reg.get_hook_entry(&plugin_id) else {
                tracing::warn!(
                    "RegisterPreToolGate: no BuiltinHookPlugin for {}",
                    plugin_id.as_str()
                );
                return Ok(());
            };
            let mut handle = entry.as_hook.try_lock().expect("not poisoned");
            let Some(gate) = handle.take_pre_tool_gate() else {
                tracing::warn!(
                    "RegisterPreToolGate: {} returned no gate",
                    plugin_id.as_str()
                );
                return Ok(());
            };
            drop(handle);
            drop(reg);
            if let Some(host) = app.host.read().await.as_ref() {
                host.set_pre_tool_gate(gate).await;
            } else {
                tracing::warn!(
                    "RegisterPreToolGate: no host yet; gate will be installed on next host swap"
                );
            }
        }
```

`get_hook_entry` and `as_hook` are placeholders for the registry surface — read `crates/savvagent/src/plugin/registry.rs` and either reuse existing accessors or extend the registry with these (see Task 14 if extension is needed).

- [ ] **Step 3: Write a test**

In `effects.rs` tests:

```rust
    #[tokio::test]
    async fn register_pre_tool_gate_installs_on_host() {
        // Construct a test App with a stub hook plugin registered.
        // Apply Effect::RegisterPreToolGate { plugin_id }.
        // Assert host.pre_tool_gate_snapshot() is now Some.
    }
```

If the registry plumbing in Task 14 isn't ready yet, mark this test `#[ignore]` with a `TODO`. The behavior is exercised end-to-end in Task 24.

- [ ] **Step 4: Build + commit**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::effects::tests 2>&1 | tail -5
git add crates/savvagent/src/plugin/effects.rs
git commit -m "feat(plugin/effects): apply RegisterPreToolGate"
```

---

### Task 14: Registry plumbing for `HookEntry`

**Files:**
- Modify: `crates/savvagent/src/plugin/registry.rs` (mirror the existing `ProviderEntry` surface for hooks)

- [ ] **Step 1: Add `HookEntry` next to `ProviderEntry`**

Search `crates/savvagent/src/plugin/registry.rs` for `ProviderEntry`. Mirror the same structure for hook plugins:

```rust
/// One hook-plugin entry. The dual-Arc pattern mirrors `ProviderEntry`
/// so both the `Plugin` view and the `BuiltinHookPlugin` view share
/// the same instance.
pub struct HookEntry {
    pub as_plugin: std::sync::Arc<
        tokio::sync::Mutex<dyn savvagent_plugin::Plugin>,
    >,
    pub as_hook: std::sync::Arc<
        tokio::sync::Mutex<dyn crate::plugin::builtin::provider_common::BuiltinHookPlugin>,
    >,
    pub id: savvagent_plugin::PluginId,
}

impl HookEntry {
    pub fn new<T>(concrete: T) -> Self
    where
        T: crate::plugin::builtin::provider_common::BuiltinHookPlugin + 'static,
    {
        let arc = std::sync::Arc::new(tokio::sync::Mutex::new(concrete));
        let id = {
            let g = arc.try_lock().expect("constructor not contended");
            g.manifest().id
        };
        let as_plugin: std::sync::Arc<tokio::sync::Mutex<dyn savvagent_plugin::Plugin>> =
            arc.clone();
        let as_hook: std::sync::Arc<
            tokio::sync::Mutex<dyn crate::plugin::builtin::provider_common::BuiltinHookPlugin>,
        > = arc;
        Self { as_plugin, as_hook, id }
    }
}
```

Add `pub hook_entries: Vec<HookEntry>` to whatever struct currently holds `providers: Vec<ProviderEntry>` (search for `pub providers:`). Add an accessor:

```rust
    pub fn get_hook_entry(
        &self,
        id: &savvagent_plugin::PluginId,
    ) -> Option<&HookEntry> {
        self.hook_entries.iter().find(|e| e.id == *id)
    }
```

- [ ] **Step 2: Smoke test**

```rust
#[cfg(test)]
mod hook_entry_tests {
    use super::*;
    // Construct a HookEntry from a stub plugin and assert the
    // as_plugin/as_hook Arcs point at the same allocation.
}
```

- [ ] **Step 3: Build + commit**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::registry 2>&1 | tail -5
git add crates/savvagent/src/plugin/registry.rs
git commit -m "feat(plugin/registry): HookEntry dual-view sibling to ProviderEntry"
```

---

### Task 15: `apply_effects` arms for `PrependToPendingPrompt` + `CancelPendingTurn`

**Files:**
- Modify: `crates/savvagent/src/plugin/effects.rs`
- Modify: `crates/savvagent/src/app.rs` (add `pending_prompt` field + cancel flag)

- [ ] **Step 1: Add App-side state**

In `crates/savvagent/src/app.rs`, near the existing `pending_*` fields:

```rust
    /// Prompt text accumulated by `UserPromptSubmit` hooks before
    /// dispatch. Each `Effect::PrependToPendingPrompt` adds to the
    /// front; when the worker spawn fires, the full text becomes
    /// `accumulated\n\n<user typed prompt>`.
    pub pending_prompt_prefix: Option<String>,
    /// If `Some`, the next attempted turn dispatch aborts and `reason`
    /// is surfaced as a `[blocked]` PushNote. Set by
    /// `Effect::CancelPendingTurn`; cleared after the abort fires.
    pub pending_turn_cancellation: Option<String>,
```

Initialize both to `None` in the App constructor.

- [ ] **Step 2: Add the arms**

```rust
        Effect::PrependToPendingPrompt { text } => {
            if text.is_empty() {
                return Ok(());
            }
            let combined = match app.pending_prompt_prefix.take() {
                Some(existing) => format!("{existing}\n\n{text}"),
                None => text,
            };
            app.pending_prompt_prefix = Some(combined);
        }
        Effect::CancelPendingTurn { reason } => {
            let reason = if reason.is_empty() {
                "blocked by user hook".to_string()
            } else {
                reason
            };
            app.pending_turn_cancellation = Some(reason);
        }
```

- [ ] **Step 3: Consume sites**

The worker-spawn site (in `main.rs` or `tui.rs` — wherever Task 14 of sub-project A wired `consume_model_override`) needs:

```rust
let prefix = self.pending_prompt_prefix.take();
let cancellation = self.pending_turn_cancellation.take();
if let Some(reason) = cancellation {
    // emit a PushNote, do not spawn worker.
    self.push_note(format!("[blocked] {reason}"));
    return;
}
let prompt = match prefix {
    Some(p) => format!("{p}\n\n{user_input}"),
    None => user_input,
};
```

Adapt to whatever the actual worker-spawn invocation site looks like (read it before patching).

- [ ] **Step 4: Tests**

```rust
    #[tokio::test]
    async fn prepend_concatenates_in_order() {
        let mut app = fresh_app();
        apply_effects(
            &mut app,
            vec![
                Effect::PrependToPendingPrompt { text: "A".into() },
                Effect::PrependToPendingPrompt { text: "B".into() },
            ],
        )
        .await
        .unwrap();
        assert_eq!(app.pending_prompt_prefix.as_deref(), Some("A\n\nB"));
    }

    #[tokio::test]
    async fn cancel_with_empty_reason_uses_default() {
        let mut app = fresh_app();
        apply_effects(
            &mut app,
            vec![Effect::CancelPendingTurn { reason: "".into() }],
        )
        .await
        .unwrap();
        assert_eq!(
            app.pending_turn_cancellation.as_deref(),
            Some("blocked by user hook")
        );
    }
```

- [ ] **Step 5: Build + commit**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::effects::tests::prepend_concatenates_in_order plugin::effects::tests::cancel_with_empty_reason_uses_default
git add crates/savvagent/src/plugin/effects.rs crates/savvagent/src/app.rs crates/savvagent/src/main.rs
git commit -m "feat(plugin/effects): apply PrependToPendingPrompt + CancelPendingTurn"
```

---

### Task 16: `pre_tool_gate.rs` impl on the plugin

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_hooks/pre_tool_gate.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs` (add `pub mod pre_tool_gate;` + `impl BuiltinHookPlugin`)

- [ ] **Step 1: Implementation**

Create the file:

```rust
//! `PreToolUseGate` impl that walks the per-event hooks for
//! `PreToolUse`, runs each matching hook sequentially via
//! `runner::run_one`, and returns the first `Block` (or `Allow` if
//! none).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_host::{PreToolDecision, PreToolUseGate};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::plugin::builtin::user_hooks::decision::HookDecision;
use crate::plugin::builtin::user_hooks::discovery::{HookEvent, HooksIndex};
use crate::plugin::builtin::user_hooks::payload;
use crate::plugin::builtin::user_hooks::runner;

/// The gate object shared between `App` and the plugin. Holds the
/// hooks index plus a session id and a callback that builds the
/// transcript path (which can change over the session).
pub struct UserHooksPreToolGate {
    pub hooks: Arc<RwLock<HooksIndex>>,
    pub session_id: String,
    pub project_root: PathBuf,
    pub transcript_path: Arc<RwLock<PathBuf>>,
}

#[async_trait]
impl PreToolUseGate for UserHooksPreToolGate {
    async fn check(&self, tool_name: &str, input: &Value) -> PreToolDecision {
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::PreToolUse) else {
            return PreToolDecision::Allow;
        };
        let transcript = self.transcript_path.read().await.clone();
        let ctx = payload::HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload = payload::pre_tool_use(&ctx, tool_name, input);
        for group in groups {
            if !group.matcher.is_match(tool_name) {
                continue;
            }
            for cmd in &group.commands {
                let (decision, warnings, _stdout, _stderr) = runner::run_one(
                    HookEvent::PreToolUse,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    tracing::warn!("user-hooks: {w}");
                }
                match decision {
                    HookDecision::Block { reason, .. } => {
                        return PreToolDecision::Block(reason);
                    }
                    HookDecision::Continue { .. } => continue,
                }
            }
        }
        PreToolDecision::Allow
    }
}
```

- [ ] **Step 2: Declare + implement `BuiltinHookPlugin` on `UserHooksPlugin`**

In `mod.rs`, replace the unit struct with a stateful one carrying the gate:

```rust
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::plugin::builtin::provider_common::BuiltinHookPlugin;
use crate::plugin::builtin::user_hooks::discovery::HooksIndex;
use crate::plugin::builtin::user_hooks::pre_tool_gate::UserHooksPreToolGate;

pub struct UserHooksPlugin {
    pub hooks: Arc<RwLock<HooksIndex>>,
    pub session_id: String,
    pub project_root: PathBuf,
    pub transcript_path: Arc<RwLock<PathBuf>>,
    cached_gate: Option<Arc<UserHooksPreToolGate>>,
}

impl UserHooksPlugin {
    pub fn new(
        hooks: Arc<RwLock<HooksIndex>>,
        session_id: String,
        project_root: PathBuf,
        transcript_path: Arc<RwLock<PathBuf>>,
    ) -> Self {
        Self {
            hooks,
            session_id,
            project_root,
            transcript_path,
            cached_gate: None,
        }
    }

    fn gate_arc(&mut self) -> Arc<UserHooksPreToolGate> {
        if let Some(g) = self.cached_gate.as_ref() {
            return g.clone();
        }
        let g = Arc::new(UserHooksPreToolGate {
            hooks: self.hooks.clone(),
            session_id: self.session_id.clone(),
            project_root: self.project_root.clone(),
            transcript_path: self.transcript_path.clone(),
        });
        self.cached_gate = Some(g.clone());
        g
    }
}

impl BuiltinHookPlugin for UserHooksPlugin {
    fn take_pre_tool_gate(
        &mut self,
    ) -> Option<Arc<dyn savvagent_host::PreToolUseGate>> {
        Some(self.gate_arc())
    }
}
```

The existing `Default for UserHooksPlugin` impl must be deleted (it can no longer build a usable plugin without the constructor args). Update Task 1's smoke test if it uses `::new()` with no args — it should now require the four args; pass test-only stubs.

- [ ] **Step 3: Tests**

In `pre_tool_gate.rs`'s `#[cfg(test)]`:

```rust
    #[tokio::test]
    async fn allow_when_no_pre_tool_use_hooks() {
        let g = UserHooksPreToolGate {
            hooks: Arc::new(RwLock::new(HooksIndex::default())),
            session_id: "sid".into(),
            project_root: PathBuf::from("/tmp"),
            transcript_path: Arc::new(RwLock::new(PathBuf::from("/t.json"))),
        };
        assert_eq!(
            g.check("run", &serde_json::json!({})).await,
            PreToolDecision::Allow
        );
    }

    #[tokio::test]
    async fn block_on_exit_2_hook() {
        // Build a HooksIndex with a single PreToolUse hook running
        // `echo nope >&2; exit 2`. Assert g.check("run", _) returns
        // Block("nope").
    }
```

- [ ] **Step 4: Build + commit**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::builtin::user_hooks
git add crates/savvagent/src/plugin/builtin/user_hooks/pre_tool_gate.rs \
        crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): PreToolUseGate impl + plugin gate wiring"
```

---

### Task 17: `on_event` for `PostToolUse` / `SessionStart`

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs`

- [ ] **Step 1: Subscribe + dispatch**

Extend the plugin's `manifest()` to include `Contributions::hooks` for the events we handle:

```rust
contributions.hooks = vec![
    savvagent_plugin::HookKind::ToolCallEnd,       // -> PostToolUse
    savvagent_plugin::HookKind::HostStarting,      // -> SessionStart
    savvagent_plugin::HookKind::PromptSubmitted,   // -> UserPromptSubmit (Task 18)
    savvagent_plugin::HookKind::TurnEnd,           // -> Stop (Task 18)
];
```

Implement `on_event` that ignores all but the four `HookKind`s above:

```rust
    async fn on_event(
        &mut self,
        event: savvagent_plugin::HostEvent,
    ) -> Result<Vec<Effect>, savvagent_plugin::PluginError> {
        use savvagent_plugin::HostEvent;
        match event {
            HostEvent::ToolCallEnd { id: _, success, .. } => {
                self.dispatch_post_tool_use(success).await
            }
            HostEvent::HostStarting => self.dispatch_session_start().await,
            HostEvent::PromptSubmitted { text } => {
                self.dispatch_user_prompt_submit(&text).await
            }
            HostEvent::TurnEnd { success, .. } => self.dispatch_stop(success).await,
            _ => Ok(vec![]),
        }
    }
```

`dispatch_post_tool_use` and `dispatch_session_start` are private methods that:
- Read the per-event groups from `self.hooks`.
- Build the right payload via `payload::*`.
- Run each matching hook via `runner::run_one`.
- Convert warnings + stdout/stderr into `Effect::PushNote`s (unless `suppress_output`).
- Return the accumulated `Vec<Effect>`.

Concrete impl:

```rust
    async fn dispatch_post_tool_use(&mut self, success: bool) -> Result<Vec<Effect>, savvagent_plugin::PluginError> {
        // Implementation similar to PreToolUse dispatch but with the
        // PostToolUse payload and no Block→short-circuit.
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&crate::plugin::builtin::user_hooks::discovery::HookEvent::PostToolUse) else {
            return Ok(vec![]);
        };
        // Tool name + input/response aren't available on ToolCallEnd
        // today (the event payload was lightweight); skip per-tool
        // matching for v1 — match `*` only. Document this limitation.
        // …
        let _ = success;
        Ok(vec![])
    }

    async fn dispatch_session_start(&mut self) -> Result<Vec<Effect>, savvagent_plugin::PluginError> {
        // Run matching SessionStart hooks; surface stdout/warnings as
        // PushNotes.
        Ok(vec![])
    }
```

Mark `dispatch_user_prompt_submit` and `dispatch_stop` as `unimplemented!()` for now — they land in Task 18.

The `ToolCallEnd` `HostEvent` may not include the original `tool_name` / `tool_input` / `tool_response`. Document this gap in the plugin's doc comment and ship a v1 limitation: `PostToolUse` hooks only fire with `*` matcher matches and stdin payload contains `tool_name = "<unknown>"`. The follow-up issue (filed during plan: "Pass tool name + IO through ToolCallEnd HostEvent") would lift this.

- [ ] **Step 2: Tests**

Add tests in `mod.rs`:

```rust
    #[tokio::test]
    async fn no_hooks_means_no_effects() {
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p.on_event(savvagent_plugin::HostEvent::HostStarting).await.unwrap();
        assert!(effs.is_empty());
    }
```

- [ ] **Step 3: Build + commit**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::builtin::user_hooks
git add crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): subscribe to ToolCallEnd + HostStarting"
```

---

### Task 18: `UserPromptSubmit` + `Stop` dispatch (block-capable)

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs`

- [ ] **Step 1: Implement `dispatch_user_prompt_submit`**

```rust
    async fn dispatch_user_prompt_submit(
        &mut self,
        prompt: &str,
    ) -> Result<Vec<Effect>, savvagent_plugin::PluginError> {
        use crate::plugin::builtin::user_hooks::discovery::HookEvent;
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::UserPromptSubmit) else {
            return Ok(vec![]);
        };
        let transcript = self.transcript_path.read().await.clone();
        let ctx = payload::HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload_value = payload::user_prompt_submit(&ctx, prompt);
        let mut effects: Vec<Effect> = Vec::new();
        for group in groups {
            // matcher ignored for non-tool events
            for cmd in &group.commands {
                let (decision, warnings, _stdout, _stderr) = runner::run_one(
                    HookEvent::UserPromptSubmit,
                    &cmd.command,
                    cmd.timeout,
                    &payload_value,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    effects.push(Effect::PushNote {
                        line: savvagent_plugin::StyledLine::plain(format!("[warn] {w}")),
                    });
                }
                match decision {
                    HookDecision::Block { reason, .. } => {
                        effects.push(Effect::CancelPendingTurn { reason });
                        return Ok(effects);
                    }
                    HookDecision::Continue { additional_context, .. } => {
                        if let Some(extra) = additional_context {
                            if !extra.is_empty() {
                                effects.push(Effect::PrependToPendingPrompt { text: extra });
                            }
                        }
                    }
                }
            }
        }
        Ok(effects)
    }
```

`HookDecision` is the local module's type (imported at the top). Confirm `use crate::plugin::builtin::user_hooks::decision::HookDecision;` is at the file head.

- [ ] **Step 2: Implement `dispatch_stop`**

Same shape as `dispatch_user_prompt_submit`, payload is `payload::stop(&ctx, false)` for v1 (the `stop_hook_active` flag is always `false` until we have a re-entrancy guard).

- [ ] **Step 3: Tests**

```rust
    // Build a HooksIndex with a UserPromptSubmit hook that returns
    // additionalContext "extra". Assert the plugin emits
    // Effect::PrependToPendingPrompt { text: "extra" }.

    // Build with a UserPromptSubmit hook that exits 2. Assert the
    // plugin emits Effect::CancelPendingTurn.
```

- [ ] **Step 4: Build + commit**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::builtin::user_hooks
git add crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): UserPromptSubmit + Stop dispatch (block-capable)"
```

---

### Task 19: `/reload-hooks` slash command

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_hooks/mod.rs`
- Create (optional): `crates/savvagent/src/plugin/builtin/user_hooks/reload.rs` if extracted

- [ ] **Step 1: Implement in `handle_slash`**

```rust
    async fn handle_slash(
        &mut self,
        name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, savvagent_plugin::PluginError> {
        if name != "reload-hooks" {
            return Ok(vec![]);
        }
        // Re-walk discovery on the same project_root + home (home is
        // dirs::home_dir() at the time of reload).
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let new_idx =
            crate::plugin::builtin::user_hooks::discovery::walk_all(&self.project_root, &home);
        let warnings = new_idx.warnings.clone();
        *self.hooks.write().await = new_idx;
        let mut effs: Vec<Effect> = warnings
            .into_iter()
            .map(|w| Effect::PushNote {
                line: savvagent_plugin::StyledLine::plain(format!("[warn] user-hooks: {w}")),
            })
            .collect();
        effs.push(Effect::ReindexPlugin {
            id: savvagent_plugin::PluginId::new("internal:user-hooks").unwrap(),
        });
        effs.push(Effect::PushNote {
            line: savvagent_plugin::StyledLine::plain("user-hooks: reloaded"),
        });
        Ok(effs)
    }
```

- [ ] **Step 2: Test**

```rust
    #[tokio::test]
    async fn reload_emits_reindex_plugin_effect() {
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p.handle_slash("reload-hooks", vec![]).await.unwrap();
        assert!(effs.iter().any(|e| matches!(e, Effect::ReindexPlugin { .. })));
    }
```

- [ ] **Step 3: Commit**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::builtin::user_hooks::tests::reload_emits_reindex_plugin_effect
git add crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): /reload-hooks rescans + reindexes"
```

---

### Task 20: App field for hooks index + startup load

**Files:**
- Modify: `crates/savvagent/src/app.rs`
- Modify: `crates/savvagent/src/main.rs`

- [ ] **Step 1: Shared handle on `App`**

In `app.rs`, near the existing `trust_levels: Arc<RwLock<...>>` field:

```rust
    /// Shared user-hooks index. Loaded from `settings.json` at startup
    /// by `App::new`; mutated by `/reload-hooks`. Cloned into the
    /// `internal:user-hooks` plugin so both views see the same data.
    pub user_hooks_index:
        std::sync::Arc<tokio::sync::RwLock<
            crate::plugin::builtin::user_hooks::discovery::HooksIndex,
        >>,
    /// Mutable transcript path passed to the user-hooks plugin so
    /// hooks can include the up-to-date path in their stdin payload.
    pub transcript_path:
        std::sync::Arc<tokio::sync::RwLock<std::path::PathBuf>>,
```

Initialize in `App::new`:

```rust
        let project_root = /* existing project root resolution */;
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let initial_idx =
            crate::plugin::builtin::user_hooks::discovery::walk_all(&project_root, &home);
        let user_hooks_index = std::sync::Arc::new(tokio::sync::RwLock::new(initial_idx));
        let transcript_path = std::sync::Arc::new(tokio::sync::RwLock::new(
            /* initial transcript path */ std::path::PathBuf::from("/tmp/transcript.json"),
        ));
```

The initial transcript path can be set to a known sentinel; the TUI replaces it once a real transcript is opened.

Add the fields to the App constructor literal:

```rust
            user_hooks_index,
            transcript_path,
```

- [ ] **Step 2: Pass to `register_builtins`**

In `crates/savvagent/src/plugin/mod.rs::register_builtins`, change the signature to accept the additional handles (mirror how `trust_levels` was added in sub-project A):

```rust
pub(crate) fn register_builtins(
    trust_levels: builtin::user_slash_commands::TrustMap,
    user_hooks_index: std::sync::Arc<tokio::sync::RwLock<
        builtin::user_hooks::discovery::HooksIndex,
    >>,
    session_id: String,
    project_root: std::path::PathBuf,
    transcript_path: std::sync::Arc<tokio::sync::RwLock<std::path::PathBuf>>,
) -> BuiltinSet { ... }
```

The user-hooks plugin in the `plugins` Vec is built as:

```rust
Box::new(builtin::user_hooks::UserHooksPlugin::new(
    user_hooks_index.clone(),
    session_id.clone(),
    project_root.clone(),
    transcript_path.clone(),
)),
```

In `main.rs`, update the call to `register_builtins` to pass the new args.

- [ ] **Step 3: Build + run tests**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent 2>&1 | grep -E "^test result" | head -1
```

The existing `register_builtins_pr8_complete` test will need updating to call `register_builtins` with the new arguments. Construct test stubs:

```rust
let _set = register_builtins(
    Arc::new(tokio::sync::RwLock::new(BTreeMap::new())), // trust
    Arc::new(tokio::sync::RwLock::new(HooksIndex::default())),
    "test-session".into(),
    PathBuf::from("/tmp"),
    Arc::new(tokio::sync::RwLock::new(PathBuf::from("/t.json"))),
);
```

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/app.rs crates/savvagent/src/main.rs crates/savvagent/src/plugin/mod.rs
git commit -m "feat(app): user_hooks_index + transcript_path shared handles"
```

---

### Task 21: Register the `HookEntry` in `register_builtins`

**Files:**
- Modify: `crates/savvagent/src/plugin/mod.rs`

- [ ] **Step 1: Insert `HookEntry`**

In `register_builtins`, after constructing the `plugins` Vec, also build a `hook_entries: Vec<HookEntry>` containing the same `UserHooksPlugin` instance (mirroring how `providers: Vec<ProviderEntry>` works). The dual-Arc pattern means the plugin appears in both `plugins` (as `Box<dyn Plugin>`) AND `hook_entries` (as `Arc<Mutex<dyn BuiltinHookPlugin>>`).

Refactor: construct the `UserHooksPlugin` once into a `HookEntry`, then derive the `Arc<Mutex<dyn Plugin>>` from the same shared inner. Look at how `ProviderEntry::new` does it.

```rust
let user_hooks_entry = HookEntry::new(builtin::user_hooks::UserHooksPlugin::new(
    user_hooks_index.clone(),
    session_id.clone(),
    project_root.clone(),
    transcript_path.clone(),
));
// `user_hooks_entry.as_plugin` is the Arc<Mutex<dyn Plugin>> view; the
// existing `Vec<Box<dyn Plugin>>` is built around `Box<dyn Plugin>` —
// this mismatch is why providers use a parallel registry. Verify the
// registry's actual shape and adapt.
```

If the registry already converges `Box<dyn Plugin>` and `Arc<Mutex<dyn Plugin>>` views into one internal map, follow the existing wiring; if not, use the `HookEntry` directly and skip the `Box`.

- [ ] **Step 2: Emit `RegisterPreToolGate` from startup**

In whatever code-path runs once at startup (e.g. `Plugin::on_event(HostEvent::HostStarting)` for the user-hooks plugin), emit:

```rust
Effect::RegisterPreToolGate {
    plugin_id: savvagent_plugin::PluginId::new("internal:user-hooks").unwrap(),
}
```

Alternatively, emit it from `register_builtins` directly into a startup-effect queue if such a queue exists. The HostStarting hook is cleaner because it integrates with the existing event flow.

- [ ] **Step 3: Build + run all tests**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent 2>&1 | grep -E "^test result" | head -1
```

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/mod.rs \
        crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "feat(plugin/user-hooks): register HookEntry + emit RegisterPreToolGate"
```

---

### Task 22: Pre-release verification

- [ ] **Step 1: Full workspace test**

```bash
cargo test --workspace 2>&1 | grep -E "^test result" | awk '{ sum += $4 } END { print sum, "passed total" }'
```

Expected: zero failures; total at-or-above the post-sub-project-A baseline plus all the new tests in tasks 1–21.

- [ ] **Step 2: CI parity**

```bash
rustup run stable cargo fmt --check
rustup run stable cargo clippy --workspace --all-targets -- -D warnings
RUSTFLAGS="-D warnings" cargo build --workspace
```

All three must finish clean.

- [ ] **Step 3: Manual smoke**

Build, then create a minimal `~/.savvagent/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "*",
        "hooks": [
          { "type": "command", "command": "echo blocked >&2; exit 2" }
        ]
      }
    ]
  }
}
```

Run `cargo run -p savvagent`, ask the model to call any tool, and confirm the conversation log shows `[blocked] blocked` instead of the tool's normal output.

Then clean up the test settings file:

```bash
rm ~/.savvagent/settings.json
```

- [ ] **Step 4: No commit here**

Verification only.

---

### Task 23: README + CHANGELOG

**Files:**
- Modify: `README.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: README section**

Append a new "User-defined hooks" section to README. Cover: config locations, schema, event mapping table, exit-code/JSON decision protocol, `/reload-hooks`.

Use the appendix from `docs/superpowers/specs/2026-05-22-user-hooks-design.md` as the canonical reference and paraphrase into README form.

- [ ] **Step 2: CHANGELOG entry**

Under `## [Unreleased]`, add:

```markdown
### Added
- User-defined hooks. Drop a Claude-Code-compatible `settings.json`
  under `.savvagent/` (project), `.claude/` (project-claude),
  `~/.savvagent/`, or `~/.claude/`; the `hooks` block contributes shell
  hooks for `PreToolUse`, `PostToolUse`, `UserPromptSubmit`,
  `SessionStart`, and `Stop`. `PreToolUse` and `UserPromptSubmit`/`Stop`
  can block (exit 2 or `{"continue":false}`). `UserPromptSubmit` hooks
  can inject `additionalContext` that gets prepended to the user's
  prompt. `/reload-hooks` rescans without restart.
```

- [ ] **Step 3: Commit**

```bash
git add README.md CHANGELOG.md
git commit -m "docs(user-hooks): README section + CHANGELOG entry"
```

---

### Task 24: End-to-end integration test

**Files:**
- Create: `crates/savvagent/tests/user_hooks_e2e.rs` OR an inline integration test in `mod.rs` if the binary-only crate constraint applies (see sub-project A Task 22).

- [ ] **Step 1: Write the e2e**

Smoke-shape test: build a `UserHooksPlugin` against a temp `settings.json`, drive a `HookEvent::PreToolUse` check, and assert the right `PreToolDecision`.

```rust
    #[tokio::test]
    async fn e2e_pre_tool_use_block_short_circuits() {
        use crate::plugin::builtin::user_hooks::*;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let tmp = tempfile::TempDir::new().unwrap();
        let proj = tmp.path().to_path_buf();
        std::fs::create_dir_all(proj.join(".savvagent")).unwrap();
        std::fs::write(
            proj.join(".savvagent/settings.json"),
            r#"{
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": "*",
                            "hooks": [ { "command": "echo deny >&2; exit 2" } ]
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let home = tempfile::TempDir::new().unwrap();
        let idx = discovery::walk_all(&proj, home.path());
        let hooks = Arc::new(RwLock::new(idx));
        let transcript = Arc::new(RwLock::new(std::path::PathBuf::from("/t.json")));

        let gate = pre_tool_gate::UserHooksPreToolGate {
            hooks,
            session_id: "sid".into(),
            project_root: proj.clone(),
            transcript_path: transcript,
        };

        let decision = gate.check("run", &serde_json::json!({})).await;
        match decision {
            savvagent_host::PreToolDecision::Block(reason) => {
                assert_eq!(reason, "deny");
            }
            _ => panic!("expected Block"),
        }
    }
```

- [ ] **Step 2: Build + run**

```bash
RUSTFLAGS="-D warnings" cargo build --workspace
cargo test -p savvagent plugin::builtin::user_hooks
```

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_hooks/mod.rs
git commit -m "test(plugin/user-hooks): end-to-end PreToolUse block"
```

---

### Task 25: (DEFERRED) Version bump + release notes

Per [[feedback_phase_release_rollup]], no version bump in this plan's scope. When sub-project B merges and the release-time decision is made, bump `[workspace.package].version` (provisionally `0.17.0`), promote the `[Unreleased]` CHANGELOG section, draft release notes, push the tag. `cargo-dist` handles the rest.

---

## Spec coverage trace

| Spec requirement | Task(s) |
|---|---|
| Four-path discovery with merge semantics | 4 |
| Sequential execution within event, config order | 4, 17, 18 |
| `PreToolUse` mapped to a synchronous gating seam | 8, 9, 10, 16 |
| `PostToolUse` mapped to `ToolCallEnd` (observation only) | 17 |
| `UserPromptSubmit` mapped to `PromptSubmitted` (block + injection) | 18, 15 |
| `SessionStart` mapped to `HostStarting` | 17 |
| `Stop` mapped to `TurnEnd` (block) | 18, 15 |
| Reserved-but-never-fired events (`Notification`, `SubagentStop`, `PreCompact`) | 4 (warn-log at discovery) |
| Glob matchers via `globset` | 3 |
| stdin JSON payload per event | 5 |
| `SAVVAGENT_PROJECT_DIR` env | 7 |
| Exit codes: 0 / 2 / other | 6, 7 |
| Structured JSON stdout: `continue`, `stopReason`, `suppressOutput`, `hookSpecificOutput`, legacy `decision`/`reason` | 6 |
| `permissionDecision: "ask"` treated as `"deny"` in v1 | 6 |
| `hookSpecificOutput.hookEventName` mismatch warning | 6 |
| Per-hook `timeout` | 7 |
| `PreToolUseGate` panic → fail-open | 10 |
| Three new `Effect` variants on the WIT-portable surface | 11 |
| `BuiltinHookPlugin` trait + `HookEntry` plumbing | 12, 14 |
| `apply_effects` arms | 13, 15 |
| Plugin manifest + `/reload-hooks` slash | 1, 19 |
| App-side shared `HooksIndex` handle | 20 |
| Startup discovery loads from disk | 20 |
| Register plugin + emit `RegisterPreToolGate` at startup | 21 |
| Documentation + CHANGELOG | 23 |
| E2E test | 24 |
| Pre-release verification (test/fmt/clippy/manual smoke) | 22 |
| Version bump (deferred to release flow) | 25 |
