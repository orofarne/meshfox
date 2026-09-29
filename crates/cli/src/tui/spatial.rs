//! A compact terminal projection of positioned siblings. Authored pixel gaps
//! reserve room for node bodies in the browser; here the bodies live in the
//! Document pane, so only relative columns and rows survive.

use meshfox_core::Canvas;
use ratatui::layout::Rect;

use super::app::App;

#[derive(Clone)]
pub(super) struct MapNode {
    pub id: String,
    pub title: String,
    pub rect: Rect,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Viewport {
    pub first_col: usize,
    pub first_row: usize,
}

pub(super) struct Projection {
    pub visible: Vec<MapNode>,
    pub all: Vec<MapNode>,
    pub viewport: Viewport,
    pub virtual_area: Rect,
    pub offset_x: usize,
    pub offset_y: usize,
}

/// The spatial projection replaces the tree's rows while a parent with
/// positioned direct children (or one of those children) is selected. The
/// parent's type is irrelevant: ordinary text nodes can own spatial graphs.
/// Disclosure still uses the tree; arrow keys and hjkl visit nearby cards.
pub(super) fn active_spatial_parent(app: &App) -> Option<String> {
    let row = app.rows.get(app.selected)?;
    let node = app.display_canvas.node(&row.node_id)?;
    for candidate in [Some(&node.id), node.parent.as_ref()].into_iter().flatten() {
        if !app.expanded.contains(candidate) {
            continue;
        }
        let children = app.display_canvas.children(candidate);
        if children.len() >= 2 && children.iter().all(|n| n.x.is_some() && n.y.is_some()) {
            return Some(candidate.clone());
        }
    }
    None
}

#[derive(Clone, Copy)]
pub(super) enum Direction { Up, Down, Left, Right }

fn compact_grid(children: &[&meshfox_core::Node]) -> (Vec<(usize, usize)>, usize, usize) {
    if children.is_empty() { return (Vec::new(), 0, 0); }
    let min_x = children.iter().filter_map(|n| n.x).fold(f64::INFINITY, f64::min);
    let min_y = children.iter().filter_map(|n| n.y).fold(f64::INFINITY, f64::min);
    let mut widths: Vec<f64> = children.iter().filter_map(|n| n.width).collect();
    widths.sort_by(f64::total_cmp);
    let x_unit = widths.get(widths.len() / 2).copied().unwrap_or(240.0).clamp(80.0, 500.0) + 40.0;
    let mut heights: Vec<f64> = children.iter().filter_map(|n| n.height).collect();
    heights.sort_by(f64::total_cmp);
    let y_unit = heights.get(heights.len() / 2).copied().unwrap_or(100.0).clamp(40.0, 300.0) + 40.0;
    let raw: Vec<(i32, i32)> = children.iter().map(|n| (
        ((n.x.unwrap_or(min_x) - min_x) / x_unit).round() as i32,
        ((n.y.unwrap_or(min_y) - min_y) / y_unit).round() as i32,
    )).collect();
    let mut cols: Vec<i32> = raw.iter().map(|&(x, _)| x).collect();
    cols.sort_unstable();
    cols.dedup();
    let mut rows: Vec<i32> = raw.iter().map(|&(_, y)| y).collect();
    rows.sort_unstable();
    rows.dedup();
    let cells = raw.into_iter().map(|(x, y)| (
        cols.binary_search(&x).expect("projected column exists"),
        rows.binary_search(&y).expect("projected row exists"),
    )).collect();
    (cells, cols.len(), rows.len())
}

/// Navigate by the compact grid seen on screen, not the original pixel
/// coordinates. Horizontal movement stays in the same visible row; vertical
/// movement prefers a column but can reach a staggered row diagonally.
pub(super) fn neighbor(canvas: &Canvas, parent_id: &str, current_id: &str, direction: Direction) -> Option<String> {
    let children = canvas.children(parent_id);
    let (compact, _, _) = compact_grid(&children);
    if current_id == parent_id {
        let reverse = matches!(direction, Direction::Up | Direction::Left);
        return compact.iter().enumerate().min_by(|(_, &(ax, ay)), (_, &(bx, by))| {
            let order = ay.cmp(&by).then_with(|| ax.cmp(&bx));
            if reverse { order.reverse() } else { order }
        }).map(|(i, _)| children[i].id.clone());
    }
    let current_index = children.iter().position(|n| n.id == current_id)?;
    let (cx, cy) = compact[current_index];
    compact.iter().enumerate().filter_map(|(i, &(x, y))| {
        if i == current_index { return None; }
        let (dx, dy) = (x as i32 - cx as i32, y as i32 - cy as i32);
        let (primary, secondary) = match direction {
            Direction::Up => (-dy, dx.abs()),
            Direction::Down => (dy, dx.abs()),
            Direction::Left if dy == 0 => (-dx, 0),
            Direction::Right if dy == 0 => (dx, 0),
            Direction::Left | Direction::Right => return None,
        };
        (primary > 0).then_some((i, primary, secondary))
    }).min_by(|a, b| {
        (a.1 + 2 * a.2).cmp(&(b.1 + 2 * b.2))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.1.cmp(&b.1))
    }).map(|(i, _, _)| children[i].id.clone())
}

pub(super) fn project(canvas: &Canvas, group_id: &str, area: Rect, selected_id: &str, previous: Viewport) -> Projection {
    let children = canvas.children(group_id);
    if children.is_empty() || area.width < 12 || area.height < 4 {
        return Projection { visible: Vec::new(), all: Vec::new(), viewport: previous,
            virtual_area: Rect::new(0, 0, 0, 0), offset_x: 0, offset_y: 0 };
    }
    let (cells, col_count, row_count) = compact_grid(&children);

    // A narrow terminal sees a window over the columns, not microscopic
    // cards. Keep its origin until the selection leaves the visible window.
    let visible_cols = col_count.min((area.width / 24).max(1) as usize);
    let selected_col = children
        .iter()
        .position(|n| n.id == selected_id)
        .map(|i| cells[i].0);
    let mut first_col = previous.first_col.min(col_count.saturating_sub(visible_cols));
    if let Some(selected_col) = selected_col {
        if selected_col < first_col { first_col = selected_col; }
        if selected_col >= first_col + visible_cols { first_col = selected_col + 1 - visible_cols; }
    }
    let slot_width = (area.width / visible_cols as u16).max(1);
    let box_width = slot_width.saturating_sub(1).min(32);
    // Four rows for a filled card, then enough clearance for a stem at
    // both ends of an orthogonal connector and its two visible corners.
    let row_pitch = 8usize;
    let visible_rows = ((area.height as usize - 4) / row_pitch) + 1;
    let selected_row = children
        .iter()
        .position(|n| n.id == selected_id)
        .map(|i| cells[i].1);
    let mut first_row = previous.first_row.min(row_count.saturating_sub(visible_rows));
    if let Some(selected_row) = selected_row {
        if selected_row < first_row { first_row = selected_row; }
        if selected_row >= first_row + visible_rows { first_row = selected_row + 1 - visible_rows; }
    }

    let viewport = Viewport { first_col, first_row };
    let offset_x = first_col * slot_width as usize;
    let offset_y = first_row * row_pitch;
    let virtual_width = col_count * slot_width as usize;
    let virtual_height = row_count * row_pitch + 4;
    // Keep routing bounded for unusually large canvases. The ordinary case
    // uses the entire compact graph; a huge graph falls back to the window.
    let full_map = virtual_width <= u16::MAX as usize
        && virtual_height <= u16::MAX as usize
        && virtual_width.saturating_mul(virtual_height) <= 1_000_000;
    let mut visible = Vec::new();
    let mut all = Vec::new();
    for (node, (col, row)) in children.iter().zip(cells) {
        if full_map {
            let virtual_rect = Rect::new((col * slot_width as usize) as u16, (row * row_pitch) as u16, box_width, 4);
            all.push(MapNode { id: node.id.clone(), title: node.title.clone(), rect: virtual_rect });
        }
        if col >= first_col && col < first_col + visible_cols && row >= first_row && row < first_row + visible_rows {
            let cell_x = area.x + (col - first_col) as u16 * slot_width;
            let rect = Rect::new(cell_x, area.y + ((row - first_row) * row_pitch) as u16, box_width, 4);
            let mapped = MapNode { id: node.id.clone(), title: node.title.clone(), rect };
            if !full_map {
                all.push(MapNode { id: node.id.clone(), title: node.title.clone(),
                    rect: Rect::new(rect.x - area.x, rect.y - area.y, rect.width, rect.height) });
            }
            visible.push(mapped);
        }
    }
    Projection { visible, all, viewport,
        virtual_area: if full_map { Rect::new(0, 0, virtual_width as u16, virtual_height as u16) }
            else { Rect::new(0, 0, area.width, area.height) },
        offset_x: if full_map { offset_x } else { 0 },
        offset_y: if full_map { offset_y } else { 0 } }
}

#[cfg(test)]
pub(super) fn layout(canvas: &Canvas, group_id: &str, area: Rect, selected_id: &str) -> Vec<MapNode> {
    project(canvas, group_id, area, selected_id, Viewport::default()).visible
}

pub(super) fn hit_test(app: &App, area: Rect, col: u16, row: u16) -> Option<String> {
    let group_id = active_spatial_parent(app)?;
    let selected_id = &app.rows.get(app.selected)?.node_id;
    let previous = app.spatial_viewports.get(&group_id).copied().unwrap_or_default();
    project(&app.display_canvas, &group_id, area, selected_id, previous).visible
        .into_iter()
        .find(|n| col >= n.rect.x && col < n.rect.x + n.rect.width && row >= n.rect.y && row < n.rect.y + n.rect.height)
        .map(|n| n.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positioned_rows_compact_body_sized_gaps() {
        let canvas = Canvas::from_markdown(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "## Group\n<!-- meshfox:node id=\"group\" type=\"group\" -->\n",
            "### A\n<!-- meshfox:node id=\"a\" x=0 y=0 w=240 h=112 -->\n",
            "### B\n<!-- meshfox:node id=\"b\" x=280 y=0 w=240 h=112 -->\n",
            "### C\n<!-- meshfox:node id=\"c\" x=0 y=800 w=240 h=112 -->\n",
        )).unwrap();
        let nodes = layout(&canvas, "group", Rect::new(0, 0, 60, 15), "a");
        let a = nodes.iter().find(|n| n.id == "a").unwrap();
        let b = nodes.iter().find(|n| n.id == "b").unwrap();
        let c = nodes.iter().find(|n| n.id == "c").unwrap();
        assert_eq!(a.rect.y, b.rect.y);
        assert_eq!(c.rect.y - a.rect.y, 8);
        assert!(b.rect.x > a.rect.x);
    }

    #[test]
    fn narrow_view_tracks_selected_column() {
        let canvas = Canvas::from_markdown(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "## Group\n<!-- meshfox:node id=\"group\" type=\"group\" -->\n",
            "### A\n<!-- meshfox:node id=\"a\" x=0 y=0 w=240 h=112 -->\n",
            "### B\n<!-- meshfox:node id=\"b\" x=280 y=0 w=240 h=112 -->\n",
            "### C\n<!-- meshfox:node id=\"c\" x=560 y=0 w=240 h=112 -->\n",
            "### D\n<!-- meshfox:node id=\"d\" x=840 y=0 w=240 h=112 -->\n",
        )).unwrap();
        let nodes = layout(&canvas, "group", Rect::new(0, 0, 50, 10), "d");
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().any(|n| n.id == "d"));
        assert!(nodes.iter().all(|n| n.rect.width >= 18));
    }

    #[test]
    fn viewport_stays_put_until_selection_reaches_an_edge() {
        let canvas = Canvas::from_markdown(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "## Group\n<!-- meshfox:node id=\"group\" -->\n",
            "### A\n<!-- meshfox:node id=\"a\" x=0 y=0 w=240 h=100 -->\n",
            "### B\n<!-- meshfox:node id=\"b\" x=0 y=160 w=240 h=100 -->\n",
            "### C\n<!-- meshfox:node id=\"c\" x=0 y=320 w=240 h=100 -->\n",
            "### D\n<!-- meshfox:node id=\"d\" x=0 y=480 w=240 h=100 -->\n",
        )).unwrap();
        let area = Rect::new(0, 0, 40, 19); // two visible rows
        let a = project(&canvas, "group", area, "a", Viewport::default());
        let b = project(&canvas, "group", area, "b", a.viewport);
        assert_eq!(a.viewport.first_row, b.viewport.first_row);
        let c = project(&canvas, "group", area, "c", b.viewport);
        assert_eq!(c.viewport.first_row, 1);
        let back_to_b = project(&canvas, "group", area, "b", c.viewport);
        assert_eq!(back_to_b.viewport.first_row, 1);
    }

    #[test]
    fn readme_neighboring_lower_nodes_keep_the_same_window() {
        let canvas = Canvas::from_markdown(include_str!("../../../../README.md")).unwrap();
        let area = Rect::new(0, 0, 128, 35);
        let first = project(&canvas, "component-diagram", area, "worker-lock", Viewport::default());
        let second = project(&canvas, "component-diagram", area, "web-dist-bundle", first.viewport);
        assert_eq!(first.viewport.first_col, second.viewport.first_col);
        assert_eq!(first.viewport.first_row, second.viewport.first_row);
        assert_eq!(first.all.iter().map(|n| (&n.id, n.rect)).collect::<Vec<_>>(),
            second.all.iter().map(|n| (&n.id, n.rect)).collect::<Vec<_>>());
    }

    #[test]
    fn readme_horizontal_navigation_follows_visible_rows_and_columns() {
        let canvas = Canvas::from_markdown(include_str!("../../../../README.md")).unwrap();
        let next = |id, direction| neighbor(&canvas, "component-diagram", id, direction);
        assert_eq!(next("coordinator-resolve", Direction::Right).as_deref(), Some("worker-lock"));
        assert_eq!(next("worker-lock", Direction::Left).as_deref(), Some("coordinator-resolve"));
        assert_eq!(next("worker-axum-http-server", Direction::Right).as_deref(), Some("web-dist-bundle"));
        assert_eq!(next("web-dist-bundle", Direction::Left).as_deref(), Some("worker-axum-http-server"));
        // Authored x differs by 60px, but these cards occupy one compact
        // column on different rows. Neither is a horizontal neighbor.
        assert_eq!(next("worker-lock", Direction::Right), None);
        assert_eq!(next("web-dist-bundle", Direction::Right), None);
    }
}
