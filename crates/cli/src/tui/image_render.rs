//! Cached visible fragments for iTerm2 and pixel-accurate Sixel clipping.
use image::DynamicImage;
use ratatui::layout::Size;
use ratatui_image::{protocol::Protocol, sliced::SlicedProtocol};
use std::{cell::RefCell, collections::VecDeque, sync::Arc};

const CACHE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 64;
const SIXEL_CACHE_ENTRIES: usize = 128;

/// Restore withdrawn overlays only when ratatui would otherwise leave the
/// bitmap anchor unchanged. Moving or cropping an image already reissues it.
#[derive(Default)]
pub(super) struct ImageRedrawState {
    previous: Option<ImageCoverage>,
}

struct ImageCoverage {
    area: ratatui::layout::Rect,
    skipped: Vec<bool>,
    anchors: std::collections::HashMap<usize, u64>,
}

impl ImageRedrawState {
    fn update(&mut self, buffer: &ratatui::buffer::Buffer) -> bool {
        use ratatui::buffer::CellDiffOption;
        use std::hash::{Hash, Hasher};
        let previous = self.previous.as_ref().filter(|p| p.area == buffer.area);
        let mut coverage = ImageCoverage {
            area: buffer.area,
            skipped: Vec::with_capacity(buffer.content.len()),
            anchors: std::collections::HashMap::new(),
        };
        let mut unchanged_anchor = false;
        let mut restore = false;
        for (index, cell) in buffer.content.iter().enumerate() {
            let skipped = cell.diff_option == CellDiffOption::Skip;
            coverage.skipped.push(skipped);
            if matches!(cell.diff_option, CellDiffOption::ForcedWidth(_)) {
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                cell.hash(&mut hash);
                let hash = hash.finish();
                unchanged_anchor = previous
                    .and_then(|p| p.anchors.get(&index))
                    .is_some_and(|&old| old == hash);
                coverage.anchors.insert(index, hash);
            } else if skipped && unchanged_anchor {
                restore |= previous.is_some_and(|p| !p.skipped[index]);
            }
        }
        self.previous = Some(coverage);
        restore
    }

    pub fn draw<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        mut render: impl FnMut(&mut ratatui::Frame),
    ) -> Result<(), B::Error> {
        let frame = terminal.draw(&mut render)?;
        let area = frame.area;
        if self.update(frame.buffer) {
            // resize to the same geometry clears both the physical viewport
            // and the diff baseline, without querying the terminal cursor.
            terminal.resize(area)?;
            terminal.draw(render)?;
        }
        Ok(())
    }
}

pub(super) struct DocumentImage {
    size: Size,
    pub sliced: Option<SlicedProtocol>,
    /// What a horizontally scrolled sliced image is re-encoded from: the
    /// library's sliced widget can only skip rows, never columns.
    window_source: Option<WindowSource>,
    fragment: Option<FragmentImage>,
}

const SLICED_WINDOWS: usize = 4;

struct WindowSource {
    pixels: DynamicImage,
    picker: ratatui_image::picker::Picker,
    cell_width: u32,
    /// Most recent first, keyed by (first column, columns).
    windows: RefCell<VecDeque<((u16, u16), Arc<SlicedProtocol>)>>,
}
struct FragmentImage {
    pixels: DynamicImage,
    cell_width: u32,
    cell_height: u32,
    is_tmux: bool,
    sixel: bool,
    cache: RefCell<VecDeque<((u16, u16, u16, u16), Arc<Protocol>)>>,
}
impl DocumentImage {
    pub fn fragment(
        size: Size,
        pixels: DynamicImage,
        cell_width: u32,
        cell_height: u32,
        is_tmux: bool,
        sixel: bool,
    ) -> Self {
        Self {
            size,
            sliced: None,
            window_source: None,
            fragment: Some(FragmentImage {
                pixels,
                cell_width,
                cell_height,
                is_tmux,
                sixel,
                cache: RefCell::new(VecDeque::new()),
            }),
        }
    }
    /// A natively sliced (Kitty/Halfblocks) image; `pixels` are what it was
    /// encoded from, kept to re-encode the window of a sideways scroll.
    pub fn sliced(
        sliced: SlicedProtocol,
        pixels: DynamicImage,
        picker: ratatui_image::picker::Picker,
        cell_width: u32,
    ) -> Self {
        Self {
            size: sliced.size(),
            sliced: Some(sliced),
            window_source: Some(WindowSource {
                pixels,
                picker,
                cell_width,
                windows: RefCell::new(VecDeque::new()),
            }),
            fragment: None,
        }
    }

    /// The `width`-column window of a sliced image starting at `skip_col`, or
    /// `None` when the whole image is shown (or this is not a sliced image).
    pub fn sliced_window(&self, skip_col: u16, width: u16) -> Option<Arc<SlicedProtocol>> {
        let source = self.window_source.as_ref()?;
        let width = width.min(self.size.width.saturating_sub(skip_col));
        if width == 0 || (skip_col == 0 && width >= self.size.width) {
            return None;
        }
        let key = (skip_col, width);
        let mut windows = source.windows.borrow_mut();
        if let Some(index) = windows.iter().position(|(k, _)| *k == key) {
            let hit = windows.remove(index)?;
            let window = hit.1.clone();
            windows.push_front(hit);
            return Some(window);
        }
        let x = u32::from(skip_col) * source.cell_width;
        let cropped = source.pixels.crop_imm(
            x,
            0,
            (u32::from(width) * source.cell_width).min(source.pixels.width().checked_sub(x)?),
            source.pixels.height(),
        );
        let window = Arc::new(
            SlicedProtocol::new_with_resize(
                &source.picker,
                cropped,
                Size::new(width, self.size.height),
                ratatui_image::Resize::Fit(None),
            )
            .ok()?,
        );
        windows.truncate(SLICED_WINDOWS - 1);
        windows.push_front((key, window.clone()));
        Some(window)
    }
    pub fn size(&self) -> Size {
        self.size
    }

    /// Whether the image is wider than the `avail` columns it is shown in, so
    /// it scrolls sideways and gets a scrollbar row under it.
    pub fn overflows(&self, avail: u16) -> bool {
        self.size.width > avail
    }

    /// Document rows the image takes up in `avail` columns: its own height
    /// plus one row for the horizontal scrollbar when it overflows.
    pub fn rows_in(&self, avail: u16) -> u16 {
        self.size
            .height
            .saturating_add(u16::from(self.overflows(avail)))
    }

    /// The furthest a `avail`-column window can be scrolled right.
    pub fn max_hscroll(&self, avail: u16) -> u16 {
        self.size.width.saturating_sub(avail)
    }

    /// Cache keys are the source row/column and the visible rows/columns;
    /// pane resize/file changes replace the whole DocumentImage, so stale
    /// pixels cannot survive them.
    #[cfg(test)]
    pub fn visible_fragment(&self, skip: u16, height: u16) -> Option<Arc<Protocol>> {
        self.visible_fragment_at(skip, height, 0, self.size().width)
    }

    /// The `width`×`height`-cell window of this image whose top-left cell is
    /// (`skip_col`, `skip`) — what a horizontally scrolled image shows.
    pub fn visible_fragment_at(
        &self,
        skip: u16,
        height: u16,
        skip_col: u16,
        width: u16,
    ) -> Option<Arc<Protocol>> {
        self.fragment_at(skip, height, skip_col, width, true)
    }

    /// Like `visible_fragment_at`, but only ever answers from the cache: `None`
    /// on a miss instead of encoding. A frame drawn mid-scroll uses it so
    /// that a clipped image costs nothing until the wheel stops.
    pub fn cached_fragment_at(
        &self,
        skip: u16,
        height: u16,
        skip_col: u16,
        width: u16,
    ) -> Option<Arc<Protocol>> {
        self.fragment_at(skip, height, skip_col, width, false)
    }

    /// Whether this image is drawn from cached bitmap fragments (iTerm2 and
    /// Sixel), the only kind whose per-scroll-step cost can be warmed.
    pub fn is_fragmented(&self) -> bool {
        self.fragment.is_some()
    }

    /// Encode the window now (when not cached yet) without drawing it.
    /// Returns whether it actually had to encode.
    pub fn warm_fragment(&self, skip: u16, height: u16, skip_col: u16, width: u16) -> bool {
        let Some(source) = self.fragment.as_ref() else {
            return false;
        };
        let height = height.min(self.size().height.saturating_sub(skip));
        let width = width.min(self.size().width.saturating_sub(skip_col));
        if height == 0 || width == 0 {
            return false;
        }
        let key = (skip, height, skip_col, width);
        if source.cache.borrow().iter().any(|(k, _)| *k == key) {
            return false;
        }
        self.fragment_at(skip, height, skip_col, width, true)
            .is_some()
    }

    fn fragment_at(
        &self,
        skip: u16,
        height: u16,
        skip_col: u16,
        width: u16,
        encode: bool,
    ) -> Option<Arc<Protocol>> {
        let source = self.fragment.as_ref()?;
        let height = height.min(self.size().height.saturating_sub(skip));
        let width = width.min(self.size().width.saturating_sub(skip_col));
        if height == 0 || width == 0 {
            return None;
        }
        let key = (skip, height, skip_col, width);
        let mut cache = source.cache.borrow_mut();
        if let Some(index) = cache.iter().position(|(cached, _)| *cached == key) {
            let hit = cache.remove(index)?;
            let protocol = hit.1.clone();
            cache.push_front(hit);
            return Some(protocol);
        }
        if !encode {
            return None;
        }
        let x = u32::from(skip_col) * source.cell_width;
        let y = u32::from(skip) * source.cell_height;
        let pixels = source.pixels.crop_imm(
            x,
            y,
            (u32::from(width) * source.cell_width).min(source.pixels.width().checked_sub(x)?),
            (u32::from(height) * source.cell_height).min(source.pixels.height().checked_sub(y)?),
        );
        // Sixel bands carry raster dimensions, palette and six-pixel row state.
        // Cropping their strings leaves the full opaque raster behind and cannot
        // represent cell boundaries not divisible by six. Encode the exact
        // cropped pixels, then reuse the resulting sequence on repeat visits.
        let size = Size::new(width, height);
        let encoded = if source.sixel {
            Protocol::Sixel(
                ratatui_image::protocol::sixel::Sixel::new(pixels, size, source.is_tmux).ok()?,
            )
        } else {
            super::app::image_cell_dimensions(Protocol::ITerm2(
                ratatui_image::protocol::iterm2::Iterm2::new(pixels, size, source.is_tmux).ok()?,
            ))
        };
        let protocol = Arc::new(encoded);
        let bytes = protocol_bytes(&protocol);
        if bytes <= CACHE_BYTES {
            let limit = if source.sixel {
                SIXEL_CACHE_ENTRIES
            } else {
                CACHE_ENTRIES
            };
            while cache.len() >= limit
                || cache.iter().map(|(_, p)| protocol_bytes(p)).sum::<usize>() + bytes > CACHE_BYTES
            {
                cache.pop_back();
            }
            cache.push_front((key, protocol.clone()));
        }
        Some(protocol)
    }
}
fn protocol_bytes(protocol: &Protocol) -> usize {
    match protocol {
        Protocol::ITerm2(image) => image.data.len(),
        Protocol::Sixel(image) => image.data.len(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_is_reissued_when_a_popup_shrinks_or_closes() {
        use ratatui::{backend::TestBackend, layout::Rect, widgets::Clear, Terminal};
        let image = image(12);
        let mut terminal = Terminal::new(TestBackend::new(8, 14)).unwrap();
        let mut redraw = ImageRedrawState::default();
        let mut draws = 0;
        // The image anchor stays outside the popup. Filtering shrinks it,
        // exposing cells whose image data has not changed.
        for (popup, expected_draws) in [
            (Some(Rect::new(1, 2, 5, 9)), 1),
            (Some(Rect::new(1, 2, 5, 9)), 2),
            (Some(Rect::new(1, 5, 5, 3)), 4),
            (None, 6),
            (None, 7),
        ] {
            redraw
                .draw(&mut terminal, |frame| {
                    draws += 1;
                    super::super::ui::render_document_image(
                        &image,
                        0,
                        Rect::new(0, 1, 2, 12),
                        frame.buffer_mut(),
                    );
                    if let Some(popup) = popup {
                        frame.render_widget(Clear, popup);
                        frame.render_widget(ratatui::widgets::Paragraph::new("choice"), popup);
                    }
                })
                .unwrap();
            assert_eq!(draws, expected_draws);
        }
    }

    #[test]
    fn scrolling_bitmaps_never_requests_an_extra_draw() {
        use ratatui::{backend::TestBackend, layout::Rect, Terminal};
        for sixel in [false, true] {
            let mut image = image(12);
            image.fragment.as_mut().unwrap().sixel = sixel;
            let mut terminal = Terminal::new(TestBackend::new(8, 14)).unwrap();
            let mut redraw = ImageRedrawState::default();
            let mut draws = 0;
            // Moving down, clipping at the bottom, then cropping at the top;
            // all transitions expose Skip cells, but the anchor already changes.
            for (step, (y, skip)) in [(1, 0), (2, 0), (3, 0), (1, 1), (1, 2), (1, 1), (1, 0)]
                .into_iter()
                .enumerate()
            {
                redraw
                    .draw(&mut terminal, |frame| {
                        draws += 1;
                        super::super::ui::render_document_image(
                            &image,
                            skip,
                            Rect::new(0, y, 2, 14 - y),
                            frame.buffer_mut(),
                        );
                    })
                    .unwrap();
                assert_eq!(draws, step + 1, "sixel={sixel}, y={y}, skip={skip}");
            }
        }
    }
    fn image(rows: u16) -> DocumentImage {
        let pixels = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(
            16,
            u32::from(rows) * 8,
            |_, y| {
                if y < u32::from(rows) * 4 {
                    image::Rgba([255, 0, 0, 255])
                } else {
                    image::Rgba([0, 0, 255, 255])
                }
            },
        ));
        DocumentImage::fragment(Size::new(2, rows), pixels, 8, 8, false, false)
    }
    #[test]
    fn cached_only_lookup_never_encodes_and_warming_fills_the_cache() {
        let image = image(6);
        assert!(image.cached_fragment_at(2, 3, 0, 2).is_none());
        assert!(image.warm_fragment(2, 3, 0, 2), "first warm encodes");
        assert!(!image.warm_fragment(2, 3, 0, 2), "second warm is a hit");
        let hit = image.cached_fragment_at(2, 3, 0, 2).unwrap();
        assert!(Arc::ptr_eq(
            &hit,
            &image.visible_fragment_at(2, 3, 0, 2).unwrap()
        ));
        assert!(image.is_fragmented());
    }

    #[test]
    fn cached_fragment_preserves_visible_pixels_and_cell_placement() {
        use base64::Engine;
        let image = image(4);
        let fragment = image.visible_fragment(1, 2).unwrap();
        assert!(Arc::ptr_eq(
            &fragment,
            &image.visible_fragment(1, 2).unwrap()
        ));
        let Protocol::ITerm2(fragment) = fragment.as_ref() else {
            panic!()
        };
        assert!(fragment.data.contains("width=2;height=2;"));
        let encoded = fragment
            .data
            .split("doNotMoveCursor=1:")
            .nth(1)
            .unwrap()
            .split('\u{7}')
            .next()
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let pixels = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(pixels.dimensions(), (16, 16));
        assert_eq!(pixels.get_pixel(4, 3).0, [255, 0, 0, 255]);
        assert_eq!(pixels.get_pixel(4, 12).0, [0, 0, 255, 255]);
        assert!(image.visible_fragment(4, 1).is_none());
        assert!(image.visible_fragment(0, 0).is_none());
    }
    #[test]
    fn fragment_cache_evicts_old_views_and_obeys_limits() {
        let image = image(128);
        let first = image.visible_fragment(0, 1).unwrap();
        for skip in 1..CACHE_ENTRIES as u16 + 8 {
            image.visible_fragment(skip, 1).unwrap();
        }
        let cache = image.fragment.as_ref().unwrap().cache.borrow();
        assert!(cache.len() <= CACHE_ENTRIES);
        assert!(cache.iter().map(|(_, p)| protocol_bytes(p)).sum::<usize>() <= CACHE_BYTES);
        drop(cache);
        assert!(!Arc::ptr_eq(&first, &image.visible_fragment(0, 1).unwrap()));
    }
    #[test]
    #[allow(deprecated)] // Deliberately test cell heights not aligned to sixel bands.
    fn sixel_fragments_declare_only_the_visible_pixel_height() {
        for cell_height in [7u16, 18, 34] {
            let pixels = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(
                48,
                u32::from(cell_height) * 4,
                |_, y| {
                    if y < u32::from(cell_height) * 2 {
                        image::Rgba([255, 0, 0, 255])
                    } else {
                        image::Rgba([0, 0, 255, 255])
                    }
                },
            ));
            for tmux in [false, true] {
                let image = DocumentImage::fragment(
                    Size::new(4, 4),
                    pixels.clone(),
                    12,
                    u32::from(cell_height),
                    tmux,
                    true,
                );
                for (skip, height) in [(0, 2), (1, 2), (3, 1)] {
                    let fragment = image.visible_fragment(skip, height).unwrap();
                    assert!(Arc::ptr_eq(
                        &fragment,
                        &image.visible_fragment(skip, height).unwrap()
                    ));
                    let Protocol::Sixel(fragment) = fragment.as_ref() else {
                        panic!()
                    };
                    let raster = fragment
                        .data
                        .split('"')
                        .nth(1)
                        .expect("Sixel raster attributes");
                    let raster: Vec<_> = raster
                        .split(|c: char| !c.is_ascii_digit())
                        .take(4)
                        .map(|s| s.parse::<u32>().unwrap())
                        .collect();
                    assert_eq!(
                        raster,
                        vec![1, 1, 48, u32::from(cell_height) * u32::from(height)]
                    );
                    assert_eq!(fragment.size, Size::new(4, height));
                    if let Ok(prefix) = std::env::var("MESHFOX_SIXEL_DIAGNOSTIC_OUT") {
                        if !tmux {
                            std::fs::write(
                                format!("{prefix}.{cell_height}.{skip}.{height}.sixel"),
                                &fragment.data,
                            )
                            .unwrap();
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[allow(deprecated)]
    fn a_horizontal_window_crops_columns_and_is_cached_per_window() {
        // 4 cells wide, 8 px each: the left half red, the right half blue.
        let pixels = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(32, 16, |x, _| {
            if x < 16 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        }));
        let image = DocumentImage::fragment(Size::new(4, 2), pixels, 8, 8, false, false);
        let right = image.visible_fragment_at(0, 2, 2, 2).unwrap();
        assert_eq!(right.size(), Size::new(2, 2));
        assert!(Arc::ptr_eq(
            &right,
            &image.visible_fragment_at(0, 2, 2, 2).unwrap()
        ));
        let left = image.visible_fragment_at(0, 2, 0, 2).unwrap();
        assert!(!Arc::ptr_eq(&left, &right));
        // The window never extends past the image, and an empty one is None.
        assert_eq!(
            image.visible_fragment_at(0, 2, 3, 9).unwrap().size(),
            Size::new(1, 2)
        );
        assert!(image.visible_fragment_at(0, 2, 4, 2).is_none());
    }
}
