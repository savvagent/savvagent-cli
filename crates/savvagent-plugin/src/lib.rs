//! `savvagent-plugin` — trait surface and WIT-portable data types.
//!
//! This crate has zero runtime behavior. It defines the data shape that
//! crosses plugin boundaries; the runtime lives in the `savvagent` crate.
//!
//! See `docs/superpowers/specs/2026-05-12-v0.9.0-plugin-system-design.md`.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

/// Concrete error type returned by plugin trait methods.
pub mod error;

pub use error::PluginError;

/// ID newtypes and small structural types that cross plugin boundaries.
pub mod types;

/// Owned styled-text types returned by plugin render methods.
pub mod styled;

/// Host-lifecycle event payloads and their [`HookKind`] discriminants.
pub mod event;
pub use event::{HookKind, HostEvent};

pub use types::{
    ChordPortable, KeyCodePortable, KeyEventPortable, KeyMods, ModelEntry, PluginId, ProviderId,
    Region, ScreenArgs, ScreenInstanceId, ThemeEntry, ThemePalette, Timestamp, TranscriptHandle,
};

pub use styled::{StyledLine, StyledSpan, TextMods, ThemeColor};

/// Closed-vocabulary effect and bound-action types returned by plugin callbacks.
pub mod effect;
pub use effect::{BoundAction, Effect};

/// Plugin manifest, contributions bundle, and per-kind spec types.
pub mod manifest;
pub use manifest::{
    Contributions, KeyScope, KeybindingSpec, Manifest, PluginKind, ProviderSpec, ScreenLayout,
    ScreenSpec, SlashSpec, SlotSpec, ToolSummarySpec,
};

/// The [`Plugin`] trait — the WIT-portable entry point each plugin implements.
pub mod plugin;
pub use plugin::Plugin;

/// The [`Screen`] trait — per-open instances pushed onto the runtime's screen stack.
pub mod screen;
pub use screen::Screen;

/// The [`InProcessToolHandler`] trait — savvagent-internal trait for tools
/// whose implementation runs on the calling tokio runtime.
pub mod in_process_tool;
pub use in_process_tool::{InProcessToolHandler, InProcessToolHandlerArc};

#[cfg(test)]
mod trait_smoke {
    use super::*;
    use async_trait::async_trait;

    struct DummyPlugin;

    #[async_trait]
    impl Plugin for DummyPlugin {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: PluginId("test:dummy".into()),
                name: "Dummy".into(),
                version: "0.0.0".into(),
                description: "Trait smoke".into(),
                kind: PluginKind::Optional,
                contributions: Contributions::default(),
            }
        }
    }

    #[tokio::test]
    async fn dummy_plugin_default_impls_do_nothing() {
        let mut p = DummyPlugin;
        assert!(p.handle_slash("noop", vec![]).await.unwrap().is_empty());
        assert!(
            p.on_event(HostEvent::HostStarting)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(p.themes().is_empty());

        // create_screen default returns ScreenNotFound for the given id.
        let create_result = p.create_screen("anything", ScreenArgs::None);
        assert!(
            matches!(create_result, Err(PluginError::ScreenNotFound(ref id)) if id == "anything")
        );

        // render_slot default returns an empty Vec.
        let lines = p.render_slot(
            "home.tips",
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            },
        );
        assert!(lines.is_empty());

        // Tool-summary defaults return None for both args and results.
        assert!(
            p.summarize_tool_call("read_file", &serde_json::json!({"path": "/x"}))
                .is_none()
        );
        assert!(
            p.summarize_tool_result("read_file", "{\"bytes\":12}")
                .is_none()
        );
    }
}
