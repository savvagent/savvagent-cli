//! Expands `@<path>` includes in agent body text at load time.
//!
//! Single-pass: an included file containing `@<other>` is NOT
//! recursively expanded. Missing files leave the literal `@<path>`
//! in place and emit a warning.

use std::path::Path;

pub struct BodyResult {
    pub body: String,
    pub warnings: Vec<String>,
}

pub fn expand(body: &str, base_dir: &Path) -> BodyResult {
    let mut out = String::with_capacity(body.len());
    let mut warnings = Vec::new();

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix('@') {
            let path = rest.trim();
            if path.is_empty() {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let resolved = if Path::new(path).is_absolute() {
                Path::new(path).to_path_buf()
            } else {
                base_dir.join(path)
            };
            match std::fs::read_to_string(&resolved) {
                Ok(contents) => {
                    out.push_str(&contents);
                    if !contents.ends_with('\n') {
                        out.push('\n');
                    }
                }
                Err(e) => {
                    warnings.push(format!("@{path}: {e}"));
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }

    BodyResult {
        body: out,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn no_includes_passthrough() {
        let r = expand("hello world\n", &std::env::temp_dir());
        assert_eq!(r.body.trim(), "hello world");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn expands_relative_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("snippet.md"), "INCLUDED").unwrap();
        let body = "intro\n@snippet.md\noutro";
        let r = expand(body, dir.path());
        assert!(r.body.contains("INCLUDED"));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn missing_file_warns_and_keeps_literal() {
        let dir = tempdir().unwrap();
        let r = expand("@nonexistent.md", dir.path());
        assert!(r.body.contains("@nonexistent.md"));
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn single_pass_no_recursive_expansion() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "@b.md").unwrap();
        fs::write(dir.path().join("b.md"), "FINAL").unwrap();
        let r = expand("@a.md", dir.path());
        // a.md's contents include literal "@b.md" — should NOT expand.
        assert!(r.body.contains("@b.md"));
        assert!(!r.body.contains("FINAL"));
    }

    #[test]
    fn empty_at_passes_through() {
        let r = expand("@\nnext line", &std::env::temp_dir());
        assert!(r.body.starts_with("@\n"));
    }
}
