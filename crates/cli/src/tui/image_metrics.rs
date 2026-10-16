//! iTerm2 Sixel uses physical pixels; generic terminal cell metrics use points.
//! https://iterm2.com/documentation-escape-codes.html (Report Cell Size).

use ratatui_image::FontSize;

/// Called only during initial construction, before the input reader starts.
/// Nested canvases inherit the resulting Picker instead of querying again.
#[cfg(unix)]
pub(super) fn query_iterm_cell_pixels() -> Option<FontSize> {
    use std::io::{IsTerminal, Read, Write};
    use std::time::{Duration, Instant};

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    let tmux = std::env::var_os("TMUX").is_some_and(|s| !s.is_empty());
    let (start, escape, end) =
        ratatui_image::picker::cap_parser::Parser::tmux_start_escape_end(tmux);
    // DSR terminates the round trip even when ReportCellSize is unsupported.
    let query = format!("{start}{escape}]1337;ReportCellSize\x07{escape}[5n{end}");
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(query.as_bytes()).ok()?;
    stdout.flush().ok()?;
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut stdin = std::io::stdin().lock();
    let mut response = Vec::new();
    while response.len() < 4096 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !super::stdin_has_input_within(remaining) {
            break;
        }
        let mut chunk = [0u8; 256];
        let read = stdin.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
        if response.windows(4).any(|s| s == b"\x1b[0n") {
            break;
        }
    }
    parse_iterm_cell_pixels(&response)
}

#[cfg(not(unix))]
pub(super) fn query_iterm_cell_pixels() -> Option<FontSize> {
    None
}

fn parse_iterm_cell_pixels(response: &[u8]) -> Option<FontSize> {
    let response = std::str::from_utf8(response).ok()?;
    let (_, fields) = response.split_once("\x1b]1337;ReportCellSize=")?;
    let end = fields.find('\x07').or_else(|| fields.find("\x1b\\"))?;
    let fields: Vec<_> = fields[..end].split(';').collect();
    if !(2..=3).contains(&fields.len()) {
        return None;
    }
    let height: f64 = fields[0].parse().ok()?;
    let width: f64 = fields[1].parse().ok()?;
    let scale: f64 = fields.get(2).unwrap_or(&"1").parse().ok()?;
    if [height, width, scale]
        .iter()
        .any(|n| !n.is_finite() || *n <= 0.0)
    {
        return None;
    }
    let physical = |points: f64| {
        let pixels = (points * scale).round();
        (pixels >= 1.0 && pixels <= u16::MAX as f64).then_some(pixels as u16)
    };
    Some(FontSize::new(physical(width)?, physical(height)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_metrics_use_reported_scale_and_accept_both_terminators() {
        for (reply, width, height) in [
            ("\x1b]1337;ReportCellSize=17.50;8.00;2.0\x07\x1b[0n", 16, 35),
            ("\x1b]1337;ReportCellSize=18;8;1\x1b\\", 8, 18),
            ("\x1b]1337;ReportCellSize=18;8\x07", 8, 18),
            ("\x1b]1337;ReportCellSize=18;8;1.5\x07", 12, 27),
        ] {
            let size = parse_iterm_cell_pixels(reply.as_bytes()).unwrap();
            assert_eq!((size.width, size.height), (width, height));
        }
    }

    #[test]
    fn missing_partial_and_invalid_metrics_do_not_override_detection() {
        for reply in [
            "\x1b[0n",
            "\x1b]1337;ReportCellSize=18;8;2",
            "\x1b]1337;ReportCellSize=18;8;NaN\x07",
            "\x1b]1337;ReportCellSize=18;8;0\x07",
            "\x1b]1337;ReportCellSize=-18;8;2\x07",
            "\x1b]1337;ReportCellSize=18;8;inf\x07",
            "\x1b]1337;ReportCellSize=999999;8;2\x07",
            "\x1b]1337;ReportCellSize=18;8;2;3\x07",
        ] {
            assert!(
                parse_iterm_cell_pixels(reply.as_bytes()).is_none(),
                "{reply:?}"
            );
        }
    }

    #[test]
    #[allow(deprecated)] // Inject terminal replies without a real terminal.
    fn sixel_pixel_raster_matches_the_reserved_cells_on_retina() {
        use ratatui::layout::Size;
        use ratatui_image::{
            picker::{Picker, ProtocolType},
            protocol::Protocol,
            Resize,
        };

        for scale in [1, 2] {
            let reply = format!("\x1b]1337;ReportCellSize=20;10;{scale}\x07");
            let metrics = parse_iterm_cell_pixels(reply.as_bytes()).unwrap();
            let mut picker = Picker::from_fontsize(metrics);
            picker.set_protocol_type(ProtocolType::Sixel);
            let protocol = picker
                .new_protocol(
                    image::DynamicImage::new_rgb8(1000, 1000),
                    Size::new(56, 24),
                    Resize::Fit(None),
                )
                .unwrap();
            let size = protocol.size();
            let Protocol::Sixel(sixel) = protocol else {
                panic!("Sixel expected")
            };
            assert_eq!((size.width, size.height), (48, 24));
            // Sixel raster attributes declare its physical pixel dimensions.
            assert!(sixel
                .data
                .contains(&format!("\"1;1;{};{}", 480 * scale, 480 * scale)));
        }
    }
}
