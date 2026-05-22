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
/// contains unknown keys with no recovery path. Malformed-frontmatter
/// files are reported and skipped at discovery time.
pub fn parse(contents: &str) -> Result<Parsed, String> {
    let mut warnings = Vec::new();
    if !contents.starts_with("---\n") && !contents.starts_with("---\r\n") {
        return Ok(Parsed {
            frontmatter: Frontmatter::default(),
            body: contents.to_string(),
            warnings,
        });
    }
    let after_open = contents
        .split_once('\n')
        .map(|(_, rest)| rest)
        .unwrap_or("");
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
            // Retry with a permissive value to extract known keys and
            // warn per unknown key.
            if let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(yaml) {
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
                        .filter(|(k, _)| k.as_str().map(|s| known.contains(&s)).unwrap_or(false))
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
    // Find a line containing only `---` (LF or CRLF endings).
    for (idx, _) in after_open.match_indices("\n---") {
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
