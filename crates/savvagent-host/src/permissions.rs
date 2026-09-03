//! Tool-call permission policy.
//!
//! Sits between the host's turn loop and [`crate::tools::ToolRegistry::call`].
//! Each tool use the model proposes is evaluated against a
//! [`PermissionPolicy`], which yields a [`Verdict`]:
//!
//! - [`Verdict::Allow`] — the call proceeds.
//! - [`Verdict::Deny`] — the call is replaced with a synthesized error
//!   `tool_result` that's fed back to the model so it can adjust.
//! - [`Verdict::Ask`] — the host emits
//!   [`crate::TurnEvent::PermissionRequested`] and waits for the embedder to
//!   call [`crate::Host::resolve_permission`] with a [`PermissionDecision`].
//!
//! # Layering (M9 PR 4)
//!
//! `evaluate` walks rule sources in this order, returning the verdict from
//! the first match:
//!
//! 1. **Sensitive-path floor.** Anything matching
//!    [`crate::sensitive_paths::is_sensitive_path`] is *always* `Deny`,
//!    regardless of any other rule. As of v0.7 the floor covers `.env*`
//!    files plus the home-directory secret stems in
//!    [`crate::sensitive_paths::SENSITIVE_HOME_STEMS`].
//! 2. **Front-matter rules** parsed from `SAVVAGENT.md` YAML front-matter
//!    (immutable for the session — project-pinned).
//! 3. **`~/.savvagent/permissions.toml`** rules (mutable — Always/Never
//!    decisions written through by [`PermissionPolicy::add_rule`]).
//! 4. **Built-in defaults.** `read_file`/`list_dir`/`glob` allow,
//!    `write_file` allow inside `project_root` / ask outside, `run` (bash)
//!    ask, anything else ask.
//!
//! Within a source, rules are matched in order and the first match wins —
//! same precedence model as a firewall. Place more-specific deny entries
//! above more-general allow entries when hand-editing.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::project;
use crate::sensitive_paths::is_sensitive_path;

/// Outcome of evaluating a tool call against the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run the tool without prompting.
    Allow,
    /// Pause the turn and ask the user.
    Ask {
        /// Short, human-readable description for the modal.
        summary: String,
    },
    /// Refuse the call. `reason` flows back to the model in the synthesized
    /// `tool_result` so it can re-plan.
    Deny {
        /// Why the policy denied the call.
        reason: String,
    },
}

/// Resolution coming back from the embedder for a [`Verdict::Ask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Run the tool this once.
    Allow,
    /// Refuse this call.
    Deny,
}

/// Policy controlling whether `tool-bash` is allowed network access.
///
/// Evaluated once per session on the first `tool-bash` invocation. The
/// resulting decision is cached on [`PermissionPolicy`] and reused for
/// all subsequent bash invocations until the session ends. Per-call
/// `/bash --net` / `/bash --no-net` flags bypass the cache without
/// updating it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BashNetworkPolicy {
    /// Prompt on first invocation, cache the answer for the session.
    #[default]
    Ask,
    /// Network always allowed — no prompt.
    Always,
    /// Network always denied — no prompt.
    Never,
}

/// User's choice in response to the [`BashNetworkPolicy::Ask`] prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashNetworkChoice {
    /// Allow network for this invocation only. Does NOT update the cache.
    Once,
    /// Allow network for the rest of the session. Updates the cache.
    AlwaysThisSession,
    /// Deny network for this invocation. Does NOT update the cache.
    DenyOnce,
    /// Deny network for the rest of the session. Updates the cache.
    DenyAlways,
    /// User explicitly backed out of the prompt (e.g. pressed Esc).
    /// Effectively deny-once at the policy level, but distinct in the
    /// transcript so the user sees the implicit deny they triggered
    /// rather than reading it as an active "deny" decision they made.
    Cancelled,
}

/// A normalized argument pattern attached to a [`Rule`].
///
/// - [`ArgPattern::Any`] — matches any args for the rule's tool.
/// - [`ArgPattern::Path`] — matches `args["path"]` when the call's path
///   *starts with* this pattern (component-wise via [`Path::starts_with`]).
///   Used for filesystem tools.
/// - [`ArgPattern::Command`] — matches the first whitespace-separated token
///   of `args["command"]`. Used for `run` (bash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgPattern {
    /// Matches any args for this tool.
    Any,
    /// Matches `args["path"]` via component-wise prefix match.
    Path(String),
    /// Matches the first token of `args["command"]`.
    Command(String),
}

impl ArgPattern {
    /// Build a pattern from concrete args, used to record an Always/Never
    /// decision the user just made in the modal. Path tools store the
    /// exact path; command tools store the first token. Editing
    /// `permissions.toml` later lets the user broaden a `Path` rule to a
    /// directory by hand.
    pub fn for_call(tool_name: &str, args: &Value) -> Self {
        match tool_name {
            "read_file" | "write_file" | "list_dir" | "glob" => args
                .get("path")
                .and_then(|v| v.as_str())
                .map(|p| ArgPattern::Path(p.to_string()))
                .unwrap_or(ArgPattern::Any),
            "run" => args
                .get("command")
                .and_then(|v| v.as_str())
                .and_then(|c| c.split_whitespace().next())
                .map(|w| ArgPattern::Command(w.to_string()))
                .unwrap_or(ArgPattern::Any),
            _ => ArgPattern::Any,
        }
    }

    /// Does this pattern match the args in front of us?
    pub fn matches(&self, args: &Value) -> bool {
        match self {
            ArgPattern::Any => true,
            ArgPattern::Path(want) => args
                .get("path")
                .and_then(|v| v.as_str())
                .map(|p| Path::new(p).starts_with(Path::new(want)))
                .unwrap_or(false),
            ArgPattern::Command(want) => args
                .get("command")
                .and_then(|v| v.as_str())
                .and_then(|c| c.split_whitespace().next())
                .map(|w| w == want)
                .unwrap_or(false),
        }
    }
}

/// One rule resolved from a config source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Tool the rule applies to (e.g. `read_file`, `run`).
    pub tool_name: String,
    /// Which calls the rule matches.
    pub pattern: ArgPattern,
    /// What to do on a match.
    pub decision: PermissionDecision,
}

// ---------------------------------------------------------------------------
// On-disk schema
// ---------------------------------------------------------------------------

/// Wire format for a rule on disk (TOML and front-matter share this shape).
/// Internally normalized to [`Rule`] via [`from_serializable`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SerializableRule {
    /// Tool this rule scopes to.
    pub tool: String,
    /// Path pattern; mutually exclusive with `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// First-word command pattern; mutually exclusive with `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// On-disk shape of `~/.savvagent/permissions.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionsToml {
    /// Allow rules. First match wins; place more-specific rules first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<SerializableRule>,
    /// Deny rules. Same matching semantics as `allow`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<SerializableRule>,
}

/// Permissions section of `SAVVAGENT.md`'s YAML front-matter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrontMatterPermissions {
    /// Allow rules.
    #[serde(default)]
    pub allow: Vec<SerializableRule>,
    /// Deny rules.
    #[serde(default)]
    pub deny: Vec<SerializableRule>,
}

fn from_serializable(s: SerializableRule, decision: PermissionDecision) -> Rule {
    let pattern = if let Some(p) = s.path {
        ArgPattern::Path(p)
    } else if let Some(c) = s.command {
        ArgPattern::Command(c)
    } else {
        ArgPattern::Any
    };
    Rule {
        tool_name: s.tool,
        pattern,
        decision,
    }
}

fn to_serializable(r: &Rule) -> SerializableRule {
    let (path, command) = match &r.pattern {
        ArgPattern::Path(p) => (Some(p.clone()), None),
        ArgPattern::Command(c) => (None, Some(c.clone())),
        ArgPattern::Any => (None, None),
    };
    SerializableRule {
        tool: r.tool_name.clone(),
        path,
        command,
    }
}

fn rules_from_permissions(allow: Vec<SerializableRule>, deny: Vec<SerializableRule>) -> Vec<Rule> {
    let mut out = Vec::with_capacity(allow.len() + deny.len());
    for r in allow {
        out.push(from_serializable(r, PermissionDecision::Allow));
    }
    for r in deny {
        out.push(from_serializable(r, PermissionDecision::Deny));
    }
    out
}

fn permissions_from_rules(rules: &[Rule]) -> PermissionsToml {
    let mut out = PermissionsToml::default();
    for r in rules {
        let s = to_serializable(r);
        match r.decision {
            PermissionDecision::Allow => out.allow.push(s),
            PermissionDecision::Deny => out.deny.push(s),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// Layered permission policy. Cheap to clone — internal state is `Arc`-shared.
#[derive(Debug, Clone)]
pub struct PermissionPolicy {
    project_root: PathBuf,
    /// Rules from `SAVVAGENT.md` front-matter — immutable for the session.
    front_matter_rules: Arc<Vec<Rule>>,
    /// Path to `~/.savvagent/permissions.toml`. `None` when `HOME` is unset.
    toml_path: Option<PathBuf>,
    /// Rules loaded from `permissions.toml`. Mutable; written through on
    /// [`PermissionPolicy::add_rule`].
    toml_rules: Arc<RwLock<Vec<Rule>>>,
    /// Configured bash-network policy for this session.
    ///
    /// Currently always [`BashNetworkPolicy::default()`] (`Ask`) from
    /// [`PermissionPolicy::default_for`]. A `permissions.toml` loader
    /// for the `[bash_network] mode = "ask"|"always"|"never"` shape is
    /// a v0.7+ follow-up. Tests can override via the
    /// `#[cfg(test)] with_bash_network` setter.
    bash_network: BashNetworkPolicy,
    /// Session-scoped cached decision. Populated by
    /// [`PermissionPolicy::resolve_bash_network`] when the user picks an
    /// "always" / "deny-always" variant. `None` means "not yet decided
    /// in this session".
    bash_network_decision: Arc<RwLock<Option<bool>>>,
}

impl PermissionPolicy {
    /// Build the default policy for `project_root`. Loads SAVVAGENT.md
    /// front-matter and `~/.savvagent/permissions.toml` if they exist;
    /// silently falls back to empty rule sets on any parse error.
    pub fn default_for(project_root: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        let front_matter_rules = Arc::new(load_front_matter_rules(&project_root));
        let toml_path = permissions_toml_path();
        let toml_rules = toml_path
            .as_deref()
            .map(load_toml_rules)
            .unwrap_or_default();
        Self {
            project_root,
            front_matter_rules,
            toml_path,
            toml_rules: Arc::new(RwLock::new(toml_rules)),
            bash_network: BashNetworkPolicy::default(),
            bash_network_decision: Arc::new(RwLock::new(None)),
        }
    }

    /// Build a policy that doesn't read from or write to disk. Front-matter
    /// is *not* loaded from `project_root`; the rule sets start empty.
    /// Intended for tests and embedders that want to manage policy state
    /// out-of-band.
    pub fn transient(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            front_matter_rules: Arc::new(Vec::new()),
            toml_path: None,
            toml_rules: Arc::new(RwLock::new(Vec::new())),
            bash_network: BashNetworkPolicy::default(),
            bash_network_decision: Arc::new(RwLock::new(None)),
        }
    }

    /// Test-only setter for the bash-network policy. Used by tests to
    /// exercise [`PermissionPolicy::resolve_bash_network`] without going
    /// through the (still-TBD) `permissions.toml` loader.
    #[cfg(test)]
    pub(crate) fn with_bash_network(mut self, policy: BashNetworkPolicy) -> Self {
        self.bash_network = policy;
        self
    }

    /// Returns the configured bash network policy.
    pub fn bash_network(&self) -> BashNetworkPolicy {
        self.bash_network
    }

    /// Returns the cached bash network decision for this session, if any.
    /// `None` means the user has not yet picked an "always" / "deny-always"
    /// variant in the `Ask` policy's modal — and so the next bash spawn
    /// will prompt again.
    pub fn bash_network_cached(&self) -> Option<bool> {
        *self.bash_network_decision.read().unwrap()
    }

    /// Resolve `tool-bash` network access for the current invocation.
    /// Honors the configured [`BashNetworkPolicy`] and the session
    /// decision cache; prompts via `prompt` only when the policy is
    /// `Ask` and the cache is empty.
    ///
    /// Returns the resolved `allow_net` boolean. May update
    /// `bash_network_decision` (the session cache) if the user picks an
    /// "always" / "deny-always" variant.
    pub fn resolve_bash_network<F: FnOnce() -> BashNetworkChoice>(&self, prompt: F) -> bool {
        match self.bash_network {
            BashNetworkPolicy::Always => {
                *self.bash_network_decision.write().unwrap() = Some(true);
                true
            }
            BashNetworkPolicy::Never => {
                *self.bash_network_decision.write().unwrap() = Some(false);
                false
            }
            BashNetworkPolicy::Ask => {
                if let Some(cached) = *self.bash_network_decision.read().unwrap() {
                    return cached;
                }
                match prompt() {
                    BashNetworkChoice::Once => true,
                    BashNetworkChoice::AlwaysThisSession => {
                        *self.bash_network_decision.write().unwrap() = Some(true);
                        true
                    }
                    // Cancelled and DenyOnce are policy-equivalent (deny
                    // without caching). They differ only in how the TUI
                    // labels the outcome in the transcript.
                    BashNetworkChoice::DenyOnce | BashNetworkChoice::Cancelled => false,
                    BashNetworkChoice::DenyAlways => {
                        *self.bash_network_decision.write().unwrap() = Some(false);
                        false
                    }
                }
            }
        }
    }

    /// Resolve a verdict for a tool call.
    pub fn evaluate(&self, tool_name: &str, args: &Value) -> Verdict {
        // 1. Sensitive-path floor — always denies, can't be overridden.
        if let Some(p) = path_arg(args) {
            if is_sensitive_path(&p) {
                return Verdict::Deny {
                    reason: format!(
                        "path `{p}` is policy-protected (matches sensitive-path deny list)"
                    ),
                };
            }
        }

        // 2. Front-matter rules.
        if let Some(decision) = match_first(&self.front_matter_rules, tool_name, args) {
            return self.decision_to_verdict(decision, tool_name, args, "SAVVAGENT.md");
        }

        // 3. permissions.toml rules.
        let toml_decision = {
            let guard = self.toml_rules.read().expect("toml_rules poisoned");
            match_first(&guard, tool_name, args)
        };
        if let Some(decision) = toml_decision {
            return self.decision_to_verdict(decision, tool_name, args, "permissions.toml");
        }

        // 4. Built-in defaults.
        self.default_verdict(tool_name, args)
    }

    fn decision_to_verdict(
        &self,
        decision: PermissionDecision,
        tool_name: &str,
        args: &Value,
        source: &str,
    ) -> Verdict {
        match decision {
            PermissionDecision::Allow => Verdict::Allow,
            PermissionDecision::Deny => Verdict::Deny {
                reason: format!("denied by {source} for {tool_name} {}", short_args(args)),
            },
        }
    }

    fn default_verdict(&self, tool_name: &str, args: &Value) -> Verdict {
        match tool_name {
            "read_file" | "list_dir" | "glob" => Verdict::Allow,
            "write_file" => match path_arg(args) {
                Some(p) if is_under(&p, &self.project_root) => Verdict::Allow,
                Some(p) => Verdict::Ask {
                    summary: format!("write_file outside project root: {p}"),
                },
                None => Verdict::Ask {
                    summary: "write_file with no path".into(),
                },
            },
            "run" => Verdict::Ask {
                summary: command_summary(args).unwrap_or_else(|| "run".into()),
            },
            "web_fetch" => Verdict::Ask {
                summary: args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(|u| format!("web_fetch: {u}"))
                    .unwrap_or_else(|| "web_fetch".into()),
            },
            "web_search" => Verdict::Ask {
                summary: args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(|q| format!("web_search: {q}"))
                    .unwrap_or_else(|| "web_search".into()),
            },
            other => Verdict::Ask {
                summary: format!("{other}({})", short_args(args)),
            },
        }
    }

    /// Persist a user-recorded Always/Never decision. Builds an
    /// [`ArgPattern`] from `(tool_name, args)`, replaces any existing rule
    /// with the same `(tool_name, pattern)` key, and writes the new
    /// `permissions.toml` to disk. The in-memory rule set is updated
    /// regardless of whether the disk write succeeds.
    pub async fn add_rule(
        &self,
        tool_name: &str,
        args: &Value,
        decision: PermissionDecision,
    ) -> std::io::Result<()> {
        let new_rule = Rule {
            tool_name: tool_name.to_string(),
            pattern: ArgPattern::for_call(tool_name, args),
            decision,
        };

        let serialized = {
            let mut guard = self.toml_rules.write().expect("toml_rules poisoned");
            guard.retain(|r| !(r.tool_name == new_rule.tool_name && r.pattern == new_rule.pattern));
            guard.push(new_rule);
            toml::to_string_pretty(&permissions_from_rules(&guard))
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        };

        if let Some(path) = &self.toml_path {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(path, serialized).await?;
        }
        Ok(())
    }

    /// Snapshot of the toml-backed rules. Useful for tests and `/tools`
    /// listings that want to surface the current policy.
    pub fn toml_rules_snapshot(&self) -> Vec<Rule> {
        self.toml_rules.read().expect("toml_rules poisoned").clone()
    }

    /// Snapshot of the project's front-matter rules.
    pub fn front_matter_rules(&self) -> &[Rule] {
        &self.front_matter_rules
    }
}

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

fn load_front_matter_rules(project_root: &Path) -> Vec<Rule> {
    let parsed = project::parse_savvagent_md(project_root);
    rules_from_permissions(parsed.permissions.allow, parsed.permissions.deny)
}

fn load_toml_rules(path: &Path) -> Vec<Rule> {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<PermissionsToml>(&text) {
            Ok(p) => rules_from_permissions(p.allow, p.deny),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

/// `~/.savvagent/permissions.toml`. `None` if `HOME` (and `USERPROFILE`
/// on Windows) are unset — the policy still works, it just can't persist.
fn permissions_toml_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(home.join(".savvagent").join("permissions.toml"))
}

// ---------------------------------------------------------------------------
// Matching helpers
// ---------------------------------------------------------------------------

fn match_first(rules: &[Rule], tool_name: &str, args: &Value) -> Option<PermissionDecision> {
    rules
        .iter()
        .find(|r| r.tool_name == tool_name && r.pattern.matches(args))
        .map(|r| r.decision)
}

fn path_arg(args: &Value) -> Option<String> {
    args.get("path").and_then(|v| v.as_str()).map(str::to_owned)
}

fn command_summary(args: &Value) -> Option<String> {
    let cmd = args.get("command").and_then(|v| v.as_str())?;
    let trimmed: String = cmd.chars().take(80).collect();
    Some(format!("run: {trimmed}"))
}

fn short_args(args: &Value) -> String {
    let s = serde_json::to_string(args).unwrap_or_else(|_| "?".into());
    if s.len() <= 80 {
        s
    } else {
        format!("{}...", &s[..80])
    }
}

fn is_under(p: &str, root: &Path) -> bool {
    let path = Path::new(p);
    if path.is_absolute() {
        return path.starts_with(root);
    }
    let mut depth: i32 = 0;
    for c in path.components() {
        use std::path::Component::*;
        match c {
            Normal(_) => depth += 1,
            CurDir => {}
            ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build an empty policy rooted at /home/me/proj — no front-matter, no
    /// toml-on-disk. Used to exercise the defaults branch in isolation.
    fn empty() -> PermissionPolicy {
        PermissionPolicy {
            project_root: PathBuf::from("/home/me/proj"),
            front_matter_rules: Arc::new(Vec::new()),
            toml_path: None,
            toml_rules: Arc::new(RwLock::new(Vec::new())),
            bash_network: BashNetworkPolicy::default(),
            bash_network_decision: Arc::new(RwLock::new(None)),
        }
    }

    #[test]
    fn read_list_glob_allowed_by_default() {
        for tool in ["read_file", "list_dir", "glob"] {
            assert_eq!(
                empty().evaluate(tool, &json!({"path": "src/lib.rs"})),
                Verdict::Allow,
                "{tool}",
            );
        }
    }

    #[test]
    fn write_file_inside_project_allowed_by_default() {
        assert_eq!(
            empty().evaluate("write_file", &json!({"path": "src/lib.rs"})),
            Verdict::Allow,
        );
    }

    #[test]
    fn write_file_outside_project_asks_by_default() {
        assert!(matches!(
            empty().evaluate("write_file", &json!({"path": "/etc/hosts"})),
            Verdict::Ask { .. }
        ));
    }

    #[test]
    fn dotenv_floor_denies_even_with_allow_rule() {
        let mut p = empty();
        // Front-matter says "allow read_file .env" — should be ignored.
        p.front_matter_rules = Arc::new(vec![Rule {
            tool_name: "read_file".into(),
            pattern: ArgPattern::Path(".env".into()),
            decision: PermissionDecision::Allow,
        }]);
        assert!(matches!(
            p.evaluate("read_file", &json!({"path": ".env"})),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn widened_floor_denies_even_with_allow_rule() {
        // Each of these paths matches a SENSITIVE_HOME_STEMS entry. The floor
        // must deny them even when a front-matter allow rule names the exact
        // path.
        let cases = [
            ".aws/credentials",
            ".gnupg/private-keys-v1.d/foo.key",
            ".config/gh/hosts.yml",
            ".netrc",
        ];
        for path in cases {
            let mut p = empty();
            p.front_matter_rules = Arc::new(vec![Rule {
                tool_name: "read_file".into(),
                pattern: ArgPattern::Path(path.into()),
                decision: PermissionDecision::Allow,
            }]);
            let verdict = p.evaluate("read_file", &json!({ "path": path }));
            assert!(
                matches!(verdict, Verdict::Deny { .. }),
                "floor must deny `{path}` even with allow rule; got {verdict:?}"
            );
        }
    }

    #[test]
    fn front_matter_overrides_default() {
        let mut p = empty();
        p.front_matter_rules = Arc::new(vec![Rule {
            tool_name: "run".into(),
            pattern: ArgPattern::Command("cargo".into()),
            decision: PermissionDecision::Allow,
        }]);
        assert_eq!(
            p.evaluate("run", &json!({"command": "cargo test"})),
            Verdict::Allow,
        );
        // A different command still falls through to the default Ask.
        assert!(matches!(
            p.evaluate("run", &json!({"command": "rm -rf /"})),
            Verdict::Ask { .. }
        ));
    }

    #[test]
    fn front_matter_beats_toml_on_conflict() {
        let mut p = empty();
        p.front_matter_rules = Arc::new(vec![Rule {
            tool_name: "run".into(),
            pattern: ArgPattern::Command("cargo".into()),
            decision: PermissionDecision::Deny,
        }]);
        *p.toml_rules.write().unwrap() = vec![Rule {
            tool_name: "run".into(),
            pattern: ArgPattern::Command("cargo".into()),
            decision: PermissionDecision::Allow,
        }];
        assert!(matches!(
            p.evaluate("run", &json!({"command": "cargo test"})),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn toml_rule_overrides_default() {
        let p = empty();
        *p.toml_rules.write().unwrap() = vec![Rule {
            tool_name: "run".into(),
            pattern: ArgPattern::Command("ls".into()),
            decision: PermissionDecision::Allow,
        }];
        assert_eq!(
            p.evaluate("run", &json!({"command": "ls -la"})),
            Verdict::Allow,
        );
    }

    #[test]
    fn first_match_wins_within_a_source() {
        let p = empty();
        // First rule denies; second would allow. First-match-wins → Deny.
        *p.toml_rules.write().unwrap() = vec![
            Rule {
                tool_name: "write_file".into(),
                pattern: ArgPattern::Path("src/secret.rs".into()),
                decision: PermissionDecision::Deny,
            },
            Rule {
                tool_name: "write_file".into(),
                pattern: ArgPattern::Path("src".into()),
                decision: PermissionDecision::Allow,
            },
        ];
        assert!(matches!(
            p.evaluate("write_file", &json!({"path": "src/secret.rs"})),
            Verdict::Deny { .. }
        ));
        // A different file under src still hits the second (broader) rule.
        assert_eq!(
            p.evaluate("write_file", &json!({"path": "src/lib.rs"})),
            Verdict::Allow,
        );
    }

    #[test]
    fn arg_pattern_path_uses_component_prefix() {
        let pat = ArgPattern::Path("src".into());
        assert!(pat.matches(&json!({"path": "src/lib.rs"})));
        assert!(pat.matches(&json!({"path": "src"})));
        assert!(!pat.matches(&json!({"path": "source/file.rs"})));
        assert!(!pat.matches(&json!({"path": "lib.rs"})));
    }

    #[test]
    fn arg_pattern_command_matches_first_token() {
        let pat = ArgPattern::Command("cargo".into());
        assert!(pat.matches(&json!({"command": "cargo build"})));
        assert!(pat.matches(&json!({"command": "cargo"})));
        assert!(!pat.matches(&json!({"command": "rustup run cargo"})));
    }

    #[test]
    fn for_call_extracts_path_and_command() {
        match ArgPattern::for_call("write_file", &json!({"path": "src/lib.rs"})) {
            ArgPattern::Path(p) => assert_eq!(p, "src/lib.rs"),
            other => panic!("{other:?}"),
        }
        match ArgPattern::for_call("run", &json!({"command": "cargo test"})) {
            ArgPattern::Command(c) => assert_eq!(c, "cargo"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            ArgPattern::for_call("mystery", &json!({"x": 1})),
            ArgPattern::Any,
        );
    }

    #[tokio::test]
    async fn add_rule_writes_through_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join(".savvagent/permissions.toml");
        let p = PermissionPolicy {
            project_root: PathBuf::from("/tmp"),
            front_matter_rules: Arc::new(Vec::new()),
            toml_path: Some(toml_path.clone()),
            toml_rules: Arc::new(RwLock::new(Vec::new())),
            bash_network: BashNetworkPolicy::default(),
            bash_network_decision: Arc::new(RwLock::new(None)),
        };

        p.add_rule(
            "run",
            &json!({"command": "cargo test"}),
            PermissionDecision::Allow,
        )
        .await
        .unwrap();

        let on_disk = std::fs::read_to_string(&toml_path).unwrap();
        let parsed: PermissionsToml = toml::from_str(&on_disk).unwrap();
        assert_eq!(parsed.allow.len(), 1);
        assert_eq!(parsed.allow[0].tool, "run");
        assert_eq!(parsed.allow[0].command.as_deref(), Some("cargo"));
        assert!(parsed.deny.is_empty());

        // Adding the same key again replaces, doesn't duplicate.
        p.add_rule(
            "run",
            &json!({"command": "cargo build"}),
            PermissionDecision::Deny,
        )
        .await
        .unwrap();
        let parsed: PermissionsToml =
            toml::from_str(&std::fs::read_to_string(&toml_path).unwrap()).unwrap();
        assert!(parsed.allow.is_empty());
        assert_eq!(parsed.deny.len(), 1);
    }

    #[test]
    fn missing_toml_path_does_not_block_evaluation() {
        let p = PermissionPolicy {
            project_root: PathBuf::from("/tmp"),
            front_matter_rules: Arc::new(Vec::new()),
            toml_path: None,
            toml_rules: Arc::new(RwLock::new(Vec::new())),
            bash_network: BashNetworkPolicy::default(),
            bash_network_decision: Arc::new(RwLock::new(None)),
        };
        assert_eq!(
            p.evaluate("read_file", &json!({"path": "src/lib.rs"})),
            Verdict::Allow,
        );
    }

    #[test]
    fn bash_network_always_resolves_without_prompt() {
        let p = PermissionPolicy::transient("/tmp/x").with_bash_network(BashNetworkPolicy::Always);
        let prompted = std::cell::Cell::new(false);
        let allowed = p.resolve_bash_network(|| {
            prompted.set(true);
            BashNetworkChoice::Once
        });
        assert!(allowed);
        assert!(!prompted.get(), "Always policy must not prompt");
        assert_eq!(*p.bash_network_decision.read().unwrap(), Some(true));
    }

    #[test]
    fn bash_network_never_resolves_false_without_prompt() {
        let p = PermissionPolicy::transient("/tmp/x").with_bash_network(BashNetworkPolicy::Never);
        let prompted = std::cell::Cell::new(false);
        let allowed = p.resolve_bash_network(|| {
            prompted.set(true);
            BashNetworkChoice::Once
        });
        assert!(!allowed);
        assert!(!prompted.get(), "Never policy must not prompt");
        assert_eq!(*p.bash_network_decision.read().unwrap(), Some(false));
    }

    #[test]
    fn bash_network_ask_prompts_then_caches_always() {
        let p = PermissionPolicy::transient("/tmp/x").with_bash_network(BashNetworkPolicy::Ask);

        // First call: prompt fires, user picks AlwaysThisSession.
        let count = std::cell::Cell::new(0);
        let allowed = p.resolve_bash_network(|| {
            count.set(count.get() + 1);
            BashNetworkChoice::AlwaysThisSession
        });
        assert!(allowed);
        assert_eq!(count.get(), 1);
        assert_eq!(*p.bash_network_decision.read().unwrap(), Some(true));

        // Second call: cached, prompt must NOT fire.
        let allowed = p.resolve_bash_network(|| {
            panic!("must not prompt when decision is cached");
        });
        assert!(allowed);
    }

    #[test]
    fn bash_network_ask_once_does_not_cache() {
        let p = PermissionPolicy::transient("/tmp/x").with_bash_network(BashNetworkPolicy::Ask);

        let count = std::cell::Cell::new(0);
        let prompt = || {
            count.set(count.get() + 1);
            BashNetworkChoice::Once
        };
        let allowed = p.resolve_bash_network(prompt);
        assert!(allowed);
        assert_eq!(*p.bash_network_decision.read().unwrap(), None);

        // Second call: must prompt again.
        let allowed = p.resolve_bash_network(prompt);
        assert!(allowed);
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn bash_network_ask_deny_once_does_not_cache() {
        let p = PermissionPolicy::transient("/tmp/x").with_bash_network(BashNetworkPolicy::Ask);
        let count = std::cell::Cell::new(0);
        let prompt = || {
            count.set(count.get() + 1);
            BashNetworkChoice::DenyOnce
        };
        assert!(!p.resolve_bash_network(prompt));
        assert_eq!(*p.bash_network_decision.read().unwrap(), None);
        // Second call must prompt again.
        assert!(!p.resolve_bash_network(prompt));
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn bash_network_ask_deny_always_caches() {
        let p = PermissionPolicy::transient("/tmp/x").with_bash_network(BashNetworkPolicy::Ask);
        let count = std::cell::Cell::new(0);
        let prompt = || {
            count.set(count.get() + 1);
            BashNetworkChoice::DenyAlways
        };
        assert!(!p.resolve_bash_network(prompt));
        assert_eq!(*p.bash_network_decision.read().unwrap(), Some(false));
        // Second call must NOT prompt — cached.
        assert!(!p.resolve_bash_network(|| {
            panic!("must not prompt when cached deny is in effect");
        }));
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn resolve_bash_network_is_not_called_with_per_call_override() {
        // The per-call override is handled in `session::wire_self_into_resolver`'s
        // closure BEFORE `resolve_bash_network` runs — verified here by reading
        // the code path. This test pins the contract at the PermissionPolicy
        // level: resolve_bash_network has no awareness of per-call overrides
        // because the closure intercepts them.
        //
        // Companion: the closure test in session.rs ensures the closure
        // honors that contract.

        let p = PermissionPolicy::transient("/tmp/x").with_bash_network(BashNetworkPolicy::Ask);

        // Pre-condition: cache empty.
        assert_eq!(*p.bash_network_decision.read().unwrap(), None);

        // resolve_bash_network has no `override` parameter — it can only
        // be reached when the closure decided no override applied. So if
        // we are here, calling it is equivalent to "no override, prompt".
        // Sanity-check: cache stays None when Once is picked.
        let _ = p.resolve_bash_network(|| BashNetworkChoice::Once);
        assert_eq!(*p.bash_network_decision.read().unwrap(), None);
    }
}
