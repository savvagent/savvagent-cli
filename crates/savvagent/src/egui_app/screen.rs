//! Pure helpers for painting/operating the plugin screen stack in egui:
//! modal geometry (egui Rect + the logical `Region` to hand `Screen::render`)
//! and de-duplication of egui's paired Key/Text events into a single stream of
//! `KeyEventPortable` suitable for `Screen::on_key`.

use egui::Rect;
use savvagent_plugin::KeyCodePortable;
use savvagent_plugin::manifest::ScreenLayout;
use savvagent_plugin::types::Region;

use crate::egui_app::convert::egui_event_to_portable;

/// Where to paint a screen and the logical `Region` to hand its `render`.
/// `outer` is in egui points; `region` is in logical monospace cols/rows. The
/// two fields describe the same rectangle in different units: `region` always
/// equals `outer` minus the layout's chrome (margin) for CenteredModal, and
/// the full `outer` extent for Fullscreen/BottomSheet.
#[derive(Debug, Clone, Copy)]
pub struct ModalGeometry {
    /// The egui rect the overlay (border + content) occupies — in points.
    pub outer: Rect,
    /// The inner area in logical monospace columns/rows, after chrome.
    /// Origin is always `(0, 0)`; screens render relative to their own region.
    pub region: Region,
}

/// Compute the overlay geometry for a `ScreenLayout` given the available rect
/// (the central area) and the monospace glyph advance/row size in points.
/// Mirrors the ratatui `paint_screen` sizing: percentage-of-area for
/// CenteredModal (clamped to >= 20 cols / >= 5 rows), with the inner region =
/// `outer.inner(Margin{h:2,v:1})` (the border overlaps the margin, not a second
/// subtraction); Fullscreen = whole area; BottomSheet = bottom `height` rows,
/// less the two rows the overlay spends on its separator + `tips()` chrome.
pub fn modal_geometry(
    avail: Rect,
    layout: &ScreenLayout,
    glyph_w: f32,
    glyph_h: f32,
) -> ModalGeometry {
    let cols = |w: f32| (w / glyph_w).floor() as u16;
    let rows = |h: f32| (h / glyph_h).floor() as u16;
    match *layout {
        // Fullscreen, plus the forward-compat fallback for any future layout
        // variant (`ScreenLayout` is `#[non_exhaustive]`): fill the area.
        ScreenLayout::Fullscreen { .. } => ModalGeometry {
            outer: avail,
            region: Region {
                x: 0,
                y: 0,
                width: cols(avail.width()),
                height: rows(avail.height()),
            },
        },
        ScreenLayout::CenteredModal {
            width_pct,
            height_pct,
            ..
        } => {
            let min_w = 20.0 * glyph_w;
            let min_h = 5.0 * glyph_h;
            let w = ((avail.width() * width_pct as f32 / 100.0).max(min_w)).min(avail.width());
            let h = ((avail.height() * height_pct as f32 / 100.0).max(min_h)).min(avail.height());
            let outer = Rect::from_center_size(avail.center(), egui::vec2(w, h));
            // chrome: the ratatui content area is `outer.inner(Margin{h:2,v:1})`
            // (ui.rs `paint_screen`) — subtract the 2-col / 1-row margin on each
            // side. The `Borders::ALL` border is drawn *within* `outer` and
            // overlaps the margin, so it is NOT a second subtraction.
            let inner_cols = cols(w).saturating_sub(2 * 2); // 2-col margin each side
            let inner_rows = rows(h).saturating_sub(2); // 1-row margin each side
            ModalGeometry {
                outer,
                region: Region {
                    x: 0,
                    y: 0,
                    width: inner_cols,
                    height: inner_rows,
                },
            }
        }
        ScreenLayout::BottomSheet { height } => {
            let h = (height as f32 * glyph_h).min(avail.height());
            let outer = Rect::from_min_size(
                egui::pos2(avail.min.x, avail.max.y - h),
                egui::vec2(avail.width(), h),
            );
            // Unlike ratatui — which overpaints the sheet's last row with
            // the screen's `tips()` — `paint_screen_overlay` draws a
            // separator *and* the tips lines as extra widgets below the
            // rendered lines, inside the same clipped `outer` rect. Hand
            // the screen a region two rows shorter so those two rows of
            // chrome have somewhere to go; otherwise the trailing lines
            // (for the palette, the windowed cursor row) fall outside the
            // clip rect and disappear.
            ModalGeometry {
                outer,
                region: Region {
                    x: 0,
                    y: 0,
                    width: cols(avail.width()),
                    height: rows(h).saturating_sub(2),
                },
            }
        }
        _ => ModalGeometry {
            outer: avail,
            region: Region {
                x: 0,
                y: 0,
                width: cols(avail.width()),
                height: rows(avail.height()),
            },
        },
    }
}

/// Collapse egui's per-frame event list into the `KeyEventPortable`s a
/// `Screen::on_key` should see, WITHOUT the Key/Text double-count: a printable
/// `Event::Text` becomes one `Char`; an `Event::Key` is forwarded only when it
/// is NOT a plain unmodified printable char (i.e. navigation/control keys, or
/// any key carrying ctrl/alt/meta — accelerators egui does not echo as Text).
pub fn portable_keys_from_events(
    events: &[egui::Event],
) -> Vec<savvagent_plugin::KeyEventPortable> {
    let mut out = Vec::new();
    for ev in events {
        let Some(k) = egui_event_to_portable(ev) else {
            continue;
        };
        let is_text = matches!(ev, egui::Event::Text(_));
        let is_plain_char = matches!(k.code, KeyCodePortable::Char(_))
            && !k.modifiers.ctrl
            && !k.modifiers.alt
            && !k.modifiers.meta;
        // From a Key event, drop plain printable chars (the paired Text event
        // carries them); keep everything from Text, and keep modified/non-char
        // keys from Key events.
        if !is_text && is_plain_char {
            continue;
        }
        out.push(k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::manifest::ScreenLayout;
    use savvagent_plugin::{KeyCodePortable, KeyMods};

    // Glyph metrics used throughout: 8pt advance, 16pt row.
    const GW: f32 = 8.0;
    const GH: f32 = 16.0;

    #[test]
    fn centered_modal_is_centered_and_percentage_sized() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let layout = ScreenLayout::CenteredModal {
            width_pct: 60,
            height_pct: 50,
            title: None,
        };
        let g = modal_geometry(avail, &layout, GW, GH);
        // 60% of 1000 = 600 wide, 50% of 800 = 400 tall, centered.
        assert!((g.outer.width() - 600.0).abs() < 1.0);
        assert!((g.outer.height() - 400.0).abs() < 1.0);
        assert!((g.outer.center().x - 500.0).abs() < 1.0);
        assert!((g.outer.center().y - 400.0).abs() < 1.0);
    }

    #[test]
    fn centered_modal_inner_region_subtracts_chrome_margin() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let layout = ScreenLayout::CenteredModal {
            width_pct: 60,
            height_pct: 50,
            title: None,
        };
        let g = modal_geometry(avail, &layout, GW, GH);
        // Inner = `outer.inner(Margin{h:2,v:1})`, matching ratatui (the border
        // overlaps the margin and is not subtracted again).
        // outer 600x400 pts -> 75x25 cols/rows; minus the 2-col/1-row margin on
        // each side (4 cols, 2 rows) -> 71 cols, 23 rows.
        assert_eq!(g.region.width, 71);
        assert_eq!(g.region.height, 23);
    }

    #[test]
    fn centered_modal_clamps_width_to_minimum() {
        // 1% width on a 1000pt-wide area = 10pt, which is below the 20-col
        // (160pt) minimum from ratatui's `.max(20)` floor. The clamp must
        // engage and keep the modal usable.
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let layout = ScreenLayout::CenteredModal {
            width_pct: 1,
            height_pct: 50,
            title: None,
        };
        let g = modal_geometry(avail, &layout, GW, GH);
        assert!(
            (g.outer.width() - 160.0).abs() < 0.5,
            "expected width floored to min_w=160pt, got {}",
            g.outer.width()
        );
    }

    #[test]
    fn centered_modal_clamps_height_to_minimum() {
        // 1% height on an 800pt-tall area = 8pt, below the 5-row (80pt)
        // minimum from ratatui's `.max(5)` floor. The clamp must engage.
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let layout = ScreenLayout::CenteredModal {
            width_pct: 50,
            height_pct: 1,
            title: None,
        };
        let g = modal_geometry(avail, &layout, GW, GH);
        assert!(
            (g.outer.height() - 80.0).abs() < 0.5,
            "expected height floored to min_h=80pt, got {}",
            g.outer.height()
        );
    }

    #[test]
    fn fullscreen_region_is_whole_area() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0));
        let layout = ScreenLayout::Fullscreen { hide_chrome: false };
        let g = modal_geometry(avail, &layout, GW, GH);
        assert_eq!(g.outer, avail);
        assert_eq!(g.region.width, 100); // 800/8
        assert_eq!(g.region.height, 50); // 800/16
    }

    #[test]
    fn bottom_sheet_is_anchored_to_the_bottom_of_the_area() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0));
        let g = modal_geometry(avail, &ScreenLayout::BottomSheet { height: 12 }, GW, GH);
        // 12 rows * 16pt = 192pt tall, flush with the bottom of `avail`.
        assert!((g.outer.height() - 192.0).abs() < 0.5);
        assert!((g.outer.max.y - avail.max.y).abs() < 0.5);
        assert_eq!(g.outer.width(), avail.width());
    }

    #[test]
    fn bottom_sheet_region_reserves_rows_for_the_separator_and_tips() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0));
        let g = modal_geometry(avail, &ScreenLayout::BottomSheet { height: 12 }, GW, GH);
        // `paint_screen_overlay` draws a separator plus the screen's tips
        // below the rendered lines inside the same clipped rect, so the
        // screen gets 12 - 2 = 10 rows rather than all 12.
        assert_eq!(g.region.height, 10);
        assert_eq!(g.region.width, 100); // 800/8
    }

    #[test]
    fn bottom_sheet_shorter_than_its_chrome_yields_an_empty_region() {
        let avail = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0));
        let g = modal_geometry(avail, &ScreenLayout::BottomSheet { height: 1 }, GW, GH);
        assert_eq!(g.region.height, 0);
    }

    #[test]
    fn text_event_yields_one_char_and_suppresses_paired_key() {
        // egui emits Key{A} + Text("a") for one press; we want a single Char('a').
        let events = vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Text("a".into()),
        ];
        let keys = portable_keys_from_events(&events);
        assert_eq!(keys.len(), 1);
        assert!(matches!(keys[0].code, KeyCodePortable::Char('a')));
    }

    #[test]
    fn non_text_key_is_forwarded() {
        let events = vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];
        let keys = portable_keys_from_events(&events);
        assert_eq!(keys.len(), 1);
        assert!(matches!(keys[0].code, KeyCodePortable::Enter));
    }

    #[test]
    fn ctrl_accelerator_key_is_forwarded_even_though_char() {
        // Ctrl+S produces no Text event, so the Key path must forward it.
        let events = vec![egui::Event::Key {
            key: egui::Key::S,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        }];
        let keys = portable_keys_from_events(&events);
        assert_eq!(keys.len(), 1);
        assert!(keys[0].modifiers.ctrl);
        assert!(matches!(keys[0].code, KeyCodePortable::Char('s')));
        let _ = KeyMods::default();
    }
}
