//! Cached render model produced off the UI thread.
//!
//! egui's `update()` is synchronous and must never `.await` or lock a plugin
//! mutex. This module mirrors `ui::HomeFrameData` as an owned, `Clone`able
//! snapshot plus an async builder that reuses `ui::compute_home_frame_data`,
//! so the egui and ratatui paths produce identical slot output. The egui paint
//! pass reads the latest snapshot from a shared cache.

use std::sync::{Arc, Mutex};

use savvagent_plugin::StyledLine;

use crate::ui::ToolEntryRender;

/// Snapshot of everything the egui paint pass needs that would otherwise
/// require async or plugin-mutex access to compute. Same shape as
/// [`crate::ui::HomeFrameData`].
#[derive(Default, Clone)]
pub struct RenderModel {
    pub banner: Vec<StyledLine>,
    pub tips: Vec<StyledLine>,
    pub footer_left: Vec<StyledLine>,
    pub footer_center: Vec<StyledLine>,
    pub footer_right: Vec<StyledLine>,
    pub tool_entries: Vec<ToolEntryRender>,
}

/// Thread-safe cache the producer writes and the UI thread reads.
pub type RenderModelCache = Arc<Mutex<RenderModel>>;

/// Build a [`RenderModel`] from the current `App`, reusing
/// [`crate::ui::compute_home_frame_data`] so the egui and ratatui paths
/// produce identical slot output. `area_cols` is the panel width in logical
/// monospace columns (slots render against a one-row-tall region).
pub async fn build_model(app: &crate::app::App, area_cols: u16) -> RenderModel {
    let rect = ratatui::layout::Rect::new(0, 0, area_cols, 1);
    let fd = crate::ui::compute_home_frame_data(app, rect).await;
    RenderModel {
        banner: fd.banner,
        tips: fd.tips,
        footer_left: fd.footer_left,
        footer_center: fd.footer_center,
        footer_right: fd.footer_right,
        tool_entries: fd.tool_entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::StyledLine;

    #[test]
    fn default_model_is_empty() {
        let m = RenderModel::default();
        assert!(m.banner.is_empty());
        assert!(m.tool_entries.is_empty());
    }

    #[test]
    fn cache_roundtrips_a_model() {
        let cache: RenderModelCache =
            std::sync::Arc::new(std::sync::Mutex::new(RenderModel::default()));
        {
            let mut g = cache.lock().unwrap();
            g.tips = vec![StyledLine { spans: vec![] }];
        }
        assert_eq!(cache.lock().unwrap().tips.len(), 1);
    }
}
