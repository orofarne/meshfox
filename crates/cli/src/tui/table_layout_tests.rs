//! Regression coverage for tables as painted terminal cells, independent of
//! the Markdown parser's internal spans and the column allocation algorithm.
use super::{
    document_layout::LayoutContext,
    markdown::{self, Segment},
};
use ratatui::{
    buffer::Buffer,
    layout::{Rect, Size},
    style::{Color, Modifier},
    widgets::Widget,
};
use std::{collections::HashMap, path::Path};

fn parse_table(md: &str) -> Segment {
    markdown::render(
        md,
        Path::new("/nonexistent"),
        &markdown::Highlighter::new(),
        "n",
        &[],
        &HashMap::new(),
        None,
        &HashMap::new(),
    )
    .0
    .into_iter()
    .find(|s| matches!(s, Segment::Table { .. }))
    .expect("semantic table")
}

fn paint(table: &Segment, width: u16) -> (Buffer, Vec<String>) {
    let layout = LayoutContext::new(Size::new(width, 60))
        .text(table)
        .unwrap();
    let area = Rect::new(0, 0, width, layout.rows() as u16);
    let mut buffer = Buffer::empty(area);
    layout.paragraph().render(area, &mut buffer);
    let rows = (0..area.height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();
    (buffer, rows)
}

#[test]
fn power_mode_table_has_a_stable_terminal_snapshot_after_resize() {
    let table = parse_table("| Mode | Current |\n|:---|---:|\n| Single shot | 0.45 mA |\n| Low power periodic | 3.2 mA |\n");
    let (_, wide_before) = paint(&table, 30);
    let (narrow, rows) = paint(&table, 20);
    assert_eq!(
        rows,
        vec![
            ["Mode      ", "Current"].join(" │ "),
            ["──────────", "───────"].join("─┼─"),
            ["Single    ", "0.45 mA"].join(" │ "),
            ["shot      ", "       "].join(" │ "),
            ["Low power ", " 3.2 mA"].join(" │ "),
            ["periodic  ", "       "].join(" │ "),
        ]
    );
    assert!(narrow[(0, 0)].modifier.contains(Modifier::BOLD));
    assert!(!narrow[(0, 2)].modifier.contains(Modifier::BOLD));
    assert_eq!(
        wide_before,
        paint(&table, 30).1,
        "widening must recover the original layout without reparsing"
    );
}

#[test]
fn scd41_table_keeps_all_four_columns_and_code_color_on_continuation_rows() {
    let table = parse_table("| Режим | Команда | Период | Средний ток |\n|---|---|---|---|\n| Periodic | `0x21B1` | 5 с | 15 / 18 мА |\n| Low power periodic | `0x21AC` | ≈30 с | 3,2 / 3,5 мА |\n| Single shot (SCD41) | `0x219D` | по запросу, 1 раз в 5 мин | 0,45 / 0,5 мА |\n| Single shot + power-down | `0x36E0` / `0x36F6` | по запросу | в даташите не указан: см. Low Power Operation |\n");
    for width in [100, 64, 40] {
        let (buffer, rows) = paint(&table, width);
        let separators: Vec<_> = (0..width)
            .filter(|&x| buffer[(x, 0)].symbol() == "│")
            .collect();
        assert_eq!(separators.len(), 3);
        let body_start = rows.iter().position(|row| row.contains('┼')).unwrap() as u16 + 1;
        for y in body_start..buffer.area.height {
            let actual: Vec<_> = (0..width)
                .filter(|&x| buffer[(x, y)].symbol() == "│")
                .collect();
            assert_eq!(
                actual, separators,
                "columns must stay aligned at width {width}, row {y}: {:?}",
                rows[y as usize]
            );
        }
        // Read each column top to bottom, ignoring cell padding and word spaces:
        // this detects text lost to clipping or moved into an adjacent column.
        let bounds = [
            0,
            separators[0] + 1,
            separators[1] + 1,
            separators[2] + 1,
            width,
        ];
        let columns: Vec<String> = (0..4)
            .map(|col| {
                (body_start..buffer.area.height)
                    .flat_map(|y| {
                        let b = &buffer;
                        (bounds[col]..bounds[col + 1]).map(move |x| b[(x, y)].symbol())
                    })
                    .collect::<String>()
                    .chars()
                    .filter(|c| !c.is_whitespace() && *c != '│')
                    .collect()
            })
            .collect();
        assert!(columns[0].ends_with("Singleshot+power-down"));
        assert_eq!(columns[1], "0x21B10x21AC0x219D0x36E0/0x36F6");
        assert!(columns[2].contains("позапросу,1разв5мин"));
        assert!(columns[3].ends_with("вдаташитенеуказан:см.LowPowerOperation"));
        assert!(
            buffer
                .content
                .iter()
                .any(|c| c.symbol() == "x" && c.fg == Color::Rgb(255, 184, 108)),
            "inline commands keep their color"
        );
    }
}
