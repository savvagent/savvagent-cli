/// Resolve an xterm 256-color palette index to its RGB triple.
///
/// Layout:
/// * `0..=15`  — system colors (ANSI 8 + bright ANSI 8)
/// * `16..=231` — 6×6×6 RGB cube
/// * `232..=255` — 24 grayscale steps
///
/// Single source of truth shared with the egui sink
/// (`crate::egui_app::convert`) so indexed colors render identically in the
/// GUI conversation log.
pub(crate) fn xterm_256_rgb(n: u8) -> (u8, u8, u8) {
    match n {
        0 => (0, 0, 0),
        1 => (128, 0, 0),
        2 => (0, 128, 0),
        3 => (128, 128, 0),
        4 => (0, 0, 128),
        5 => (128, 0, 128),
        6 => (0, 128, 128),
        7 => (192, 192, 192),
        8 => (128, 128, 128),
        9 => (255, 0, 0),
        10 => (0, 255, 0),
        11 => (255, 255, 0),
        12 => (0, 0, 255),
        13 => (255, 0, 255),
        14 => (0, 255, 255),
        15 => (255, 255, 255),
        16..=231 => {
            let idx = n - 16;
            let r = idx / 36;
            let g = (idx / 6) % 6;
            let b = idx % 6;
            let to_comp = |x: u8| -> u8 {
                if x == 0 {
                    0
                } else {
                    (55_u16 + 40_u16 * x as u16).min(255) as u8
                }
            };
            (to_comp(r), to_comp(g), to_comp(b))
        }
        232..=255 => {
            let v = (8_u16 + 10_u16 * (n as u16 - 232)).min(255) as u8;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_system_colors_match_named_equivalents() {
        assert_eq!(xterm_256_rgb(0), (0, 0, 0));
        assert_eq!(xterm_256_rgb(7), (192, 192, 192)); // ANSI white = gray
        assert_eq!(xterm_256_rgb(8), (128, 128, 128)); // bright black = dark gray
        assert_eq!(xterm_256_rgb(15), (255, 255, 255));
    }

    #[test]
    fn indexed_rgb_cube_uses_xterm_step_values() {
        // 6×6×6 cube starts at 16 with (0,0,0) and increments by the
        // xterm convention `55 + 40·x` for x>0.
        assert_eq!(xterm_256_rgb(16), (0, 0, 0));
        assert_eq!(xterm_256_rgb(17), (0, 0, 95)); // (0,0,1) → blue 0x5f
        assert_eq!(xterm_256_rgb(231), (255, 255, 255));
    }

    #[test]
    fn indexed_grayscale_ramp_steps_by_ten() {
        assert_eq!(xterm_256_rgb(232), (8, 8, 8));
        assert_eq!(xterm_256_rgb(233), (18, 18, 18));
        assert_eq!(xterm_256_rgb(255), (238, 238, 238));
    }
}
