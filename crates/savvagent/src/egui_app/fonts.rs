//! Bundled monospace font install.
//!
//! The line-based log and the plugin slots rely on a stable monospace face
//! with box-drawing and status glyphs (`✓`, `✗`, `…`). egui's built-in
//! monospace (Hack) covers most of these, but bundling DejaVu Sans Mono makes
//! the rendering independent of whatever fonts happen to be installed on the
//! host and gives a consistent glyph set across platforms. License:
//! `assets/fonts/DejaVuSansMono-LICENSE.txt` (Bitstream Vera; DejaVu changes
//! public domain).

/// Register the bundled DejaVu Sans Mono as the highest-priority
/// [`egui::FontFamily::Monospace`] face. Called once from
/// `SavvagentApp::new` with the creation context's egui context.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dejavu-mono".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/DejaVuSansMono.ttf"
        ))),
    );
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "dejavu-mono".to_owned());
    ctx.set_fonts(fonts);
}
