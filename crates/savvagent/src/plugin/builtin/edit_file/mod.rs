//! `internal:edit-file` — basic in-TUI editor.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::EditFileScreen;

/// Built-in plugin that provides a basic in-TUI file editor screen.
///
/// Registers the `/edit <path>` slash command and the `edit-file` screen.
pub struct EditFilePlugin;

impl EditFilePlugin {
    /// Creates a new `EditFilePlugin` instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for EditFilePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for EditFilePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "edit".into(),
            summary: rust_i18n::t!("slash.edit-summary").to_string(),
            args_hint: Some("<path>".into()),
            requires_arg: true,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: "edit-file".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 90,
                height_pct: 85,
                title: Some(rust_i18n::t!("picker.edit-file.modal-title").to_string()),
            },
        }];

        Manifest {
            id: PluginId::new("internal:edit-file").expect("valid built-in id"),
            name: "Edit file".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.edit-file-description").to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        let path = args
            .into_iter()
            .next()
            .ok_or_else(|| PluginError::InvalidArgs("usage: /edit <path>".into()))?;
        // The TUI's `@` file picker inserts paths with a leading `@`
        // (e.g. `/edit @src/main.rs`). Strip it before handing to the
        // screen so the underlying open() call sees a real filesystem
        // path. v0.8's legacy `/edit` handler stripped `@` the same way.
        let path = path.strip_prefix('@').unwrap_or(&path).to_string();
        Ok(vec![Effect::OpenScreen {
            id: "edit-file".into(),
            args: ScreenArgs::EditFile { path },
        }])
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        // Marker screen; see view_file/mod.rs for the same pattern.
        match (id, args) {
            ("edit-file", ScreenArgs::EditFile { path }) => Ok(Box::new(EditFileScreen::new(path))),
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the post-v0.9 hotfix: `/edit @src/foo.rs`
    /// produced by the `@` file picker must strip the `@` before reaching
    /// `EditFileScreen::open`, which doesn't understand the prefix.
    #[tokio::test]
    async fn handle_slash_strips_at_prefix_from_path() {
        let mut p = EditFilePlugin::new();
        let effs = p
            .handle_slash("edit", vec!["@src/foo.rs".into()])
            .await
            .expect("handle_slash must succeed");
        match effs.first() {
            Some(Effect::OpenScreen {
                id,
                args: ScreenArgs::EditFile { path },
            }) => {
                assert_eq!(id, "edit-file");
                assert_eq!(path, "src/foo.rs", "leading '@' must be stripped");
            }
            other => panic!("expected OpenScreen::EditFile, got {other:?}"),
        }
    }

    /// Bare paths (no `@`) pass through unchanged.
    #[tokio::test]
    async fn handle_slash_leaves_bare_path_alone() {
        let mut p = EditFilePlugin::new();
        let effs = p
            .handle_slash("edit", vec!["src/foo.rs".into()])
            .await
            .expect("handle_slash must succeed");
        match effs.first() {
            Some(Effect::OpenScreen {
                args: ScreenArgs::EditFile { path },
                ..
            }) => assert_eq!(path, "src/foo.rs"),
            other => panic!("expected OpenScreen::EditFile, got {other:?}"),
        }
    }
}
