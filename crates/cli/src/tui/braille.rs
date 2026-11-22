//! Pictures as Unicode Braille, for terminals with no graphics protocol.
//!
//! One cell carries a 2×4 grid of dots (`U+2800` + a bit per dot), four times
//! the pixels of the half-block `▀` mode, with one ink color per cell on the
//! cell's background: a good match for formulas and line drawings (ink on a
//! plain page), a poor one for photographs. The picture is turned into a grid
//! of cells once and drawn straight into the frame buffer, so scrolling and
//! clipping are just a window over that grid.

use image::{imageops::FilterType, DynamicImage};
use ratatui::style::Color;

/// Dot bit for (column, row) inside a cell: Braille dots 1-3 and 7 are the
/// left column top to bottom, 4-6 and 8 the right one.
const DOT: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// A pixel counts as ink when it differs from the page by at least this
/// share of the strongest difference in the picture.
const INK_SHARE: f32 = 0.2;
/// Ink fainter than this (of the full range) is stretched up to it, so a
/// washed-out picture still shows its dots.
const INK_STRENGTH: f32 = 0.7;
/// ... and the strongest difference must itself be at least this much
/// (of 255), or the picture is blank.
const MIN_CONTRAST: f32 = 0.06;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub symbol: char,
    pub fg: Color,
    pub bg: Color,
}

#[derive(Clone, Debug)]
pub struct CellGrid {
    pub width: u16,
    pub height: u16,
    cells: Vec<Cell>,
}

impl CellGrid {
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        (x < self.width && y < self.height)
            .then(|| &self.cells[usize::from(y) * usize::from(self.width) + usize::from(x)])
    }

    /// Copies the part of the grid whose top-left cell is (`skip_col`,
    /// `skip_row`) into `area` of `buffer`.
    pub fn render(
        &self,
        skip_row: u16,
        skip_col: u16,
        area: ratatui::layout::Rect,
        buffer: &mut ratatui::buffer::Buffer,
    ) {
        for dy in 0..area.height {
            for dx in 0..area.width {
                let Some(cell) = self.cell(skip_col + dx, skip_row + dy) else {
                    continue;
                };
                if let Some(target) = buffer.cell_mut((area.x + dx, area.y + dy)) {
                    let mut utf8 = [0u8; 4];
                    target
                        .set_symbol(cell.symbol.encode_utf8(&mut utf8))
                        .set_fg(cell.fg)
                        .set_bg(cell.bg);
                }
            }
        }
    }
}

type Rgb = [f32; 3];

/// `pixel` (straight RGBA) over `page`.
fn over(pixel: [u8; 4], page: Rgb) -> Rgb {
    let a = f32::from(pixel[3]) / 255.0;
    let mut out = [0.0; 3];
    for i in 0..3 {
        out[i] = f32::from(pixel[i]) * a + page[i] * (1.0 - a);
    }
    out
}

/// The page color when the terminal didn't tell us: the commonest color of the
/// picture's opaque pixels (a diagram or formula is mostly page).
fn commonest_color(image: &image::RgbaImage) -> Rgb {
    let mut counts: std::collections::HashMap<(u8, u8, u8), (u32, [u32; 3])> = Default::default();
    for p in image.pixels().filter(|p| p[3] >= 128) {
        let entry = counts
            .entry((p[0] >> 4, p[1] >> 4, p[2] >> 4))
            .or_insert((0, [0; 3]));
        entry.0 += 1;
        for i in 0..3 {
            entry.1[i] += u32::from(p[i]);
        }
    }
    counts
        .values()
        .max_by_key(|(n, _)| *n)
        .map(|(n, sum)| [0, 1, 2].map(|i| sum[i] as f32 / *n as f32))
        .unwrap_or([0.0; 3])
}

fn difference(a: Rgb, b: Rgb) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0, f32::max) / 255.0
}

/// Turns `image` into `cols`×`rows` Braille cells. `surface` is the terminal's
/// background when known (it becomes every cell's background); otherwise the
/// picture's own commonest color decides what is page and the cells keep the
/// terminal's default background.
pub fn to_grid(
    image: &DynamicImage,
    cols: u16,
    rows: u16,
    surface: Option<(u8, u8, u8)>,
) -> CellGrid {
    let (cols, rows) = (cols.max(1), rows.max(1));
    let samples = image::imageops::resize(
        &image.to_rgba8(),
        u32::from(cols) * 2,
        u32::from(rows) * 4,
        FilterType::Lanczos3,
    );
    let page: Rgb = match surface {
        Some((r, g, b)) => [f32::from(r), f32::from(g), f32::from(b)],
        None => commonest_color(&samples),
    };
    let bg = surface.map_or(Color::Reset, |(r, g, b)| Color::Rgb(r, g, b));

    let strength: Vec<f32> = samples
        .pixels()
        .map(|p| difference(over(p.0, page), page))
        .collect();
    let strongest = strength.iter().copied().fold(0.0, f32::max);
    let threshold = strongest * INK_SHARE;
    let blank = strongest < MIN_CONTRAST;

    let width = samples.width() as usize;
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    for cy in 0..usize::from(rows) {
        for cx in 0..usize::from(cols) {
            let mut mask = 0u8;
            let mut inkiest = (0.0f32, page);
            for (dx, column) in DOT.iter().enumerate() {
                for (dy, bit) in column.iter().enumerate() {
                    let (x, y) = (cx * 2 + dx, cy * 4 + dy);
                    let d = strength[y * width + x];
                    if !blank && d >= threshold {
                        mask |= bit;
                        if d > inkiest.0 {
                            inkiest = (d, over(samples.get_pixel(x as u32, y as u32).0, page));
                        }
                    }
                }
            }
            cells.push(if mask == 0 {
                Cell {
                    symbol: ' ',
                    fg: Color::Reset,
                    bg,
                }
            } else {
                let k = (INK_STRENGTH / strongest).clamp(1.0, 3.0);
                let [r, g, b] = [0, 1, 2].map(|i| {
                    (page[i] + (inkiest.1[i] - page[i]) * k)
                        .round()
                        .clamp(0.0, 255.0) as u8
                });
                Cell {
                    symbol: char::from_u32(0x2800 + u32::from(mask)).unwrap_or(' '),
                    fg: Color::Rgb(r, g, b),
                    bg,
                }
            });
        }
    }
    CellGrid {
        width: cols,
        height: rows,
        cells,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn page(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([250, 241, 199, 255]))
    }

    #[test]
    fn a_blank_picture_is_blank_cells_on_the_surface() {
        let grid = to_grid(
            &DynamicImage::ImageRgba8(page(8, 8)),
            2,
            2,
            Some((250, 241, 199)),
        );
        for y in 0..2 {
            for x in 0..2 {
                let cell = grid.cell(x, y).unwrap();
                assert_eq!(cell.symbol, ' ');
                assert_eq!(cell.bg, Color::Rgb(250, 241, 199));
            }
        }
    }

    #[test]
    fn ink_becomes_the_dots_it_covers_in_the_ink_color() {
        // 4×8 px = 2×2 cells at 2×4 dots each: ink fills the top-left dot
        // column of the first cell (x=0, all four rows).
        let mut img = page(4, 8);
        for y in 0..4 {
            img.put_pixel(0, y, Rgba([10, 20, 30, 255]));
        }
        let grid = to_grid(&DynamicImage::ImageRgba8(img), 2, 2, Some((250, 241, 199)));
        let cell = grid.cell(0, 0).unwrap();
        // Left column: dots 1, 2, 3 and 7.
        assert_eq!(cell.symbol, '\u{2847}');
        assert_eq!(cell.fg, Color::Rgb(10, 20, 30));
        assert_eq!(grid.cell(1, 0).unwrap().symbol, ' ');
        assert_eq!(grid.cell(0, 1).unwrap().symbol, ' ');
    }

    #[test]
    fn every_dot_position_maps_to_its_braille_bit() {
        // One ink pixel per position of a single cell.
        for (dx, column) in DOT.iter().enumerate() {
            for (dy, bit) in column.iter().enumerate() {
                let mut img = page(2, 4);
                img.put_pixel(dx as u32, dy as u32, Rgba([0, 0, 0, 255]));
                let grid = to_grid(&DynamicImage::ImageRgba8(img), 1, 1, Some((250, 241, 199)));
                assert_eq!(
                    grid.cell(0, 0).unwrap().symbol as u32,
                    0x2800 + u32::from(*bit),
                    "dot at ({dx},{dy})"
                );
            }
        }
    }

    #[test]
    fn without_a_known_surface_the_commonest_color_is_the_page() {
        let mut img = RgbaImage::from_pixel(2, 4, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 255, 255, 255]));
        // Mostly black page, one white pixel is the ink; background stays default.
        let grid = to_grid(&DynamicImage::ImageRgba8(img), 1, 1, None);
        let cell = grid.cell(0, 0).unwrap();
        assert_eq!(cell.symbol as u32, 0x2801);
        assert_eq!(cell.fg, Color::Rgb(255, 255, 255));
        assert_eq!(cell.bg, Color::Reset);
    }

    #[test]
    fn transparent_pixels_are_page_and_render_copies_a_window() {
        let mut img = RgbaImage::from_pixel(4, 8, Rgba([0, 0, 0, 0]));
        img.put_pixel(2, 4, Rgba([200, 0, 0, 255]));
        let grid = to_grid(&DynamicImage::ImageRgba8(img), 2, 2, Some((10, 10, 10)));
        assert_eq!(grid.cell(1, 1).unwrap().symbol as u32, 0x2801);
        let area = ratatui::layout::Rect::new(3, 5, 1, 1);
        let mut buffer = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 10));
        // Window starting at cell (1, 1): its top-left cell lands at (3, 5).
        grid.render(1, 1, area, &mut buffer);
        assert_eq!(buffer[(3, 5)].symbol(), "\u{2801}");
        assert_eq!(buffer[(3, 5)].fg, Color::Rgb(200, 0, 0));
        assert_eq!(buffer[(3, 5)].bg, Color::Rgb(10, 10, 10));
        assert_eq!(buffer[(4, 5)].symbol(), " ");
    }
}
