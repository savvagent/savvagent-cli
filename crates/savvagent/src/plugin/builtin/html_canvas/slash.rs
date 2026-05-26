//! Logic for the /save-canvas slash command.
//!
//! The slash dispatcher in `crates/savvagent/src/plugin/effects.rs` calls
//! [`dispatch`] directly (bypassing the Plugin trait) because the
//! command needs access to App-owned canvas state.

use std::path::{Path, PathBuf};

use savvagent_plugin::{ContentBlockId, Effect, UrlTarget};

use crate::plugin::builtin::html_canvas::auto_export::write_canvas;

/// Parsed args for /save-canvas.
#[derive(Debug, PartialEq, Eq)]
pub struct SaveCanvasArgs {
    /// Output path, or None to derive from cwd.
    pub path: Option<PathBuf>,
    /// Specific block id to save, or None to save the most recent.
    pub block: Option<ContentBlockId>,
    /// Whether to open the file after writing.
    pub open: bool,
}

/// Parse args from the slash invocation. `args` is everything after
/// the command name on the input line, already tokenised.
pub fn parse_args(args: &[String]) -> Result<SaveCanvasArgs, String> {
    let mut path: Option<PathBuf> = None;
    let mut block: Option<ContentBlockId> = None;
    let mut open = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--block" => {
                let v = args.get(i + 1).ok_or("--block requires an argument")?;
                let n: u32 = v.parse().map_err(|_| format!("invalid block id: {v}"))?;
                block = Some(ContentBlockId(n));
                i += 2;
            }
            "--open" => {
                open = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if path.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                path = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }

    Ok(SaveCanvasArgs { path, block, open })
}

/// Dispatch the slash. `canvases` is the App-supplied set of currently
/// known canvases in transcript order; `cwd` is the working directory.
pub fn dispatch(
    args: SaveCanvasArgs,
    canvases: &[(ContentBlockId, String)],
    cwd: &Path,
) -> Result<DispatchResult, String> {
    let (id, source) = match args.block {
        Some(id) => canvases
            .iter()
            .find(|(eid, _)| *eid == id)
            .ok_or_else(|| format!("no canvas with id {}", id.0))?
            .clone(),
        None => canvases
            .last()
            .ok_or("no canvas in transcript yet")?
            .clone(),
    };

    let path = args
        .path
        .unwrap_or_else(|| cwd.join(format!("savvagent-canvas-{}.html", id.0)));
    write_canvas(&path, &source).map_err(|e| format!("write failed: {e}"))?;

    let mut effects = Vec::new();
    if args.open {
        effects.push(Effect::OpenUrl {
            url: format!("file://{}", path.display()),
            target: UrlTarget::SystemBrowser,
        });
    }

    Ok(DispatchResult { path, effects })
}

/// Result of a successful [`dispatch`] call.
#[derive(Debug)]
pub struct DispatchResult {
    /// The path the canvas was written to.
    pub path: PathBuf,
    /// Effects to apply after writing (e.g. `OpenUrl` when `--open` is set).
    pub effects: Vec<Effect>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_args_returns_defaults() {
        let a = parse_args(&[]).unwrap();
        assert_eq!(
            a,
            SaveCanvasArgs {
                path: None,
                block: None,
                open: false
            }
        );
    }

    #[test]
    fn parse_explicit_path() {
        let a = parse_args(&["./out.html".into()]).unwrap();
        assert_eq!(a.path.as_deref(), Some(Path::new("./out.html")));
    }

    #[test]
    fn parse_block_flag() {
        let a = parse_args(&["--block".into(), "7".into()]).unwrap();
        assert_eq!(a.block, Some(ContentBlockId(7)));
    }

    #[test]
    fn parse_open_flag() {
        let a = parse_args(&["--open".into()]).unwrap();
        assert!(a.open);
    }

    #[test]
    fn parse_combined() {
        let a = parse_args(&[
            "./x.html".into(),
            "--block".into(),
            "2".into(),
            "--open".into(),
        ])
        .unwrap();
        assert_eq!(a.path.as_deref(), Some(Path::new("./x.html")));
        assert_eq!(a.block, Some(ContentBlockId(2)));
        assert!(a.open);
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let e = parse_args(&["--bogus".into()]).unwrap_err();
        assert!(e.contains("unknown flag"));
    }

    #[test]
    fn dispatch_writes_file_and_emits_open_effect() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let canvases = vec![(ContentBlockId(0), "<p>hi</p>".to_string())];
        let r = dispatch(
            SaveCanvasArgs {
                path: None,
                block: None,
                open: true,
            },
            &canvases,
            cwd,
        )
        .unwrap();
        assert!(r.path.exists());
        assert_eq!(r.effects.len(), 1);
        assert!(
            matches!(&r.effects[0], Effect::OpenUrl { target, .. } if *target == UrlTarget::SystemBrowser)
        );
    }
}
