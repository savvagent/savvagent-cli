//! User-edited routing rules from `~/.savvagent/routing.toml`.
//!
//! Layer 3 of the router stack. The rules are parsed once at
//! `Host::start` and re-parsed on `/route reload`; the evaluator is a
//! pure function called from `Router::pick` after `@`-override and
//! modality have had their say.
//!
//! The struct shape matches the spec's `routing.toml` example verbatim
//! plus a `version = 1` field for forward-compat. Predicate fields use
//! `Option<bool>` so `match = { has_image = true }` is distinguishable
//! from "predicate absent" (the alternative — bare `bool` defaulting
//! to `false` — would conflate "match only image turns" with "match
//! only non-image turns").
//!
//! `RuleMatch` is `#[non_exhaustive]` so future predicates can land
//! additively.

use std::path::{Path, PathBuf};

use savvagent_protocol::ProviderId;
use serde::Deserialize;

use crate::router::modality::RequiredModalities;

/// Current `routing.toml` schema version. Loaders return
/// [`RoutingRulesError::UnsupportedVersion`] for any file declaring a
/// higher version; the host startup path turns that into a tracing
/// warning and an empty rule set.
pub const ROUTING_RULES_SCHEMA_VERSION: u32 = 1;

/// In-memory representation of `~/.savvagent/routing.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingRules {
    /// Optional default `provider/model` from the file's `default = "..."`.
    pub default: Option<DefaultPick>,
    /// Whether the user opted into the heuristic classifier (Layer 4,
    /// not yet implemented).
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

impl DefaultPick {
    /// Build a `DefaultPick` from a validated `(provider, model)` pair.
    /// Returns `Err(BadModel)` when `model` is empty or contains `/`.
    /// The loader's internal path validates with extra context (file
    /// path, rule index, rule name) and emits `RoutingRulesError`
    /// directly — external callers should use this constructor instead
    /// of building the struct field-by-field so the same invariants
    /// hold regardless of construction site.
    pub fn new(provider: ProviderId, model: impl Into<String>) -> Result<Self, BadModel> {
        let model = model.into();
        if model.is_empty() {
            return Err(BadModel::Empty);
        }
        if model.contains('/') {
            return Err(BadModel::ContainsSlash { got: model });
        }
        Ok(Self { provider, model })
    }
}

/// Validation failure when constructing a `DefaultPick` via
/// [`DefaultPick::new`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BadModel {
    /// Model id was empty.
    #[error("model id is empty")]
    Empty,
    /// Model id contained a `/` (would be ambiguous with provider/model parsing).
    #[error("model id `{got}` contains '/'")]
    ContainsSlash {
        /// The offending value.
        got: String,
    },
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
    /// Require / forbid PDF. Reserved: in v1 the modality detector
    /// never sets `RequiredModalities::has_pdf = true`, so
    /// `has_pdf = true` predicates never match; `has_pdf = false`
    /// matches every turn until the detector grows the field.
    pub has_pdf: Option<bool>,
    /// Require / forbid audio. Reserved: in v1 the modality detector
    /// never sets `RequiredModalities::has_audio = true`, so
    /// `has_audio = true` predicates never match; `has_audio = false`
    /// matches every turn until the detector grows the field.
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
    #[error(
        "routing.toml at {path:?}: rule {index} `{name}`: `use` must be `provider/model`, got `{got}`"
    )]
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
    #[error(
        "routing.toml at {path:?}: rule {index} `{name}`: max_input_chars ({max}) < min_input_chars ({min})"
    )]
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
    /// `reload_routing_rules` was called but `HostConfig::routing_rules_path`
    /// was `None`. The host has nothing to re-read.
    #[error("no routing.toml path configured")]
    NoPathConfigured,
    /// `default = "..."` at the top of the file is not `provider/model`.
    /// Reported with file path + the bad value rather than a synthetic
    /// rule index, so the error message points the user at the `default`
    /// field instead of "rule 0 `default`".
    #[error("routing.toml at {path:?}: `default` must be `provider/model`, got `{got}`")]
    BadDefaultSyntax {
        /// File path.
        path: PathBuf,
        /// The bad `default` value.
        got: String,
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
    /// Empty rules. No rule ever matches; the rules layer of
    /// `Router::pick` is effectively a no-op.
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
            Some(s) if !s.trim().is_empty() => match parse_provider_model_internal(&s) {
                Ok(d) => Some(d),
                Err(_) => {
                    return Err(RoutingRulesError::BadDefaultSyntax {
                        path: path.to_path_buf(),
                        got: s,
                    });
                }
            },
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
        connected: &[crate::router::ProviderView<'_>],
    ) -> Option<(String, DefaultPick)> {
        let text_lower = signals.user_text.to_lowercase();
        let len = signals.user_text.chars().count();
        for rule in &self.rules {
            if !match_satisfied(&rule.match_, signals.required, &text_lower, len) {
                continue;
            }
            if !connected.iter().any(|v| *v.id == rule.use_.provider) {
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
    if let Some(b) = m.has_image
        && required.has_image != b
    {
        return false;
    }
    if let Some(b) = m.has_pdf
        && required.has_pdf != b
    {
        return false;
    }
    if let Some(b) = m.has_audio
        && required.has_audio != b
    {
        return false;
    }
    if let Some(max) = m.max_input_chars
        && char_len > max
    {
        return false;
    }
    if let Some(min) = m.min_input_chars
        && char_len < min
    {
        return false;
    }
    if !m.keywords.is_empty() && !m.keywords.iter().any(|k| text_lower.contains(k)) {
        return false;
    }
    true
}

/// Parse `provider/model` with no surrounding file context. Returns
/// `Err(())` for any structural problem — wrong number of `/`, empty
/// halves, or an invalid provider id. The caller wraps the failure in
/// the appropriate context-bearing `RoutingRulesError` variant.
fn parse_provider_model_internal(raw: &str) -> Result<DefaultPick, ()> {
    // Exactly one `/` is required. `split_once` would still succeed for
    // `"a/b/c"` (split at the first `/`); the count check here makes
    // the "multiple slashes" path explicit and matches the docs.
    if raw.matches('/').count() != 1 {
        return Err(());
    }
    // SAFETY: count check above guarantees exactly one `/`, so
    // `split_once` cannot return `None` here.
    let (p, m) = raw
        .split_once('/')
        .expect("count check above guarantees exactly one '/'");
    let provider = ProviderId::new(p.trim()).map_err(|_| ())?;
    let model = m.trim().to_string();
    if model.is_empty() {
        return Err(());
    }
    Ok(DefaultPick { provider, model })
}

fn parse_provider_model(
    path: &Path,
    rule_index: usize,
    name: &str,
    raw: &str,
) -> Result<DefaultPick, RoutingRulesError> {
    parse_provider_model_internal(raw).map_err(|()| RoutingRulesError::BadUseSyntax {
        path: path.to_path_buf(),
        index: rule_index,
        name: name.to_string(),
        got: raw.to_string(),
    })
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

    fn caps_with_models(models: &[&str]) -> crate::capabilities::ProviderCapabilities {
        use crate::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
        ProviderCapabilities::new(
            models
                .iter()
                .map(|m| ModelCapabilities {
                    id: (*m).into(),
                    display_name: (*m).into(),
                    supports_vision: false,
                    supports_audio: false,
                    context_window: 0,
                    cost_tier: CostTier::Standard,
                })
                .collect(),
            models[0].into(),
        )
        .expect("valid caps")
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
        let a_caps = caps_with_models(&["claude-opus-4-7"]);
        let conn: Vec<crate::router::ProviderView> = vec![crate::router::ProviderView {
            id: &a,
            capabilities: &a_caps,
        }];
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
        let a_caps = caps_with_models(&["claude-opus-4-7"]);
        let conn: Vec<crate::router::ProviderView> = vec![crate::router::ProviderView {
            id: &a,
            capabilities: &a_caps,
        }];
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
        let a_caps = caps_with_models(&["claude-opus-4-7"]);
        let g_caps = caps_with_models(&["gemini-2.0-flash"]);
        let conn: Vec<crate::router::ProviderView> = vec![
            crate::router::ProviderView {
                id: &a,
                capabilities: &a_caps,
            },
            crate::router::ProviderView {
                id: &g,
                capabilities: &g_caps,
            },
        ];
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
        let a_caps = caps_with_models(&["claude-opus-4-7"]);
        let conn: Vec<crate::router::ProviderView> = vec![crate::router::ProviderView {
            id: &a,
            capabilities: &a_caps,
        }]; // gemini disconnected
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
        let a_caps = caps_with_models(&["claude-opus-4-7"]);
        let conn: Vec<crate::router::ProviderView> = vec![crate::router::ProviderView {
            id: &a,
            capabilities: &a_caps,
        }];
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
        let g_caps = caps_with_models(&["gemini-2.0-flash-vision"]);
        let conn: Vec<crate::router::ProviderView> = vec![crate::router::ProviderView {
            id: &g,
            capabilities: &g_caps,
        }];
        assert!(r.evaluate(&signals("", true), &conn).is_some());
        assert!(r.evaluate(&signals("", false), &conn).is_none());
    }

    #[test]
    fn rejects_use_with_multiple_slashes() {
        let (_d, path) = tmp_routing(
            r#"
[[rule]]
name = "multi-slash"
use = "anthropic/claude/latest"
"#,
        );
        let err = RoutingRules::load_from_path(&path).expect_err("rejects");
        assert!(matches!(err, RoutingRulesError::BadUseSyntax { .. }));
    }

    #[test]
    fn default_pick_new_accepts_valid_pair() {
        let p = ProviderId::new("anthropic").unwrap();
        let d = DefaultPick::new(p.clone(), "claude-opus-4-7").expect("valid");
        assert_eq!(d.provider, p);
        assert_eq!(d.model, "claude-opus-4-7");
    }

    #[test]
    fn default_pick_new_rejects_empty_model() {
        let p = ProviderId::new("anthropic").unwrap();
        let err = DefaultPick::new(p, "").expect_err("rejects");
        assert!(matches!(err, BadModel::Empty));
    }

    #[test]
    fn default_pick_new_rejects_slash_in_model() {
        let p = ProviderId::new("anthropic").unwrap();
        let err = DefaultPick::new(p, "a/b").expect_err("rejects");
        assert!(matches!(err, BadModel::ContainsSlash { .. }));
    }

    #[test]
    fn rejects_bad_default_with_dedicated_variant() {
        // `default = "anthropic"` (no slash). Must surface as
        // `BadDefaultSyntax`, not `BadUseSyntax`, so the error message
        // points the user at the `default` field rather than at a fake
        // "rule 0".
        let (_d, path) = tmp_routing(
            r#"
default = "anthropic"
"#,
        );
        let err = RoutingRules::load_from_path(&path).expect_err("rejects");
        assert!(
            matches!(err, RoutingRulesError::BadDefaultSyntax { .. }),
            "expected BadDefaultSyntax, got {err:?}"
        );
    }
}
