//! `CanvasState` — persisted interactive state for `HtmlCanvas`.
//!
//! Wire format: JSON. Phase 2 schema_version = 1. The host treats
//! the bytes as opaque; encoding choice lives entirely in this module.
//! `HtmlCanvas` also keeps a live `CanvasState` in memory as its
//! interactive-state log (Blitz's document is `!Send` and cannot be
//! retained, so semantic state is tracked here and replayed onto a
//! freshly-parsed document on each render/dispatch).

#![warn(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Phase 2 v3 transcript state blob. Embedded as base64 in the
/// `Canvas` Entry's `state` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CanvasState {
    /// Wire schema version. Phase 2 ships v1.
    pub schema_version: u32,
    /// Form values keyed by stringified NodeId.
    pub form_values: BTreeMap<String, String>,
    /// Expanded `<details>` element keys (stringified NodeId).
    pub open_details: BTreeSet<String>,
    /// Scroll offsets keyed by stringified NodeId, as (x_px, y_px).
    pub scroll: BTreeMap<String, (u32, u32)>,
    /// Currently focused element id (stringified NodeId), if any.
    pub focused: Option<String>,
}

impl CanvasState {
    /// Serialize to JSON bytes for `snapshot_state` return.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("CanvasState serializes infallibly")
    }

    /// Parse JSON bytes from `restore_state` input.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("CanvasState JSON: {e}"))
    }

    /// True if every field is at its default — used by `snapshot_state`
    /// to return `None` when nothing interesting happened.
    pub fn is_empty(&self) -> bool {
        self.form_values.is_empty()
            && self.open_details.is_empty()
            && self.scroll.is_empty()
            && self.focused.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_bytes() {
        let mut s = CanvasState {
            schema_version: 1,
            ..CanvasState::default()
        };
        s.form_values.insert("42".into(), "hello".into());
        s.open_details.insert("88".into());
        s.scroll.insert("12".into(), (0, 50));
        s.focused = Some("42".into());

        let bytes = s.to_bytes();
        let back = CanvasState::from_bytes(&bytes).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn default_state_is_empty() {
        let s = CanvasState::default();
        assert!(s.is_empty());
    }

    #[test]
    fn from_bytes_error_on_garbage() {
        assert!(CanvasState::from_bytes(b"not json at all").is_err());
    }
}
