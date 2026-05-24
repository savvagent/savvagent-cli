//! Four-path discovery for agent definition files. Mirrors sub-projects
//! A and B precedence: project beats user, savvagent beats claude.

use std::path::{Path, PathBuf};

use crate::plugin::builtin::user_agents::body::expand;
use crate::plugin::builtin::user_agents::frontmatter::parse;
use crate::plugin::builtin::user_agents::spec::AgentSpec;

/// Discover agent definitions across the four standard paths. First-wins
/// dedup by agent name (slug).
pub fn discover(project_root: &Path, user_home: &Path) -> Vec<AgentSpec> {
    let paths = [
        project_root.join(".savvagent").join("agents"),
        project_root.join(".claude").join("agents"),
        user_home.join(".savvagent").join("agents"),
        user_home.join(".claude").join("agents"),
    ];

    let mut out: Vec<AgentSpec> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = Default::default();

    for dir in &paths {
        let walker = walk_dir(dir);
        for path in walker {
            let Some(slug) = slug_from_path(&path) else {
                continue;
            };
            if !seen_names.insert(slug.clone()) {
                continue;
            }
            match load_agent(&path, &slug) {
                Ok(spec) => out.push(spec),
                Err(e) => {
                    tracing::warn!("agent {path:?} skipped: {e}");
                }
            }
        }
    }

    out
}

fn walk_dir(dir: &Path) -> Vec<PathBuf> {
    use ignore::WalkBuilder;
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    for entry in WalkBuilder::new(dir).build().flatten() {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            && entry.path().extension().and_then(|s| s.to_str()) == Some("md")
        {
            out.push(entry.into_path());
        }
    }
    out.sort();
    out
}

fn slug_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    if stem.is_empty() {
        return None;
    }
    if stem
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        Some(stem.to_string())
    } else {
        tracing::warn!(
            "agent {path:?} skipped: invalid slug `{stem}` (must be lowercase-kebab-case)"
        );
        None
    }
}

fn load_agent(path: &Path, slug: &str) -> Result<AgentSpec, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let result = parse(&raw, slug)?;
    for w in &result.warnings {
        tracing::warn!("agent {path:?}: {w}");
    }
    let base = path.parent().unwrap_or(Path::new("."));
    let expanded = expand(&result.spec.body, base);
    for w in &expanded.warnings {
        tracing::warn!("agent {path:?}: {w}");
    }
    let mut spec = result.spec;
    spec.body = expanded.body;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_agent(dir: &Path, slug: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(format!("{slug}.md")), body).unwrap();
    }

    const MINIMAL: &str = "---\ndescription: test agent\n---\nbody";

    #[test]
    fn precedence_project_savvagent_beats_user_claude() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_agent(
            &project.path().join(".savvagent/agents"),
            "shared",
            "---\ndescription: project version\n---\nproject body",
        );
        write_agent(
            &user.path().join(".claude/agents"),
            "shared",
            "---\ndescription: user version\n---\nuser body",
        );
        let agents = discover(project.path(), user.path());
        assert_eq!(agents.len(), 1);
        assert!(agents[0].body.contains("project body"));
    }

    #[test]
    fn precedence_savvagent_beats_claude_within_project() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_agent(
            &project.path().join(".savvagent/agents"),
            "shared",
            "---\ndescription: savvagent version\n---\nsavvagent body",
        );
        write_agent(
            &project.path().join(".claude/agents"),
            "shared",
            "---\ndescription: claude version\n---\nclaude body",
        );
        let agents = discover(project.path(), user.path());
        assert_eq!(agents.len(), 1);
        assert!(agents[0].body.contains("savvagent body"));
    }

    #[test]
    fn malformed_file_skipped_gracefully() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_agent(
            &project.path().join(".savvagent/agents"),
            "bad",
            "not even close to YAML",
        );
        write_agent(&project.path().join(".savvagent/agents"), "good", MINIMAL);
        let agents = discover(project.path(), user.path());
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"good"));
        assert!(!names.contains(&"bad"));
    }

    #[test]
    fn invalid_slug_skipped() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_agent(
            &project.path().join(".savvagent/agents"),
            "BadCaps",
            MINIMAL,
        );
        let agents = discover(project.path(), user.path());
        assert!(agents.is_empty());
    }

    #[test]
    fn nonexistent_dirs_ok() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        let agents = discover(project.path(), user.path());
        assert!(agents.is_empty());
    }

    #[test]
    fn user_agents_discovered_when_no_project() {
        let project = tempdir().unwrap();
        let user = tempdir().unwrap();
        write_agent(&user.path().join(".savvagent/agents"), "personal", MINIMAL);
        let agents = discover(project.path(), user.path());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "personal");
    }
}
