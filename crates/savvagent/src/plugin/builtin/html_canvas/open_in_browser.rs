//! Ctrl-O "open in browser" for a focused HTML canvas.
//!
//! Writes the canvas's final HTML source to a temp file and shells out
//! to the platform opener (`xdg-open` / `open` / `start`). Split into a
//! pure [`write_temp_html`] (testable) and a thin [`shell_open`].

use std::io;
use std::path::{Path, PathBuf};

use savvagent_plugin::ContentBlockId;

/// Write `source` to a temp file named `savvagent-canvas-<id>.html` under
/// the OS temp dir and return its path. On Unix the file is created with
/// 0o600 permissions. Overwrites any previous file for the same id.
pub fn write_temp_html(id: ContentBlockId, source: &str) -> io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("savvagent-canvas-{}.html", id.0));
    std::fs::write(&path, source)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&path)?.permissions();
        perm.set_mode(0o600);
        std::fs::set_permissions(&path, perm)?;
    }
    Ok(path)
}

/// The platform browser-opener command (`open` on macOS, `start` on
/// Windows, `xdg-open` elsewhere).
pub fn opener_command() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "start"
    } else {
        "xdg-open"
    }
}

/// Spawn the platform opener on `path`. Non-blocking: the child is
/// detached and we don't wait for it.
pub fn shell_open(path: &Path) -> io::Result<()> {
    tokio::process::Command::new(opener_command())
        .arg(path)
        .spawn()
        .map(|_child| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_temp_html_writes_readable_file() {
        let path = write_temp_html(ContentBlockId(42), "<p>hello canvas</p>").expect("write");
        assert!(path.exists(), "temp file should exist");
        assert!(
            path.file_name().unwrap().to_string_lossy().contains("42"),
            "filename embeds the block id"
        );
        let read = std::fs::read_to_string(&path).expect("read");
        assert_eq!(read, "<p>hello canvas</p>");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn write_temp_html_sets_secure_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let path = write_temp_html(ContentBlockId(7), "<p>x</p>").expect("write");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "temp file mode must be 0o600");
        let _ = std::fs::remove_file(&path);
    }
}
