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
    let mut segments: Vec<String> = Vec::new();
    if let Some(parent) = rel.parent() {
        for comp in parent.components() {
            let s = comp
                .as_os_str()
                .to_str()
                .ok_or_else(|| "non-utf8 path segment".to_string())?;
            if !s.is_empty() {
                segments.push(s.to_string());
            }
        }
    }
    segments.push(stem.to_string());
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
        return Err(format!("segment '{seg}' must start with [a-z0-9]"));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
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
