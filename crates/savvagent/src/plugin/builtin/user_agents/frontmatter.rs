//! YAML frontmatter parser for agent definition files.

use std::collections::HashSet;

use serde::Deserialize;

use crate::plugin::builtin::user_agents::spec::{AgentSpec, ToolsScope};

#[derive(Debug)]
pub struct FrontmatterResult {
    pub spec: AgentSpec,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawFrontmatter {
    name: Option<String>,
    description: Option<String>,
    tools: Option<ToolsField>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ToolsField {
    Str(String),
    List(Vec<String>),
}

pub fn parse(raw: &str, filename_slug: &str) -> Result<FrontmatterResult, String> {
    let (front, body) = split_frontmatter(raw)?;
    let raw_front: RawFrontmatter =
        serde_yaml_ng::from_str(front).map_err(|e| format!("malformed frontmatter: {e}"))?;

    let mut warnings = Vec::new();

    let description = raw_front
        .description
        .ok_or_else(|| "missing required field: description".to_string())?;

    if body.trim().is_empty() {
        return Err("empty body".into());
    }

    let name = match raw_front.name {
        Some(n) if n != filename_slug => {
            warnings.push(format!(
                "frontmatter name `{n}` disagrees with filename slug `{filename_slug}`; filename wins"
            ));
            filename_slug.to_string()
        }
        _ => filename_slug.to_string(),
    };

    let tools = match raw_front.tools {
        None => ToolsScope::Inherit,
        Some(ToolsField::List(list)) if list.is_empty() => ToolsScope::Empty,
        Some(ToolsField::List(list)) => ToolsScope::Allowed(list.into_iter().collect()),
        Some(ToolsField::Str(s)) => {
            let set: HashSet<String> = s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if set.is_empty() {
                ToolsScope::Empty
            } else {
                ToolsScope::Allowed(set)
            }
        }
    };

    Ok(FrontmatterResult {
        spec: AgentSpec {
            name,
            description,
            tools,
            model: raw_front.model,
            body: body.to_string(),
        },
        warnings,
    })
}

fn split_frontmatter(raw: &str) -> Result<(&str, &str), String> {
    if !raw.starts_with("---") {
        return Err("no frontmatter delimiter".into());
    }
    let rest = &raw[3..];
    // Allow a leading newline after the opening `---`.
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let Some(end) = rest.find("\n---") else {
        return Err("unterminated frontmatter".into());
    };
    let front = &rest[..end];
    let body = &rest[end + 4..]; // skip "\n---"
    let body = body.strip_prefix('\n').unwrap_or(body);
    Ok((front, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "---\nname: code-reviewer\ndescription: Reviews diffs.\ntools: tool-fs:read_file, tool-grep:search\nmodel: claude-sonnet-4-6\n---\nYou are a reviewer.";

    #[test]
    fn parses_full_frontmatter() {
        let r = parse(FULL, "code-reviewer").expect("parse");
        assert_eq!(r.spec.name, "code-reviewer");
        assert_eq!(r.spec.description, "Reviews diffs.");
        assert_eq!(r.spec.model.as_deref(), Some("claude-sonnet-4-6"));
        assert!(matches!(r.spec.tools, ToolsScope::Allowed(_)));
        assert_eq!(r.spec.body.trim(), "You are a reviewer.");
    }

    #[test]
    fn tools_as_yaml_list() {
        let raw =
            "---\ndescription: x\ntools:\n  - tool-fs:read_file\n  - tool-grep:search\n---\nbody";
        let r = parse(raw, "agent").expect("parse");
        match r.spec.tools {
            ToolsScope::Allowed(set) => {
                assert!(set.contains("tool-fs:read_file"));
                assert!(set.contains("tool-grep:search"));
            }
            _ => panic!("expected Allowed"),
        }
    }

    #[test]
    fn empty_tools_list_is_empty_scope() {
        let raw = "---\ndescription: x\ntools: []\n---\nbody";
        let r = parse(raw, "agent").expect("parse");
        assert_eq!(r.spec.tools, ToolsScope::Empty);
    }

    #[test]
    fn missing_tools_is_inherit() {
        let raw = "---\ndescription: x\n---\nbody";
        let r = parse(raw, "agent").expect("parse");
        assert_eq!(r.spec.tools, ToolsScope::Inherit);
    }

    #[test]
    fn missing_description_fails() {
        let raw = "---\nname: x\n---\nbody";
        let err = parse(raw, "agent").unwrap_err();
        assert!(err.contains("description"));
    }

    #[test]
    fn empty_body_fails() {
        let raw = "---\ndescription: x\n---\n";
        let err = parse(raw, "agent").unwrap_err();
        assert!(err.contains("body"));
    }

    #[test]
    fn name_mismatch_warns_but_filename_wins() {
        let raw = "---\nname: mismatched\ndescription: x\n---\nbody";
        let r = parse(raw, "agent").expect("parse");
        assert_eq!(r.spec.name, "agent");
        assert!(r.warnings.iter().any(|w| w.contains("name")));
    }
}
