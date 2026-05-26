//! Auto-export each finalized HTML canvas to
//! `~/.savvagent/canvases/<unix>-<turn>-<block>.html`.

use std::path::{Path, PathBuf};

use savvagent_plugin::ContentBlockId;

/// Compute the auto-export path under `base_dir` for the given
/// (turn_id, block_id) at the given unix timestamp.
pub fn auto_export_path(
    base_dir: &Path,
    unix_ts: u64,
    turn_id: u32,
    block_id: ContentBlockId,
) -> PathBuf {
    base_dir.join(format!("{unix_ts:010}-{turn_id:06}-{}.html", block_id.0))
}

/// Write `source` to `path` with 0o600 permissions, creating `parent`
/// with 0o700 if it doesn't exist.
pub fn write_canvas(path: &Path, source: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(parent)?.permissions();
            perm.set_mode(0o700);
            std::fs::set_permissions(parent, perm)?;
        }
    }
    std::fs::write(path, source)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(path)?.permissions();
        perm.set_mode(0o600);
        std::fs::set_permissions(path, perm)?;
    }
    Ok(())
}

/// Compute the canvases base directory from `$HOME`. Returns `None`
/// when `$HOME` is unset or empty (tests that redirect `$HOME` use
/// [`HomeGuard`]; production always has `$HOME` set).
pub fn canvases_dir() -> Option<PathBuf> {
    let raw = std::env::var_os("HOME")?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw).join(".savvagent").join("canvases"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_export_path_format() {
        let p = auto_export_path(
            Path::new("/tmp/canvases"),
            1_716_300_000,
            12,
            ContentBlockId(3),
        );
        assert_eq!(
            p.to_string_lossy(),
            "/tmp/canvases/1716300000-000012-3.html"
        );
    }

    #[test]
    fn write_canvas_creates_file_with_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("subdir").join("x.html");
        write_canvas(&path, "<p>hi</p>").unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read, "<p>hi</p>");
    }

    #[cfg(unix)]
    #[test]
    fn write_canvas_sets_secure_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("dir").join("y.html");
        write_canvas(&path, "<p>x</p>").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode must be 0o600");
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "dir mode must be 0o700");
    }
}
