//! Concrete plugin error type. Closed `#[non_exhaustive]` enum — no
//! `anyhow::Error`, no `Box<dyn Error>`.
//! See `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md`.

use std::fmt;

/// Errors returned by plugin trait methods.
///
/// The enum is `#[non_exhaustive]` so that new variants can be added in
/// minor releases without breaking downstream `match` arms.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginError {
    /// Requested screen id was not found in any plugin's manifest.
    ScreenNotFound(String),
    /// A slash-command was dispatched to a plugin that does not handle it.
    SlashNotHandled(String),
    /// Arguments supplied to a plugin method were invalid or malformed.
    InvalidArgs(String),
    /// An unexpected condition occurred inside the plugin.
    Internal(String),
    /// No registered plugin advertises a `ContentRendererSpec` for the
    /// given block_type. Returned by `Plugin::create_renderer` default impl.
    ContentRendererNotFound(String),
    /// `ContentRenderer::restore_state` could not interpret the
    /// supplied bytes (corrupt, schema-incompatible, or the renderer's
    /// own decoder returned an error). The host treats this as a
    /// soft failure: log a warning, drop the bytes, continue rendering
    /// from defaults.
    StateRestoreFailed(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::ScreenNotFound(id) => write!(f, "screen not found: {id}"),
            PluginError::SlashNotHandled(name) => write!(f, "slash not handled: /{name}"),
            PluginError::InvalidArgs(msg) => write!(f, "invalid args: {msg}"),
            PluginError::Internal(msg) => write!(f, "internal: {msg}"),
            Self::ContentRendererNotFound(block_type) => write!(
                f,
                "no plugin claims content renderer for block_type '{}'",
                block_type
            ),
            Self::StateRestoreFailed(msg) => write!(f, "state restore failed: {msg}"),
        }
    }
}

impl std::error::Error for PluginError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_not_found_renders() {
        let e = PluginError::ScreenNotFound("themes.picker".to_string());
        assert_eq!(format!("{e}"), "screen not found: themes.picker");
    }

    #[test]
    fn slash_not_handled_renders() {
        let e = PluginError::SlashNotHandled("theme".to_string());
        assert_eq!(format!("{e}"), "slash not handled: /theme");
    }

    #[test]
    fn invalid_args_renders() {
        let e = PluginError::InvalidArgs("usage: /view <path>".to_string());
        assert_eq!(format!("{e}"), "invalid args: usage: /view <path>");
    }

    #[test]
    fn internal_renders() {
        let e = PluginError::Internal("io: permission denied".to_string());
        assert_eq!(format!("{e}"), "internal: io: permission denied");
    }

    #[test]
    fn state_restore_failed_display() {
        let e = PluginError::StateRestoreFailed("schema v2 not understood".to_string());
        assert_eq!(
            format!("{e}"),
            "state restore failed: schema v2 not understood",
        );
    }
}
