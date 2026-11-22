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

/// A cell whose pixels differ by less than this (of 255, per channel) is flat:
/// no dots, just its own color as the background.
const FLAT: f32 = 14.0;

/// Line art detection: colors covering at least `COLOR_SHARE_PERMILLE`‰ of the
/// picture are "flat" ones (anti-aliasing shades are each only a sliver, a
/// photograph has a wide tonal range); they must cover `FLAT_COVERAGE_PERCENT`%,
/// and `STRONG_PERCENT`% of what isn't page must stand out strongly.
const COLOR_SHARE_PERMILLE: usize = 5;
const FLAT_COVERAGE_PERCENT: usize = 75;
const STRONG_PERCENT: usize = 50;
/// In line art a sample is ink when it differs from the page by at least this
/// share of the strongest difference in the picture ...
const INK_SHARE: f32 = 0.3;
/// ... and a picture whose strongest difference is under this (of 255) is blank.
const MIN_CONTRAST: f32 = 15.0;

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

/// The commonest color among `colors` (4 bits per channel buckets): the page.
fn commonest_color(colors: impl Iterator<Item = Rgb>) -> Rgb {
    let mut counts: std::collections::HashMap<(u8, u8, u8), (u32, Rgb)> = Default::default();
    for c in colors {
        let key = (c[0] as u8 >> 4, c[1] as u8 >> 4, c[2] as u8 >> 4);
        let entry = counts.entry(key).or_insert((0, [0.0; 3]));
        entry.0 += 1;
        for i in 0..3 {
            entry.1[i] += c[i];
        }
    }
    counts
        .values()
        .max_by_key(|(n, _)| *n)
        .map(|(n, sum)| sum.map(|v| v / *n as f32))
        .unwrap_or([0.0; 3])
}

fn luma(c: Rgb) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn mean(colors: impl Iterator<Item = Rgb>) -> Rgb {
    let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
    for c in colors {
        for i in 0..3 {
            sum[i] += c[i];
        }
        n += 1.0;
    }
    if n == 0.0 {
        return [0.0; 3];
    }
    sum.map(|v| v / n)
}

fn distance(a: Rgb, b: Rgb) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0, f32::max)
}

fn to_color(c: Rgb) -> Color {
    let [r, g, b] = c.map(|v| v.round().clamp(0.0, 255.0) as u8);
    Color::Rgb(r, g, b)
}

/// Splits one cell's eight samples into two classes by luminance (the split
/// that maximizes the between-class variance — Otsu's method) and returns
/// which samples are dots: the smaller class (ink is usually the minority),
/// the one further from `page` on a tie.
fn dots_of(samples: &[Rgb; 8], page: Rgb) -> u8 {
    let mut order: Vec<usize> = (0..8).collect();
    order.sort_by(|&a, &b| luma(samples[a]).total_cmp(&luma(samples[b])));
    let lumas: Vec<f32> = order.iter().map(|&i| luma(samples[i])).collect();
    let total: f32 = lumas.iter().sum();
    let (mut best, mut best_at) = (-1.0f32, 1);
    let mut low_sum = 0.0;
    for k in 1..8 {
        low_sum += lumas[k - 1];
        let (n_low, n_high) = (k as f32, (8 - k) as f32);
        let (m_low, m_high) = (low_sum / n_low, (total - low_sum) / n_high);
        let between = n_low * n_high * (m_low - m_high).powi(2);
        if between > best {
            best = between;
            best_at = k;
        }
    }
    // `order[..best_at]` are the dark samples, the rest the light ones.
    let dark_are_dots = match best_at.cmp(&(8 - best_at)) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        // A tie: the class further from the page is the ink.
        std::cmp::Ordering::Equal => {
            let class_distance = |ranks: std::ops::Range<usize>| {
                let n = ranks.len() as f32;
                let sum: f32 = ranks.map(|r| distance(samples[order[r]], page)).sum();
                sum / n
            };
            class_distance(0..best_at) >= class_distance(best_at..8)
        }
    };
    let mut mask = 0u8;
    for (rank, &index) in order.iter().enumerate() {
        if (rank < best_at) == dark_are_dots {
            mask |= 1 << index;
        }
    }
    mask
}

/// Whether the picture is line art: a few flat colors cover most of it (the
/// page and the ink; a smooth tonal range doesn't) and, of what isn't page,
/// most stands out strongly (strokes, not the faint shading of an object).
/// Judged on the source pixels, sampled with a stride — a thumbnail would blur
/// thin strokes into mid-tones.
fn is_line_art(image: &DynamicImage) -> bool {
    let source = image.to_rgba8();
    let total = source.pixels().len();
    let stride = (total / 250_000).max(1);
    let mut buckets: std::collections::HashMap<(u8, u8, u8), (usize, Rgb)> = Default::default();
    let mut sampled = 0usize;
    let mut colors: Vec<Rgb> = Vec::new();
    for p in source.pixels().step_by(stride).filter(|p| p[3] >= 128) {
        let c = [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])];
        let entry = buckets
            .entry((p[0] >> 4, p[1] >> 4, p[2] >> 4))
            .or_insert((0, [0.0; 3]));
        entry.0 += 1;
        for i in 0..3 {
            entry.1[i] += c[i];
        }
        colors.push(c);
        sampled += 1;
    }
    if sampled == 0 {
        return false;
    }
    let flat: usize = buckets
        .values()
        .filter(|(n, _)| n * 1000 >= sampled * COLOR_SHARE_PERMILLE)
        .map(|(n, _)| *n)
        .sum();
    if flat * 100 < sampled * FLAT_COVERAGE_PERCENT {
        return false;
    }
    let Some((n, sum)) = buckets.values().max_by_key(|(n, _)| *n) else {
        return false;
    };
    let page = sum.map(|v| v / *n as f32);
    let strongest = colors
        .iter()
        .map(|c| distance(*c, page))
        .fold(0.0, f32::max);
    if strongest < MIN_CONTRAST {
        return false;
    }
    let (mut off_page, mut strong) = (0usize, 0usize);
    for c in &colors {
        let d = distance(*c, page);
        if d > 20.0 {
            off_page += 1;
            if d >= strongest * 0.5 {
                strong += 1;
            }
        }
    }
    off_page > 0 && strong * 100 >= off_page * STRONG_PERCENT
}

/// Line art: a sample is a dot when it stands out from the page by
/// `INK_SHARE` of the strongest difference; the dots take the color of the
/// cell's strongest sample, a fully inked cell is a solid block, the rest of
/// the cell is the page. Lights every plausible ink sample, because a real
/// terminal draws Braille dots small and dim.
fn line_art_grid(
    samples: &image::RgbaImage,
    cols: u16,
    rows: u16,
    backdrop: Rgb,
    page: Rgb,
) -> CellGrid {
    let composed: Vec<Rgb> = samples.pixels().map(|p| over(p.0, backdrop)).collect();
    let strength: Vec<f32> = composed.iter().map(|c| distance(*c, page)).collect();
    let strongest = strength.iter().copied().fold(0.0, f32::max);
    let blank = strongest < MIN_CONTRAST;
    let threshold = strongest * INK_SHARE;
    let bg = to_color(page);
    let width = samples.width() as usize;
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    for cy in 0..usize::from(rows) {
        for cx in 0..usize::from(cols) {
            let (mut mask, mut inkiest) = (0u8, (0.0f32, page));
            for (dx, column) in DOT.iter().enumerate() {
                for (dy, bit) in column.iter().enumerate() {
                    let at = (cy * 4 + dy) * width + cx * 2 + dx;
                    if !blank && strength[at] >= threshold {
                        mask |= bit;
                        if strength[at] > inkiest.0 {
                            inkiest = (strength[at], composed[at]);
                        }
                    }
                }
            }
            // The downscale blurred the strokes; bring the cell's strongest
            // sample back to the full strength of the picture's ink, same hue.
            let k = if inkiest.0 > 0.0 {
                strongest / inkiest.0
            } else {
                1.0
            };
            let ink = to_color(
                [0, 1, 2].map(|i| (page[i] + (inkiest.1[i] - page[i]) * k).clamp(0.0, 255.0)),
            );
            cells.push(match mask {
                0 => Cell {
                    symbol: ' ',
                    fg: Color::Reset,
                    bg,
                },
                0xFF => Cell {
                    symbol: '█',
                    fg: ink,
                    bg,
                },
                _ => Cell {
                    symbol: char::from_u32(0x2800 + u32::from(mask)).unwrap_or(' '),
                    fg: ink,
                    bg,
                },
            });
        }
    }
    CellGrid {
        width: cols,
        height: rows,
        cells,
    }
}

/// Turns `image` into `cols`×`rows` Braille cells. Each cell is split into
/// two color classes, dots in the minority class's color over the other
/// class's color; a cell with nothing to split is just its color. `surface`
/// (the terminal's background, when known) is what transparency shows.
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
        FilterType::CatmullRom,
    );
    // What transparency shows: the terminal's background, or (unknown) the
    // picture's own commonest opaque color.
    let backdrop: Rgb = match surface {
        Some((r, g, b)) => [f32::from(r), f32::from(g), f32::from(b)],
        None => commonest_color(
            samples
                .pixels()
                .filter(|p| p[3] >= 128)
                .map(|p| [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])]),
        ),
    };
    // The picture's page: its commonest color once composited. A formula on
    // the terminal's background has the background as its page, a product
    // shot on white has white.
    let page = commonest_color(samples.pixels().map(|p| over(p.0, backdrop)));

    if is_line_art(image) {
        return line_art_grid(&samples, cols, rows, backdrop, page);
    }

    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    for cy in 0..u32::from(rows) {
        for cx in 0..u32::from(cols) {
            // Samples in bit order: index = position in DOT.
            let mut colors = [[0.0f32; 3]; 8];
            let mut seen = false;
            let mut order = [(0usize, 0usize); 8];
            for (dx, column) in DOT.iter().enumerate() {
                for (dy, bit) in column.iter().enumerate() {
                    let index = bit.trailing_zeros() as usize;
                    order[index] = (dx, dy);
                    let pixel = samples.get_pixel(cx * 2 + dx as u32, cy * 4 + dy as u32).0;
                    seen |= pixel[3] != 0;
                    colors[index] = over(pixel, backdrop);
                }
            }
            let spread = (0..3)
                .map(|c| {
                    let (lo, hi) = colors.iter().fold((255.0f32, 0.0f32), |(lo, hi), p| {
                        (lo.min(p[c]), hi.max(p[c]))
                    });
                    hi - lo
                })
                .fold(0.0, f32::max);
            if spread < FLAT {
                let flat = if seen || surface.is_some() {
                    to_color(mean(colors.iter().copied()))
                } else {
                    Color::Reset
                };
                cells.push(Cell {
                    symbol: ' ',
                    fg: Color::Reset,
                    bg: flat,
                });
                continue;
            }
            let mask = dots_of(&colors, page);
            let mut rest = mean((0..8).filter(|i| mask >> i & 1 == 0).map(|i| colors[i]));
            // The dots' color is the sample furthest from the rest, not the
            // average: averaging thin strokes with their blurred edges washes
            // the ink out to gray.
            let ink = (0..8)
                .filter(|i| mask >> i & 1 == 1)
                .map(|i| colors[i])
                .max_by(|a, b| distance(*a, rest).total_cmp(&distance(*b, rest)))
                .unwrap_or(rest);
            // The "rest" of an edge cell averages page with half-covered
            // pixels, a muddy gray. When it is near the page compared with
            // how strongly the ink stands out, it is the page.
            if distance(rest, page) <= 0.5 * distance(ink, rest) {
                rest = page;
            }
            cells.push(Cell {
                symbol: char::from_u32(0x2800 + u32::from(mask)).unwrap_or(' '),
                fg: to_color(ink),
                bg: to_color(rest),
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
    fn without_a_known_surface_the_minority_color_is_the_ink() {
        let mut img = RgbaImage::from_pixel(2, 4, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 255, 255, 255]));
        // Mostly black, one white pixel: the minority class is the dots.
        let grid = to_grid(&DynamicImage::ImageRgba8(img), 1, 1, None);
        let cell = grid.cell(0, 0).unwrap();
        assert_eq!(cell.symbol as u32, 0x2801);
        assert_eq!(cell.fg, Color::Rgb(255, 255, 255));
        assert_eq!(cell.bg, Color::Rgb(0, 0, 0));
    }

    #[test]
    fn a_picture_on_its_own_white_page_stays_white_on_a_dark_terminal() {
        // The case that filled whole product photos with dots: the picture's
        // page is white, the terminal's is dark; flat white must stay flat.
        let mut img = RgbaImage::from_pixel(16, 32, Rgba([255, 255, 255, 255]));
        for x in 4..12 {
            img.put_pixel(x, 16, Rgba([20, 20, 20, 255]));
        }
        let grid = to_grid(&DynamicImage::ImageRgba8(img), 8, 8, Some((39, 40, 34)));
        let flat = grid.cell(0, 0).unwrap();
        assert_eq!(flat.symbol, ' ');
        assert_eq!(flat.bg, Color::Rgb(255, 255, 255));
        let dots = (0..8)
            .flat_map(|x| (0..8).map(move |y| (x, y)))
            .filter(|&(x, y)| grid.cell(x, y).unwrap().symbol != ' ')
            .count();
        assert!(
            (1..=6).contains(&dots),
            "only the stroke has dots, got {dots}"
        );
    }

    #[test]
    fn thin_strokes_keep_their_ink_color_instead_of_washing_out() {
        // A one-sample light stroke on the dark page: the dots stay as light
        // as the stroke, and the cell's background is the page, not a gray mix.
        let mut img = RgbaImage::from_pixel(8, 8, Rgba([39, 40, 34, 255]));
        for y in 0..8 {
            img.put_pixel(3, y, Rgba([240, 235, 228, 255]));
        }
        let grid = to_grid(&DynamicImage::ImageRgba8(img), 4, 2, Some((39, 40, 34)));
        let cell = grid.cell(1, 0).unwrap();
        assert_ne!(cell.symbol, ' ');
        let Color::Rgb(r, ..) = cell.fg else { panic!() };
        assert!(r > 200, "ink washed out: {:?}", cell.fg);
        assert_eq!(cell.bg, Color::Rgb(39, 40, 34));
    }

    fn formula(tex: &str) -> DynamicImage {
        let svg = crate::tui::math::to_svg(tex, true)
            .unwrap()
            .replace("currentColor", "#f2ede6");
        crate::tui::svg_raster::rasterize(
            &svg,
            Some(meshfox_core::image_attrs::Background::Rgb(39, 40, 34)),
        )
        .unwrap()
    }

    #[test]
    fn formulas_are_line_art_and_photographs_are_not() {
        assert!(is_line_art(&formula(r"\frac{a}{b} + \sqrt{x}")));
        // A smooth tonal range, like a photograph.
        let photo = RgbaImage::from_fn(64, 64, |x, y| {
            Rgba([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8, 255])
        });
        assert!(!is_line_art(&DynamicImage::ImageRgba8(photo)));
    }

    #[test]
    fn a_formula_lights_its_strokes_in_the_text_color_on_the_terminal_background() {
        let img = formula(r"\mathrm{Re} = \frac{\rho U L}{\mu}");
        let cols = 40;
        let rows =
            ((f32::from(cols) * img.height() as f32 / img.width() as f32) / 2.0).ceil() as u16;
        let grid = to_grid(&img, cols, rows, Some((39, 40, 34)));
        let mut lit = 0;
        for y in 0..grid.height {
            for x in 0..grid.width {
                let cell = grid.cell(x, y).unwrap();
                assert_eq!(cell.bg, Color::Rgb(39, 40, 34));
                if cell.symbol != ' ' {
                    lit += 1;
                    let Color::Rgb(r, ..) = cell.fg else { panic!() };
                    assert!(r > 200, "ink too dim: {:?}", cell.fg);
                }
            }
        }
        assert!(lit > 10, "the formula must leave marks, got {lit}");
    }

    #[test]
    #[ignore]
    fn is_line_art_of_a_file() {
        let path = std::env::var("BRAILLE_PHOTO").unwrap();
        println!("line art: {}", is_line_art(&image::open(path).unwrap()));
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
