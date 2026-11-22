//! LaTeX math for the terminal: the shared typesetting lives in `meshfox-math`.
pub use meshfox_math::*;

#[cfg(test)]
mod tests {
    use base64::Engine;

    #[test]
    fn data_url_rasterizes_through_the_shared_svg_path() {
        let url = super::svg_data_url(r"\int_0^1 x\,dx").unwrap();
        let b64 = url.strip_prefix("data:image/svg+xml;base64,").unwrap();
        let svg = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        let img = super::super::svg_raster::rasterize(std::str::from_utf8(&svg).unwrap(), None);
        assert!(img.is_some());
    }
}
