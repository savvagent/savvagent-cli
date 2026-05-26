//! `internal:changelog` plugin: streams CHANGELOG.md from
//! raw.githubusercontent.com and renders it via tui-markdown in a
//! dedicated screen. Closes #68.
//!
//! On `/changelog`, [`ChangelogPlugin::handle_slash`] returns
//! `Effect::OpenScreen { id: "changelog", args: ScreenArgs::Changelog }`.
//! The runtime then calls [`ChangelogPlugin::create_screen`], which
//! constructs a [`screen::ChangelogScreen`] in [`screen::ChangelogState::Loading`]
//! and spawns a tokio task that calls
//! [`fetch::ChangelogFetcher::fetch`] and writes the result into the
//! screen's shared state.
//!
//! The fetcher is trait-injected so unit tests can substitute a stub.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

pub mod fetch;
pub mod screen;

pub use fetch::{ChangelogFetcher, GithubChangelogFetcher};
pub use screen::{ChangelogScreen, ChangelogState};

const SCREEN_ID: &str = "changelog";

/// The plugin instance held by the runtime.
pub struct ChangelogPlugin {
    /// Fetcher used by every newly-opened screen instance. Defaults to
    /// [`GithubChangelogFetcher`]; tests substitute a stub.
    fetcher: Arc<dyn ChangelogFetcher>,
}

impl ChangelogPlugin {
    pub fn new() -> Self {
        Self::with_fetcher(Arc::new(GithubChangelogFetcher))
    }

    pub fn with_fetcher(fetcher: Arc<dyn ChangelogFetcher>) -> Self {
        Self { fetcher }
    }
}

impl Default for ChangelogPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ChangelogPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "changelog".into(),
            summary: rust_i18n::t!("changelog.slash-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: SCREEN_ID.into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 90,
                height_pct: 85,
                title: Some(rust_i18n::t!("changelog.modal-title").to_string()),
            },
        }];

        Manifest {
            id: PluginId::new("internal:changelog").expect("valid built-in id"),
            name: "Changelog".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.changelog-description").to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "changelog" {
            return Ok(vec![]);
        }
        Ok(vec![Effect::OpenScreen {
            id: SCREEN_ID.into(),
            args: ScreenArgs::Changelog,
        }])
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            (SCREEN_ID, ScreenArgs::Changelog) => {
                let state = Arc::new(Mutex::new(ChangelogState::Loading));
                let screen = ChangelogScreen::new(Arc::clone(&state));
                spawn_fetch_task(Arc::clone(&self.fetcher), state);
                Ok(Box::new(screen))
            }
            (SCREEN_ID, _other) => Err(PluginError::InvalidArgs("/changelog takes no args".into())),
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}

/// Spawn a task that calls `fetcher.fetch()` and publishes the result
/// into the supplied shared state cell. Free function so it's directly
/// testable and so the retry path (later) can call it without holding
/// `&self`.
fn spawn_fetch_task(fetcher: Arc<dyn ChangelogFetcher>, state: Arc<Mutex<ChangelogState>>) {
    tokio::spawn(async move {
        let new_state = match fetcher.fetch().await {
            Ok(markdown) => ChangelogState::Loaded {
                lines: screen::markdown_to_styled_lines(&markdown),
            },
            Err(e) => ChangelogState::Failed {
                error: e.to_string(),
            },
        };
        if let Ok(mut guard) = state.lock() {
            *guard = new_state;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::HOME_LOCK;

    /// In-test fetcher: returns canned markdown or a canned error.
    struct StubFetcher {
        result: Mutex<Result<String, String>>,
    }

    impl StubFetcher {
        fn ok(md: &str) -> Self {
            Self {
                result: Mutex::new(Ok(md.into())),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                result: Mutex::new(Err(msg.into())),
            }
        }
    }

    #[async_trait]
    impl ChangelogFetcher for StubFetcher {
        async fn fetch(&self) -> anyhow::Result<String> {
            match &*self.result.lock().unwrap() {
                Ok(s) => Ok(s.clone()),
                Err(m) => Err(anyhow::anyhow!(m.clone())),
            }
        }
    }

    #[test]
    fn manifest_contributes_slash_and_screen() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = ChangelogPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:changelog");
        assert_eq!(m.contributions.slash_commands.len(), 1);
        assert_eq!(m.contributions.slash_commands[0].name, "changelog");
        assert_eq!(m.contributions.screens.len(), 1);
        assert_eq!(m.contributions.screens[0].id, SCREEN_ID);
        assert_eq!(m.kind, PluginKind::Optional);
    }

    #[tokio::test]
    async fn slash_changelog_emits_open_screen() {
        let mut p = ChangelogPlugin::with_fetcher(Arc::new(StubFetcher::ok("# x")));
        let effects = p.handle_slash("changelog", vec![]).await.unwrap();
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::OpenScreen { id, args } => {
                assert_eq!(id, SCREEN_ID);
                assert!(matches!(args, ScreenArgs::Changelog));
            }
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn slash_ignores_other_commands() {
        let mut p = ChangelogPlugin::with_fetcher(Arc::new(StubFetcher::ok("# x")));
        let effects = p.handle_slash("not-changelog", vec![]).await.unwrap();
        assert!(effects.is_empty());
    }

    #[tokio::test]
    async fn create_screen_with_correct_id_and_args_returns_screen() {
        let p = ChangelogPlugin::with_fetcher(Arc::new(StubFetcher::ok("# x")));
        let screen = p.create_screen(SCREEN_ID, ScreenArgs::Changelog).unwrap();
        assert_eq!(screen.id(), SCREEN_ID);
    }

    #[tokio::test]
    async fn create_screen_with_unknown_id_returns_screen_not_found() {
        let p = ChangelogPlugin::with_fetcher(Arc::new(StubFetcher::ok("# x")));
        let result = p.create_screen("not-changelog", ScreenArgs::Changelog);
        assert!(matches!(result, Err(PluginError::ScreenNotFound(_))));
    }

    #[tokio::test]
    async fn create_screen_with_wrong_args_returns_invalid_args() {
        let p = ChangelogPlugin::with_fetcher(Arc::new(StubFetcher::ok("# x")));
        let result = p.create_screen(SCREEN_ID, ScreenArgs::None);
        assert!(matches!(result, Err(PluginError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn spawn_fetch_task_writes_loaded_on_success() {
        let state = Arc::new(Mutex::new(ChangelogState::Loading));
        spawn_fetch_task(
            Arc::new(StubFetcher::ok("# Heading\n\nbody")),
            Arc::clone(&state),
        );
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if matches!(*state.lock().unwrap(), ChangelogState::Loaded { .. }) {
                return;
            }
        }
        panic!("state never transitioned to Loaded");
    }

    #[tokio::test]
    async fn spawn_fetch_task_writes_failed_on_error() {
        let state = Arc::new(Mutex::new(ChangelogState::Loading));
        spawn_fetch_task(Arc::new(StubFetcher::err("DNS error")), Arc::clone(&state));
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if let ChangelogState::Failed { error } = &*state.lock().unwrap() {
                assert!(error.contains("DNS error"), "got: {error}");
                return;
            }
        }
        panic!("state never transitioned to Failed");
    }
}
