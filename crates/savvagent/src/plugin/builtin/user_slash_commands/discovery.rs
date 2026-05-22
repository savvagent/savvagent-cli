//! Walks the four well-known command directories and produces a
//! per-name, precedence-respecting index of discovered commands.

use std::path::{Path, PathBuf};

use crate::plugin::builtin::user_slash_commands::frontmatter::{self, Frontmatter};
use crate::plugin::builtin::user_slash_commands::name;

/// One discovered command file, ready to be turned into a `SlashSpec`.
#[derive(Debug, Clone)]
pub struct Discovered {
    /// Namespaced command name (no leading `/`).
    pub name: String,
    /// Absolute path to the source file on disk.
    pub path: PathBuf,
    /// Parsed frontmatter (defaulted if absent).
    pub frontmatter: Frontmatter,
    /// Cached markdown body (everything after the closing `---`).
    pub body: String,
    /// Origin scope; drives precedence and the trust check.
    pub origin: Origin,
    /// Non-fatal warnings collected during parse.
    #[allow(dead_code)] // populated during walk; surfaced to the log in a future pass
    pub warnings: Vec<String>,
}

/// Where this command came from. Drives precedence and trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `<project>/.savvagent/commands/`
    ProjectSavvagent,
    /// `<project>/.claude/commands/`
    ProjectClaude,
    /// `~/.savvagent/commands/`
    UserSavvagent,
    /// `~/.claude/commands/`
    UserClaude,
}

impl Origin {
    /// `true` if this origin is project-local (subject to trust prompts).
    pub fn is_project(self) -> bool {
        matches!(self, Origin::ProjectSavvagent | Origin::ProjectClaude)
    }
}

/// Walk one directory and return every valid `.md` file found.
///
/// Files with invalid names or malformed frontmatter are skipped and
/// surfaced as `warnings` in the return value; they never abort the
/// walk. Missing root → empty result, no warnings.
pub fn walk_one(root: &Path, origin: Origin) -> (Vec<Discovered>, Vec<String>) {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    if !root.exists() {
        return (out, warnings);
    }
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        let name_str = match name::from_relative_path(&rel) {
            Ok(n) => n,
            Err(why) => {
                warnings.push(format!("{}: {why}", path.display()));
                continue;
            }
        };
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        match frontmatter::parse(&contents) {
            Ok(parsed) => {
                for w in &parsed.warnings {
                    warnings.push(format!("{}: {w}", path.display()));
                }
                out.push(Discovered {
                    name: name_str,
                    path: path.to_path_buf(),
                    frontmatter: parsed.frontmatter,
                    body: parsed.body,
                    origin,
                    warnings: parsed.warnings,
                });
            }
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    (out, warnings)
}

use std::collections::BTreeMap;

/// Final per-name index after applying precedence across all four
/// search paths.
#[derive(Debug, Default, Clone)]
pub struct Index {
    /// Map from namespaced command name to its winning entry.
    pub commands: BTreeMap<String, Discovered>,
    /// Aggregated warnings, in the order they were produced.
    pub warnings: Vec<String>,
}

/// Walk all four directories with precedence:
/// project-savvagent > project-claude > user-savvagent > user-claude.
/// First hit per name wins; later hits at lower precedence are silently
/// dropped (the warning vec collects per-walk warnings only, not
/// shadowed-by-precedence notices).
pub fn walk_all(project_root: &Path, home: &Path) -> Index {
    let layers = [
        (
            project_root.join(".savvagent").join("commands"),
            Origin::ProjectSavvagent,
        ),
        (
            project_root.join(".claude").join("commands"),
            Origin::ProjectClaude,
        ),
        (
            home.join(".savvagent").join("commands"),
            Origin::UserSavvagent,
        ),
        (home.join(".claude").join("commands"), Origin::UserClaude),
    ];
    let mut index = Index::default();
    for (root, origin) in layers {
        let (found, warns) = walk_one(&root, origin);
        index.warnings.extend(warns);
        for d in found {
            index.commands.entry(d.name.clone()).or_insert(d);
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn missing_root_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("nonexistent");
        let (out, warns) = walk_one(&root, Origin::ProjectSavvagent);
        assert!(out.is_empty());
        assert!(warns.is_empty());
    }

    #[test]
    fn picks_up_md_files_with_namespacing() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "review.md", "---\ndescription: r\n---\nBody");
        write(
            tmp.path(),
            "team/lint.md",
            "---\ndescription: l\n---\nBody2",
        );
        write(tmp.path(), "not-markdown.txt", "ignored");

        let (out, warns) = walk_one(tmp.path(), Origin::ProjectSavvagent);
        let names: Vec<_> = out.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"review"));
        assert!(names.contains(&"team:lint"));
        assert_eq!(out.len(), 2);
        assert!(warns.is_empty());
    }

    #[test]
    fn invalid_slug_is_skipped_with_warning() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "GoodName.md", "body");
        let (out, warns) = walk_one(tmp.path(), Origin::ProjectSavvagent);
        assert!(out.is_empty());
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("GoodName.md"));
    }

    #[test]
    fn malformed_frontmatter_is_skipped_with_warning() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "bad.md", "---\n: : :\n---\nbody");
        let (out, warns) = walk_one(tmp.path(), Origin::ProjectSavvagent);
        assert!(out.is_empty());
        assert_eq!(warns.len(), 1);
    }

    #[test]
    fn origin_propagates() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "x.md", "body");
        let (out, _) = walk_one(tmp.path(), Origin::UserClaude);
        assert_eq!(out[0].origin, Origin::UserClaude);
    }

    #[test]
    fn precedence_project_over_user_and_savvagent_over_claude() {
        let proj = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        write(
            &proj.path().join(".savvagent/commands"),
            "x.md",
            "from project savvagent",
        );
        write(
            &proj.path().join(".claude/commands"),
            "x.md",
            "from project claude",
        );
        write(
            &home.path().join(".savvagent/commands"),
            "x.md",
            "from user savvagent",
        );
        write(
            &home.path().join(".claude/commands"),
            "x.md",
            "from user claude",
        );
        write(
            &home.path().join(".savvagent/commands"),
            "user_only.md",
            "user only",
        );

        let index = walk_all(proj.path(), home.path());
        let x = index.commands.get("x").unwrap();
        assert_eq!(x.origin, Origin::ProjectSavvagent);
        assert!(x.body.contains("from project savvagent"));
        assert!(index.commands.contains_key("user_only"));
    }
}
