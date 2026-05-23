//! Compiled tool-name matchers built from `MatcherGroup::matcher` strings.

use globset::{Glob, GlobMatcher};

/// A compiled glob pattern paired with the raw source string (for logs).
#[derive(Debug, Clone)]
pub struct CompiledMatcher {
    /// Raw matcher string retained for diagnostics; not yet surfaced.
    #[allow(dead_code)]
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
