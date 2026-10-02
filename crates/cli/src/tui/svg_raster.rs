//! SVG → pixels for the TUI. A terminal can only show a raster image
//! (`ratatui-image` takes a `DynamicImage`), and the `image` crate has no
//! SVG decoder, so an SVG Markdown image (`![](data:image/svg+xml;base64,…)`
//! — `output="image"`'s own output, or a hand-written one) goes through
//! `resvg` first.
//!
//! The SVG's *text* is themed before parsing, the same contract the web UI
//! applies (`meshfox_core::svg`'s module doc): `currentColor` follows the
//! theme's foreground, and `var(--mf-*)` is substituted textually because
//! resvg has no CSS-variable support at all (not even `var()` fallbacks —
//! checked against resvg 0.45). The TUI is always "dark chrome" (see
//! `theme.rs`), so it always uses the dark theme rather than guessing the
//! terminal's actual background.

use meshfox_core::image_attrs::Background;
use meshfox_core::svg::{self, Theme};
use resvg::{tiny_skia, usvg};
use std::sync::{Arc, OnceLock};

/// Aim for a raster whose longer side is in this range: small SVGs (a
/// 100×50 diagram) are scaled *up* so the terminal's image protocol has
/// real pixels to work with, huge ones are scaled down so they don't
/// cost memory for detail a 56×24-cell budget can't show anyway.
const MIN_LONG_SIDE: f32 = 800.0;
const MAX_LONG_SIDE: f32 = 2000.0;
const MAX_UPSCALE: f32 = 8.0;

/// Loaded once: scanning the system's font directories takes long enough
/// to notice, and every SVG in a document would otherwise redo it.
fn font_db() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

/// Rasterizes `svg_text` (themed first), painting `bg` behind it when it's
/// a color — `None`/`Transparent` leave the pixels transparent. `None` if
/// the SVG doesn't parse or has no drawable size.
pub fn rasterize(svg_text: &str, bg: Option<Background>) -> Option<image::DynamicImage> {
    let theme = Theme::dark();
    let themed = svg::resolve_vars(&svg::inject_theme(svg_text, &theme), &theme);
    let mut options = usvg::Options::default();
    options.fontdb = font_db();
    let tree = usvg::Tree::from_str(&themed, &options).ok()?;

    let size = tree.size();
    let long = size.width().max(size.height());
    if !(long.is_finite() && long > 0.0) {
        return None;
    }
    let scale = if long < MIN_LONG_SIDE {
        (MIN_LONG_SIDE / long).min(MAX_UPSCALE)
    } else if long > MAX_LONG_SIDE {
        MAX_LONG_SIDE / long
    } else {
        1.0
    };
    let width = (size.width() * scale).ceil().max(1.0) as u32;
    let height = (size.height() * scale).ceil().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    if let Some(Background::Rgb(r, g, b)) = bg {
        pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
    }
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    // tiny-skia stores premultiplied alpha; `image` wants straight alpha.
    let mut raw = Vec::with_capacity(pixmap.pixels().len() * 4);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        raw.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    image::RgbaImage::from_raw(width, height, raw).map(image::DynamicImage::ImageRgba8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(img: &image::DynamicImage, x: u32, y: u32) -> [u8; 4] {
        img.to_rgba8().get_pixel(x, y).0
    }

    const SQUARE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 10 10"><rect width="100" height="100" viewBox="0 0 10 10" fill="#ff0000"/></svg>"##;

    #[test]
    fn rasterizes_and_scales_a_small_svg_up() {
        let img = rasterize(SQUARE, None).expect("valid svg");
        assert_eq!((img.width(), img.height()), (800, 800));
        assert_eq!(pixel(&img, 400, 400), [255, 0, 0, 255]);
    }

    #[test]
    fn upscaling_is_capped_so_a_tiny_svg_is_not_blown_up_absurdly() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>"##;
        let img = rasterize(svg, None).unwrap();
        assert_eq!((img.width(), img.height()), (80, 80));
    }

    #[test]
    fn a_wide_svg_keeps_its_aspect_ratio() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100"/></svg>"##;
        let img = rasterize(svg, None).unwrap();
        assert_eq!((img.width(), img.height()), (800, 400));
    }

    #[test]
    fn a_huge_svg_is_scaled_down() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="1000"><rect width="4000" height="1000"/></svg>"##;
        let img = rasterize(svg, None).unwrap();
        assert_eq!((img.width(), img.height()), (2000, 500));
    }

    #[test]
    fn transparent_pixels_stay_transparent_without_a_bg() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 10 10"><rect width="5" height="10" fill="#ff0000"/></svg>"##;
        let img = rasterize(svg, None).unwrap();
        assert_eq!(pixel(&img, 600, 400)[3], 0);
        let img = rasterize(svg, Some(Background::Transparent)).unwrap();
        assert_eq!(pixel(&img, 600, 400)[3], 0);
    }

    #[test]
    fn bg_fills_what_the_svg_leaves_empty_and_shows_the_svg_on_top() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 10 10"><rect width="5" height="10" fill="#ff0000"/></svg>"##;
        let img = rasterize(svg, Background::parse("#ffffff")).unwrap();
        assert_eq!(pixel(&img, 600, 400), [255, 255, 255, 255]);
        assert_eq!(pixel(&img, 100, 400), [255, 0, 0, 255]);
    }

    #[test]
    fn current_color_follows_the_tui_theme() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 10 10"><rect width="100" height="100" viewBox="0 0 10 10" fill="currentColor"/></svg>"##;
        let img = rasterize(svg, None).unwrap();
        // The dark theme's foreground, not resvg's default black.
        assert_eq!(pixel(&img, 400, 400), [0xf2, 0xed, 0xe6, 255]);
    }

    #[test]
    fn mf_vars_are_substituted_and_fallbacks_survive_unknown_names() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 10 10"><rect width="5" height="10" fill="var(--mf-accent, #000)"/><rect x="5" width="5" height="10" fill="var(--mf-nope, #0000ff)"/></svg>"##;
        let img = rasterize(svg, None).unwrap();
        assert_eq!(pixel(&img, 100, 400), [0xff, 0x6e, 0x15, 255]);
        assert_eq!(pixel(&img, 600, 400), [0, 0, 255, 255]);
    }

    #[test]
    fn a_tools_own_colors_are_left_alone() {
        let img = rasterize(SQUARE, None).unwrap();
        assert_eq!(pixel(&img, 10, 10), [255, 0, 0, 255]);
    }

    #[test]
    fn garbage_is_none() {
        assert!(rasterize("not svg", None).is_none());
        assert!(rasterize("<svg", None).is_none());
        assert!(rasterize("", None).is_none());
    }
}
