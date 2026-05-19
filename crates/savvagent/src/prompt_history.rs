//! Per-project prompt history for the main input field.
//!
//! Submitted prompts are appended to `~/.savvagent/prompt_history/<hash>.jsonl`
//! where `<hash>` is a 16-hex FNV-1a-64 digest of the canonicalized project
//! root. Up at an empty prompt recalls the most recent entry; further Up/Down
//! navigate the list while the recalled text is still in the textarea.
//!
//! On-disk format is JSON Lines (one JSON-encoded string per line). Bad lines
//! are skipped on read so a hand-edit can't tank the history. Saves are
//! atomic — write `<file>.tmp`, then rename onto the target.
//!
//! The 1000-entry cap is enforced on every append; older entries are dropped
//! from the front. Consecutive duplicates and empty/whitespace-only prompts
//! are not recorded.

use std::path::{Path, PathBuf};

/// Maximum number of entries kept on disk and in memory. Older entries are
/// dropped from the front when this cap is exceeded.
pub const HISTORY_CAP: usize = 1000;

/// Tracks an in-progress browse through history. `marker` is the text we
/// last placed in the textarea so we can detect when the user has started
/// editing — if the current textarea content no longer matches, browsing
/// is implicitly cancelled and Up falls back to its "fresh recall" path
/// (which itself requires an empty input).
#[derive(Debug, Clone)]
struct BrowseState {
    /// Index into `entries`. Always `< entries.len()`.
    pos: usize,
    /// Text last written to the textarea by recall_prev/recall_next.
    marker: String,
}

/// Per-project prompt history. Owns the on-disk file path so callers don't
/// have to re-derive it on every append.
#[derive(Default)]
pub struct PromptHistory {
    /// Chronological, oldest → newest.
    entries: Vec<String>,
    /// `~/.savvagent/prompt_history/<hash>.jsonl`. `None` when `$HOME` is
    /// unavailable; appends still work in memory but won't persist.
    path: Option<PathBuf>,
    browse: Option<BrowseState>,
}

impl PromptHistory {
    /// Load history for `project_root`. Missing file → empty. Parse errors
    /// on individual lines are skipped (warn-logged). When `$HOME` isn't
    /// set we still return a usable in-memory instance — appends just
    /// won't survive the session.
    pub fn load(project_root: &Path) -> Self {
        let Some(path) = file_path_for(project_root) else {
            return Self::default();
        };
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => parse_jsonl(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "prompt_history: read failed; starting with empty history"
                );
                Vec::new()
            }
        };
        Self {
            entries,
            path: Some(path),
            browse: None,
        }
    }

    /// Append a submitted prompt. Empty/whitespace-only prompts and prompts
    /// equal to the most recent entry are dropped (no append, no save).
    /// Exits any in-progress browse so the next Up starts fresh.
    pub fn append(&mut self, prompt: String) {
        self.browse = None;
        if prompt.trim().is_empty() {
            return;
        }
        if self.entries.last().is_some_and(|last| last == &prompt) {
            return;
        }
        self.entries.push(prompt);
        while self.entries.len() > HISTORY_CAP {
            self.entries.remove(0);
        }
        self.save();
    }

    /// Recall the previous prompt. Returns `Some(text)` to write into the
    /// textarea, or `None` if there's nothing applicable (empty history,
    /// or non-empty input without an active browse).
    ///
    /// Contract (matches the "only when input is completely empty" UX rule):
    /// - With an active browse where `current_text == marker`, step one
    ///   entry older. At the oldest entry, stay put — return that entry so
    ///   the caller's textarea content doesn't blip.
    /// - Otherwise, only enter browsing when `current_text` is empty after
    ///   trim and the history is non-empty.
    pub fn recall_prev(&mut self, current_text: &str) -> Option<String> {
        if let Some(state) = &self.browse {
            if state.marker == current_text {
                let new_pos = state.pos.saturating_sub(1);
                let entry = self.entries.get(new_pos)?.clone();
                self.browse = Some(BrowseState {
                    pos: new_pos,
                    marker: entry.clone(),
                });
                return Some(entry);
            }
        }
        if !current_text.is_empty() {
            return None;
        }
        if self.entries.is_empty() {
            return None;
        }
        let pos = self.entries.len() - 1;
        let entry = self.entries[pos].clone();
        self.browse = Some(BrowseState {
            pos,
            marker: entry.clone(),
        });
        Some(entry)
    }

    /// Recall the next (newer) prompt during an active browse. Returns
    /// `Some(text)` for a newer entry, `Some(String::new())` when stepping
    /// past the newest (caller clears the textarea), or `None` when there's
    /// no active browse to step within.
    pub fn recall_next(&mut self, current_text: &str) -> Option<String> {
        let state = self.browse.as_ref()?;
        if state.marker != current_text {
            // User edited; treat browse as cancelled. Don't clear the
            // textarea behind their back.
            self.browse = None;
            return None;
        }
        let new_pos = state.pos + 1;
        if new_pos >= self.entries.len() {
            self.browse = None;
            return Some(String::new());
        }
        let entry = self.entries[new_pos].clone();
        self.browse = Some(BrowseState {
            pos: new_pos,
            marker: entry.clone(),
        });
        Some(entry)
    }

    /// Read-only entries snapshot. Used by tests; not part of the
    /// runtime input loop.
    #[cfg(test)]
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Whether a browse is currently in progress. Used by tests.
    #[cfg(test)]
    pub fn is_browsing(&self) -> bool {
        self.browse.is_some()
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(e) = write_atomic(path, &self.entries) {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "prompt_history: save failed; entry kept in memory only"
            );
        }
    }
}

/// `~/.savvagent/prompt_history/<hash>.jsonl`, where `<hash>` is the
/// FNV-1a-64 digest of the canonical project root path. Returns `None`
/// when neither `$HOME` nor `$USERPROFILE` is set.
///
/// We hash the path (rather than embedding the path literally) so the
/// filename has a predictable length and shape regardless of how deep
/// the project lives.
fn file_path_for(project_root: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let key = canonical.to_string_lossy();
    let hash = fnv1a_64(key.as_bytes());
    Some(
        home.join(".savvagent")
            .join("prompt_history")
            .join(format!("{hash:016x}.jsonl")),
    )
}

/// FNV-1a 64-bit. Stable across Rust versions; deterministic across
/// machines for the same input bytes. Good enough for filename derivation
/// (collisions across user-local project paths are astronomically rare).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn parse_jsonl(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (lineno, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<String>(raw) {
            Ok(s) => out.push(s),
            Err(e) => {
                tracing::warn!(
                    line = lineno + 1,
                    error = %e,
                    "prompt_history: skipping malformed JSONL line"
                );
            }
        }
    }
    // Defensive cap — if a hand-edit grew the file past the limit, trim it
    // on load so the next save can't be the one that finally truncates it.
    while out.len() > HISTORY_CAP {
        out.remove(0);
    }
    out
}

fn write_atomic(path: &Path, entries: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = String::new();
    for entry in entries {
        // serde_json on a &String produces a JSON string literal — quotes,
        // escapes, and Unicode handled for us. One line per entry.
        match serde_json::to_string(entry) {
            Ok(line) => {
                text.push_str(&line);
                text.push('\n');
            }
            Err(e) => {
                tracing::warn!(error = %e, "prompt_history: serialize failed; entry dropped");
            }
        }
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{HOME_LOCK, HomeGuard};

    #[test]
    fn load_missing_file_returns_empty() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let h = PromptHistory::load(Path::new("/tmp/some-project"));
        assert!(h.entries().is_empty());
    }

    #[test]
    fn append_then_load_roundtrip() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("first prompt".into());
        h.append("second prompt".into());

        let loaded = PromptHistory::load(&project);
        assert_eq!(loaded.entries(), &["first prompt", "second prompt"]);
    }

    #[test]
    fn append_dedupes_consecutive_duplicates() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("ls".into());
        h.append("ls".into());
        h.append("pwd".into());
        h.append("ls".into());
        assert_eq!(h.entries(), &["ls", "pwd", "ls"]);
    }

    #[test]
    fn append_drops_empty_prompts() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("".into());
        h.append("   ".into());
        h.append("real".into());
        assert_eq!(h.entries(), &["real"]);
    }

    #[test]
    fn append_enforces_cap() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        for i in 0..(HISTORY_CAP + 25) {
            h.append(format!("p{i}"));
        }
        assert_eq!(h.entries().len(), HISTORY_CAP);
        // Front should be dropped — entry 0 is gone, latest is preserved.
        assert_eq!(h.entries().first().unwrap(), "p25");
        assert_eq!(
            h.entries().last().unwrap(),
            &format!("p{}", HISTORY_CAP + 24)
        );
    }

    #[test]
    fn recall_prev_requires_empty_input_for_fresh_browse() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("a".into());
        h.append("b".into());

        // Non-empty input → no recall.
        assert_eq!(h.recall_prev("partial"), None);
        assert!(!h.is_browsing());

        // Empty input → recall newest, enter browse mode.
        assert_eq!(h.recall_prev(""), Some("b".to_string()));
        assert!(h.is_browsing());
    }

    #[test]
    fn recall_prev_steps_back_while_marker_matches() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("a".into());
        h.append("b".into());
        h.append("c".into());

        assert_eq!(h.recall_prev(""), Some("c".to_string()));
        assert_eq!(h.recall_prev("c"), Some("b".to_string()));
        assert_eq!(h.recall_prev("b"), Some("a".to_string()));
        // At oldest — stays put so the textarea doesn't flicker.
        assert_eq!(h.recall_prev("a"), Some("a".to_string()));
    }

    #[test]
    fn editing_during_browse_disables_further_recall() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("a".into());
        h.append("b".into());

        assert_eq!(h.recall_prev(""), Some("b".to_string()));
        // User typed; current_text != marker and is non-empty → no recall.
        assert_eq!(h.recall_prev("b extra"), None);
    }

    #[test]
    fn recall_next_returns_empty_when_walking_past_newest() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("a".into());
        h.append("b".into());

        assert_eq!(h.recall_prev(""), Some("b".to_string()));
        assert_eq!(h.recall_prev("b"), Some("a".to_string()));
        assert_eq!(h.recall_next("a"), Some("b".to_string()));
        // Walking past newest exits browse and signals "clear".
        assert_eq!(h.recall_next("b"), Some(String::new()));
        assert!(!h.is_browsing());
    }

    #[test]
    fn recall_next_does_nothing_when_not_browsing() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("a".into());
        assert_eq!(h.recall_next(""), None);
    }

    #[test]
    fn append_cancels_browse() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let mut h = PromptHistory::load(&project);
        h.append("a".into());
        h.append("b".into());
        h.recall_prev("");
        assert!(h.is_browsing());
        h.append("new prompt".into());
        assert!(!h.is_browsing());
    }

    #[test]
    fn newlines_and_quotes_survive_roundtrip() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let nasty = "line one\nline two with \"quotes\" and \\backslash";
        let mut h = PromptHistory::load(&project);
        h.append(nasty.to_string());

        let loaded = PromptHistory::load(&project);
        assert_eq!(loaded.entries(), &[nasty.to_string()]);
    }

    #[test]
    fn different_project_roots_have_independent_history() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();

        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();

        let mut ha = PromptHistory::load(a.path());
        ha.append("from a".into());
        let mut hb = PromptHistory::load(b.path());
        hb.append("from b".into());

        let reload_a = PromptHistory::load(a.path());
        let reload_b = PromptHistory::load(b.path());
        assert_eq!(reload_a.entries(), &["from a"]);
        assert_eq!(reload_b.entries(), &["from b"]);
    }

    #[test]
    fn malformed_lines_are_skipped_on_load() {
        let _lock = HOME_LOCK.lock().unwrap();
        let _home = HomeGuard::new();
        let project = std::env::current_dir().unwrap();

        let path = file_path_for(&project).expect("HOME set");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "\"good\"\nnot-valid-json\n\"also-good\"\n").unwrap();

        let h = PromptHistory::load(&project);
        assert_eq!(h.entries(), &["good", "also-good"]);
    }
}
