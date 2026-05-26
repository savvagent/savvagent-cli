//! Cell ↔ pixel coordinate translation for canvas mouse events.
//!
//! Terminal mouse events arrive in cells (row, column). The renderer
//! needs frame-relative pixel coords for synthetic event dispatch.
//! Translation depends on the (cell_width_px, cell_height_px) reported
//! by `ratatui-image::Picker` at startup and the canvas's render rect
//! (top-left cell + size in cells).
//!
//! Pure functions; no Blitz dependency. Tested in isolation.

#![warn(missing_docs)]

/// Pixel dimensions of one terminal cell, as reported by
/// `ratatui-image::Picker::from_query_stdio` at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPixelSize {
    /// Pixel width of one cell. Common values: 8, 10, 12.
    pub width: u16,
    /// Pixel height of one cell. Common values: 16, 20.
    pub height: u16,
}

/// Cell-coordinate rect occupied by a rendered canvas on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    /// Column of the top-left cell.
    pub col: u16,
    /// Row of the top-left cell.
    pub row: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

/// Translate a terminal-cell mouse event to a frame-relative pixel
/// coordinate. Returns `None` if the cell is outside `rect`.
pub fn cell_to_pixel(
    rect: CellRect,
    cell_size: CellPixelSize,
    event_col: u16,
    event_row: u16,
) -> Option<(u32, u32)> {
    if event_col < rect.col
        || event_row < rect.row
        || event_col >= rect.col + rect.width
        || event_row >= rect.row + rect.height
    {
        return None;
    }
    let dx_cells = event_col - rect.col;
    let dy_cells = event_row - rect.row;
    let x_px = u32::from(dx_cells) * u32::from(cell_size.width);
    let y_px = u32::from(dy_cells) * u32::from(cell_size.height);
    Some((x_px, y_px))
}

/// Does the given cell-coord pair land inside `rect`?
pub fn contains_cell(rect: CellRect, col: u16, row: u16) -> bool {
    col >= rect.col
        && row >= rect.row
        && col < rect.col + rect.width
        && row < rect.row + rect.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(col: u16, row: u16, w: u16, h: u16) -> CellRect {
        CellRect {
            col,
            row,
            width: w,
            height: h,
        }
    }

    fn cs(w: u16, h: u16) -> CellPixelSize {
        CellPixelSize {
            width: w,
            height: h,
        }
    }

    #[test]
    fn inside_returns_pixel_offset() {
        let rect = r(10, 5, 40, 12);
        let cell = cs(8, 16);
        // Click on the cell at (col=12, row=7): 2 cells right, 2 cells down.
        assert_eq!(cell_to_pixel(rect, cell, 12, 7), Some((16, 32)));
    }

    #[test]
    fn outside_returns_none() {
        let rect = r(10, 5, 40, 12);
        let cell = cs(8, 16);
        assert!(cell_to_pixel(rect, cell, 9, 5).is_none()); // left of
        assert!(cell_to_pixel(rect, cell, 50, 5).is_none()); // right of
        assert!(cell_to_pixel(rect, cell, 10, 4).is_none()); // above
        assert!(cell_to_pixel(rect, cell, 10, 17).is_none()); // below
    }

    #[test]
    fn top_left_cell_maps_to_origin() {
        let rect = r(10, 5, 40, 12);
        let cell = cs(8, 16);
        assert_eq!(cell_to_pixel(rect, cell, 10, 5), Some((0, 0)));
    }

    #[test]
    fn contains_cell_matches_bounds() {
        let rect = r(10, 5, 40, 12);
        assert!(contains_cell(rect, 10, 5));
        assert!(contains_cell(rect, 49, 16));
        assert!(!contains_cell(rect, 9, 5));
        assert!(!contains_cell(rect, 50, 5));
    }
}
