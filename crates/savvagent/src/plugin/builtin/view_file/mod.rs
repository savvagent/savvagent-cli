//! `internal:view-file` — read-only file viewer.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::ViewFileScreen;

/// Built-in plugin that provides a read-only file viewer screen.
///
/// Registers the `/view <path>` slash command and the `view-file` screen.
pub struct ViewFilePlugin;

impl ViewFilePlugin {
    /// Creates a new `ViewFilePlugin` instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ViewFilePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ViewFilePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "view".into(),
            summary: rust_i18n::t!("slash.view-summary").to_string(),
            args_hint: Some("<path>".into()),
            requires_arg: true,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: "view-file".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 90,
                height_pct: 85,
                title: Some(rust_i18n::t!("picker.view-file.modal-title").to_string()),
            },
        }];

        Manifest {
            id: PluginId::new("internal:view-file").expect("valid built-in id"),
            name: "View file".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.view-file-description").to_string(),
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
            .ok_or_else(|| PluginError::InvalidArgs("usage: /view <path>".into()))?;
        // The TUI's `@` file picker inserts paths with a leading `@`
        // (e.g. `/view @src/main.rs`). Strip it before handing to the
        // screen so the underlying open() call sees a real filesystem
        // path. v0.8's legacy `/view` handler stripped `@` the same way.
        let path = path.strip_prefix('@').unwrap_or(&path).to_string();
        Ok(vec![Effect::OpenScreen {
            id: "view-file".into(),
            args: ScreenArgs::ViewFile { path },
        }])
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        // The screen is a marker; `apply_effects::open_screen` constructs
        // the actual screen (with the resolved path) after loading the file
        // into `App::editor`, and replaces this placeholder before the
        // screen lands on the stack.
        match (id, args) {
            ("view-file", ScreenArgs::ViewFile { path }) => Ok(Box::new(ViewFileScreen::new(path))),
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the post-v0.9 hotfix: `/view @src/foo.rs`
    /// produced by the `@` file picker must strip the `@` before reaching
    /// `ViewFileScreen::open`, which doesn't understand the prefix.
    #[tokio::test]
    async fn handle_slash_strips_at_prefix_from_path() {
        let mut p = ViewFilePlugin::new();
        let effs = p
            .handle_slash("view", vec!["@src/foo.rs".into()])
            .await
            .expect("handle_slash must succeed");
        match effs.first() {
            Some(Effect::OpenScreen {
                id,
                args: ScreenArgs::ViewFile { path },
            }) => {
                assert_eq!(id, "view-file");
                assert_eq!(path, "src/foo.rs", "leading '@' must be stripped");
            }
            other => panic!("expected OpenScreen::ViewFile, got {other:?}"),
        }
    }

    /// Bare paths (no `@`) pass through unchanged.
    #[tokio::test]
    async fn handle_slash_leaves_bare_path_alone() {
        let mut p = ViewFilePlugin::new();
        let effs = p
            .handle_slash("view", vec!["src/foo.rs".into()])
            .await
            .expect("handle_slash must succeed");
        match effs.first() {
            Some(Effect::OpenScreen {
                args: ScreenArgs::ViewFile { path },
                ..
            }) => assert_eq!(path, "src/foo.rs"),
            other => panic!("expected OpenScreen::ViewFile, got {other:?}"),
        }
    }
}
