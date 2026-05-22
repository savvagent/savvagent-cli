# User-defined slash commands — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users add slash commands by dropping markdown files under `.savvagent/commands/` (project) or `~/.savvagent/commands/` (user), with `.claude/commands/` fallback for compatibility, YAML frontmatter for metadata, and templating tokens (`$ARGUMENTS`, `$N`, `!<cmd>`, `@<file>`) in the body.

**Architecture:** One new built-in plugin `internal:user-slash-commands` under `crates/savvagent/src/plugin/builtin/user_slash_commands/`. Synchronous discovery in `Plugin::manifest()` (cached behind a `OnceCell`); contributes one `SlashSpec` per discovered file plus a static `/reload-commands` entry. Dispatch path: trust check → template expansion → emit `Effect::PromptSend { text }` (existing). Trust modal is a new `Screen`. Three new `Effect` variants: `SetNextTurnModelOverride`, `SetTrustLevel`, `ReindexPlugin`.

**Tech Stack:** Rust 2024, `serde_yaml_ng` (already workspace dep) for frontmatter, `ignore` (already workspace dep) for directory walking, `serde_json` for the trust file, async-trait `Plugin` impl, `tokio::process::Command` for shell substitution, `tempfile` for tests.

---

## Spec drift from `2026-05-21-user-slash-commands-design.md`

Two refinements discovered while reading the v0.9.0 plugin runtime; each is a strict improvement that keeps the spec's user-facing contract intact:

1. **No `Effect::SubmitPrompt`.** The existing `Effect::PromptSend { text }` already submits a synthetic user prompt with the right semantics. Reuse it.
2. **Manifest re-indexing for `/reload-commands`.** `Manifest` is read once at registration. To refresh discovered commands without a TUI restart, this plan adds `Effect::ReindexPlugin { id: PluginId }` — the runtime re-calls the plugin's `manifest()` and rebuilds the slash index. Cleaner than inventing per-command `Add/RemoveSlashCommand` effects.

The plan delta for the one-turn model override remains: `Effect::SetNextTurnModelOverride { id: String }` (independent from the persistent `SetActiveModel`).

---

## File map

**Create:**
- `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs` — `Plugin` impl, dispatch, contributions.
- `crates/savvagent/src/plugin/builtin/user_slash_commands/frontmatter.rs` — YAML parse.
- `crates/savvagent/src/plugin/builtin/user_slash_commands/name.rs` — namespaced slug validation.
- `crates/savvagent/src/plugin/builtin/user_slash_commands/discovery.rs` — directory walks + precedence.
- `crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs` — `$ARGUMENTS`/`$N`/`@`/`!` expansion.
- `crates/savvagent/src/plugin/builtin/user_slash_commands/trust.rs` — `~/.savvagent/trusted-projects.json` round-trip.
- `crates/savvagent/src/plugin/builtin/user_slash_commands/trust_modal.rs` — `Screen` impl for first-run trust prompt.

**Modify:**
- `crates/savvagent-plugin/src/effect.rs` — add three `Effect` variants.
- `crates/savvagent/src/plugin/builtin/mod.rs` — declare the new module.
- `crates/savvagent/src/plugin/mod.rs` (around `register_builtins()`) — register the plugin.
- `crates/savvagent/src/plugin/effects.rs` — `apply_effects` arms for the new variants.
- `crates/savvagent/src/app.rs` — add `next_turn_model_override`, `pending_slash_after_trust` fields; consume override in worker spawn.
- `README.md` — new section + on-disk paths reference.
- `CHANGELOG.md` — release entry.

---

## Conventions

- All `cargo test` invocations specify the crate to keep iteration fast.
- Tests touching `HOME` must take `HOME_LOCK` (see `crates/savvagent/src/plugin/builtin/themes/` for the existing pattern); tests touching `rust_i18n::set_locale` must reset to `"en"` inside the lock per the codebase's locale-isolation rule.
- Commits land on the current branch (no branching mid-plan unless the engineer prefers PR-per-task; either is fine).

---

### Task 1: Skeleton plugin + registration

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/mod.rs`
- Modify: `crates/savvagent/src/plugin/mod.rs:80-110` (`register_builtins`)

- [ ] **Step 1: Write the smoke test**

In `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs`:

```rust
//! `internal:user-slash-commands` — discovers and dispatches user-defined
//! slash commands from `.savvagent/commands/` and `.claude/commands/`.
//!
//! See `docs/superpowers/specs/2026-05-21-user-slash-commands-design.md`.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
};

/// Built-in plugin that exposes user-authored slash commands.
pub struct UserSlashCommandsPlugin;

impl UserSlashCommandsPlugin {
    /// Construct a new [`UserSlashCommandsPlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for UserSlashCommandsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for UserSlashCommandsPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "reload-commands".into(),
            summary: "Rescan user-defined slash command directories".into(),
            args_hint: None,
            requires_arg: false,
        }];
        Manifest {
            id: PluginId::new("internal:user-slash-commands").expect("valid built-in id"),
            name: "User slash commands".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "User-defined commands from .savvagent/commands/ and .claude/commands/"
                .into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        // Implemented in Task 18.
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_reload_commands() {
        let p = UserSlashCommandsPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:user-slash-commands");
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-commands"));
    }
}
```

- [ ] **Step 2: Wire the module in `builtin/mod.rs`**

Append to `crates/savvagent/src/plugin/builtin/mod.rs` (alphabetical position is fine; mirror the style of neighboring entries):

```rust
/// `internal:user-slash-commands` — discovers user-authored slash commands
/// from `.savvagent/commands/` / `.claude/commands/` and dispatches them
/// with templating expansion.
pub mod user_slash_commands;
```

- [ ] **Step 3: Register the plugin in `register_builtins()`**

In `crates/savvagent/src/plugin/mod.rs`, inside the `plugins` Vec around line 90, add (alphabetical insertion is fine — locate the position between `themes` and any later entry, or append before the closing `]`):

```rust
        Box::new(builtin::user_slash_commands::UserSlashCommandsPlugin::new()),
```

- [ ] **Step 4: Run the smoke test**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::tests::manifest_has_reload_commands
```

Expected: PASS (one test).

- [ ] **Step 5: Confirm the workspace still builds**

```bash
cargo build --workspace
```

Expected: success, no warnings introduced.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs \
        crates/savvagent/src/plugin/builtin/mod.rs \
        crates/savvagent/src/plugin/mod.rs
git commit -m "feat(plugin/user-slash-commands): plugin skeleton with /reload-commands"
```

---

### Task 2: Frontmatter parsing

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_slash_commands/frontmatter.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs` (add `mod frontmatter;`)

- [ ] **Step 1: Write the failing tests**

In `crates/savvagent/src/plugin/builtin/user_slash_commands/frontmatter.rs`:

```rust
//! Parses optional YAML frontmatter from command markdown files.
//!
//! Frontmatter is delimited by a leading `---\n` line and a trailing
//! `\n---\n` line. The body is whatever follows the second delimiter.
//! Files without frontmatter are valid; the entire content is the body.

use serde::Deserialize;

/// Parsed frontmatter values; every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontmatter {
    /// One-line palette summary; defaults to the file's relative path.
    #[serde(default)]
    pub description: Option<String>,
    /// Argument placeholder rendered next to the command name in the palette.
    #[serde(default, alias = "argument-hint")]
    pub argument_hint: Option<String>,
    /// Tool-pattern allowlist; parsed but not enforced in v1.
    #[serde(default, alias = "allowed-tools")]
    pub allowed_tools: Option<Vec<String>>,
    /// One-turn model override id.
    #[serde(default)]
    pub model: Option<String>,
}

/// Outcome of splitting a command file into frontmatter + body.
#[derive(Debug, Clone)]
pub struct Parsed {
    /// Parsed (or default) frontmatter.
    pub frontmatter: Frontmatter,
    /// Markdown body (everything after the closing `---` line, or the
    /// entire file when no frontmatter is present).
    pub body: String,
    /// Warnings to surface to the log without aborting the load.
    pub warnings: Vec<String>,
}

/// Parse a command file's contents into frontmatter + body.
///
/// Returns `Err` only when frontmatter is present but malformed *or*
/// contains unknown keys. Malformed-frontmatter files are reported and
/// then skipped at discovery time.
pub fn parse(contents: &str) -> Result<Parsed, String> {
    let mut warnings = Vec::new();
    if !contents.starts_with("---\n") && !contents.starts_with("---\r\n") {
        return Ok(Parsed {
            frontmatter: Frontmatter::default(),
            body: contents.to_string(),
            warnings,
        });
    }
    let after_open = contents.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
    let Some((yaml, body)) = split_closing(after_open) else {
        warnings.push("unterminated frontmatter; treating file as bodyless".into());
        return Ok(Parsed {
            frontmatter: Frontmatter::default(),
            body: String::new(),
            warnings,
        });
    };
    // First try strict parse (rejects unknown keys).
    match serde_yaml_ng::from_str::<Frontmatter>(yaml) {
        Ok(fm) => Ok(Parsed {
            frontmatter: fm,
            body: body.to_string(),
            warnings,
        }),
        Err(strict_err) => {
            // Retry with a lenient struct to extract the known keys
            // and warn per unknown key.
            #[derive(Deserialize)]
            struct Lenient(serde_yaml_ng::Value);
            if let Ok(Lenient(value)) = serde_yaml_ng::from_str(yaml) {
                if let Some(map) = value.as_mapping() {
                    let known = ["description", "argument-hint", "allowed-tools", "model"];
                    for (k, _) in map {
                        if let Some(name) = k.as_str() {
                            if !known.contains(&name) {
                                warnings.push(format!("unknown frontmatter key: {name}"));
                            }
                        }
                    }
                    let cleaned: serde_yaml_ng::Mapping = map
                        .iter()
                        .filter(|(k, _)| {
                            k.as_str().map(|s| known.contains(&s)).unwrap_or(false)
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    if let Ok(fm) = serde_yaml_ng::from_value::<Frontmatter>(
                        serde_yaml_ng::Value::Mapping(cleaned),
                    ) {
                        return Ok(Parsed {
                            frontmatter: fm,
                            body: body.to_string(),
                            warnings,
                        });
                    }
                }
            }
            Err(format!("frontmatter parse error: {strict_err}"))
        }
    }
}

fn split_closing(after_open: &str) -> Option<(&str, &str)> {
    // Look for a line containing only `---` (LF or CRLF endings).
    for (idx, line) in after_open.match_indices("\n---") {
        let after = &after_open[idx + 4..];
        if after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n") {
            let yaml = &after_open[..idx];
            let body_start = if let Some(stripped) = after.strip_prefix("\r\n") {
                stripped
            } else {
                after.strip_prefix('\n').unwrap_or(after)
            };
            return Some((yaml, body_start));
        }
        // Drop unused `line` to silence warning.
        let _ = line;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_returns_whole_body() {
        let p = parse("Just a body").unwrap();
        assert_eq!(p.body, "Just a body");
        assert_eq!(p.frontmatter, Frontmatter::default());
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn well_formed_frontmatter() {
        let src = "---\ndescription: Hi\nargument-hint: <file>\nmodel: claude-sonnet-4-6\n---\nBody here\n";
        let p = parse(src).unwrap();
        assert_eq!(p.frontmatter.description.as_deref(), Some("Hi"));
        assert_eq!(p.frontmatter.argument_hint.as_deref(), Some("<file>"));
        assert_eq!(p.frontmatter.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(p.body, "Body here\n");
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn allowed_tools_list_parses() {
        let src = "---\nallowed-tools:\n  - read_file\n  - \"Bash(git diff:*)\"\n---\nbody";
        let p = parse(src).unwrap();
        assert_eq!(
            p.frontmatter.allowed_tools.as_deref(),
            Some(&["read_file".to_string(), "Bash(git diff:*)".to_string()][..])
        );
    }

    #[test]
    fn unknown_keys_warn_and_strip() {
        let src = "---\ndescription: hi\nzzz: extra\n---\nbody";
        let p = parse(src).unwrap();
        assert_eq!(p.frontmatter.description.as_deref(), Some("hi"));
        assert!(p.warnings.iter().any(|w| w.contains("zzz")));
        assert_eq!(p.body, "body");
    }

    #[test]
    fn unterminated_frontmatter_warns() {
        let src = "---\ndescription: hi\nno closing delimiter ever";
        let p = parse(src).unwrap();
        assert!(p.warnings.iter().any(|w| w.contains("unterminated")));
    }

    #[test]
    fn malformed_yaml_returns_error() {
        let src = "---\n: :bad: yaml: :\n---\nbody";
        assert!(parse(src).is_err());
    }
}
```

- [ ] **Step 2: Declare the module**

Add to `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs`:

```rust
mod frontmatter;
```

- [ ] **Step 3: Add `serde_yaml_ng` to `crates/savvagent/Cargo.toml`**

If not already a direct dep of the `savvagent` crate, add it to `[dependencies]`:

```toml
serde_yaml_ng.workspace = true
```

Verify the existing workspace declaration in `Cargo.toml` (already `serde_yaml_ng = "0.10"`); nothing to add there.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::frontmatter::tests
```

Expected: 6 PASS, 0 FAIL.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/Cargo.toml \
        crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/frontmatter.rs
git commit -m "feat(plugin/user-slash-commands): YAML frontmatter parsing"
```

---

### Task 3: Namespaced command-name validation

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_slash_commands/name.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs` (add `mod name;`)

- [ ] **Step 1: Write the failing tests**

In `crates/savvagent/src/plugin/builtin/user_slash_commands/name.rs`:

```rust
//! Validates and constructs namespaced slash-command names from file paths.
//!
//! A discovered file at `<root>/team/security/audit.md` becomes the
//! command `/team:security:audit`. Each segment must match
//! `[a-z0-9][-a-z0-9_]*`.

use std::path::Path;

/// Compute the namespaced command name (without the leading `/`) for a
/// markdown file path relative to its containing `commands/` root.
///
/// Returns `Ok(name)` on success, `Err(reason)` otherwise.
pub fn from_relative_path(rel: &Path) -> Result<String, String> {
    let stem = rel
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "non-utf8 or missing file stem".to_string())?;
    let mut segments: Vec<&str> = rel
        .parent()
        .and_then(|p| {
            p.components()
                .map(|c| c.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default();
    segments.push(stem);
    for seg in &segments {
        validate_segment(seg)?;
    }
    Ok(segments.join(":"))
}

fn validate_segment(seg: &str) -> Result<(), String> {
    if seg.is_empty() {
        return Err("empty segment".into());
    }
    let mut chars = seg.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(format!(
            "segment '{seg}' must start with [a-z0-9]"
        ));
    }
    for c in chars {
        if !(c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || c == '-'
            || c == '_')
        {
            return Err(format!("segment '{seg}' contains invalid char '{c}'"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn flat_name() {
        assert_eq!(
            from_relative_path(&PathBuf::from("review.md")).unwrap(),
            "review"
        );
    }

    #[test]
    fn one_level_namespace() {
        assert_eq!(
            from_relative_path(&PathBuf::from("team/lint.md")).unwrap(),
            "team:lint"
        );
    }

    #[test]
    fn nested_namespace_flattens() {
        assert_eq!(
            from_relative_path(&PathBuf::from("team/security/audit.md")).unwrap(),
            "team:security:audit"
        );
    }

    #[test]
    fn uppercase_rejected() {
        assert!(from_relative_path(&PathBuf::from("Review.md")).is_err());
    }

    #[test]
    fn leading_dash_rejected() {
        assert!(from_relative_path(&PathBuf::from("-bad.md")).is_err());
    }

    #[test]
    fn allows_digits_and_underscore() {
        assert_eq!(
            from_relative_path(&PathBuf::from("v2/run_it.md")).unwrap(),
            "v2:run_it"
        );
    }
}
```

- [ ] **Step 2: Declare the module**

In `mod.rs` add:

```rust
mod name;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::name::tests
```

Expected: 6 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/name.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs
git commit -m "feat(plugin/user-slash-commands): namespaced name validation"
```

---

### Task 4: Discovery — single directory walk

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_slash_commands/discovery.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs` (add `mod discovery;`)
- Modify: `crates/savvagent/Cargo.toml` (add `ignore`, `tempfile` dev-dep)

- [ ] **Step 1: Add deps**

In `crates/savvagent/Cargo.toml`:

```toml
[dependencies]
# ...existing entries...
ignore.workspace = true

[dev-dependencies]
# ...existing entries...
tempfile = "3"
```

(Workspace already exposes `ignore = "0.4"`; verify in root `Cargo.toml`.)

- [ ] **Step 2: Write the failing tests + skeleton**

In `crates/savvagent/src/plugin/builtin/user_slash_commands/discovery.rs`:

```rust
//! Walks the four well-known command directories and produces a
//! per-name, precedence-respecting index of discovered commands.

use std::path::{Path, PathBuf};

use crate::plugin::builtin::user_slash_commands::frontmatter::{self, Frontmatter};
use crate::plugin::builtin::user_slash_commands::name;

/// One discovered command file, ready to be turned into a `SlashSpec`.
#[derive(Debug, Clone)]
pub struct Discovered {
    /// Namespaced command name (no leading `/`).
    pub name: String,
    /// Absolute path to the source file on disk.
    pub path: PathBuf,
    /// Parsed frontmatter (defaulted if absent).
    pub frontmatter: Frontmatter,
    /// Cached markdown body (everything after the closing `---`).
    pub body: String,
    /// Origin scope; used by precedence and the trust check.
    pub origin: Origin,
    /// Non-fatal warnings collected during parse.
    pub warnings: Vec<String>,
}

/// Where this command came from. Drives precedence and trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `<project>/.savvagent/commands/`
    ProjectSavvagent,
    /// `<project>/.claude/commands/`
    ProjectClaude,
    /// `~/.savvagent/commands/`
    UserSavvagent,
    /// `~/.claude/commands/`
    UserClaude,
}

impl Origin {
    /// `true` if this origin is project-local (subject to trust prompts).
    pub fn is_project(self) -> bool {
        matches!(self, Origin::ProjectSavvagent | Origin::ProjectClaude)
    }
}

/// Walk one directory and return every valid `.md` file found.
///
/// Files with invalid names or malformed frontmatter are skipped and
/// surfaced as `warnings` in the return value; they never abort the
/// walk.
pub fn walk_one(root: &Path, origin: Origin) -> (Vec<Discovered>, Vec<String>) {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    if !root.exists() {
        return (out, warnings);
    }
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        let name_str = match name::from_relative_path(&rel) {
            Ok(n) => n,
            Err(why) => {
                warnings.push(format!("{}: {why}", path.display()));
                continue;
            }
        };
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        match frontmatter::parse(&contents) {
            Ok(parsed) => {
                for w in &parsed.warnings {
                    warnings.push(format!("{}: {w}", path.display()));
                }
                out.push(Discovered {
                    name: name_str,
                    path: path.to_path_buf(),
                    frontmatter: parsed.frontmatter,
                    body: parsed.body,
                    origin,
                    warnings: parsed.warnings,
                });
            }
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    (out, warnings)
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
    fn missing_root_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("nonexistent");
        let (out, warns) = walk_one(&root, Origin::ProjectSavvagent);
        assert!(out.is_empty());
        assert!(warns.is_empty());
    }

    #[test]
    fn picks_up_md_files_with_namespacing() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "review.md", "---\ndescription: r\n---\nBody");
        write(
            tmp.path(),
            "team/lint.md",
            "---\ndescription: l\n---\nBody2",
        );
        write(tmp.path(), "not-markdown.txt", "ignored");

        let (out, warns) = walk_one(tmp.path(), Origin::ProjectSavvagent);
        let names: Vec<_> = out.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"review"));
        assert!(names.contains(&"team:lint"));
        assert_eq!(out.len(), 2);
        assert!(warns.is_empty());
    }

    #[test]
    fn invalid_slug_is_skipped_with_warning() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "GoodName.md", "body");
        let (out, warns) = walk_one(tmp.path(), Origin::ProjectSavvagent);
        assert!(out.is_empty());
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("GoodName.md"));
    }

    #[test]
    fn malformed_frontmatter_is_skipped_with_warning() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "bad.md", "---\n: : :\n---\nbody");
        let (out, warns) = walk_one(tmp.path(), Origin::ProjectSavvagent);
        assert!(out.is_empty());
        assert_eq!(warns.len(), 1);
    }

    #[test]
    fn origin_propagates() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "x.md", "body");
        let (out, _) = walk_one(tmp.path(), Origin::UserClaude);
        assert_eq!(out[0].origin, Origin::UserClaude);
    }
}
```

- [ ] **Step 3: Declare the module**

In `mod.rs`:

```rust
mod discovery;
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::discovery::tests
```

Expected: 5 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/Cargo.toml \
        crates/savvagent/src/plugin/builtin/user_slash_commands/discovery.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs
git commit -m "feat(plugin/user-slash-commands): single-directory discovery walker"
```

---

### Task 5: Discovery — four-path precedence

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/discovery.rs`

- [ ] **Step 1: Add failing test**

Append to the existing `tests` module in `discovery.rs`:

```rust
    #[test]
    fn precedence_project_over_user_and_savvagent_over_claude() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent/commands"),
            "x.md",
            "from project savvagent",
        );
        write(
            &proj.path().join(".claude/commands"),
            "x.md",
            "from project claude",
        );
        write(
            &home.path().join(".savvagent/commands"),
            "x.md",
            "from user savvagent",
        );
        write(
            &home.path().join(".claude/commands"),
            "x.md",
            "from user claude",
        );
        // Add a user-only command to verify it survives.
        write(
            &home.path().join(".savvagent/commands"),
            "user_only.md",
            "user only",
        );

        let index = walk_all(proj.path(), home.path());
        // x should resolve to project-savvagent body.
        let x = &index.commands.get("x").unwrap();
        assert_eq!(x.origin, Origin::ProjectSavvagent);
        assert!(x.body.contains("from project savvagent"));
        // user_only is present.
        assert!(index.commands.contains_key("user_only"));
    }
```

- [ ] **Step 2: Implement `walk_all` and the index type**

Add to `discovery.rs` (above the `#[cfg(test)]` block):

```rust
use std::collections::BTreeMap;

/// Final per-name index, after applying precedence rules across all four
/// search paths.
#[derive(Debug, Default)]
pub struct Index {
    /// Map from namespaced command name to its winning entry.
    pub commands: BTreeMap<String, Discovered>,
    /// Aggregated warnings, in the order they were produced.
    pub warnings: Vec<String>,
}

/// Walk all four directories with precedence: project-savvagent >
/// project-claude > user-savvagent > user-claude. First hit per name
/// wins; later hits at lower precedence are silently dropped.
pub fn walk_all(project_root: &Path, home: &Path) -> Index {
    let layers = [
        (
            project_root.join(".savvagent").join("commands"),
            Origin::ProjectSavvagent,
        ),
        (
            project_root.join(".claude").join("commands"),
            Origin::ProjectClaude,
        ),
        (
            home.join(".savvagent").join("commands"),
            Origin::UserSavvagent,
        ),
        (home.join(".claude").join("commands"), Origin::UserClaude),
    ];
    let mut index = Index::default();
    for (root, origin) in layers {
        let (found, warns) = walk_one(&root, origin);
        index.warnings.extend(warns);
        for d in found {
            index.commands.entry(d.name.clone()).or_insert(d);
        }
    }
    index
}
```

- [ ] **Step 3: Run the new test**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::discovery::tests::precedence_project_over_user_and_savvagent_over_claude
```

Expected: PASS.

- [ ] **Step 4: Run all discovery tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::discovery::tests
```

Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/discovery.rs
git commit -m "feat(plugin/user-slash-commands): four-path precedence walk"
```

---

### Task 6: Trust file round-trip

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_slash_commands/trust.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs` (add `mod trust;`)

- [ ] **Step 1: Write the failing tests + impl**

In `crates/savvagent/src/plugin/builtin/user_slash_commands/trust.rs`:

```rust
//! Loads and saves `~/.savvagent/trusted-projects.json` — the persistent
//! store of "always trust this project's commands" decisions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Trust level for a given project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
    /// User chose "trust always" — persisted to disk.
    Always,
    /// User chose "block shell, allow text-only this session" —
    /// in-memory only; not persisted.
    SessionTextOnly,
    /// User cancelled the prompt — dispatch aborted.
    Cancelled,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileSchema {
    #[serde(default)]
    projects: BTreeMap<String, String>,
}

/// File path the trust store lives at. `home` is the user's home dir
/// (caller supplies it so the function is testable).
pub fn trust_file_path(home: &Path) -> PathBuf {
    home.join(".savvagent").join("trusted-projects.json")
}

/// Load the persisted trust set. Missing or malformed files return an
/// empty set with a warning; never panics.
pub fn load(home: &Path) -> (BTreeMap<PathBuf, TrustLevel>, Option<String>) {
    let path = trust_file_path(home);
    if !path.exists() {
        return (BTreeMap::new(), None);
    }
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return (BTreeMap::new(), Some(format!("trust file unreadable: {e}"))),
    };
    let parsed: FileSchema = match serde_json::from_str(&contents) {
        Ok(p) => p,
        Err(e) => return (BTreeMap::new(), Some(format!("trust file malformed: {e}"))),
    };
    let mut out = BTreeMap::new();
    for (k, v) in parsed.projects {
        if v == "always" {
            out.insert(PathBuf::from(k), TrustLevel::Always);
        }
    }
    (out, None)
}

/// Persist the `Always` entries to disk. `SessionTextOnly` and
/// `Cancelled` are not stored.
pub fn save(
    home: &Path,
    levels: &BTreeMap<PathBuf, TrustLevel>,
) -> Result<(), String> {
    let mut schema = FileSchema::default();
    for (k, v) in levels {
        if matches!(v, TrustLevel::Always) {
            if let Some(s) = k.to_str() {
                schema.projects.insert(s.to_string(), "always".into());
            }
        }
    }
    let path = trust_file_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    let body = serde_json::to_string_pretty(&schema)
        .map_err(|e| format!("serialize trust file: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("write trust file: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_when_file_absent() {
        let tmp = TempDir::new().unwrap();
        let (m, warn) = load(tmp.path());
        assert!(m.is_empty());
        assert!(warn.is_none());
    }

    #[test]
    fn round_trip_persists_only_always() {
        let tmp = TempDir::new().unwrap();
        let mut input = BTreeMap::new();
        input.insert(PathBuf::from("/proj/a"), TrustLevel::Always);
        input.insert(PathBuf::from("/proj/b"), TrustLevel::SessionTextOnly);
        save(tmp.path(), &input).unwrap();

        let (loaded, warn) = load(tmp.path());
        assert!(warn.is_none());
        assert_eq!(loaded.get(&PathBuf::from("/proj/a")), Some(&TrustLevel::Always));
        assert!(!loaded.contains_key(&PathBuf::from("/proj/b")));
    }

    #[test]
    fn malformed_file_returns_empty_with_warning() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".savvagent")).unwrap();
        std::fs::write(trust_file_path(tmp.path()), "{ not json").unwrap();
        let (m, warn) = load(tmp.path());
        assert!(m.is_empty());
        assert!(warn.is_some());
    }
}
```

- [ ] **Step 2: Declare the module**

Add to `mod.rs`:

```rust
mod trust;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::trust::tests
```

Expected: 3 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/trust.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs
git commit -m "feat(plugin/user-slash-commands): trusted-projects.json round-trip"
```

---

### Task 7: Template expansion — `$ARGUMENTS` and `$N`

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs` (add `mod template;`)

- [ ] **Step 1: Write the failing tests + impl**

In `crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs`:

```rust
//! Single-pass templating expansion for command bodies.

/// Outcome of expanding a command body.
#[derive(Debug, Clone, Default)]
pub struct Expanded {
    /// The rendered prompt text.
    pub text: String,
    /// Non-fatal warnings emitted during expansion.
    pub warnings: Vec<String>,
}

/// Substitute `$ARGUMENTS` and `$1`/`$2`/… in `body`.
///
/// `$ARGUMENTS` becomes the raw argument string (`args.join(" ")`).
/// `$N` substitutions reference whitespace-split positional args;
/// out-of-range positions expand to the empty string.
pub fn expand_args(body: &str, args: &[String]) -> String {
    let raw = args.join(" ");
    let mut out = body.replace("$ARGUMENTS", &raw);
    // Replace $1..$9 explicitly to avoid greedy '$10' confusion in v1.
    for (idx, a) in args.iter().take(9).enumerate() {
        out = out.replace(&format!("${}", idx + 1), a);
    }
    // Strip leftover $1..$9 tokens that referenced out-of-range positions.
    for n in (args.len() + 1)..=9 {
        out = out.replace(&format!("${n}"), "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_token() {
        assert_eq!(expand_args("hello $ARGUMENTS", &s(&["a", "b"])), "hello a b");
    }

    #[test]
    fn positional() {
        assert_eq!(
            expand_args("first=$1 second=$2", &s(&["foo", "bar"])),
            "first=foo second=bar"
        );
    }

    #[test]
    fn out_of_range_is_empty() {
        assert_eq!(expand_args("[$3]", &s(&["foo"])), "[]");
    }

    #[test]
    fn no_args_is_identity_modulo_blanking_positionals() {
        assert_eq!(expand_args("plain body", &[]), "plain body");
        assert_eq!(expand_args("hi $1", &[]), "hi ");
    }
}
```

- [ ] **Step 2: Declare the module**

Add to `mod.rs`:

```rust
mod template;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::template::tests
```

Expected: 4 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs
git commit -m "feat(plugin/user-slash-commands): \$ARGUMENTS and \$N expansion"
```

---

### Task 8: Template expansion — `@<path>` file inclusion

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs`

- [ ] **Step 1: Add the failing test**

Append to the existing `tests` module in `template.rs`:

```rust
    #[test]
    fn at_path_inlines_file_contents() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "INSIDE").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let body = format!("before\n@{path}\nafter");
        let exp = expand_files(&body);
        assert!(exp.text.contains("INSIDE"));
        assert!(exp.warnings.is_empty());
    }

    #[test]
    fn at_path_missing_warns_and_keeps_literal() {
        let body = "see @/no/such/file/exists.txt please";
        let exp = expand_files(body);
        assert!(exp.text.contains("@/no/such/file/exists.txt"));
        assert_eq!(exp.warnings.len(), 1);
    }
```

- [ ] **Step 2: Implement `expand_files`**

Add to `template.rs` (above the `#[cfg(test)]` block):

```rust
/// Expand `@<path>` tokens by inlining the file contents.
///
/// Token shape: `@` followed by a contiguous run of non-whitespace,
/// non-`@` chars. Missing files leave the literal in place and emit a
/// warning. Single-pass: included files are *not* re-expanded.
pub fn expand_files(body: &str) -> Expanded {
    let mut out = String::with_capacity(body.len());
    let mut warnings = Vec::new();
    let mut chars = body.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '@' {
            out.push(c);
            continue;
        }
        // Look back: avoid eating an email-ish `name@host`.
        let prev = body[..i].chars().rev().next();
        if let Some(p) = prev {
            if !p.is_whitespace() && p != '\n' && p != '(' && p != '[' && p != '{' && p != ',' && p != '\'' && p != '"' {
                out.push('@');
                continue;
            }
        }
        let start = i + 1;
        let mut end = start;
        for (j, ch) in body[start..].char_indices() {
            if ch.is_whitespace() {
                end = start + j;
                break;
            }
            end = start + j + ch.len_utf8();
        }
        let path = &body[start..end];
        if path.is_empty() {
            out.push('@');
            continue;
        }
        // Advance the outer iterator past `end`.
        while let Some(&(idx, _)) = chars.peek() {
            if idx >= end {
                break;
            }
            chars.next();
        }
        match std::fs::read_to_string(path) {
            Ok(contents) => out.push_str(&contents),
            Err(_) => {
                warnings.push(format!("@{path}: file not found"));
                out.push('@');
                out.push_str(path);
            }
        }
    }
    Expanded { text: out, warnings }
}
```

- [ ] **Step 3: Add `tempfile` to dev-dependencies if not already there (done in Task 4)**

Verify `[dev-dependencies] tempfile = "3"` is in `crates/savvagent/Cargo.toml`.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::template::tests
```

Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs
git commit -m "feat(plugin/user-slash-commands): @<path> file inclusion"
```

---

### Task 9: Template expansion — `!<cmd>` shell substitution

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs`

- [ ] **Step 1: Add failing tests**

Append to the existing `tests` module in `template.rs`:

```rust
    #[tokio::test]
    async fn shell_substitution_inlines_stdout() {
        let exp = expand_shell("hello\n!echo from-shell\nworld").await.unwrap();
        assert!(exp.text.contains("from-shell"));
        assert!(exp.warnings.is_empty());
    }

    #[tokio::test]
    async fn shell_substitution_nonzero_exit_is_error() {
        let err = expand_shell("!false").await.unwrap_err();
        assert!(err.contains("exit"));
    }

    #[tokio::test]
    async fn shell_substitution_inline_form() {
        let exp = expand_shell("before !`echo X` after").await.unwrap();
        assert!(exp.text.contains("X"));
    }
```

- [ ] **Step 2: Implement `expand_shell`**

Add to `template.rs` (above `#[cfg(test)]`):

```rust
use tokio::process::Command;

/// Expand `!<cmd>` tokens by running the shell and inlining stdout.
///
/// Two forms accepted:
/// - Line-leading: a line starting with `!` (whitespace allowed before)
///   treats the rest of the line as the command.
/// - Inline: `` !`cmd` `` (a `!` followed by a backtick-delimited
///   command) substitutes stdout in place.
///
/// Non-zero exit aborts expansion and returns `Err(stderr)`. The caller
/// surfaces the error in the conversation log and does not submit the
/// prompt.
pub async fn expand_shell(body: &str) -> Result<Expanded, String> {
    let mut out = String::new();
    let warnings: Vec<String> = Vec::new();

    // Pass 1: inline backtick form `!` + `` `cmd` ``.
    let mut cursor = 0usize;
    while let Some(pos) = body[cursor..].find("!`") {
        let abs = cursor + pos;
        out.push_str(&body[cursor..abs]);
        let cmd_start = abs + 2;
        let Some(close) = body[cmd_start..].find('`') else {
            // Unmatched backtick — leave the rest as-is.
            out.push_str(&body[abs..]);
            cursor = body.len();
            break;
        };
        let cmd = &body[cmd_start..cmd_start + close];
        let stdout = run_shell(cmd).await?;
        out.push_str(&stdout);
        cursor = cmd_start + close + 1;
    }
    out.push_str(&body[cursor..]);

    // Pass 2: line-leading `!cmd` form.
    let mut final_out = String::new();
    for line in out.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(cmd) = trimmed.strip_prefix('!') {
            let cmd = cmd.trim_end_matches('\n').trim_end_matches('\r');
            if cmd.is_empty() {
                final_out.push_str(line);
                continue;
            }
            let stdout = run_shell(cmd).await?;
            final_out.push_str(&stdout);
            if !stdout.ends_with('\n') && line.ends_with('\n') {
                final_out.push('\n');
            }
        } else {
            final_out.push_str(line);
        }
    }
    Ok(Expanded { text: final_out, warnings })
}

async fn run_shell(cmd: &str) -> Result<String, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
        .map_err(|e| format!("!{cmd}: spawn failed: {e}"))?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("!{cmd}: exited {code} — {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::template::tests
```

Expected: 9 PASS (4 args + 2 file + 3 shell).

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs
git commit -m "feat(plugin/user-slash-commands): !<cmd> shell substitution"
```

---

### Task 10: Combined `expand_all` orchestration

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs`

- [ ] **Step 1: Add the failing test**

Append to `template.rs` `tests` module:

```rust
    #[tokio::test]
    async fn expand_all_runs_in_order() {
        let body = "hello $ARGUMENTS\n!echo SHELL\n@/no/such/file";
        let out = expand_all(body, &s(&["world"]), TrustLevel::Always)
            .await
            .unwrap();
        assert!(out.text.contains("hello world"));
        assert!(out.text.contains("SHELL"));
        assert!(out.text.contains("@/no/such/file"));
        assert_eq!(out.warnings.len(), 1);
    }

    #[tokio::test]
    async fn session_text_only_skips_shell_with_error() {
        let body = "!echo X";
        let err = expand_all(body, &[], TrustLevel::SessionTextOnly)
            .await
            .unwrap_err();
        assert!(err.contains("shell substitution disabled"));
    }
```

- [ ] **Step 2: Implement `expand_all`**

Add to `template.rs`:

```rust
use crate::plugin::builtin::user_slash_commands::trust::TrustLevel;

/// Whether the body contains any `!<cmd>` token. Used by the dispatcher
/// to decide whether to invoke the trust check.
pub fn contains_shell_token(body: &str) -> bool {
    if body.contains("!`") {
        return true;
    }
    body.lines()
        .any(|l| l.trim_start().starts_with('!') && !l.trim_start().starts_with("!="))
}

/// Run all expansion passes in order: `$ARGUMENTS`/`$N` → `@<path>` →
/// `!<cmd>`. Single-pass: included files are not re-expanded.
pub async fn expand_all(
    body: &str,
    args: &[String],
    trust: TrustLevel,
) -> Result<Expanded, String> {
    let with_args = expand_args(body, args);
    let files = expand_files(&with_args);
    let has_shell = contains_shell_token(&files.text);
    let mut warnings = files.warnings;
    let shell_text = match trust {
        TrustLevel::Always => {
            let exp = expand_shell(&files.text).await?;
            warnings.extend(exp.warnings);
            exp.text
        }
        TrustLevel::SessionTextOnly => {
            if has_shell {
                return Err(
                    "shell substitution disabled for this session (trust=session-text-only)"
                        .into(),
                );
            }
            files.text
        }
        TrustLevel::Cancelled => {
            return Err("dispatch aborted: user cancelled trust prompt".into());
        }
    };
    Ok(Expanded {
        text: shell_text,
        warnings,
    })
}
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::template::tests
```

Expected: 11 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/template.rs
git commit -m "feat(plugin/user-slash-commands): expand_all orchestration"
```

---

### Task 11: New `Effect` variants (`SetNextTurnModelOverride`, `SetTrustLevel`, `ReindexPlugin`)

**Files:**
- Modify: `crates/savvagent-plugin/src/effect.rs`

- [ ] **Step 1: Add the variants**

At the bottom of the `Effect` enum in `crates/savvagent-plugin/src/effect.rs` (before the closing `}` — the enum is `#[non_exhaustive]` so adding variants is non-breaking):

```rust
    /// Override the model used by the next *single* turn submitted via
    /// [`Effect::PromptSend`]. Cleared after the turn completes. Used by
    /// user-defined slash commands whose frontmatter contains `model:`.
    SetNextTurnModelOverride {
        /// Bare model id (e.g. `"claude-sonnet-4-6"`). The runtime looks
        /// the id up against the active provider's catalog; unknown ids
        /// are warn-logged and ignored, leaving the active model in
        /// place.
        id: String,
    },
    /// Result of a trust prompt. Emitted by the trust modal screen and
    /// consumed by the runtime to update the in-memory trust map (and
    /// to persist `Always` decisions to
    /// `~/.savvagent/trusted-projects.json`). When applied, the runtime
    /// resumes the slash command that triggered the prompt (stored on
    /// `App::pending_slash_after_trust`).
    SetTrustLevel {
        /// Canonical project root path the decision applies to.
        project_root: std::path::PathBuf,
        /// User's choice: `"always"`, `"session-text-only"`, or
        /// `"cancelled"`. Kept as a string here to avoid pulling the
        /// runtime's `TrustLevel` enum into the WIT-portable surface.
        decision: String,
    },
    /// Re-call `Plugin::manifest()` for the named plugin and rebuild the
    /// derived manifest indexes (slash commands, render slots, hooks,
    /// keybindings) from the updated bundle. Used by
    /// `/reload-commands` so user-defined commands edited on disk show
    /// up without restarting the TUI.
    ReindexPlugin {
        /// Plugin whose manifest should be re-read.
        id: crate::types::PluginId,
    },
```

- [ ] **Step 2: Add construction-smoke tests**

At the bottom of `effect.rs`:

```rust
#[cfg(test)]
mod added_effects_smoke {
    use super::*;
    use crate::types::PluginId;
    use std::path::PathBuf;

    #[test]
    fn constructable() {
        let _ = Effect::SetNextTurnModelOverride {
            id: "x".into(),
        };
        let _ = Effect::SetTrustLevel {
            project_root: PathBuf::from("/p"),
            decision: "always".into(),
        };
        let _ = Effect::ReindexPlugin {
            id: PluginId::new("internal:user-slash-commands").unwrap(),
        };
    }
}
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p savvagent-plugin --lib effect::added_effects_smoke
```

Expected: 1 PASS.

- [ ] **Step 4: Confirm the workspace still builds (apply_effects will need updating in later tasks; for now any `match` arms must compile due to `#[non_exhaustive]` external-side)**

```bash
cargo build --workspace
```

Expected: success. If `apply_effects` in `crates/savvagent/src/plugin/effects.rs` has an exhaustive match without a wildcard, the build fails here — that's the trigger to add the `_ => {}` arm or proceed to Task 13 immediately.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-plugin/src/effect.rs
git commit -m "feat(plugin/effect): SetNextTurnModelOverride, SetTrustLevel, ReindexPlugin"
```

---

### Task 12: `App` field additions for override + pending dispatch

**Files:**
- Modify: `crates/savvagent/src/app.rs`

- [ ] **Step 1: Locate the `App` struct**

Open `crates/savvagent/src/app.rs`. The `App` struct is around line 380-700 (it's large). Find an appropriate clustering of optional fields (look for similar `Option<…>` fields, e.g. `pending_model_change` referenced from `effects.rs:925`).

- [ ] **Step 2: Add the two new fields**

Insert near the other `pending_*` fields:

```rust
    /// One-turn model override populated by
    /// [`savvagent_plugin::Effect::SetNextTurnModelOverride`] and consumed
    /// by the worker spawn at the start of the next turn. `None` means
    /// "use the provider's currently-active model."
    pub next_turn_model_override: Option<String>,
    /// `(command_name, args)` that should re-dispatch after the trust
    /// modal resolves. Set by the user-slash-commands plugin before
    /// emitting `Effect::OpenScreen("trust_modal")`; cleared by
    /// `apply_effects` after the re-dispatch (or on cancel).
    pub pending_slash_after_trust: Option<(String, Vec<String>)>,
```

- [ ] **Step 3: Initialize them**

Find the `App::new`-style constructor (`impl App { pub fn new(...) ... }`). Add to the struct literal:

```rust
            next_turn_model_override: None,
            pending_slash_after_trust: None,
```

- [ ] **Step 4: Confirm the workspace builds**

```bash
cargo build --workspace
```

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/app.rs
git commit -m "feat(app): next_turn_model_override + pending_slash_after_trust fields"
```

---

### Task 13: `apply_effects` arm for `SetNextTurnModelOverride`

**Files:**
- Modify: `crates/savvagent/src/plugin/effects.rs`

- [ ] **Step 1: Locate the dispatch site**

In `crates/savvagent/src/plugin/effects.rs`, find the existing `Effect::PromptSend` arm at line 215 (`Effect::PromptSend { text } => app.submit_prompt(text),`). The other arms surround it.

- [ ] **Step 2: Add the new arm**

Insert near `Effect::SetActiveModel` (around line 88):

```rust
        Effect::SetNextTurnModelOverride { id } => {
            app.next_turn_model_override = Some(id);
        }
```

- [ ] **Step 3: Write a unit test**

In the existing test module at the bottom of `effects.rs`, add:

```rust
    #[tokio::test]
    async fn set_next_turn_model_override_writes_field() {
        use savvagent_plugin::Effect;
        let mut app = test_app();
        let effs = vec![Effect::SetNextTurnModelOverride {
            id: "claude-sonnet-4-6".into(),
        }];
        apply_effects(&mut app, effs);
        assert_eq!(
            app.next_turn_model_override.as_deref(),
            Some("claude-sonnet-4-6")
        );
    }
```

(`test_app()` is the existing helper used by neighbor tests — search the file for `fn test_app(` to confirm its name; if it's named differently, use that name.)

- [ ] **Step 4: Run the test**

```bash
cargo test -p savvagent --lib plugin::effects::tests::set_next_turn_model_override_writes_field
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/effects.rs
git commit -m "feat(plugin/effects): apply SetNextTurnModelOverride"
```

---

### Task 14: Consume the model override in the worker spawn

**Files:**
- Modify: `crates/savvagent/src/app.rs` (the worker spawn path)
- Modify: `crates/savvagent/src/tui.rs` (if the spawn site lives there — confirm by grepping for `submit_prompt`)

- [ ] **Step 1: Locate the turn-spawn site**

```bash
rg -n "submit_prompt\|spawn_turn\|run_turn_streaming" crates/savvagent/src/app.rs crates/savvagent/src/tui.rs | head -10
```

Identify where the worker task is spawned and the host's `complete`/`run_turn_streaming` is invoked.

- [ ] **Step 2: Read and consume the override**

Just before the worker spawn, take the override out of `App`:

```rust
let model_override = self.next_turn_model_override.take();
```

Pass `model_override` into the worker task. If the host's API exposes a per-turn model selection, plumb it through there; otherwise (most likely case) call `host.set_model(model_override)` before the turn and restore the previous model after — but **only** if the override is `Some`. Inspect `crates/savvagent-host/src/lib.rs` for the actual API; adapt the call to fit the existing pattern.

- [ ] **Step 3: Write an integration test**

If the host exposes a `set_model` or per-turn override knob, mock or assert that consuming the override clears the field and applies it. If no such API exists yet, this task ships only the take-and-clear behavior (the override is consumed; missing host-side wiring becomes a follow-up issue). Document the gap in the commit message.

- [ ] **Step 4: Build and run the existing test suite to confirm no regression**

```bash
cargo test -p savvagent
```

Expected: existing tests still pass; any new test added in Step 3 also passes.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/app.rs crates/savvagent/src/tui.rs
git commit -m "feat(app): consume next_turn_model_override on worker spawn"
```

---

### Task 15: `apply_effects` arm for `ReindexPlugin`

**Files:**
- Modify: `crates/savvagent/src/plugin/effects.rs`
- Reference: `crates/savvagent/src/plugin/manifests.rs::Indexes::build`

- [ ] **Step 1: Add a method to rebuild a single plugin's contributions**

The cleanest path is a new method on the runtime's manifest index. Read `crates/savvagent/src/plugin/manifests.rs` and find `Indexes::build`. Add a sibling method:

```rust
    /// Re-call `Plugin::manifest()` for `plugin_id` and replace this
    /// plugin's contributions in every derived index (slash, slots,
    /// hooks, keybindings, screens, tool_summaries). Other plugins are
    /// untouched.
    pub fn reindex_plugin(
        &mut self,
        plugin_id: &savvagent_plugin::PluginId,
        registry: &crate::plugin::registry::PluginRegistry,
    ) {
        // Remove existing entries owned by plugin_id from each index.
        self.slash_commands
            .retain(|_, owner| owner != plugin_id);
        // Repeat for slots, hooks, keybindings, screens, tool_summaries…
        // Mirror the lookup-by-owner pattern already used in `build`.
        //
        // Then call the plugin's manifest() and re-insert its
        // contributions exactly as Indexes::build would.
        if let Some(plugin) = registry.get(plugin_id) {
            let m = plugin.manifest();
            // … same insertion logic Indexes::build uses, factored if
            // needed.
        }
    }
```

Open `Indexes::build` and factor the per-plugin insertion into a helper that both `build` and `reindex_plugin` can call. (If `build` already loops `for plugin in registry { … }`, lift the loop body into `fn insert_one(&mut self, plugin: &dyn Plugin)`.)

- [ ] **Step 2: Add the effect arm**

In `crates/savvagent/src/plugin/effects.rs`, near the existing slash-related arms:

```rust
        Effect::ReindexPlugin { id } => {
            app.manifest_indexes
                .reindex_plugin(&id, &app.plugin_registry);
        }
```

(Names `manifest_indexes` and `plugin_registry` are placeholders — use whatever the existing arms reference. Search the file for `.indexes` / `.registry` to find the correct identifiers.)

- [ ] **Step 3: Write a test**

In `effects.rs` tests module:

```rust
    #[tokio::test]
    async fn reindex_plugin_rebuilds_slash_index() {
        // Build a test app with the user_slash_commands plugin pre-registered.
        // Mutate the plugin's discovery cache to remove a command.
        // Apply Effect::ReindexPlugin and assert the slash index no longer
        // contains that command.
        // (Full impl depends on existing test helpers; mirror the
        //  set_active_model test in the same module.)
    }
```

If the existing test helpers don't expose enough seams to write this concretely, leave a `#[ignore]` test with a `// TODO: needs test seam` body and open a follow-up issue. Don't block the plan on it.

- [ ] **Step 4: Build and run**

```bash
cargo test -p savvagent
```

Expected: existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/effects.rs crates/savvagent/src/plugin/manifests.rs
git commit -m "feat(plugin/effects): apply ReindexPlugin via Indexes::reindex_plugin"
```

---

### Task 16: Trust modal `Screen` impl

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/user_slash_commands/trust_modal.rs`
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs` (add `mod trust_modal;`)

- [ ] **Step 1: Read an existing modal Screen for the pattern**

```bash
ls crates/savvagent/src/plugin/builtin/themes/
```

Pick the smallest modal `Screen` impl (e.g. theme picker) as the template.

- [ ] **Step 2: Write the Screen impl**

In `trust_modal.rs`:

```rust
//! First-run trust modal: `y` / `n` / `q` decision for project-local
//! command directories that include shell substitution.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, ScreenArgs,
    StyledLine, StyledSpan,
};
use std::path::PathBuf;

/// The trust modal pushed onto the screen stack via
/// `Effect::OpenScreen { id: "trust_modal", args: ScreenArgs::Path(_) }`.
pub struct TrustModal {
    project_root: PathBuf,
}

impl TrustModal {
    /// Construct from `ScreenArgs::Path` carrying the project root path.
    pub fn from_args(args: ScreenArgs) -> Result<Self, PluginError> {
        match args {
            ScreenArgs::Path(p) => Ok(Self { project_root: p }),
            _ => Err(PluginError::ScreenNotFound("trust_modal".into())),
        }
    }
}

#[async_trait]
impl Screen for TrustModal {
    fn id(&self) -> &str {
        "trust_modal"
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let mut lines = vec![StyledLine {
            spans: vec![StyledSpan::plain(
                "This project ships commands under .savvagent/commands/ and .claude/commands/.",
            )],
        }];
        lines.push(StyledLine {
            spans: vec![StyledSpan::plain(
                "Some of them may run shell commands. Trust this project?",
            )],
        });
        lines.push(StyledLine { spans: vec![] });
        lines.push(StyledLine {
            spans: vec![StyledSpan::plain(
                "  [y] Trust always",
            )],
        });
        lines.push(StyledLine {
            spans: vec![StyledSpan::plain(
                "  [n] Block shell, allow text-only this session",
            )],
        });
        lines.push(StyledLine {
            spans: vec![StyledSpan::plain("  [q] Cancel")],
        });
        lines
    }

    async fn on_key(
        &mut self,
        ev: KeyEventPortable,
    ) -> Result<Vec<Effect>, PluginError> {
        let decision = match ev.code {
            KeyCodePortable::Char('y') | KeyCodePortable::Char('Y') => Some("always"),
            KeyCodePortable::Char('n') | KeyCodePortable::Char('N') => {
                Some("session-text-only")
            }
            KeyCodePortable::Char('q')
            | KeyCodePortable::Char('Q')
            | KeyCodePortable::Esc => Some("cancelled"),
            _ => None,
        };
        match decision {
            Some(d) => Ok(vec![
                Effect::SetTrustLevel {
                    project_root: self.project_root.clone(),
                    decision: d.into(),
                },
                Effect::CloseScreen,
            ]),
            None => Ok(vec![]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modal() -> TrustModal {
        TrustModal {
            project_root: PathBuf::from("/proj/x"),
        }
    }

    fn key(c: char) -> KeyEventPortable {
        KeyEventPortable {
            code: KeyCodePortable::Char(c),
            mods: savvagent_plugin::KeyMods::default(),
        }
    }

    #[tokio::test]
    async fn y_returns_always() {
        let mut m = modal();
        let effs = m.on_key(key('y')).await.unwrap();
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => assert_eq!(decision, "always"),
            _ => panic!("expected SetTrustLevel first"),
        }
        assert!(matches!(effs[1], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn n_returns_session_text_only() {
        let mut m = modal();
        let effs = m.on_key(key('n')).await.unwrap();
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => {
                assert_eq!(decision, "session-text-only")
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn q_returns_cancelled() {
        let mut m = modal();
        let effs = m.on_key(key('q')).await.unwrap();
        match &effs[0] {
            Effect::SetTrustLevel { decision, .. } => assert_eq!(decision, "cancelled"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn unrelated_key_is_noop() {
        let mut m = modal();
        let effs = m.on_key(key('z')).await.unwrap();
        assert!(effs.is_empty());
    }
}
```

If `StyledSpan::plain` or `KeyEventPortable`/`KeyMods::default()` don't match the exact public API, check `crates/savvagent-plugin/src/styled.rs` and `types.rs` and adjust. The intent is what matters: three lines of decision text and a key handler that maps y/n/q.

- [ ] **Step 3: Declare the module + contribute the ScreenSpec**

Add to `mod.rs`:

```rust
mod trust_modal;
```

And update `UserSlashCommandsPlugin::manifest()`'s `contributions` to register the screen:

```rust
        contributions.screens = vec![savvagent_plugin::ScreenSpec {
            id: "trust_modal".into(),
            layout: savvagent_plugin::ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 30,
                title: Some("Trust project commands?".into()),
            },
        }];
```

And implement `Plugin::create_screen` on `UserSlashCommandsPlugin`:

```rust
    fn create_screen(
        &self,
        id: &str,
        args: savvagent_plugin::ScreenArgs,
    ) -> Result<Box<dyn savvagent_plugin::Screen>, PluginError> {
        match id {
            "trust_modal" => Ok(Box::new(trust_modal::TrustModal::from_args(args)?)),
            _ => Err(PluginError::ScreenNotFound(id.into())),
        }
    }
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::trust_modal::tests
```

Expected: 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/trust_modal.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs
git commit -m "feat(plugin/user-slash-commands): trust-prompt modal screen"
```

---

### Task 17: `apply_effects` arm for `SetTrustLevel`

**Files:**
- Modify: `crates/savvagent/src/plugin/effects.rs`
- Modify: `crates/savvagent/src/app.rs` (add an in-memory trust map field)

- [ ] **Step 1: Add the in-memory map to `App`**

In `app.rs`, near `pending_slash_after_trust`:

```rust
    /// In-memory trust state for the session. Loaded from
    /// `~/.savvagent/trusted-projects.json` at startup; `Always`
    /// decisions persist back to that file via `Effect::SetTrustLevel`.
    pub trust_levels:
        std::collections::BTreeMap<std::path::PathBuf, savvagent_plugin::Effect>,
```

Actually use the concrete `TrustLevel` type, not `Effect`. Replace with:

```rust
    pub trust_levels: std::collections::BTreeMap<
        std::path::PathBuf,
        crate::plugin::builtin::user_slash_commands::trust::TrustLevel,
    >,
```

Initialize in the constructor:

```rust
            trust_levels: std::collections::BTreeMap::new(),
```

- [ ] **Step 2: Load trust at startup**

In `App::new` (or wherever `home_dir()` is resolved), after constructing the App:

```rust
let (loaded, warn) = crate::plugin::builtin::user_slash_commands::trust::load(&home);
if let Some(w) = warn {
    tracing::warn!("user-slash-commands: {w}");
}
app.trust_levels = loaded;
```

- [ ] **Step 3: Add the effect arm**

In `effects.rs`:

```rust
        Effect::SetTrustLevel {
            project_root,
            decision,
        } => {
            use crate::plugin::builtin::user_slash_commands::trust::{self, TrustLevel};
            let level = match decision.as_str() {
                "always" => TrustLevel::Always,
                "session-text-only" => TrustLevel::SessionTextOnly,
                _ => TrustLevel::Cancelled,
            };
            if matches!(level, TrustLevel::Cancelled) {
                app.trust_levels.remove(&project_root);
                app.pending_slash_after_trust = None;
            } else {
                app.trust_levels.insert(project_root.clone(), level);
                if matches!(level, TrustLevel::Always) {
                    if let Some(home) = dirs::home_dir() {
                        if let Err(e) = trust::save(&home, &app.trust_levels) {
                            tracing::warn!("trust file save: {e}");
                        }
                    }
                }
                // Re-dispatch the pending slash command, if any.
                if let Some((name, args)) = app.pending_slash_after_trust.take() {
                    // Use the existing slash router; fire-and-forget the
                    // resulting Effects via apply_effects in the next
                    // event loop tick (mirror the pattern used by
                    // `Effect::RunSlash` at <line N>).
                    return_effects.push(Effect::RunSlash { name, args });
                }
            }
        }
```

(`return_effects` is the typical name for the secondary effects buffer in `apply_effects`. If the existing dispatcher works differently, adapt — the goal is "after applying SetTrustLevel, re-issue the pending command.")

- [ ] **Step 4: Write a test**

In `effects.rs` tests:

```rust
    #[tokio::test]
    async fn set_trust_level_always_persists_and_resumes() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Override HOME for the duration of the test.
        let _g = crate::test_utils::HOME_LOCK.lock().await;
        std::env::set_var("HOME", tmp.path());
        let mut app = test_app();
        app.pending_slash_after_trust = Some(("review".into(), vec!["foo".into()]));

        apply_effects(
            &mut app,
            vec![Effect::SetTrustLevel {
                project_root: PathBuf::from("/proj/x"),
                decision: "always".into(),
            }],
        );

        // Persisted.
        let (loaded, _) = crate::plugin::builtin::user_slash_commands::trust::load(tmp.path());
        assert!(loaded.contains_key(&PathBuf::from("/proj/x")));
        // Resumed.
        assert!(app.pending_slash_after_trust.is_none());
    }
```

If `crate::test_utils::HOME_LOCK` doesn't exist, search for `HOME_LOCK` in the codebase and use the right path. Acquire it under tokio with `.lock().await`.

- [ ] **Step 5: Run**

```bash
cargo test -p savvagent --lib plugin::effects::tests::set_trust_level_always_persists_and_resumes
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/effects.rs crates/savvagent/src/app.rs
git commit -m "feat(plugin/effects): apply SetTrustLevel with persistence and resume"
```

---

### Task 18: Plugin `manifest()` — synchronous initial discovery

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs`

- [ ] **Step 1: Add the cached discovery field**

Replace the unit struct with a stateful one:

```rust
use once_cell::sync::OnceCell;
use std::path::PathBuf;

use crate::plugin::builtin::user_slash_commands::discovery::{Index, walk_all};

pub struct UserSlashCommandsPlugin {
    project_root: PathBuf,
    home: PathBuf,
    cache: OnceCell<Index>,
}

impl UserSlashCommandsPlugin {
    pub fn new() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            project_root,
            home,
            cache: OnceCell::new(),
        }
    }

    /// Override the search roots; used by tests.
    #[cfg(test)]
    pub fn with_roots(project_root: PathBuf, home: PathBuf) -> Self {
        Self {
            project_root,
            home,
            cache: OnceCell::new(),
        }
    }

    fn index(&self) -> &Index {
        self.cache
            .get_or_init(|| walk_all(&self.project_root, &self.home))
    }
}
```

Add `once_cell = "1"` and `dirs = "5"` to `crates/savvagent/Cargo.toml` if not already there (likely already present — check first).

- [ ] **Step 2: Build dynamic `SlashSpec` list in `manifest()`**

Replace the existing `manifest()` body:

```rust
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        // Static: /reload-commands.
        contributions.slash_commands.push(SlashSpec {
            name: "reload-commands".into(),
            summary: "Rescan user-defined slash command directories".into(),
            args_hint: None,
            requires_arg: false,
        });
        // Dynamic: one per discovered command.
        for d in self.index().commands.values() {
            let summary = d
                .frontmatter
                .description
                .clone()
                .unwrap_or_else(|| d.path.display().to_string());
            contributions.slash_commands.push(SlashSpec {
                name: d.name.clone(),
                summary,
                args_hint: d.frontmatter.argument_hint.clone(),
                requires_arg: false,
            });
        }
        // Static: trust modal screen.
        contributions.screens = vec![savvagent_plugin::ScreenSpec {
            id: "trust_modal".into(),
            layout: savvagent_plugin::ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 30,
                title: Some("Trust project commands?".into()),
            },
        }];
        Manifest {
            id: PluginId::new("internal:user-slash-commands").expect("valid built-in id"),
            name: "User slash commands".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "User-defined commands from .savvagent/commands/ and .claude/commands/"
                .into(),
            kind: PluginKind::Core,
            contributions,
        }
    }
```

- [ ] **Step 3: Write a test**

```rust
    #[test]
    fn manifest_includes_discovered_commands() {
        use std::fs;
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("review.md"),
            "---\ndescription: Review the diff\n---\nbody",
        )
        .unwrap();

        let p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
        );
        let m = p.manifest();
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-commands"));
        assert!(names.contains(&"review"));
        let review = m
            .contributions
            .slash_commands
            .iter()
            .find(|s| s.name == "review")
            .unwrap();
        assert_eq!(review.summary, "Review the diff");
    }
```

- [ ] **Step 4: Run**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::tests::manifest_includes_discovered_commands
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs \
        crates/savvagent/Cargo.toml
git commit -m "feat(plugin/user-slash-commands): manifest contributes discovered commands"
```

---

### Task 19: Plugin `handle_slash` — main dispatch (no trust check yet)

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs`

- [ ] **Step 1: Implement `handle_slash` for happy-path discovered commands**

```rust
    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name == "reload-commands" {
            // Handled in Task 20.
            return Ok(vec![]);
        }
        let Some(d) = self.index().commands.get(name) else {
            return Ok(vec![]);
        };
        // Trust check happens in Task 21; for now, assume Always.
        let trust = crate::plugin::builtin::user_slash_commands::trust::TrustLevel::Always;
        let expanded = match crate::plugin::builtin::user_slash_commands::template::expand_all(
            &d.body, &args, trust,
        )
        .await
        {
            Ok(e) => e,
            Err(msg) => {
                return Ok(vec![Effect::PushNote {
                    line: savvagent_plugin::StyledLine {
                        spans: vec![savvagent_plugin::StyledSpan::plain(format!(
                            "[error] {msg}"
                        ))],
                    },
                }]);
            }
        };
        let mut effs = Vec::new();
        for w in expanded.warnings {
            effs.push(Effect::PushNote {
                line: savvagent_plugin::StyledLine {
                    spans: vec![savvagent_plugin::StyledSpan::plain(format!("[warn] {w}"))],
                },
            });
        }
        if let Some(id) = d.frontmatter.model.clone() {
            effs.push(Effect::SetNextTurnModelOverride { id });
        }
        effs.push(Effect::PromptSend {
            text: expanded.text,
        });
        Ok(effs)
    }
```

- [ ] **Step 2: Write a test**

```rust
    #[tokio::test]
    async fn handle_slash_emits_prompt_send() {
        use std::fs;
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("hello.md"),
            "---\ndescription: hi\n---\nHello $1",
        )
        .unwrap();

        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
        );
        let effs = p
            .handle_slash("hello", vec!["world".into()])
            .await
            .unwrap();
        assert!(effs
            .iter()
            .any(|e| matches!(e, Effect::PromptSend { text } if text.contains("Hello world"))));
    }

    #[tokio::test]
    async fn handle_slash_unknown_command_returns_empty() {
        let p_proj = tempfile::TempDir::new().unwrap();
        let p_home = tempfile::TempDir::new().unwrap();
        let mut p = UserSlashCommandsPlugin::with_roots(
            p_proj.path().to_path_buf(),
            p_home.path().to_path_buf(),
        );
        let effs = p.handle_slash("does-not-exist", vec![]).await.unwrap();
        assert!(effs.is_empty());
    }

    #[tokio::test]
    async fn handle_slash_with_model_emits_override() {
        use std::fs;
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("h.md"),
            "---\nmodel: claude-sonnet-4-6\n---\nbody",
        )
        .unwrap();
        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
        );
        let effs = p.handle_slash("h", vec![]).await.unwrap();
        assert!(effs.iter().any(
            |e| matches!(e, Effect::SetNextTurnModelOverride { id } if id == "claude-sonnet-4-6")
        ));
    }
```

- [ ] **Step 3: Run**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::tests
```

Expected: previous tests still pass plus 3 new ones.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs
git commit -m "feat(plugin/user-slash-commands): handle_slash dispatches discovered commands"
```

---

### Task 20: `/reload-commands` clears cache + emits `ReindexPlugin`

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs`

- [ ] **Step 1: Convert the OnceCell to a `Mutex<Option<Index>>` so it can be cleared**

Replace:

```rust
cache: OnceCell<Index>,
```

with:

```rust
cache: std::sync::Mutex<Option<Index>>,
```

And replace `index(&self)`:

```rust
    fn index(&self) -> Index {
        let mut g = self.cache.lock().unwrap();
        if g.is_none() {
            *g = Some(walk_all(&self.project_root, &self.home));
        }
        g.as_ref().unwrap().clone()
    }
```

(`Index` must impl `Clone`; if it doesn't yet, add `#[derive(Clone)]` on `Index` and `Discovered` in `discovery.rs`.)

Update all call sites of `self.index()` to take the cloned `Index` by value.

- [ ] **Step 2: Implement the reload arm**

In `handle_slash`:

```rust
        if name == "reload-commands" {
            *self.cache.lock().unwrap() = None;
            // Touching index() repopulates the cache.
            let _ = self.index();
            return Ok(vec![
                Effect::ReindexPlugin {
                    id: PluginId::new("internal:user-slash-commands").unwrap(),
                },
                Effect::PushNote {
                    line: savvagent_plugin::StyledLine {
                        spans: vec![savvagent_plugin::StyledSpan::plain(
                            "user-slash-commands: reloaded",
                        )],
                    },
                },
            ]);
        }
```

- [ ] **Step 3: Write a test**

```rust
    #[tokio::test]
    async fn reload_emits_reindex_and_picks_up_new_files() {
        use std::fs;
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
        );

        // Initially empty.
        let m = p.manifest();
        assert!(m
            .contributions
            .slash_commands
            .iter()
            .all(|s| s.name != "added"));

        // Add a command on disk.
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("added.md"), "body").unwrap();

        // Reload.
        let effs = p.handle_slash("reload-commands", vec![]).await.unwrap();
        assert!(effs.iter().any(|e| matches!(e, Effect::ReindexPlugin { .. })));

        // Manifest now contains it.
        let m = p.manifest();
        assert!(m
            .contributions
            .slash_commands
            .iter()
            .any(|s| s.name == "added"));
    }
```

- [ ] **Step 4: Run**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::tests::reload_emits_reindex_and_picks_up_new_files
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/discovery.rs
git commit -m "feat(plugin/user-slash-commands): /reload-commands rescans + reindexes"
```

---

### Task 21: Wire in trust check before shell expansion

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs`

- [ ] **Step 1: Thread the trust map through the dispatch**

`handle_slash` currently assumes `TrustLevel::Always`. The plugin doesn't have access to `App::trust_levels`. The minimal surgery: pass a callback or share state via a constructor.

Option chosen here (cleanest for the plugin trait): store an `Arc<RwLock<TrustMap>>` shared with `App`. Adjust the plugin constructor:

```rust
use std::sync::{Arc, RwLock};
use std::collections::BTreeMap;
use crate::plugin::builtin::user_slash_commands::trust::TrustLevel;

pub type SharedTrustMap = Arc<RwLock<BTreeMap<PathBuf, TrustLevel>>>;

pub struct UserSlashCommandsPlugin {
    project_root: PathBuf,
    home: PathBuf,
    cache: std::sync::Mutex<Option<Index>>,
    trust: SharedTrustMap,
}

impl UserSlashCommandsPlugin {
    pub fn new(trust: SharedTrustMap) -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            project_root,
            home,
            cache: std::sync::Mutex::new(None),
            trust,
        }
    }
```

`App::trust_levels` becomes the same `Arc<RwLock<…>>`; `apply_effects::SetTrustLevel` writes through the lock. `register_builtins` passes the shared handle in.

- [ ] **Step 2: Implement the trust check inside `handle_slash`**

```rust
        // Check trust if the body has any shell substitution.
        let needs_shell = crate::plugin::builtin::user_slash_commands::template::contains_shell_token(&d.body);
        let project_local = d.origin.is_project();
        let trust = if needs_shell && project_local {
            let map = self.trust.read().unwrap();
            map.get(&self.project_root)
                .copied()
                .unwrap_or(TrustLevel::SessionTextOnly)
        } else {
            TrustLevel::Always
        };

        // If untrusted, stash and open modal.
        if needs_shell && project_local && trust == TrustLevel::SessionTextOnly {
            // Stash on App via a side-channel effect.
            return Ok(vec![
                Effect::StashPendingSlash {
                    name: name.into(),
                    args,
                },
                Effect::OpenScreen {
                    id: "trust_modal".into(),
                    args: savvagent_plugin::ScreenArgs::Path(
                        self.project_root.clone(),
                    ),
                },
            ]);
        }
        // …rest of the existing happy path…
```

Add the new `Effect::StashPendingSlash { name: String, args: Vec<String> }` variant to `savvagent-plugin::Effect` (mirror Task 11 pattern: variant + apply arm that writes `app.pending_slash_after_trust = Some((name, args));`).

- [ ] **Step 3: Update `register_builtins` to pass the shared trust map**

In `crates/savvagent/src/plugin/mod.rs::register_builtins`, after the existing setup, pass the `App`'s shared trust handle to the plugin constructor. If the registry is built before `App`, refactor so the trust map is constructed first as a free-standing `Arc<RwLock<…>>`, passed to both `App` and the plugin.

- [ ] **Step 4: Write a test**

```rust
    #[tokio::test]
    async fn untrusted_project_with_shell_opens_modal() {
        use std::fs;
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("danger.md"), "!echo evil").unwrap();

        let trust = SharedTrustMap::default();
        let mut p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
            trust,
        );
        let effs = p.handle_slash("danger", vec![]).await.unwrap();
        assert!(effs.iter().any(|e| matches!(e, Effect::OpenScreen { id, .. } if id == "trust_modal")));
        assert!(effs.iter().any(|e| matches!(e, Effect::StashPendingSlash { .. })));
    }

    #[tokio::test]
    async fn trusted_project_with_shell_runs_directly() {
        // Same setup, but pre-populate the trust map with Always for the
        // project root, assert the dispatch returns Effect::PromptSend.
    }
```

(`with_roots` is the test constructor; update it to accept the trust handle.)

- [ ] **Step 5: Run**

```bash
cargo test -p savvagent --lib plugin::builtin::user_slash_commands::tests
```

Expected: all existing tests still pass + 2 new ones.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-plugin/src/effect.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs \
        crates/savvagent/src/plugin/mod.rs \
        crates/savvagent/src/plugin/effects.rs \
        crates/savvagent/src/app.rs
git commit -m "feat(plugin/user-slash-commands): trust gate on project-local shell commands"
```

---

### Task 22: End-to-end integration test

**Files:**
- Create: `crates/savvagent/tests/user_slash_commands.rs`

- [ ] **Step 1: Write the integration test**

```rust
//! End-to-end: discovery → manifest → handle_slash → expected effects.

use savvagent::plugin::builtin::user_slash_commands::UserSlashCommandsPlugin;
use savvagent_plugin::{Effect, Plugin};
use std::fs;
use tempfile::TempDir;

#[tokio::test]
async fn end_to_end_review_command() {
    let proj = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let dir = proj.path().join(".savvagent/commands");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("review.md"),
        "---\ndescription: Review the current diff\nargument-hint: <range>\n---\nReview $ARGUMENTS\n",
    )
    .unwrap();

    let mut p = UserSlashCommandsPlugin::with_roots(
        proj.path().to_path_buf(),
        home.path().to_path_buf(),
    );

    let m = p.manifest();
    let entry = m
        .contributions
        .slash_commands
        .iter()
        .find(|s| s.name == "review")
        .expect("review command discovered");
    assert_eq!(entry.summary, "Review the current diff");
    assert_eq!(entry.args_hint.as_deref(), Some("<range>"));

    let effs = p
        .handle_slash("review", vec!["HEAD~3..".into()])
        .await
        .unwrap();
    let prompt = effs
        .iter()
        .find_map(|e| match e {
            Effect::PromptSend { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap();
    assert!(prompt.contains("Review HEAD~3.."));
}
```

(`with_roots` must be `pub` for this integration test; mark accordingly in `mod.rs`.)

- [ ] **Step 2: Run**

```bash
cargo test -p savvagent --test user_slash_commands
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/tests/user_slash_commands.rs \
        crates/savvagent/src/plugin/builtin/user_slash_commands/mod.rs
git commit -m "test(plugin/user-slash-commands): end-to-end integration test"
```

---

### Task 23: README + CHANGELOG

**Files:**
- Modify: `README.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add the README section**

In `README.md`, add (near the existing TUI features / slash command documentation):

````markdown
## User-defined slash commands

Drop a markdown file under any of these directories and it becomes a slash command:

- `<project>/.savvagent/commands/` — project-local, preferred
- `<project>/.claude/commands/` — Claude Code compatible, project-local
- `~/.savvagent/commands/` — user-wide
- `~/.claude/commands/` — Claude Code compatible, user-wide

Project paths outrank user paths; within the same scope, `.savvagent/` outranks `.claude/`. Subdirectories become namespaces: `commands/team/lint.md` → `/team:lint`.

### Format

```markdown
---
description: Review the current diff
argument-hint: [commit range]
model: claude-sonnet-4-6
---

Please review the following diff and flag any issues:

!git diff $ARGUMENTS
```

| Token | Behavior |
|---|---|
| `$ARGUMENTS` | raw arg string |
| `$1`, `$2`, … | positional args |
| `@<path>` | inlined file contents (missing files leave the literal in place + warn) |
| `!<cmd>` | shell stdout; non-zero exit aborts dispatch |

### Trust prompt

The first time you invoke a project-local command that includes `!<cmd>`, Savvagent asks whether to trust the project. Decisions persist in `~/.savvagent/trusted-projects.json` (only "trust always" is stored).

### Reload

After editing a command file, run `/reload-commands` to rescan all four directories.

### `allowed-tools`

Parsed but not yet enforced; reserved for the upcoming agents sub-project.
````

Also update the existing "On-disk paths" reference to include:

- `~/.savvagent/trusted-projects.json` — project-trust persistence
- `~/.savvagent/commands/` — user-wide slash commands
- `.savvagent/commands/` (per project) — project-local slash commands

- [ ] **Step 2: Add the CHANGELOG entry**

In `CHANGELOG.md`, add an `## [Unreleased]` section (or under the next planned version stub) with:

```markdown
### Added
- User-defined slash commands. Drop markdown files under
  `.savvagent/commands/` (project), `.claude/commands/` (project-claude),
  `~/.savvagent/commands/`, or `~/.claude/commands/`; each becomes a
  slash command. Frontmatter supports `description`, `argument-hint`,
  `model`, and (forthcoming) `allowed-tools`. Body templating supports
  `$ARGUMENTS`, `$1`/`$N`, `@<file>`, and `!<cmd>`. Project-local
  commands that use `!<cmd>` are gated behind a first-run trust prompt
  whose decisions persist to `~/.savvagent/trusted-projects.json`.
  `/reload-commands` rescans directories after edits.
```

- [ ] **Step 3: Commit**

```bash
git add README.md CHANGELOG.md
git commit -m "docs(user-slash-commands): README section + CHANGELOG entry"
```

---

### Task 24: Pre-release verification

- [ ] **Step 1: Full workspace test**

```bash
cargo test --workspace
```

Expected: all green. If anything is red, debug before proceeding.

- [ ] **Step 2: Match CI toolchain locally**

```bash
rustup run stable cargo fmt --check
rustup run stable cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both pass. Per `[[feedback_match_ci_toolchain_locally]]`, use the stable toolchain explicitly to match what CI runs; local default can lag.

- [ ] **Step 3: Smoke-test the TUI by hand**

```bash
cargo build
mkdir -p .savvagent/commands
cat > .savvagent/commands/test.md <<'EOF'
---
description: Smoke test
---
Hello $ARGUMENTS
EOF
cargo run -p savvagent
```

Inside the TUI:
1. Type `/test world` → confirm the rendered prompt `Hello world` is submitted.
2. Type `/reload-commands` → confirm a `user-slash-commands: reloaded` note appears.
3. Type `/help` (or however the palette opens) and confirm `test` shows with its description.

- [ ] **Step 4: Clean up the smoke-test file**

```bash
rm -rf .savvagent/commands/test.md
```

(Do not commit the smoke-test fixture.)

- [ ] **Step 5: Do NOT commit anything here**

No code changes in this task — verification only. Proceed to Task 25 only if all three previous steps were green.

---

### Task 25: (DEFERRED) Version bump and release notes

This task is intentionally not executed as part of the feature implementation; it runs as part of the release flow per `[[feedback_phase_release_rollup]]` and `[[feedback_release_notes]]`. Recorded here so it isn't forgotten:

- [ ] Bump `[workspace.package].version` in the root `Cargo.toml` and mirror into `[workspace.dependencies]` literals (provisionally `0.16.0`; confirm against the latest pushed tag at release time).
- [ ] Promote the `## [Unreleased]` CHANGELOG section to the new version with a date.
- [ ] Draft release notes (GitHub Release body).
- [ ] Push the tag — cargo-dist's Release workflow publishes the binaries. Do **not** `gh release create` manually per `[[feedback_cargo_dist_release.md]]`.

---

## Spec coverage trace

| Spec requirement | Task(s) |
|---|---|
| Four discovery paths with precedence | 4, 5 |
| Subdirectory namespacing `team:lint` | 3, 4 |
| Frontmatter fields (description, argument-hint, allowed-tools, model) | 2 |
| Unknown frontmatter keys warn-and-keep | 2 |
| `$ARGUMENTS` / `$N` expansion | 7 |
| `@<path>` expansion + missing-file warning | 8 |
| `!<cmd>` expansion + non-zero abort | 9 |
| Single-pass (no recursion) | 10 |
| Trust file at `~/.savvagent/trusted-projects.json` | 6, 17 |
| First-run trust modal (y/n/q) | 16, 21 |
| Persist only `Always`; `SessionTextOnly` blocks shell only | 6, 10, 17 |
| `/reload-commands` slash command | 1, 20 |
| Synchronous discovery in `manifest()` | 18 |
| `model:` one-turn override | 11, 12, 13, 14, 19 |
| `Effect::PromptSend` for synthetic prompt | (uses existing) 19 |
| New effects: `SetNextTurnModelOverride`, `SetTrustLevel`, `ReindexPlugin`, `StashPendingSlash` | 11, 21 |
| Trust modal `Screen` | 16 |
| Built-in plugin registration | 1, 21 |
| README + CHANGELOG | 23 |
| Pre-release verification (test/fmt/clippy/manual smoke) | 24 |
| Version bump (deferred to release flow) | 25 |
