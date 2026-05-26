//! The `Plugin` trait — the WIT-portable entry point each plugin implements.

use async_trait::async_trait;

use crate::effect::Effect;
use crate::error::PluginError;
use crate::event::HostEvent;
use crate::manifest::Manifest;
use crate::screen::Screen;
use crate::styled::StyledLine;
use crate::types::{Region, ScreenArgs, ThemeEntry};

/// The runtime-facing trait every plugin implements. Each `Plugin` instance
/// is constructed once at startup, registered in the runtime's `PluginRegistry`,
/// and dispatched into by the slash/screen/event/slot/keybinding routers
/// according to its manifest.
///
/// Methods that take `&self` (`manifest`, `create_screen`, `render_slot`, `themes`)
/// are called from read-only paths and must be cheap, idempotent, and free of
/// internal mutation that requires `&mut`. The runtime may call them at any time,
/// including the render hot path. Methods that take `&mut self` (`handle_slash`,
/// `on_event`) are state-mutating; their concurrency discipline is enforced by
/// the runtime (see the runtime crate's PluginRegistry once PR 2/3 lands).
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Returns this plugin's static metadata. Called once at registration time
    /// and cached in the runtime's manifest indexes. Must be cheap and deterministic.
    fn manifest(&self) -> Manifest;

    /// Handle a slash command this plugin registered in its manifest.
    ///
    /// `name` is the bare command without the leading `/`. `args` is everything after
    /// the command name on the input line. Default impl returns no effects.
    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        let _ = (name, args);
        Ok(vec![])
    }

    /// Construct a fresh `Screen` instance for an `OpenScreen` effect. `id` is the
    /// screen id this plugin declared in its manifest. Each call creates a new
    /// instance — per-open state lives in the returned `Screen`. Default impl
    /// returns `PluginError::ScreenNotFound`.
    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        let _ = args;
        Err(PluginError::ScreenNotFound(id.to_string()))
    }

    /// Handle a `HostEvent`. Called for each event whose `HookKind` appears in
    /// this plugin's manifest hooks list. Sequential per event (other subscribers
    /// run after); accumulated effects are applied as a batch.
    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        let _ = event;
        Ok(vec![])
    }

    /// Render this plugin's contribution to the given slot. Called every frame;
    /// must be cheap and side-effect-free. `slot_id` is one of the slot ids this
    /// plugin declared in its manifest.
    ///
    /// Implementations should return an empty `Vec` for any `slot_id` they don't
    /// recognise — the runtime may call `render_slot` for any slot the plugin's
    /// manifest declared, and forgetting a match arm should not panic the process.
    fn render_slot(&self, slot_id: &str, region: Region) -> Vec<StyledLine> {
        let _ = (slot_id, region);
        vec![]
    }

    /// Render a one-line summary of a tool call's arguments. Called from the
    /// async pre-render step in the host for every `Entry::Tool` whose tool
    /// name this plugin claims via `ToolSummarySpec`. Return `None` to fall
    /// through to the host's JSON-highlighter default; return
    /// `Some(spans)` to override.
    ///
    /// Must be cheap and side-effect-free; called once per visible tool entry
    /// per frame.
    fn summarize_tool_call(
        &self,
        name: &str,
        args: &serde_json::Value,
    ) -> Option<Vec<crate::StyledSpan>> {
        let _ = (name, args);
        None
    }

    /// Render a one-line summary of a tool call's result. Called from the
    /// async pre-render step in the host once per completed `Entry::Tool`
    /// whose tool name this plugin claims via `ToolSummarySpec`. Return
    /// `None` to fall through to the host's JSON-highlighter (or raw-text
    /// fallback for non-JSON result bodies).
    ///
    /// `result_text` is the raw payload the host built from the tool's
    /// `CallToolResult`. Plugins typically parse it as their tool crate's
    /// `*Output` type via `serde_json::from_str` and format the typed
    /// struct; on parse failure they return `None`.
    fn summarize_tool_result(
        &self,
        name: &str,
        result_text: &str,
    ) -> Option<Vec<crate::StyledSpan>> {
        let _ = (name, result_text);
        None
    }

    /// Construct a fresh `ContentRenderer` for an inline content block.
    /// Called when the conversation log encounters a `ContentBlock` whose
    /// `type` discriminator matches one of this plugin's
    /// `ContentRendererSpec`s. Each invocation produces a new
    /// instance — per-block state lives in the returned renderer.
    ///
    /// Default impl returns [`PluginError::ContentRendererNotFound`].
    fn create_renderer(
        &self,
        block_type: &str,
        id: crate::content::ContentBlockId,
        source: &str,
    ) -> Result<Box<dyn crate::content::ContentRenderer>, PluginError> {
        let _ = (id, source);
        Err(PluginError::ContentRendererNotFound(block_type.to_string()))
    }

    /// Static theme catalog this plugin contributes. Pulled once at registration
    /// time and merged into the runtime's theme catalog.
    fn themes(&self) -> Vec<ThemeEntry> {
        vec![]
    }
}
