# Multi-provider pool — Phase 5 (user routing rules) implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land Layer 3 of the router stack — user-edited routing rules in `~/.savvagent/routing.toml`. A turn whose latest user message matches a rule's predicates is redirected to that rule's `provider/model`. `/route reload` re-reads the file at runtime; `/route show` prints the active rules and the most-recent decision. `@`-overrides and modality redirects still win when they apply.

**Architecture:**
- New host-side module `crates/savvagent-host/src/router/rules.rs` owns `RoutingRules` (parser + evaluator) and `RoutingRulesError`. `Router::pick` gains two parameters (`rules: &RoutingRules`, `user_text: &str`) and a new Layer-3 step between Modality and Default. `RoutingReason::Rule { name }` is the new variant on the existing `#[non_exhaustive]` enum.
- `Host` gains `routing_rules: Arc<RwLock<RoutingRules>>`, `Host::reload_routing_rules()`, and `Host::routing_rules_snapshot()`. The most-recent decision is *not* stashed on the host — `/route show` sources it from the TUI's transcript via `App::log`.
- New TUI plugin `internal:route` at `crates/savvagent/src/plugin/builtin/route/` registers `/route` with `reload` / `show` subcommands. Two new `Effect` variants (`ReloadRoutingRules`, `ShowRoutingRules`) are handled in `apply_effects` (which has host access). Plugin `handle_slash` has no `&Host`, so the effect indirection is required.
- `legacy_model.rs` is unchanged. The new precedence step (`routing.toml#default`) is added in the **callers** — the two model-resolution sites in `crates/savvagent/src/main.rs` (`resolve_initial_model_for` and the multi-provider startup chain around line 432). New order: `SAVVAGENT_MODEL` env → `models.toml` → `routing.toml#default` → `provider.default_model`.
- Workspace version bumps to `0.19.0` (per-phase scaffolding; the actual tag rolls up all phases later per [[project_multi_provider_release.md]] in user memory).

**Tech Stack:** Rust 2024, Tokio, `async-trait`, `toml` (already in workspace). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-05-18-multi-provider-pool-phase-5-design.md`. Parent spec: `docs/superpowers/specs/2026-05-15-multi-provider-pool-and-auto-routing-design.md`.

---

## File structure (Phase 5)

**New files:**
- `crates/savvagent-host/src/router/rules.rs` — `RoutingRules`, `RoutingRule`, `RuleMatch`, `DefaultPick`, `RoutingRulesError`, `RuleSignals`, parser + evaluator.
- `crates/savvagent-host/tests/route_rules_e2e.rs` — end-to-end integration tests (rule fires; rule with disconnected provider falls through; reload-mid-turn race).
- `crates/savvagent/src/plugin/builtin/route/mod.rs` — `RoutePlugin`, manifest, slash dispatch, plugin-internal unit tests.
- `crates/savvagent/src/routing_pref.rs` — TUI-side helper that resolves the `~/.savvagent/routing.toml` path and loads a `Option<DefaultPick>` for the model-resolution chain. (Loader for the *full* `RoutingRules` is host-side; this helper exists so `main.rs` can read just `#default` without depending on the full rules type during startup.)

**Modified files:**
- `crates/savvagent-host/src/router/mod.rs` — declare and re-export the `rules` submodule.
- `crates/savvagent-host/src/router/router.rs` — add `RoutingReason::Rule { name: String }` variant; extend `Router::pick` signature with `rules: &RoutingRules, user_text: &str`; update Display impl.
- `crates/savvagent-host/src/lib.rs` — re-export `RoutingRules`, `RoutingRule`, `RuleMatch`, `DefaultPick`, `RoutingRulesError`.
- `crates/savvagent-host/src/config.rs` — add `HostConfig::routing_rules_path: Option<PathBuf>`; default to `None`.
- `crates/savvagent-host/src/session.rs` — `Host` gains the new fields and methods; `run_turn_inner` builds `user_text`, takes a `routing_rules` snapshot, threads both into `Router::pick`.
- `crates/savvagent/src/main.rs` — register the `RoutePlugin` in the built-in plugin set; populate `HostConfig::routing_rules_path`; insert `routing.toml#default` into the two model-resolution chains (`resolve_initial_model_for` + multi-provider startup wiring near `legacy_model` block); add `apply_effects` branches for the two new effects.
- `crates/savvagent/src/plugin/effects.rs` — implement `ReloadRoutingRules` and `ShowRoutingRules` effect handlers (snapshot rules, scan `App::log` for last badge, push styled lines).
- `crates/savvagent-plugin/src/effect.rs` — add `Effect::ReloadRoutingRules` and `Effect::ShowRoutingRules` variants on the `#[non_exhaustive]` enum.
- `crates/savvagent/src/plugin/builtin/mod.rs` — export `RoutePlugin`; include it in the built-in plugin enumerator.
- `crates/savvagent/locales/en.toml` — add `slash.route-summary`, `plugin.route-description`, and a `[routing]` section.
- `crates/savvagent/locales/es.toml`, `pt.toml`, `hi.toml` — TODO placeholders mirroring en.toml keys.
- `Cargo.toml` (workspace root) — bump `[workspace.package].version` to `0.19.0` and every `version = "0.18.0"` literal in `[workspace.dependencies]` to `0.19.0`.
- `CHANGELOG.md` — add `## 0.19.0 - 2026-05-18` entry.
- `README.md` — add a short user-facing routing-rules section with the sample TOML.

---

## Task 1: `RoutingRules` types + parser (host crate, no router yet)

**Files:**
- Create: `crates/savvagent-host/src/router/rules.rs`
- Modify: `crates/savvagent-host/src/router/mod.rs`
- Modify: `crates/savvagent-host/src/lib.rs`

Pure data + parsing. No async, no `Router` change yet — the goal is to make `RoutingRules::load_from_path` and `RoutingRules::evaluate` self-contained and well-tested before wiring anything to a turn.

- [ ] **Step 1: Write the failing tests**

Append to `crates/savvagent-host/src/router/rules.rs` (file does not exist yet):

```rust
//! User-edited routing rules from `~/.savvagent/routing.toml`.
//!
//! Phase 5 ships Layer 3 of the parent spec's router stack. The rules
//! are parsed once at `Host::start` and re-parsed on `/route reload`;
//! the evaluator is a pure function called from `Router::pick` after
//! `@`-override and modality have had their say.
//!
//! The struct shape matches the parent spec's `routing.toml` example
//! verbatim plus a `version = 1` field for forward-compat. Predicate
//! fields use `Option<bool>` so `match = { has_image = true }` is
//! distinguishable from "predicate absent" (the alternative — bare
//! `bool` defaulting to `false` — would conflate "match only image
//! turns" with "match only non-image turns").
//!
//! `RuleMatch` is `#[non_exhaustive]` so Phase 6 predicates can land
//! additively.

use std::path::{Path, PathBuf};

use savvagent_protocol::ProviderId;
use serde::Deserialize;

use crate::router::modality::RequiredModalities;

/// Current `routing.toml` schema version. Loaders reject files with a
/// higher version + a styled warning + empty fallback.
pub const ROUTING_RULES_SCHEMA_VERSION: u32 = 1;

/// In-memory representation of `~/.savvagent/routing.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingRules {
    /// Optional default `provider/model` from the file's `default = "..."`.
    pub default: Option<DefaultPick>,
    /// Whether the user opted into the Phase 6 heuristic classifier.
    pub heuristics: bool,
    /// Rules in TOML order; first match wins during evaluation.
    pub rules: Vec<RoutingRule>,
}

/// One `(provider, model)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultPick {
    /// The provider.
    pub provider: ProviderId,
    /// The provider-relative model id.
    pub model: String,
}

/// One `[[rule]]` entry from `routing.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingRule {
    /// Human-readable name from `name = "..."`.
    pub name: String,
    /// Predicates that must all match for the rule to fire.
    pub match_: RuleMatch,
    /// Where to route when the rule matches.
    pub use_: DefaultPick,
}

/// Per-turn predicates. AND across set fields.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleMatch {
    /// Require / forbid the latest user message to carry an image.
    pub has_image: Option<bool>,
    /// Require / forbid PDF (reserved; never matches in v1).
    pub has_pdf: Option<bool>,
    /// Require / forbid audio (reserved; never matches in v1).
    pub has_audio: Option<bool>,
    /// Case-insensitive substring match against the latest user
    /// message's concatenated text. Empty Vec = no keyword constraint.
    pub keywords: Vec<String>,
    /// Inclusive upper bound on latest-user-message text length.
    pub max_input_chars: Option<usize>,
    /// Inclusive lower bound on latest-user-message text length.
    pub min_input_chars: Option<usize>,
}

/// Per-turn signals the evaluator reads. Built once in `run_turn_inner`.
pub struct RuleSignals<'a> {
    /// Modality flags computed from the latest user message.
    pub required: RequiredModalities,
    /// Concatenated `Text` blocks of the latest user message.
    pub user_text: &'a str,
}

/// What can go wrong parsing `routing.toml`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RoutingRulesError {
    /// `version` in the file is newer than this build supports.
    #[error("routing.toml at {path:?}: schema version {found} not supported (max {max})")]
    UnsupportedVersion {
        /// Where the file was found.
        path: PathBuf,
        /// Version the file declared.
        found: u32,
        /// Version this build understands.
        max: u32,
    },
    /// `toml::de::Error` while parsing the file.
    #[error("routing.toml at {path:?}: {source}")]
    Parse {
        /// Where the file was found.
        path: PathBuf,
        /// Underlying parse error.
        #[source]
        source: toml::de::Error,
    },
    /// `use` did not contain a `/`.
    #[error("routing.toml at {path:?}: rule {index} `{name}`: `use` must be `provider/model`, got `{got}`")]
    BadUseSyntax {
        /// Where the file was found.
        path: PathBuf,
        /// 1-based rule index.
        index: usize,
        /// `name` of the offending rule.
        name: String,
        /// The bad `use` value.
        got: String,
    },
    /// `max_input_chars < min_input_chars`.
    #[error("routing.toml at {path:?}: rule {index} `{name}`: max_input_chars ({max}) < min_input_chars ({min})")]
    BoundsInverted {
        /// Where the file was found.
        path: PathBuf,
        /// 1-based rule index.
        index: usize,
        /// `name` of the offending rule.
        name: String,
        /// Upper bound from the rule.
        max: usize,
        /// Lower bound from the rule.
        min: usize,
    },
    /// I/O error while reading the file.
    #[error("routing.toml at {path:?}: io error: {source}")]
    Io {
        /// Where the file was found.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

// ----- TOML wire shape (serde-only; never exposed via the public API) -----

#[derive(Debug, Deserialize, Default)]
struct WireRules {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    heuristics: bool,
    #[serde(rename = "rule", default)]
    rules: Vec<WireRule>,
}

#[derive(Debug, Deserialize, Default)]
struct WireRule {
    name: String,
    #[serde(default, rename = "match")]
    match_: WireMatch,
    #[serde(rename = "use")]
    use_: String,
}

#[derive(Debug, Deserialize, Default)]
struct WireMatch {
    #[serde(default)]
    has_image: Option<bool>,
    #[serde(default)]
    has_pdf: Option<bool>,
    #[serde(default)]
    has_audio: Option<bool>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    max_input_chars: Option<usize>,
    #[serde(default)]
    min_input_chars: Option<usize>,
}

impl RoutingRules {
    /// Empty rules. Behaves like Phase 4 (no rule ever matches).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load and parse a `routing.toml`. File-absent → `Ok(empty())`.
    pub fn load_from_path(path: &Path) -> Result<Self, RoutingRulesError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty());
            }
            Err(source) => {
                return Err(RoutingRulesError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let wire: WireRules = toml::from_str(&text).map_err(|source| RoutingRulesError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_wire(path, wire)
    }

    fn from_wire(path: &Path, wire: WireRules) -> Result<Self, RoutingRulesError> {
        let version = wire.version.unwrap_or(1);
        if version > ROUTING_RULES_SCHEMA_VERSION {
            return Err(RoutingRulesError::UnsupportedVersion {
                path: path.to_path_buf(),
                found: version,
                max: ROUTING_RULES_SCHEMA_VERSION,
            });
        }
        let default = match wire.default {
            Some(s) if !s.trim().is_empty() => Some(parse_provider_model(path, 0, "default", &s)?),
            _ => None,
        };
        let mut rules = Vec::with_capacity(wire.rules.len());
        for (i, r) in wire.rules.into_iter().enumerate() {
            let idx = i + 1;
            let use_ = parse_provider_model(path, idx, &r.name, &r.use_)?;
            if let (Some(max), Some(min)) = (r.match_.max_input_chars, r.match_.min_input_chars) {
                if max < min {
                    return Err(RoutingRulesError::BoundsInverted {
                        path: path.to_path_buf(),
                        index: idx,
                        name: r.name.clone(),
                        max,
                        min,
                    });
                }
            }
            rules.push(RoutingRule {
                name: r.name,
                match_: RuleMatch {
                    has_image: r.match_.has_image,
                    has_pdf: r.match_.has_pdf,
                    has_audio: r.match_.has_audio,
                    keywords: r
                        .match_
                        .keywords
                        .into_iter()
                        .map(|k| k.to_lowercase())
                        .collect(),
                    max_input_chars: r.match_.max_input_chars,
                    min_input_chars: r.match_.min_input_chars,
                },
                use_,
            });
        }
        Ok(Self {
            default,
            heuristics: wire.heuristics,
            rules,
        })
    }

    /// Evaluate against per-turn signals. Returns the first matching
    /// rule's name + target, or `None` when no rule matches or the
    /// matched rule's provider is not in `connected`.
    pub fn evaluate(
        &self,
        signals: &RuleSignals<'_>,
        connected: &[&ProviderId],
    ) -> Option<(String, DefaultPick)> {
        let text_lower = signals.user_text.to_lowercase();
        let len = signals.user_text.chars().count();
        for rule in &self.rules {
            if !match_satisfied(&rule.match_, signals.required, &text_lower, len) {
                continue;
            }
            if !connected.iter().any(|p| **p == rule.use_.provider) {
                tracing::info!(
                    rule = %rule.name,
                    provider = %rule.use_.provider.as_str(),
                    "routing rule skipped: target provider not connected"
                );
                continue;
            }
            return Some((rule.name.clone(), rule.use_.clone()));
        }
        None
    }
}

fn match_satisfied(
    m: &RuleMatch,
    required: RequiredModalities,
    text_lower: &str,
    char_len: usize,
) -> bool {
    if let Some(b) = m.has_image && required.has_image != b {
        return false;
    }
    if let Some(b) = m.has_pdf && required.has_pdf != b {
        return false;
    }
    if let Some(b) = m.has_audio && required.has_audio != b {
        return false;
    }
    if let Some(max) = m.max_input_chars && char_len > max {
        return false;
    }
    if let Some(min) = m.min_input_chars && char_len < min {
        return false;
    }
    if !m.keywords.is_empty() && !m.keywords.iter().any(|k| text_lower.contains(k)) {
        return false;
    }
    true
}

fn parse_provider_model(
    path: &Path,
    rule_index: usize,
    name: &str,
    raw: &str,
) -> Result<DefaultPick, RoutingRulesError> {
    let (p, m) = raw.split_once('/').ok_or_else(|| RoutingRulesError::BadUseSyntax {
        path: path.to_path_buf(),
        index: rule_index,
        name: name.to_string(),
        got: raw.to_string(),
    })?;
    let provider = ProviderId::new(p.trim()).map_err(|_| RoutingRulesError::BadUseSyntax {
        path: path.to_path_buf(),
        index: rule_index,
        name: name.to_string(),
        got: raw.to_string(),
    })?;
    let model = m.trim().to_string();
    if model.is_empty() {
        return Err(RoutingRulesError::BadUseSyntax {
            path: path.to_path_buf(),
            index: rule_index,
            name: name.to_string(),
            got: raw.to_string(),
        });
    }
    Ok(DefaultPick { provider, model })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::modality::RequiredModalities;
    use std::io::Write;

    fn tmp_routing(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("routing.toml");
        let mut f = std::fs::File::create(&path).expect("create routing.toml");
        f.write_all(content.as_bytes()).expect("write");
        (dir, path)
    }

    #[test]
    fn absent_file_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.toml");
        let r = RoutingRules::load_from_path(&path).expect("ok");
        assert!(r.rules.is_empty());
        assert!(r.default.is_none());
        assert!(!r.heuristics);
    }

    #[test]
    fn parses_full_example() {
        let (_d, path) = tmp_routing(
            r#"
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
"#,
        );
        let r = RoutingRules::load_from_path(&path).expect("parses");
        assert_eq!(r.rules.len(), 2);
        assert_eq!(r.rules[0].name, "vision-for-images");
        assert_eq!(r.rules[0].use_.provider.as_str(), "gemini");
        assert_eq!(r.rules[0].use_.model, "gemini-2.0-flash-vision");
        assert_eq!(r.default.as_ref().unwrap().model, "claude-opus-4-7");
    }

    #[test]
    fn rejects_future_schema_version() {
        let (_d, path) = tmp_routing("version = 2\n");
        let err = RoutingRules::load_from_path(&path).expect_err("rejects");
        assert!(matches!(err, RoutingRulesError::UnsupportedVersion { .. }));
    }

    #[test]
    fn rejects_use_without_slash() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "bad"
use = "anthropic"
"#,
        );
        let err = RoutingRules::load_from_path(&path).expect_err("rejects");
        assert!(matches!(err, RoutingRulesError::BadUseSyntax { .. }));
    }

    #[test]
    fn rejects_inverted_bounds() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "bad-bounds"
match = { max_input_chars = 10, min_input_chars = 100 }
use = "anthropic/claude-opus-4-7"
"#,
        );
        let err = RoutingRules::load_from_path(&path).expect_err("rejects");
        assert!(matches!(err, RoutingRulesError::BoundsInverted { .. }));
    }

    #[test]
    fn keywords_are_lowercased_at_parse() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "refactor"
match = { keywords = ["RefactoR", "DESIGN"] }
use = "anthropic/claude-opus-4-7"
"#,
        );
        let r = RoutingRules::load_from_path(&path).expect("parses");
        assert_eq!(r.rules[0].match_.keywords, vec!["refactor", "design"]);
    }

    fn signals<'a>(text: &'a str, image: bool) -> RuleSignals<'a> {
        RuleSignals {
            required: RequiredModalities {
                has_image: image,
                ..Default::default()
            },
            user_text: text,
        }
    }

    #[test]
    fn evaluate_matches_keyword() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "refactor"
match = { keywords = ["refactor"] }
use = "anthropic/claude-opus-4-7"
"#,
        );
        let r = RoutingRules::load_from_path(&path).unwrap();
        let a = ProviderId::new("anthropic").unwrap();
        let conn: Vec<&ProviderId> = vec![&a];
        let hit = r.evaluate(&signals("please refactor this", false), &conn);
        let (name, pick) = hit.expect("matches");
        assert_eq!(name, "refactor");
        assert_eq!(pick.provider, a);
        assert_eq!(pick.model, "claude-opus-4-7");
    }

    #[test]
    fn evaluate_and_semantics_within_one_match() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "short-refactor"
match = { keywords = ["refactor"], max_input_chars = 30 }
use = "anthropic/claude-opus-4-7"
"#,
        );
        let r = RoutingRules::load_from_path(&path).unwrap();
        let a = ProviderId::new("anthropic").unwrap();
        let conn: Vec<&ProviderId> = vec![&a];
        // Both predicates pass:
        assert!(r.evaluate(&signals("refactor pls", false), &conn).is_some());
        // Keyword passes, length fails:
        let long = "refactor ".to_string() + &"x".repeat(100);
        assert!(r.evaluate(&signals(&long, false), &conn).is_none());
    }

    #[test]
    fn evaluate_first_match_wins() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "first"
match = { keywords = ["x"] }
use = "anthropic/claude-opus-4-7"

[[rule]]
name = "second"
match = { keywords = ["x"] }
use = "gemini/gemini-2.0-flash"
"#,
        );
        let r = RoutingRules::load_from_path(&path).unwrap();
        let a = ProviderId::new("anthropic").unwrap();
        let g = ProviderId::new("gemini").unwrap();
        let conn: Vec<&ProviderId> = vec![&a, &g];
        let (name, _pick) = r.evaluate(&signals("xenon", false), &conn).unwrap();
        assert_eq!(name, "first");
    }

    #[test]
    fn evaluate_skips_disconnected_provider() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "first"
match = { keywords = ["x"] }
use = "gemini/gemini-2.0-flash"

[[rule]]
name = "second"
match = { keywords = ["x"] }
use = "anthropic/claude-opus-4-7"
"#,
        );
        let r = RoutingRules::load_from_path(&path).unwrap();
        let a = ProviderId::new("anthropic").unwrap();
        let conn: Vec<&ProviderId> = vec![&a]; // gemini disconnected
        let (name, pick) = r.evaluate(&signals("xenon", false), &conn).unwrap();
        assert_eq!(name, "second");
        assert_eq!(pick.provider, a);
    }

    #[test]
    fn evaluate_empty_match_table_matches_any_turn() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "catch-all"
match = {}
use = "anthropic/claude-opus-4-7"
"#,
        );
        let r = RoutingRules::load_from_path(&path).unwrap();
        let a = ProviderId::new("anthropic").unwrap();
        let conn: Vec<&ProviderId> = vec![&a];
        assert!(r.evaluate(&signals("anything", false), &conn).is_some());
        assert!(r.evaluate(&signals("", true), &conn).is_some());
    }

    #[test]
    fn evaluate_has_image_predicate() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "vision"
match = { has_image = true }
use = "gemini/gemini-2.0-flash-vision"
"#,
        );
        let r = RoutingRules::load_from_path(&path).unwrap();
        let g = ProviderId::new("gemini").unwrap();
        let conn: Vec<&ProviderId> = vec![&g];
        assert!(r.evaluate(&signals("", true), &conn).is_some());
        assert!(r.evaluate(&signals("", false), &conn).is_none());
    }
}
```

Then modify `crates/savvagent-host/src/router/mod.rs`:

```rust
//! Routing layers. Phase 5 ships Layer 3 (user rules) plus the existing
//! Layer 1 (`@`-prefix), Layer 2 (modality), and Layer 5 (default).

pub mod legacy_model;
pub mod modality;
pub mod namespace;
pub mod prefix;
pub mod rules;
#[allow(clippy::module_inception)]
pub mod router;

pub use legacy_model::{LegacyModelResolution, ProviderView, resolve_legacy_model};
pub use modality::{
    RequiredModalities, RequiredModalityKind, pick_vision_capable, required_modalities,
};
pub use router::{Router, RoutingDecision, RoutingOverride, RoutingReason};
pub use rules::{
    DefaultPick, ROUTING_RULES_SCHEMA_VERSION, RoutingRule, RoutingRules, RoutingRulesError,
    RuleMatch, RuleSignals,
};
```

And `crates/savvagent-host/src/lib.rs` `pub use router::{...}` line (around line 26-30):

```rust
pub use router::{
    DefaultPick, LegacyModelResolution, ProviderView, ROUTING_RULES_SCHEMA_VERSION,
    RequiredModalities, RequiredModalityKind, Router, RoutingDecision, RoutingOverride,
    RoutingReason, RoutingRule, RoutingRules, RoutingRulesError, RuleMatch, RuleSignals,
    pick_vision_capable, required_modalities, resolve_legacy_model,
};
```

Also add `tempfile` to `[dev-dependencies]` in `crates/savvagent-host/Cargo.toml` if not already present:

```toml
[dev-dependencies]
tempfile = { workspace = true }
```

(Check `Cargo.toml` first — `tempfile` may already be wired up workspace-wide; if so, leave the dev-deps block alone.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent-host --lib router::rules`
Expected: FAIL — module doesn't compile yet, or the new tests fail because `rules.rs` isn't wired into `router/mod.rs` until you save it.

- [ ] **Step 3: Make tests pass**

Already done by Step 1's code blocks — re-run to confirm:

Run: `cargo test -p savvagent-host --lib router::rules`
Expected: all 10 `router::rules::tests::*` PASS.

- [ ] **Step 4: Lint and format**

Run: `rustup run stable cargo fmt --all`
Run: `rustup run stable cargo clippy -p savvagent-host --all-targets -- -D warnings`
Expected: no diff from fmt; no clippy errors.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/router/rules.rs crates/savvagent-host/src/router/mod.rs crates/savvagent-host/src/lib.rs crates/savvagent-host/Cargo.toml
git commit -m "feat(host): RoutingRules parser + evaluator (Phase 5 skeleton)"
```

---

## Task 2: `RoutingReason::Rule` variant + Display update

**Files:**
- Modify: `crates/savvagent-host/src/router/router.rs`

Smallest possible change to the `RoutingReason` enum so subsequent router tasks can name the new variant. Display matches Phase 4's `Modality(image)` style: `Rule(<name>)` bare, no quotes.

- [ ] **Step 1: Write the failing test**

Append to `crates/savvagent-host/src/router/router.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn routing_reason_rule_displays() {
        let r = RoutingReason::Rule {
            name: "deep-reasoning".to_string(),
        };
        assert_eq!(format!("{r}"), "Rule(deep-reasoning)");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent-host --lib router::router::tests::routing_reason_rule_displays`
Expected: compile error — `RoutingReason::Rule` variant doesn't exist.

- [ ] **Step 3: Add the variant + Display arm**

In `crates/savvagent-host/src/router/router.rs`, extend `RoutingReason`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RoutingReason {
    Override,
    Modality {
        kind: modality::RequiredModalityKind,
    },
    /// A user-defined rule from `routing.toml` matched this turn.
    Rule {
        /// The matching rule's `name` field.
        name: String,
    },
    Default,
}
```

And the Display impl:

```rust
impl std::fmt::Display for RoutingReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingReason::Override => f.write_str("Override"),
            RoutingReason::Modality { kind } => write!(f, "Modality({kind})"),
            RoutingReason::Rule { name } => write!(f, "Rule({name})"),
            RoutingReason::Default => f.write_str("Default"),
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p savvagent-host --lib router::router::tests`
Expected: all router tests PASS, including the new `routing_reason_rule_displays`.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/src/router/router.rs
git commit -m "feat(host): RoutingReason::Rule variant + Display"
```

---

## Task 3: `Router::pick` Layer 3 — rules evaluation

**Files:**
- Modify: `crates/savvagent-host/src/router/router.rs`

`Router::pick` gains two new parameters (`rules: &RoutingRules, user_text: &str`). The new layer runs **between Modality and Default**. Phase 6 will later insert Heuristic between Rules and Default.

- [ ] **Step 1: Write the failing tests**

Append to `crates/savvagent-host/src/router/router.rs`'s `#[cfg(test)] mod tests`:

```rust
    use crate::router::rules::{DefaultPick, RoutingRule, RoutingRules, RuleMatch};

    fn rules_with_one_rule(name: &str, pick: DefaultPick, match_: RuleMatch) -> RoutingRules {
        RoutingRules {
            default: None,
            heuristics: false,
            rules: vec![RoutingRule {
                name: name.to_string(),
                match_,
                use_: pick,
            }],
        }
    }

    #[test]
    fn pick_rule_matches_and_routes() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("haiku");
        let g_caps = caps("flash");
        let views = vec![
            ProviderView { id: &a_id, capabilities: &a_caps },
            ProviderView { id: &g_id, capabilities: &g_caps },
        ];
        let rules = rules_with_one_rule(
            "refactor",
            DefaultPick { provider: g_id.clone(), model: "flash".into() },
            RuleMatch { keywords: vec!["refactor".into()], ..Default::default() },
        );
        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "please refactor this",
        );
        assert_eq!(r.provider_id, g_id);
        assert_eq!(r.model_id, "flash");
        assert_eq!(r.reason, RoutingReason::Rule { name: "refactor".into() });
    }

    #[test]
    fn pick_override_beats_matching_rule() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("haiku");
        let g_caps = caps("flash");
        let views = vec![
            ProviderView { id: &a_id, capabilities: &a_caps },
            ProviderView { id: &g_id, capabilities: &g_caps },
        ];
        let rules = rules_with_one_rule(
            "x",
            DefaultPick { provider: g_id.clone(), model: "flash".into() },
            RuleMatch { keywords: vec!["x".into()], ..Default::default() },
        );
        let override_ = RoutingOverride { provider: a_id.clone(), model: Some("haiku".into()) };
        let r = Router::pick(
            Some(override_),
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "xenon",
        );
        assert_eq!(r.reason, RoutingReason::Override);
        assert_eq!(r.provider_id, a_id);
    }

    #[test]
    fn pick_modality_beats_matching_rule() {
        // Active = anthropic with both haiku (no vision) and opus
        // (vision). Image attached. A keyword rule also matches.
        // Modality (Layer 2) wins — rules run later in pick order.
        use crate::router::modality::RequiredModalityKind;
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = ProviderCapabilities::new(
            vec![
                ModelCapabilities {
                    id: "haiku".into(), display_name: "haiku".into(),
                    supports_vision: false, supports_audio: false,
                    context_window: 0, cost_tier: CostTier::Standard,
                },
                ModelCapabilities {
                    id: "opus".into(), display_name: "opus".into(),
                    supports_vision: true, supports_audio: false,
                    context_window: 0, cost_tier: CostTier::Standard,
                },
            ],
            "haiku".into(),
        ).expect("valid caps");
        let g_id = ProviderId::new("gemini").unwrap();
        let g_caps = caps("flash");
        let views = vec![
            ProviderView { id: &a_id, capabilities: &a_caps },
            ProviderView { id: &g_id, capabilities: &g_caps },
        ];
        let rules = rules_with_one_rule(
            "x",
            DefaultPick { provider: g_id.clone(), model: "flash".into() },
            RuleMatch { keywords: vec!["x".into()], ..Default::default() },
        );
        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities { has_image: true, ..Default::default() },
            &rules,
            "xenon",
        );
        assert_eq!(r.provider_id, a_id);
        assert_eq!(r.model_id, "opus");
        assert_eq!(r.reason, RoutingReason::Modality { kind: RequiredModalityKind::Image });
    }

    #[test]
    fn pick_falls_through_when_rule_target_disconnected() {
        // Rule points at gemini; only anthropic connected. Rule is
        // silently skipped; Default fires.
        let a_id = ProviderId::new("anthropic").unwrap();
        let g_id = ProviderId::new("gemini").unwrap();
        let a_caps = caps("haiku");
        let views = vec![ProviderView { id: &a_id, capabilities: &a_caps }];
        let rules = rules_with_one_rule(
            "x",
            DefaultPick { provider: g_id, model: "flash".into() },
            RuleMatch { keywords: vec!["x".into()], ..Default::default() },
        );
        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &rules,
            "xenon",
        );
        assert_eq!(r.reason, RoutingReason::Default);
        assert_eq!(r.provider_id, a_id);
    }

    #[test]
    fn pick_empty_rules_falls_through_to_default() {
        let a_id = ProviderId::new("anthropic").unwrap();
        let a_caps = caps("haiku");
        let views = vec![ProviderView { id: &a_id, capabilities: &a_caps }];
        let r = Router::pick(
            None,
            &views,
            &a_id,
            "haiku",
            RequiredModalities::default(),
            &RoutingRules::empty(),
            "anything",
        );
        assert_eq!(r.reason, RoutingReason::Default);
    }
```

You also need to update every existing test in this file that calls `Router::pick(...)` with the OLD 5-arg signature — they all need two trailing args added: `&RoutingRules::empty(), ""`. Run `cargo test -p savvagent-host --lib router::router` to enumerate the failures and edit each one. (They're short and mechanical; expected count is 6 existing `pick_*` tests.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent-host --lib router::router`
Expected: compile errors — `Router::pick` doesn't take the new params yet.

- [ ] **Step 3: Update `Router::pick` signature + body**

In `crates/savvagent-host/src/router/router.rs`, replace the entire `impl Router { ... }` block with:

```rust
impl Router {
    /// Pick a `(provider, model, reason)` triple for a turn.
    ///
    /// Layers (first match wins):
    /// 1. **Override** — `@`-prefix from the user input.
    /// 2. **Modality** — same-provider redirect when the active model
    ///    lacks a required modality.
    /// 3. **Rules** — first matching rule from `~/.savvagent/routing.toml`.
    /// 4. (Phase 6 will insert Heuristic here.)
    /// 5. **Default** — active provider + active model.
    pub fn pick(
        override_: Option<RoutingOverride>,
        providers: &[crate::router::ProviderView<'_>],
        active_provider: &ProviderId,
        active_model: &str,
        required: modality::RequiredModalities,
        rules: &crate::router::rules::RoutingRules,
        user_text: &str,
    ) -> RoutingDecision {
        // Layer 1: explicit override.
        if let Some(o) = override_ {
            if let Some(view) = providers.iter().find(|p| p.id == &o.provider) {
                let model_id = o
                    .model
                    .unwrap_or_else(|| view.capabilities.default_model_id().to_string());
                return RoutingDecision {
                    provider_id: o.provider,
                    model_id,
                    reason: RoutingReason::Override,
                };
            }
            // Stale override; fall through.
        }

        // Layer 2: modality redirect (same-provider only).
        if let Some(kind) = required.primary_kind()
            && let Some((p, m)) =
                modality::pick_vision_capable(required, active_provider, active_model, providers)
        {
            return RoutingDecision {
                provider_id: p,
                model_id: m,
                reason: RoutingReason::Modality { kind },
            };
        }

        // Layer 3: user rules.
        let connected: Vec<&ProviderId> = providers.iter().map(|v| v.id).collect();
        let signals = crate::router::rules::RuleSignals {
            required,
            user_text,
        };
        if let Some((name, pick)) = rules.evaluate(&signals, &connected) {
            // Provider validated by evaluate(); model id passed as-is.
            // Unknown-model fallback to the provider's default lives at
            // load time in Task 1 (see RoutingRulesError handling) and
            // optionally at pick-time below — Phase 5 does load-time
            // only, so trust the model id here.
            return RoutingDecision {
                provider_id: pick.provider,
                model_id: pick.model,
                reason: RoutingReason::Rule { name },
            };
        }

        // Layer 5: default.
        RoutingDecision {
            provider_id: active_provider.clone(),
            model_id: active_model.to_string(),
            reason: RoutingReason::Default,
        }
    }
}
```

Update each pre-existing `Router::pick(...)` call in the test module to add the two trailing args, e.g.:

```rust
let r = Router::pick(
    None,
    &views,
    &a_id,
    "claude-opus-4-7",
    RequiredModalities::default(),
    &RoutingRules::empty(),
    "",
);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p savvagent-host --lib router::router`
Expected: all router tests PASS, including the new 5 Phase-5 tests.

- [ ] **Step 5: Lint and format**

Run: `rustup run stable cargo fmt --all`
Run: `rustup run stable cargo clippy -p savvagent-host --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent-host/src/router/router.rs
git commit -m "feat(host): Router::pick Layer 3 — user routing rules (Phase 5)"
```

---

## Task 4: `Host` integration — fields, methods, `run_turn_inner` wire-up

**Files:**
- Modify: `crates/savvagent-host/src/config.rs`
- Modify: `crates/savvagent-host/src/session.rs`

Adds the host plumbing: config field for the file path, host field for the live rules, two new methods, and the actual `Router::pick` call site update in `run_turn_inner`.

- [ ] **Step 1: Add `HostConfig::routing_rules_path`**

In `crates/savvagent-host/src/config.rs`, add a field to `HostConfig` (right after `force_disconnect_grace_ms`):

```rust
    /// Filesystem path to the user's `routing.toml`. `None` means
    /// "don't load any rules; treat as `RoutingRules::empty()`." The
    /// TUI sets this to `~/.savvagent/routing.toml`; tests and the
    /// headless example pass `None`.
    pub routing_rules_path: Option<PathBuf>,
```

Initialize it in `HostConfig::new` (right after `force_disconnect_grace_ms: 500,`):

```rust
            routing_rules_path: None,
```

Add a field-level entry to `impl Debug for HostConfig`:

```rust
            .field("routing_rules_path", &self.routing_rules_path)
```

- [ ] **Step 2: Add the host field and constructor wiring**

In `crates/savvagent-host/src/session.rs`, locate the `pub struct Host { ... }` definition. Add a field alongside the existing `Arc<RwLock<...>>` fields:

```rust
    /// User-edited routing rules (`~/.savvagent/routing.toml`). Loaded
    /// once at `Host::start` and swapped atomically by
    /// `reload_routing_rules`. Snapshotted (cloned) before any `.await`
    /// in `run_turn_inner`, same discipline as `active_provider` etc.
    routing_rules: Arc<RwLock<crate::router::RoutingRules>>,
```

In `Host::start` (or wherever the `Host { ... }` struct is initialized after `HostConfig` is consumed), load the rules from the configured path. If `routing_rules_path` is `Some(path)`, call `RoutingRules::load_from_path(&path)`; on error log a `tracing::warn!` and fall back to `RoutingRules::empty()`. Then store as `Arc::new(RwLock::new(rules))`.

Find the existing constructor — look for a line like `let host = Host { ... };` in `session.rs`. Add the new field initialization. (The exact location varies; search for one of the existing `Arc::new(RwLock::new(...))` initializers, e.g. for `pool` or `state`, and add the new field alongside.)

- [ ] **Step 3: Add the two new methods on `Host`**

In `crates/savvagent-host/src/session.rs`, inside `impl Host { ... }`, add:

```rust
    /// Re-read `routing_rules_path` and atomically swap the in-memory
    /// rules. Returns the new rule count on success. On parse error the
    /// existing rules are kept (deliberate refinement vs Phase 5 spec
    /// startup behavior — see spec assumption #13) and the error is
    /// returned to the caller for surfacing.
    pub async fn reload_routing_rules(
        &self,
    ) -> Result<usize, crate::router::RoutingRulesError> {
        let Some(path) = self.config.routing_rules_path.clone() else {
            // Nothing to reload from; report zero rules and clear in-memory.
            let mut g = self.routing_rules.write().await;
            *g = crate::router::RoutingRules::empty();
            return Ok(0);
        };
        let new_rules = crate::router::RoutingRules::load_from_path(&path)?;
        let count = new_rules.rules.len();
        let mut g = self.routing_rules.write().await;
        *g = new_rules;
        Ok(count)
    }

    /// Snapshot the current routing rules (clone). Lets `/route show`
    /// render its output without holding the lock across an `.await`.
    pub async fn routing_rules_snapshot(&self) -> crate::router::RoutingRules {
        self.routing_rules.read().await.clone()
    }
```

- [ ] **Step 4: Update `run_turn_inner` to thread the rules into `Router::pick`**

In `crates/savvagent-host/src/session.rs`, find the `Router::pick(...)` call (currently around line 672) and the modality-detection step preceding it. Replace the surrounding block with:

```rust
        // Phase 4: detect modality requirements on the just-built `messages`.
        let required = modality::required_modalities(&messages);

        // Phase 5: build per-turn signals for the rules layer. user_text
        // is the concatenated text of the latest user message — image-
        // only turns end up with the empty string, which is fine
        // (keyword predicates won't match; bounds predicates still work).
        let user_text: String = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        // Snapshot active_id/active_model + routing_rules BEFORE taking
        // the pool guard — keeps the .await-safe lock discipline.
        let active_id: ProviderId = self.active_provider.read().await.clone();
        let active_model: String = self.current_model.read().await.clone();
        let rules_snapshot: crate::router::RoutingRules =
            self.routing_rules.read().await.clone();
        let decision = {
            let pool = self.pool.read().await;
            let views: Vec<crate::router::ProviderView<'_>> = pool
                .iter()
                .map(|(id, entry)| crate::router::ProviderView {
                    id,
                    capabilities: entry.capabilities(),
                })
                .collect();
            crate::router::Router::pick(
                override_,
                &views,
                &active_id,
                &active_model,
                required,
                &rules_snapshot,
                &user_text,
            )
            // pool guard dropped at end of this block
        };
```

(Make sure the `use` block at the top of `session.rs` already imports `ContentBlock`, `Role`. If not, add them — they're in `savvagent_protocol`.)

- [ ] **Step 5: Run tests to verify nothing regressed**

Run: `cargo test -p savvagent-host`
Expected: all tests PASS (rules path is None in tests; behavior identical to Phase 4).

- [ ] **Step 6: Lint and format**

Run: `rustup run stable cargo fmt --all`
Run: `rustup run stable cargo clippy -p savvagent-host --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/savvagent-host/src/config.rs crates/savvagent-host/src/session.rs
git commit -m "feat(host): wire routing_rules into Host + Router::pick"
```

---

## Task 5: `Effect` variants + pending-flag drain (host-touching surface)

**Files:**
- Modify: `crates/savvagent-plugin/src/effect.rs`
- Modify: `crates/savvagent/src/app.rs` (add pending flags + helpers)
- Modify: `crates/savvagent/src/plugin/effects.rs` (set pending flags)
- Modify: `crates/savvagent/src/main.rs` (add drain functions; wire into `run_app`)

**Important:** `pub async fn apply_effects(app: &mut App, effects: Vec<Effect>)` has no `host_slot` parameter — verified at `crates/savvagent/src/plugin/effects.rs:32`, and the file's own comment at lines 91-93 documents this for `Effect::SetActiveModel`: "`apply_effects` doesn't receive `host_slot`, `project_root`, or `tool_bins`." The canonical pattern is therefore to queue a flag on `App` and have `main.rs::run_app` drain it (see `app.pending_model_change` + `main.rs::apply_pending_model_change` at `main.rs:1138`). Phase 5 mirrors that pattern for routing.

- [ ] **Step 1: Add `Effect` variants**

In `crates/savvagent-plugin/src/effect.rs`, inside `pub enum Effect { ... }`, add:

```rust
    /// Re-read `~/.savvagent/routing.toml` and swap the host's stored
    /// rules. Sets `App::pending_routing_reload` so `main.rs::run_app`
    /// can drain it with host access (see `Effect::SetActiveModel` for
    /// the canonical pattern this mirrors).
    ReloadRoutingRules,
    /// Print the active routing rules and the most recent decision as
    /// styled notes. Sets `App::pending_routing_show` for the same
    /// reason as `ReloadRoutingRules`.
    ShowRoutingRules,
```

Add unit tests at the bottom of the same file's `mod tests`:

```rust
    #[test]
    fn reload_routing_rules_constructs() {
        let _ = Effect::ReloadRoutingRules;
    }

    #[test]
    fn show_routing_rules_constructs() {
        let _ = Effect::ShowRoutingRules;
    }
```

Run: `cargo test -p savvagent-plugin --lib effect::tests`
Expected: PASS.

- [ ] **Step 2: Add the pending flags + helpers on `App`**

In `crates/savvagent/src/app.rs`, find the `PendingModelChange` definition (line 137) and add a unit-struct sibling immediately below it:

```rust
/// Queued routing-rules action emitted by `Effect::ReloadRoutingRules`
/// or `Effect::ShowRoutingRules`. The `run_app` loop drains these
/// flags after each `apply_effects` call because `apply_effects`
/// doesn't have host access.
#[derive(Debug, Clone, Copy, Default)]
pub struct PendingRoutingAction;
```

Find the `App { ... }` struct body and add two fields near `pending_model_change`:

```rust
    /// Queued by `Effect::ReloadRoutingRules`; drained by
    /// `main.rs::apply_pending_routing_reload`.
    pub pending_routing_reload: Option<PendingRoutingAction>,
    /// Queued by `Effect::ShowRoutingRules`; drained by
    /// `main.rs::apply_pending_routing_show`.
    pub pending_routing_show: Option<PendingRoutingAction>,
```

Initialize them in `App::new` (alongside `pending_model_change: None,`):

```rust
            pending_routing_reload: None,
            pending_routing_show: None,
```

Now add a helper for parsing routing badges. After `push_styled_note` (around line 1173), add:

```rust
    /// Scan the entries backwards for the most recent `Entry::RouteBadge`
    /// and parse it into `(provider, model, reason)`. The badge format is
    /// `"provider/model — Reason"` (see `apply_turn_event`'s
    /// `RouteSelected` arm at line 543). Returns `None` when no badge is
    /// present in this session yet, or when the format can't be parsed.
    pub fn most_recent_routing_decision(&self) -> Option<(String, String, String)> {
        let badge = self.entries.iter().rev().find_map(|e| match e {
            Entry::RouteBadge(s) => Some(s.as_str()),
            _ => None,
        })?;
        let (left, reason) = badge.split_once(" — ")?;
        let (provider, model) = left.split_once('/')?;
        Some((provider.to_string(), model.to_string(), reason.to_string()))
    }
```

Add a tiny unit test in the same file's `#[cfg(test)] mod tests` near other `Entry`-related tests:

```rust
    #[test]
    fn most_recent_routing_decision_parses_badge() {
        let mut app = App::new_for_test();
        app.entries
            .push(Entry::RouteBadge("anthropic/claude-opus-4-7 — Override".into()));
        app.entries.push(Entry::Assistant("hi".into()));
        let got = app.most_recent_routing_decision().expect("parses");
        assert_eq!(got.0, "anthropic");
        assert_eq!(got.1, "claude-opus-4-7");
        assert_eq!(got.2, "Override");
    }

    #[test]
    fn most_recent_routing_decision_none_when_no_badge() {
        let app = App::new_for_test();
        assert!(app.most_recent_routing_decision().is_none());
    }
```

(If `App::new_for_test` doesn't exist, search the file for an existing test constructor — likely `App::new()` with default args.)

- [ ] **Step 3: Implement the effect handlers in `apply_effects` (flag-set only)**

In `crates/savvagent/src/plugin/effects.rs`, locate the `match eff { ... }` block in `apply_one` (around line 48). Add two arms — both simply set the pending flag, matching the `Effect::SetActiveModel` pattern at lines 88-94:

```rust
        Effect::ReloadRoutingRules => {
            app.pending_routing_reload = Some(crate::app::PendingRoutingAction);
        }
        Effect::ShowRoutingRules => {
            app.pending_routing_show = Some(crate::app::PendingRoutingAction);
        }
```

Verify `PendingRoutingAction` is reachable via `crate::app::PendingRoutingAction` (re-export at the top of `effects.rs` if needed, mirroring how `PendingModelChange` is imported on line 10).

- [ ] **Step 4: Add the drain functions in `main.rs`**

In `crates/savvagent/src/main.rs`, immediately after `apply_pending_model_change` (around line 1138), add the two drain functions:

```rust
/// Drain `app.pending_routing_reload` (set by `Effect::ReloadRoutingRules`)
/// and reload `~/.savvagent/routing.toml` via the host. No-op when nothing
/// is queued. Mirrors `apply_pending_model_change`'s drain pattern.
async fn apply_pending_routing_reload(app: &mut App, host_slot: &HostSlot) {
    if app.pending_routing_reload.take().is_none() {
        return;
    }
    let Some(host) = current_host(host_slot).await else {
        app.push_note(
            rust_i18n::t!("routing.reload-failed", err = "host not connected yet").to_string(),
        );
        return;
    };
    match host.reload_routing_rules().await {
        Ok(count) => {
            app.push_note(
                rust_i18n::t!("routing.reloaded", count = count.to_string()).to_string(),
            );
        }
        Err(e) => {
            app.push_note(rust_i18n::t!("routing.reload-failed", err = e.to_string()).to_string());
        }
    }
}

/// Drain `app.pending_routing_show` (set by `Effect::ShowRoutingRules`)
/// and render the routing-rules summary. No-op when nothing is queued.
async fn apply_pending_routing_show(app: &mut App, host_slot: &HostSlot) {
    if app.pending_routing_show.take().is_none() {
        return;
    }
    let Some(host) = current_host(host_slot).await else {
        app.push_note(rust_i18n::t!("routing.show-no-rules").to_string());
        return;
    };
    let rules = host.routing_rules_snapshot().await;
    render_routing_show(app, &rules);
}

/// Render `/route show` output as plain styled notes onto `App`. Pure
/// function over the snapshot — no further host access required.
fn render_routing_show(app: &mut App, rules: &savvagent_host::RoutingRules) {
    if rules.rules.is_empty() {
        app.push_note(rust_i18n::t!("routing.show-no-rules").to_string());
    } else {
        app.push_note(rust_i18n::t!("routing.show-header").to_string());
        let connected: Vec<savvagent_protocol::ProviderId> =
            app.connected_provider_ids().cloned().collect();
        for (i, rule) in rules.rules.iter().enumerate() {
            let idx = i + 1;
            let match_desc = format_rule_match(&rule.match_);
            let key = if connected.contains(&rule.use_.provider) {
                "routing.show-rule-line"
            } else {
                "routing.show-rule-skipped"
            };
            let line = rust_i18n::t!(
                key,
                index = idx.to_string(),
                name = rule.name.as_str(),
                r#match = match_desc,
                provider = rule.use_.provider.as_str(),
                model = rule.use_.model.as_str(),
            )
            .to_string();
            app.push_note(line);
        }
    }
    match &rules.default {
        Some(d) => app.push_note(
            rust_i18n::t!(
                "routing.show-default",
                provider = d.provider.as_str(),
                model = d.model.as_str()
            )
            .to_string(),
        ),
        None => app.push_note(rust_i18n::t!("routing.show-no-default").to_string()),
    }
    if rules.heuristics {
        app.push_note(rust_i18n::t!("routing.show-heuristics-pending").to_string());
    }
    match app.most_recent_routing_decision() {
        Some((provider, model, reason)) => app.push_note(
            rust_i18n::t!(
                "routing.show-last",
                provider = provider,
                model = model,
                reason = reason
            )
            .to_string(),
        ),
        None => app.push_note(rust_i18n::t!("routing.show-no-last").to_string()),
    }
}

fn format_rule_match(m: &savvagent_host::RuleMatch) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = m.has_image {
        parts.push(format!("has_image={b}"));
    }
    if let Some(b) = m.has_pdf {
        parts.push(format!("has_pdf={b}"));
    }
    if let Some(b) = m.has_audio {
        parts.push(format!("has_audio={b}"));
    }
    if !m.keywords.is_empty() {
        parts.push(format!("keywords=[{}]", m.keywords.join(",")));
    }
    if let Some(n) = m.max_input_chars {
        parts.push(format!("max_input_chars={n}"));
    }
    if let Some(n) = m.min_input_chars {
        parts.push(format!("min_input_chars={n}"));
    }
    if parts.is_empty() {
        "<any>".to_string()
    } else {
        parts.join(", ")
    }
}
```

Also add an `App::connected_provider_ids` helper. In `crates/savvagent/src/app.rs`, near `most_recent_routing_decision` (added in Step 2), add:

```rust
    /// Owning vec of provider ids currently in the host pool. Owning
    /// (not borrowing) so the caller can clone before any `.await` that
    /// might take a different App reference. Source: the field
    /// populated by the `RegisterProvider` arm of `apply_effects`
    /// (effects.rs:95-149). The real field name in this codebase is
    /// `registered_providers: HashMap<String, Box<dyn ProviderClient>>`
    /// (`app.rs:421`); the keys are the stable provider-id strings.
    pub fn connected_provider_ids(&self) -> Vec<savvagent_protocol::ProviderId> {
        self.registered_providers
            .keys()
            .filter_map(|s| savvagent_protocol::ProviderId::new(s).ok())
            .collect()
    }
```

Update the call site in `render_routing_show` to consume the vec directly (instead of `.cloned().collect()`):

```rust
        let connected: Vec<savvagent_protocol::ProviderId> = app.connected_provider_ids();
```

- [ ] **Step 5: Wire the drains into `run_app`**

In `crates/savvagent/src/main.rs`, locate every site that calls `apply_pending_model_change(app, &host_slot, ...)` (three call sites confirmed: ~line 687, ~line 2287, ~line 2432). Immediately after each call, add the two new drain calls:

```rust
                apply_pending_routing_reload(app, &host_slot).await;
                apply_pending_routing_show(app, &host_slot).await;
```

Adapt the variable name (`host_slot` vs `&host_slot`) to whatever the surrounding context uses.

- [ ] **Step 6: Run tests**

Run: `cargo test -p savvagent-plugin --lib effect`
Run: `cargo test -p savvagent --lib`
Expected: PASS. The integration test in Task 8 exercises the host machinery directly; the App/plugin pieces here are validated by unit tests + the manual smoke test in the final verification block.

- [ ] **Step 7: Lint and format**

Run: `rustup run stable cargo fmt --all`
Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/savvagent-plugin/src/effect.rs crates/savvagent/src/app.rs crates/savvagent/src/plugin/effects.rs crates/savvagent/src/main.rs
git commit -m "feat(tui): pending-flag drain for routing reload/show effects"
```

---

## Task 6: `/route` plugin

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/route/mod.rs`
- Modify: `crates/savvagent/src/plugin/builtin/mod.rs`
- Modify: `crates/savvagent/src/main.rs` (register the plugin in the built-in set)

The plugin owns the `/route` slash registration and routes `reload` / `show` subcommands to the two new effects.

- [ ] **Step 1: Write the plugin module + tests**

Create `crates/savvagent/src/plugin/builtin/route/mod.rs`:

```rust
//! `internal:route` — manage user routing rules from `~/.savvagent/routing.toml`.
//!
//! Two subcommands:
//! - `/route reload` → `Effect::ReloadRoutingRules`
//! - `/route show`   → `Effect::ShowRoutingRules`
//! - `/route` (bare) → same as `show` (parity with `/sandbox` no-args = status).

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
    StyledLine,
};

/// Plugin that registers the `/route` slash command.
pub struct RoutePlugin;

impl RoutePlugin {
    /// Construct a new [`RoutePlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for RoutePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for RoutePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "route".into(),
            summary: rust_i18n::t!("slash.route-summary").to_string(),
            args_hint: Some("[reload | show]".into()),
            requires_arg: false,
        }];
        Manifest {
            id: PluginId::new("internal:route").expect("valid built-in id"),
            name: "Routing rules".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.route-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "route" {
            return Ok(vec![]);
        }
        let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
        match sub {
            "" | "show" => Ok(vec![Effect::ShowRoutingRules]),
            "reload" => Ok(vec![Effect::ReloadRoutingRules]),
            other => {
                let msg = rust_i18n::t!("routing.route-usage").to_string();
                // `StyledLine::plain` is the only public constructor in
                // savvagent-plugin/src/styled.rs; the muted styling for
                // notes is applied by App::push_styled_note's renderer.
                Ok(vec![Effect::PushNote {
                    line: StyledLine::plain(format!("{msg} (got `{other}`)")),
                }])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bare_route_is_show() {
        let mut p = RoutePlugin::new();
        let effs = p.handle_slash("route", vec![]).await.unwrap();
        assert_eq!(effs.len(), 1);
        assert!(matches!(effs[0], Effect::ShowRoutingRules));
    }

    #[tokio::test]
    async fn route_show_emits_show_effect() {
        let mut p = RoutePlugin::new();
        let effs = p.handle_slash("route", vec!["show".into()]).await.unwrap();
        assert!(matches!(effs[0], Effect::ShowRoutingRules));
    }

    #[tokio::test]
    async fn route_reload_emits_reload_effect() {
        let mut p = RoutePlugin::new();
        let effs = p.handle_slash("route", vec!["reload".into()]).await.unwrap();
        assert!(matches!(effs[0], Effect::ReloadRoutingRules));
    }

    #[tokio::test]
    async fn unknown_subcommand_emits_usage_note() {
        let mut p = RoutePlugin::new();
        let effs = p.handle_slash("route", vec!["wat".into()]).await.unwrap();
        assert!(matches!(effs[0], Effect::PushNote { .. }));
    }
}
```

Verified: `StyledLine::plain` is the only public constructor in `crates/savvagent-plugin/src/styled.rs`. Muted styling for notes is applied by `App::push_styled_note`'s renderer, not by a separate constructor.

- [ ] **Step 2: Register the module in the builtin index**

In `crates/savvagent/src/plugin/builtin/mod.rs`, find the `mod connect;` / `mod save;` / etc. block and add:

```rust
pub mod route;
pub use route::RoutePlugin;
```

In whatever function builds the "list of built-in plugins" (search for `RoutePlugin` analog like `SavePlugin` registration — likely a `build_builtin_plugins` or `register_builtins` function), add:

```rust
plugins.push(Box::new(crate::plugin::builtin::RoutePlugin::new()));
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p savvagent --lib plugin::builtin::route`
Expected: 4 tests PASS.

- [ ] **Step 4: Lint and format**

Run: `rustup run stable cargo fmt --all`
Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/route crates/savvagent/src/plugin/builtin/mod.rs
git commit -m "feat(tui): /route reload + /route show plugin (Phase 5)"
```

---

## Task 7: TUI startup wiring — config path + model precedence + i18n

**Files:**
- Create: `crates/savvagent/src/routing_pref.rs`
- Modify: `crates/savvagent/src/main.rs`
- Modify: `crates/savvagent/locales/en.toml`
- Modify: `crates/savvagent/locales/es.toml`
- Modify: `crates/savvagent/locales/pt.toml`
- Modify: `crates/savvagent/locales/hi.toml`

Wires `HostConfig::routing_rules_path` to `~/.savvagent/routing.toml`, inserts `routing.toml#default` into the two model-resolution sites at the bottom of the precedence chain, and ships the i18n strings.

- [ ] **Step 1: Write the `routing_pref` helper + tests**

Create `crates/savvagent/src/routing_pref.rs`:

```rust
//! TUI-side helper that resolves `~/.savvagent/routing.toml` and reads
//! just the `default = "..."` field for the model-resolution chain.
//!
//! The full `RoutingRules` parser lives in `savvagent-host` and runs at
//! `Host::start`; this helper exists so `main.rs` can consult the
//! file's `default` during startup without depending on the full type.

use std::path::PathBuf;

use savvagent_host::{DefaultPick, RoutingRules};

/// Where the user's `routing.toml` lives, or `None` when no `$HOME`.
pub fn routing_toml_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let home = PathBuf::from(home);
    Some(home.join(".savvagent").join("routing.toml"))
}

/// Load `routing.toml`'s `default` field. Missing file → `None`. Parse
/// errors are logged at `warn!` and treated as `None` — the caller falls
/// back to the next layer in the precedence chain.
pub fn load_default_pick() -> Option<DefaultPick> {
    let path = routing_toml_path()?;
    match RoutingRules::load_from_path(&path) {
        Ok(rules) => rules.default,
        Err(e) => {
            tracing::warn!(error = %e, "could not load routing.toml#default at startup; ignoring");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_path_under_home() {
        // serial guard around HOME — match existing pattern in models_pref.rs
        // (use HOME_LOCK from tests). For brevity here just assert the
        // *shape* when HOME is set; the broader file-roundtrip cases are
        // covered by RoutingRules::load_from_path's own tests.
        let original_home = std::env::var_os("HOME");
        // SAFETY: single-threaded test
        unsafe { std::env::set_var("HOME", "/tmp/savvagent-test-home"); }
        let p = routing_toml_path().expect("HOME set");
        assert!(p.ends_with(".savvagent/routing.toml"));
        // Restore.
        match original_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
```

Add a `mod routing_pref;` declaration in `crates/savvagent/src/main.rs` (near the existing `mod models_pref;`).

- [ ] **Step 2: Populate `HostConfig::routing_rules_path` at startup**

In `crates/savvagent/src/main.rs`, find where `HostConfig::new(...)` is built (look for `.with_project_root(...)` chains). Add:

```rust
        host_config.routing_rules_path = crate::routing_pref::routing_toml_path();
```

right after the `HostConfig` has been constructed. Use the field directly (no `with_*` builder needed because this is internal startup wiring; if you prefer a builder, add `HostConfig::with_routing_rules_path` in `crates/savvagent-host/src/config.rs` mirroring the existing `with_*` helpers).

- [ ] **Step 3: Insert `routing.toml#default` into `resolve_initial_model_for`**

In `crates/savvagent/src/main.rs`, replace `resolve_initial_model_for` (around line 1258) with:

```rust
/// Resolve the effective model id for `provider_id`. Precedence (highest first):
///   SAVVAGENT_MODEL env > ~/.savvagent/models.toml > routing.toml#default > spec.default_model.
fn resolve_initial_model_for(spec: &ProviderSpec) -> String {
    if let Ok(env_model) = std::env::var("SAVVAGENT_MODEL")
        && !env_model.is_empty()
    {
        return env_model;
    }
    let pref = models_pref::ModelsPref::load();
    if let Some(persisted) = pref.get(spec.id) {
        return persisted.to_string();
    }
    if let Some(d) = crate::routing_pref::load_default_pick()
        && d.provider.as_str() == spec.id
    {
        return d.model;
    }
    spec.default_model.to_string()
}
```

- [ ] **Step 4: Insert `routing.toml#default` into the multi-provider startup chain**

In `crates/savvagent/src/main.rs`, locate the block around line 432-447 (`let model = if let Some(m) = resolved_model { ... } else { ... }`). Update the `else` branch to consult `routing_pref` between `models.toml` and `default_model_id`:

```rust
            let model = if let Some(m) = resolved_model {
                m
            } else {
                let base = reg.capabilities.default_model_id().to_string();
                let pref = models_pref::ModelsPref::load();
                if let Some(persisted) = pref.get(reg.id.as_str()) {
                    persisted.to_string()
                } else if let Some(d) = crate::routing_pref::load_default_pick() {
                    if d.provider == reg.id {
                        d.model
                    } else {
                        base
                    }
                } else {
                    base
                }
            };
```

- [ ] **Step 5: Add i18n strings to `en.toml`**

In `crates/savvagent/locales/en.toml`, locate the existing `slash.*` block (around line 17). Add `route-summary` there:

```toml
route-summary                 = "Manage routing rules (reload | show)"
```

Locate the `plugin.*` block and add:

```toml
route-description             = "User routing rules from ~/.savvagent/routing.toml"
```

Add a brand-new top-level section near the end of the file:

```toml
[routing]
reloaded                = "Reloaded routing.toml — %{count} rule(s) active."
reload-failed           = "Couldn't reload routing.toml: %{err}"
show-header             = "Active routing rules (in order):"
show-rule-line          = "[%{index}] %{name} — match: %{match} → %{provider}/%{model}"
show-rule-skipped       = "[%{index}] %{name} — match: %{match} → %{provider}/%{model}  (skipped: provider not connected)"
show-no-rules           = "No routing rules. Edit ~/.savvagent/routing.toml and run /route reload."
show-default            = "Default: %{provider}/%{model}"
show-no-default         = "Default: (using /model selection)"
show-heuristics-pending = "heuristics: enabled — classifier ships in a future release"
show-last               = "Last decision: %{provider}/%{model} — %{reason}"
show-no-last            = "No turns this session yet."
route-usage             = "Usage: /route [show | reload]"
```

- [ ] **Step 6: Add TODO placeholders to es/pt/hi locale files**

In each of `es.toml`, `pt.toml`, `hi.toml`, add the same keys with English text + a `# TODO: translate` comment per the project's existing per-phase practice. Example for `es.toml`:

```toml
# TODO: translate routing.* keys (Phase 5)
[routing]
reloaded                = "Reloaded routing.toml — %{count} rule(s) active."
# ... same as en.toml for the other keys, plus comments.
```

Mirror the new `slash.route-summary` and `plugin.route-description` entries in the same files.

- [ ] **Step 7: Run tests**

Run: `cargo test -p savvagent --lib routing_pref`
Run: `cargo test -p savvagent --lib`
Expected: PASS.

- [ ] **Step 8: Lint and format**

Run: `rustup run stable cargo fmt --all`
Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/savvagent/src/routing_pref.rs crates/savvagent/src/main.rs crates/savvagent/locales/
git commit -m "feat(tui): routing.toml#default precedence + /route i18n strings"
```

---

## Task 8: Integration test — end-to-end Phase 5 scenarios

**Files:**
- Create: `crates/savvagent-host/tests/route_rules_e2e.rs`

Three scenarios, each in its own `#[tokio::test]`:

1. **Rule fires** — a host with anthropic + gemini, routing.toml with one keyword rule pointing at gemini, run a streaming turn with matching user text, assert `TurnEvent::RouteSelected { reason: Rule(...) }`.
2. **Skip-disconnected** — same routing.toml but only anthropic connected, assert the turn falls through to Default.
3. **Reload-mid-turn race** — concurrent loop of `Router::pick` reads vs `reload_routing_rules` writes; assert no panic, no deadlock, final state matches the last write.

- [ ] **Step 1: Write the failing tests**

Create `crates/savvagent-host/tests/route_rules_e2e.rs`:

```rust
//! End-to-end Phase 5 routing-rules integration tests.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use savvagent_host::{
    DefaultPick, Host, HostConfig, ProviderEndpoint, ProviderRegistration, RoutingRule, RoutingRules,
    RoutingReason, RuleMatch, RuleSignals, StartupConnectPolicy, TurnEvent,
    capabilities::{CostTier, ModelCapabilities, ProviderCapabilities},
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ContentBlock, ListModelsResponse, Message, ProviderError,
    ProviderId, Role, StreamEvent,
};
use tokio::sync::{Mutex, mpsc};

/// Stub provider that records every `complete` call's model and returns
/// a single text message.
struct StubProvider {
    model_seen: Arc<Mutex<Option<String>>>,
    response_text: String,
}

#[async_trait::async_trait]
impl ProviderClient for StubProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        *self.model_seen.lock().await = Some(req.model.clone());
        if let Some(tx) = events {
            let _ = tx
                .send(StreamEvent::MessageDelta {
                    text: self.response_text.clone(),
                })
                .await;
            let _ = tx.send(StreamEvent::MessageStop {}).await;
        }
        Ok(CompleteResponse {
            content: vec![ContentBlock::Text {
                text: self.response_text.clone(),
            }],
            stop_reason: Some("end_turn".into()),
            usage: None,
        })
    }
    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        Ok(ListModelsResponse { models: vec![] })
    }
}

fn caps_of(model: &str) -> ProviderCapabilities {
    ProviderCapabilities::new(
        vec![ModelCapabilities {
            id: model.into(),
            display_name: model.into(),
            supports_vision: false,
            supports_audio: false,
            context_window: 0,
            cost_tier: CostTier::Standard,
        }],
        model.into(),
    )
    .expect("caps")
}

fn write_routing_toml(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("routing.toml");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    (dir, path)
}

async fn build_host(
    routing_path: Option<std::path::PathBuf>,
    providers: Vec<(ProviderId, ProviderCapabilities, Arc<dyn ProviderClient + Send + Sync>)>,
) -> Arc<Host> {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused/mcp".into(),
        },
        providers[0].1.default_model_id(),
    );
    cfg.routing_rules_path = routing_path;
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.providers = providers
        .into_iter()
        .map(|(id, caps, client)| {
            let dn = id.as_str().to_string();
            ProviderRegistration::new(id, dn, client, caps)
        })
        .collect();
    Arc::new(Host::start(cfg).await.expect("host starts"))
}

#[tokio::test]
async fn rule_fires_and_routes_to_named_provider() {
    let a_id = ProviderId::new("anthropic").unwrap();
    let g_id = ProviderId::new("gemini").unwrap();
    let a_caps = caps_of("claude-haiku-4-5");
    let g_caps = caps_of("gemini-2.0-flash");
    let a_seen = Arc::new(Mutex::new(None));
    let g_seen = Arc::new(Mutex::new(None));
    let a_client: Arc<dyn ProviderClient + Send + Sync> = Arc::new(StubProvider {
        model_seen: a_seen.clone(),
        response_text: "anthropic answer".into(),
    });
    let g_client: Arc<dyn ProviderClient + Send + Sync> = Arc::new(StubProvider {
        model_seen: g_seen.clone(),
        response_text: "gemini answer".into(),
    });
    let (_d, path) = write_routing_toml(
        r#"
[[rule]]
name = "to-gemini"
match = { keywords = ["refactor"] }
use = "gemini/gemini-2.0-flash"
"#,
    );
    let host = build_host(
        Some(path),
        vec![
            (a_id.clone(), a_caps, a_client),
            (g_id.clone(), g_caps, g_client),
        ],
    )
    .await;

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let host2 = host.clone();
    let runner =
        tokio::spawn(async move { host2.run_turn_streaming("please refactor this", Some(tx)).await });

    let mut saw_rule = false;
    while let Some(ev) = rx.recv().await {
        if let TurnEvent::RouteSelected {
            reason: RoutingReason::Rule { name },
            provider_id,
            ..
        } = ev
        {
            assert_eq!(name, "to-gemini");
            assert_eq!(provider_id, g_id);
            saw_rule = true;
        }
    }
    let _ = runner.await.unwrap();
    assert!(saw_rule, "expected a RouteSelected with reason::Rule");
    assert_eq!(g_seen.lock().await.as_deref(), Some("gemini-2.0-flash"));
}

#[tokio::test]
async fn rule_with_disconnected_provider_falls_through() {
    let a_id = ProviderId::new("anthropic").unwrap();
    let a_caps = caps_of("claude-haiku-4-5");
    let a_seen = Arc::new(Mutex::new(None));
    let a_client: Arc<dyn ProviderClient + Send + Sync> = Arc::new(StubProvider {
        model_seen: a_seen.clone(),
        response_text: "ok".into(),
    });
    let (_d, path) = write_routing_toml(
        r#"
[[rule]]
name = "to-gemini-disconnected"
match = { keywords = ["refactor"] }
use = "gemini/gemini-2.0-flash"
"#,
    );
    let host = build_host(Some(path), vec![(a_id.clone(), a_caps, a_client)]).await;

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let host2 = host.clone();
    let runner = tokio::spawn(async move { host2.run_turn_streaming("please refactor", Some(tx)).await });

    let mut last_reason: Option<RoutingReason> = None;
    while let Some(ev) = rx.recv().await {
        if let TurnEvent::RouteSelected { reason, .. } = ev {
            last_reason = Some(reason);
        }
    }
    let _ = runner.await.unwrap();
    assert_eq!(last_reason, Some(RoutingReason::Default));
    assert_eq!(a_seen.lock().await.as_deref(), Some("claude-haiku-4-5"));
}

#[tokio::test]
async fn reload_during_turn_does_not_deadlock_or_panic() {
    let a_id = ProviderId::new("anthropic").unwrap();
    let a_caps = caps_of("claude-haiku-4-5");
    let a_seen = Arc::new(Mutex::new(None));
    let a_client: Arc<dyn ProviderClient + Send + Sync> = Arc::new(StubProvider {
        model_seen: a_seen.clone(),
        response_text: "ok".into(),
    });
    let (_d, path) = write_routing_toml(
        r#"
[[rule]]
name = "r1"
match = { keywords = ["x"] }
use = "anthropic/claude-haiku-4-5"
"#,
    );
    let host = build_host(Some(path.clone()), vec![(a_id, a_caps, a_client)]).await;

    let host_t = host.clone();
    let reloader = tokio::spawn(async move {
        for i in 0..20 {
            // Rewrite the file with a slightly different rule each iteration.
            let body = format!(
                "[[rule]]\nname = \"r{i}\"\nmatch = {{ keywords = [\"x\"] }}\nuse = \"anthropic/claude-haiku-4-5\"\n"
            );
            std::fs::write(&path, body).unwrap();
            let _ = host_t.reload_routing_rules().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    for _ in 0..10 {
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let host2 = host.clone();
        let runner =
            tokio::spawn(async move { host2.run_turn_streaming("xenon", Some(tx)).await });
        while let Some(_ev) = rx.recv().await {}
        let _ = runner.await.unwrap();
    }

    reloader.await.unwrap();
    // No panic, no deadlock = pass.
}
```

Per [[feedback_test_locale_isolation]] in user memory: these tests don't touch `rust_i18n::set_locale`, so no `HOME_LOCK` is required. If you find a need to read locale-aware strings in the assertions, wrap in the existing `HOME_LOCK` + reset-to-en pattern.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p savvagent-host --test route_rules_e2e`
Expected: compile error or runtime failure — the tests reference the new `routing_rules_path` field + `reload_routing_rules` method added in Task 4.

- [ ] **Step 3: Make tests pass**

Most failures should resolve once Task 4 has landed. If `Host::run_turn_streaming` is the wrong entrypoint (e.g. the streaming version is named differently), search `crates/savvagent-host/src/session.rs` for `pub async fn run_turn_streaming` and adjust. If a stub provider needs additional trait methods, add them as `unreachable!()`.

Re-run: `cargo test -p savvagent-host --test route_rules_e2e`
Expected: 3 tests PASS.

- [ ] **Step 4: Lint and format**

Run: `rustup run stable cargo fmt --all`
Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent-host/tests/route_rules_e2e.rs
git commit -m "test(host): end-to-end Phase 5 routing rules (3 scenarios)"
```

---

## Task 9: Release wiring — version bump, CHANGELOG, README

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `CHANGELOG.md`
- Modify: `README.md`

Per [[feedback_release_notes]] and [[feedback_release_docs]] in user memory, the version bump, CHANGELOG entry, and README update ride in the same commit. Per [[feedback_phase_release_rollup.md]], no tag is pushed in this phase — the rollup tag waits until all multi-provider phases complete.

- [ ] **Step 1: Bump workspace version**

In `Cargo.toml`, change `[workspace.package].version = "0.18.0"` to `"0.19.0"`. Search the same file for every `version = "0.18.0"` literal in `[workspace.dependencies]` and bump each to `"0.19.0"` (per [[feedback_semver]] — internal crate refs must mirror the bump).

Run: `cargo build --workspace`
Expected: clean build (cargo regenerates the lockfile with new versions).

- [ ] **Step 2: Add `0.19.0` CHANGELOG entry**

In `CHANGELOG.md`, add at the top (under `# Changelog` / before `## 0.18.0`). Use whatever today's actual date is when you run this task — example below uses `2026-05-19`:

```markdown
## 0.19.0 - 2026-05-19

### Added

- **User-edited routing rules** (`~/.savvagent/routing.toml`). Routes a turn to a specific `provider/model` based on per-turn predicates (`has_image`, `keywords`, `max_input_chars`, `min_input_chars`). Layer 3 of the multi-provider router, between modality redirects and the default model.
- **`/route show`** prints the active rules, the default, the heuristics-enabled state (Phase 6 stub), and the most recent routing decision.
- **`/route reload`** re-reads `routing.toml` without restarting the TUI. Parse errors keep the prior rules in place and surface a styled note.
- **`routing.toml#default`** is consulted between `~/.savvagent/models.toml` and the provider's hard-coded default during model resolution. Env (`SAVVAGENT_MODEL`) and `models.toml` still take precedence; routing.toml's default replaces the provider's built-in fallback when neither higher layer applies.

### Changed

- `Router::pick` now takes `rules: &RoutingRules` and `user_text: &str` parameters (additive).
- `RoutingReason` gains a `Rule { name }` variant rendered as `Rule(<name>)` in the transcript badge.
```

- [ ] **Step 3: Add a user-facing README section**

In `README.md`, find the existing routing / modality section. Append:

```markdown
### Routing rules

Edit `~/.savvagent/routing.toml` to route turns to specific provider/model combinations based on the message. Example:

```toml
version = 1
default = "anthropic/claude-opus-4-7"

[[rule]]
name = "vision-for-images"
match = { has_image = true }
use = "gemini/gemini-2.0-flash-vision"

[[rule]]
name = "haiku-for-shortform"
match = { max_input_chars = 400 }
use = "anthropic/claude-haiku-4-5"
```

Rules evaluate top-to-bottom; the first match wins. Run `/route reload` after editing the file. Run `/route show` to see the active rules and the most recent routing decision. `@provider:model` overrides and modality redirects still take precedence over rules.
```

- [ ] **Step 4: Verify everything builds**

Run: `cargo build --workspace`
Run: `cargo test --workspace`
Run: `rustup run stable cargo fmt --all`
Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
Expected: all clean. Per [[feedback_match_ci_toolchain_locally]], use the rustup-stable toolchain explicitly to match CI.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md README.md
git commit -m "release(0.19.0): user routing rules + /route reload | /route show"
```

Per [[feedback_phase_release_rollup.md]] in user memory: this is a per-phase scaffolding commit. **Do not push a tag**; the rollup tag for the whole multi-provider initiative lands once all phases are done.

---

## Final verification

After all tasks land:

- [ ] **Workspace tests + lint + fmt all green**

Run: `cargo test --workspace`
Run: `rustup run stable cargo fmt --all -- --check`
Run: `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`

- [ ] **Smoke-test the slash commands manually**

```bash
cargo run -p savvagent
```

In the TUI:
1. `/route show` with no `~/.savvagent/routing.toml` → "No routing rules…" + default line.
2. Create a `~/.savvagent/routing.toml` with a keyword rule.
3. `/route reload` → "Reloaded routing.toml — 1 rule(s) active."
4. `/route show` → rule listed; "No turns this session yet."
5. Send a turn that matches the keyword → assistant entry's badge shows `Rule(<name>)`.
6. `/route show` again → "Last decision: <provider>/<model> — Rule(<name>)".

- [ ] **Verify CI is green on the PR**

Per [[feedback_verify_ci_after_push]] in user memory: after the PR is opened, run `gh run watch` on the head SHA before claiming green.

---

## Notes for the implementer

- **Trust the snapshot-before-await discipline.** Every `.await` in `run_turn_inner` already follows the lock-then-clone pattern. The `routing_rules` snapshot Task 4 adds is one more application of the same discipline; don't be tempted to hold the RwLock across `complete`.
- **`RoutingReason::Rule { name }` semver.** The enum was already `#[non_exhaustive]` per Phase 3, so adding the variant is fully additive. No downstream callers need to add a wildcard arm because they already had one.
- **`models.toml` is unchanged.** Phase 5's only interaction with `models_pref.rs` is *reading* its data inside `resolve_initial_model_for`; the file format and the picker flow are untouched.
- **Per-vendor capability fallback is load-time only.** If a rule's `use.model` isn't in the named provider's `ProviderCapabilities`, the loader currently passes it through verbatim and the router uses it as-is — the provider's request handler will reject if it's truly unknown. If you want stricter load-time validation, extend Task 1's parser; the test for that case is `pick_rule_uses_provider_default_when_model_unknown` (not in this plan, deferred to Phase 6 polish).
- **Cross-provider rule activation is the intended new capability.** Phase 4 deliberately refused silent cross-provider modality hops; Phase 5 enables them by user opt-in (the user edited a routing.toml that crosses the boundary). The `Rule(<name>)` badge is the audit surface, and `/route show` makes the file's effect inspectable.
