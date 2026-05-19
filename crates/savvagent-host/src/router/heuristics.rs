//! Layer 4 of the router stack — hardcoded heuristic classifier.
//!
//! Gated on `RoutingRules::heuristics == true`. Pure functions, no I/O,
//! no async. Adding new `HeuristicKind` variants is additive thanks to
//! `#[non_exhaustive]`.

/// Coding-flavored substring keywords (lowercase). Substring (not
/// whole-word) match — `function` matches `functional`, `refactor`
/// matches `refactored`. List is hardcoded in v1; users who want a
/// different list write explicit `[[rule]]` entries (rules run before
/// the heuristic, so a rule match always beats this).
const CODING_KEYWORDS: &[&str] = &[
    "refactor",
    "implement",
    "debug",
    "fix bug",
    "compile",
    "stack trace",
    "function",
    "class",
    "error",
];

/// Max character length for a turn to qualify as a short factoid.
const SHORT_FACTOID_MAX_CHARS: usize = 200;

/// What the classifier matched. `#[non_exhaustive]` so future kinds
/// (translation, summarization, …) land additively.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeuristicKind {
    /// Short question, e.g. "what is 2+2?". Routes to a cheap model.
    ShortFactoid,
    /// Coding-flavored instruction. Routes to a premium model.
    Coding,
}

impl std::fmt::Display for HeuristicKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            HeuristicKind::ShortFactoid => "short",
            HeuristicKind::Coding => "coding",
        })
    }
}

/// Classify a user message. `None` = no heuristic match; the router
/// falls through to the next layer (Default).
///
/// Precedence inside the classifier:
/// 1. Coding keyword present → `Coding` (more specific signal wins).
/// 2. ≤200 chars and contains '?' → `ShortFactoid`.
/// 3. Else `None`.
pub fn classify(user_text: &str) -> Option<HeuristicKind> {
    let lower = user_text.to_lowercase();
    if CODING_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return Some(HeuristicKind::Coding);
    }
    if user_text.contains('?') && user_text.chars().count() <= SHORT_FACTOID_MAX_CHARS {
        return Some(HeuristicKind::ShortFactoid);
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::router::heuristics::{classify, HeuristicKind};

    #[test]
    fn classify_returns_none_for_empty_input() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
        assert_eq!(classify("hello"), None);
    }

    #[test]
    fn classify_short_factoid_requires_question_mark() {
        assert_eq!(classify("what is 2+2?"), Some(HeuristicKind::ShortFactoid));
        // Same text without a `?` is *not* a short factoid.
        assert_eq!(classify("what is 2+2"), None);
    }

    #[test]
    fn classify_short_factoid_respects_200_char_threshold() {
        assert_eq!(classify("is this short?"), Some(HeuristicKind::ShortFactoid));
        // 201 chars + `?` is over the cutoff → no match.
        let long = format!("is {}?", "x".repeat(200));
        assert_eq!(classify(&long), None);
    }

    #[test]
    fn classify_coding_matches_each_keyword_case_insensitive() {
        for kw in [
            "refactor",
            "implement",
            "debug",
            "fix bug",
            "compile",
            "stack trace",
            "function",
            "class",
            "error",
        ] {
            let upper = kw.to_uppercase();
            assert_eq!(
                classify(&format!("please {upper} this")),
                Some(HeuristicKind::Coding),
                "uppercase keyword '{upper}' should match Coding"
            );
            assert_eq!(
                classify(&format!("please {kw} this")),
                Some(HeuristicKind::Coding),
                "lowercase keyword '{kw}' should match Coding"
            );
        }
    }

    #[test]
    fn classify_coding_beats_short_factoid_when_both_match() {
        // 24 chars, contains `?`, AND contains the keyword `debug`.
        // The more specific signal (Coding) must win.
        assert_eq!(
            classify("can you debug this?"),
            Some(HeuristicKind::Coding)
        );
    }

    #[test]
    fn classify_substring_match_documented() {
        // `function` matches `functional`. This is the v1 contract;
        // whole-word matching is a future opt-in.
        assert_eq!(
            classify("functional programming"),
            Some(HeuristicKind::Coding)
        );
    }

    #[test]
    fn heuristic_kind_display() {
        assert_eq!(format!("{}", HeuristicKind::ShortFactoid), "short");
        assert_eq!(format!("{}", HeuristicKind::Coding), "coding");
    }
}
