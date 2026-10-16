//! Cached visible fragments for iTerm2 and pixel-accurate Sixel clipping.
use image::DynamicImage;
use ratatui::layout::Size;
use ratatui_image::{protocol::Protocol, sliced::SlicedProtocol};
use std::{cell::RefCell, collections::VecDeque, sync::Arc};

const CACHE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 32;
const SIXEL_CACHE_ENTRIES: usize = 128;

pub(super) struct DocumentImage {
    size: Size,
    pub sliced: Option<SlicedProtocol>,
    fragment: Option<FragmentImage>,
}
struct FragmentImage {
    pixels: DynamicImage,
    cell_height: u32,
    is_tmux: bool,
    sixel: bool,
    cache: RefCell<VecDeque<((u16, u16), Arc<Protocol>)>>,
}
impl DocumentImage {
    pub fn fragment(
        size: Size,
        pixels: DynamicImage,
        cell_height: u32,
        is_tmux: bool,
        sixel: bool,
    ) -> Self {
        Self {
            size,
            sliced: None,
            fragment: Some(FragmentImage {
                pixels,
                cell_height,
                is_tmux,
                sixel,
                cache: RefCell::new(VecDeque::new()),
            }),
        }
    }
    pub fn sliced(sliced: SlicedProtocol) -> Self {
        Self {
            size: sliced.size(),
            sliced: Some(sliced),
            fragment: None,
        }
    }
    pub fn size(&self) -> Size {
        self.size
    }

    /// Cache keys are source row and visible row count; pane resize/file changes
    /// replace the whole DocumentImage, so stale pixels cannot survive them.
    pub fn visible_fragment(&self, skip: u16, height: u16) -> Option<Arc<Protocol>> {
        let source = self.fragment.as_ref()?;
        let height = height.min(self.size().height.saturating_sub(skip));
        if height == 0 {
            return None;
        }
        let key = (skip, height);
        let mut cache = source.cache.borrow_mut();
        if let Some(index) = cache.iter().position(|(cached, _)| *cached == key) {
            let hit = cache.remove(index)?;
            let protocol = hit.1.clone();
            cache.push_front(hit);
            return Some(protocol);
        }
        let y = u32::from(skip) * source.cell_height;
        let pixels = source.pixels.crop_imm(
            0,
            y,
            source.pixels.width(),
            (u32::from(height) * source.cell_height).min(source.pixels.height().checked_sub(y)?),
        );
        // Sixel bands carry raster dimensions, palette and six-pixel row state.
        // Cropping their strings leaves the full opaque raster behind and cannot
        // represent cell boundaries not divisible by six. Encode the exact
        // cropped pixels, then reuse the resulting sequence on repeat visits.
        let size = Size::new(self.size().width, height);
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
        DocumentImage::fragment(Size::new(2, rows), pixels, 8, false, false)
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
        let image = image(64);
        let first = image.visible_fragment(0, 1).unwrap();
        for skip in 1..40 {
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
}
