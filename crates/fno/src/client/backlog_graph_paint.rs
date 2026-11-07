use crate::backlog_graph::{EdgeKind, Graph, GraphNode, Stub};
use crate::text_style::{char_w, TLine, TSeg, TextStyle};
use std::collections::{BTreeMap, HashMap};

const NODE_STEP: usize = 6;
const MIN_COL_W: usize = 14;
const MAX_NODE_W: usize = 30;

struct Grid {
    cells: Vec<Vec<Option<(char, TextStyle)>>>,
}

impl Grid {
    fn new(width: usize, height: usize) -> Self {
        Self {
            cells: vec![vec![None; width]; height],
        }
    }

    fn put(&mut self, x: usize, y: usize, ch: char, role: TextStyle) {
        if let Some(cell) = self.cells.get_mut(y).and_then(|row| row.get_mut(x)) {
            *cell = Some((ch, role));
        }
    }

    fn horizontal(&mut self, x0: usize, x1: usize, y: usize, ch: char, role: TextStyle) {
        for x in x0.min(x1)..=x0.max(x1) {
            self.put(x, y, ch, role);
        }
    }

    fn vertical(&mut self, x: usize, y0: usize, y1: usize, ch: char, role: TextStyle) {
        for y in y0.min(y1)..=y0.max(y1) {
            self.put(x, y, ch, role);
        }
    }

    fn line(&self, y: usize) -> TLine {
        let Some(row) = self.cells.get(y) else {
            return TLine::plain("");
        };
        let mut segs = Vec::new();
        let mut text = String::new();
        let mut role = None;
        for cell in row {
            let (ch, next_role) = cell.unwrap_or((' ', TextStyle::Meta));
            if let Some(current) = role.filter(|current| *current != next_role) {
                segs.push(TSeg {
                    text: std::mem::take(&mut text),
                    role: current,
                });
            }
            role = Some(next_role);
            text.push(ch);
        }
        if let Some(role) = role {
            segs.push(TSeg { text, role });
        }
        TLine::of(&segs).trim_end()
    }
}

#[derive(Clone, Copy)]
struct Pos {
    x: usize,
    y: usize,
    w: usize,
}

pub(crate) fn lines(
    graph: &Graph,
    selected: Option<&str>,
    width: usize,
) -> (Vec<TLine>, Option<usize>) {
    let width = width.max(1);
    let mut layers: BTreeMap<u16, Vec<&GraphNode>> = BTreeMap::new();
    for node in &graph.nodes {
        layers.entry(node.layer).or_default().push(node);
    }
    for nodes in layers.values_mut() {
        nodes.sort_by_key(|node| node.slot);
    }
    let layer_ids: Vec<u16> = layers.keys().copied().collect();
    if layer_ids.is_empty() {
        return (vec![TLine::meta("no graph nodes")], None);
    }

    let cols = if width < 66 {
        1
    } else {
        (width / MIN_COL_W).max(1).min(layer_ids.len())
    };
    let col_w = (width / cols).max(1);
    let node_w = col_w.saturating_sub(2).min(MAX_NODE_W).max(1);
    let bands = layer_ids.len().div_ceil(cols);
    let mut band_h = vec![1usize; bands];
    for (index, layer) in layer_ids.iter().enumerate() {
        let height = layers.get(layer).map(Vec::len).unwrap_or(0);
        if let Some(slot) = band_h.get_mut(index / cols) {
            *slot = (*slot).max(height);
        }
    }
    let mut band_y = vec![2usize; bands];
    for band in 1..bands {
        let prev = band_y
            .get(band - 1)
            .zip(band_h.get(band - 1))
            .map(|(&y, &h)| y + h * NODE_STEP + 2)
            .unwrap_or(4);
        if let Some(slot) = band_y.get_mut(band) {
            *slot = prev;
        }
    }
    let last = bands
        .checked_sub(1)
        .and_then(|band| band_y.get(band).zip(band_h.get(band)))
        .map(|(&y, &h)| y + h * NODE_STEP + 1)
        .unwrap_or(1);
    let height = last;
    let mut grid = Grid::new(width, height);
    let mut positions = HashMap::new();
    let mut selected_y = None;

    for (index, layer) in layer_ids.iter().enumerate() {
        let band = index / cols;
        let col = index % cols;
        let x = col * col_w + 1;
        let band_top = band_y.get(band).copied().unwrap_or(2);
        let layer_nodes = layers.get(layer).map(Vec::as_slice).unwrap_or(&[]);
        for (slot, node) in layer_nodes.iter().enumerate() {
            let y = band_top + slot * NODE_STEP;
            let is_selected = selected == Some(node.id.as_str());
            positions.insert(node.id.as_str(), Pos { x, y, w: node_w });
            if is_selected {
                selected_y = Some(y + 2);
            }
        }
    }
    draw_edges(&mut grid, graph, &positions, width);
    for (index, layer) in layer_ids.iter().enumerate() {
        let band = index / cols;
        let col = index % cols;
        let x = col * col_w + 1;
        let band_top = band_y.get(band).copied().unwrap_or(2);
        let layer_nodes = layers.get(layer).map(Vec::as_slice).unwrap_or(&[]);
        for (slot, node) in layer_nodes.iter().enumerate() {
            let y = band_top + slot * NODE_STEP;
            let role = node_role(node, selected);
            // The cursor rides the border, in the one inverse role no other
            // board surface uses, so the focused box reads at a glance and
            // no lead color can impersonate it.
            let border_role = if selected == Some(node.id.as_str()) {
                TextStyle::Tab
            } else {
                role
            };
            let labels = node_lines(node, node_w);
            draw_box(&mut grid, x, y, node_w, &labels, border_role, role);
        }
    }
    for (index, layer) in layer_ids.iter().enumerate() {
        let band = index / cols;
        let col = index % cols;
        let x = col * col_w;
        let label = format!("layer {}", layer);
        let band_top = band_y.get(band).copied().unwrap_or(2);
        for (offset, ch) in label.chars().enumerate().take(col_w) {
            grid.put(
                x + offset,
                band_top.saturating_sub(1),
                ch,
                TextStyle::Section,
            );
        }
    }

    let mut lines = vec![TLine::of(&[
        TSeg {
            text: format!("family {}  ", graph.root),
            role: TextStyle::Section,
        },
        TSeg {
            text: format!("focus {}  ", graph.focus),
            role: TextStyle::Badge,
        },
        TSeg {
            text: "parent ─  contained ┄  blocked →  related ·".into(),
            role: TextStyle::Meta,
        },
    ])
    .trunc(width)];
    let body_start = lines.len();
    for y in 0..height {
        lines.push(grid.line(y).trunc(width));
    }
    for unavailable in &graph.unavailable {
        lines.push(TLine::meta(format!(
            "unavailable: {} ({})",
            unavailable.feature, unavailable.reason
        )));
    }
    let follow = selected_y.map(|y| body_start + y);
    (lines, follow)
}

pub(crate) fn action_id(node: &GraphNode) -> Option<String> {
    node.card.as_ref().map(|card| card.id.clone()).or_else(|| {
        matches!(&node.stub, Some(Stub::Outside))
            .then(|| node.id.strip_prefix("outside:").map(str::to_string))
            .flatten()
    })
}

fn node_label(node: &GraphNode) -> String {
    match &node.stub {
        Some(Stub::Done { count }) => format!("✓ {count} done"),
        Some(Stub::More { count }) => format!("+{count} more"),
        Some(Stub::Outside) => format!("↗ {}", node.id.trim_start_matches("outside:")),
        None => node.id.clone(),
    }
}

fn node_lines(node: &GraphNode, box_width: usize) -> [String; 3] {
    let content_width = box_width.saturating_sub(2).max(1);
    let Some(card) = &node.card else {
        return [
            truncate_cells(&node_label(node), content_width),
            String::new(),
            String::new(),
        ];
    };

    let live = if card.live { "● " } else { "" };
    let id = truncate_cells(&format!("{live}{}", card.id), content_width);
    let (title, rest) = take_cells(&card.title, content_width);
    let (second_title, rest) = take_cells(&rest, content_width);
    let second_title = if rest.is_empty() {
        second_title
    } else {
        truncate_cells(&format!("{second_title}…"), content_width)
    };
    let lead = card
        .lead
        .as_ref()
        .map(|lead| truncate_cells(&format!("@{}", lead.name), content_width))
        .unwrap_or_default();
    let third = if second_title.is_empty() {
        lead
    } else {
        second_title
    };
    [id, title, third]
}

fn take_cells(value: &str, max_cells: usize) -> (String, String) {
    let value = value.trim_start();
    let mut end = 0;
    let mut cells = 0;
    for (index, ch) in value.char_indices() {
        let width = char_w(ch).max(1);
        if cells + width > max_cells {
            break;
        }
        cells += width;
        end = index + ch.len_utf8();
    }
    if end == value.len() {
        return (value.trim_end().to_string(), String::new());
    }
    if end == 0 {
        end = value.chars().next().map(char::len_utf8).unwrap_or(0);
    }
    let split = value[..end]
        .rfind(char::is_whitespace)
        .filter(|split| *split > 0)
        .unwrap_or(end);
    (
        value[..split].trim_end().to_string(),
        value[split..].trim_start().to_string(),
    )
}

fn truncate_cells(value: &str, max_cells: usize) -> String {
    let (prefix, rest) = take_cells(value, max_cells);
    if rest.is_empty() {
        prefix
    } else if max_cells <= char_w('…').max(1) {
        prefix
    } else {
        let ellipsis = '…';
        let prefix = take_cells(value, max_cells.saturating_sub(char_w(ellipsis).max(1))).0;
        format!("{prefix}{ellipsis}")
    }
}

fn node_role(node: &GraphNode, selected: Option<&str>) -> TextStyle {
    if selected == Some(node.id.as_str()) {
        return TextStyle::Title;
    }
    let Some(lead) = node.card.as_ref().and_then(|card| card.lead.as_ref()) else {
        return TextStyle::Item;
    };
    let hash = lead.name.bytes().fold(0_u32, |value, byte| {
        value.wrapping_mul(31).wrapping_add(byte as u32)
    });
    match hash % 4 {
        0 => TextStyle::Section,
        1 => TextStyle::Badge,
        2 => TextStyle::OK,
        _ => TextStyle::ERROR,
    }
}

fn draw_box(
    grid: &mut Grid,
    x: usize,
    y: usize,
    width: usize,
    labels: &[String; 3],
    border_role: TextStyle,
    text_role: TextStyle,
) {
    if width < 3 {
        let role = if border_role == TextStyle::Tab {
            border_role
        } else {
            text_role
        };
        for (row, label) in labels.iter().enumerate() {
            for (offset, ch) in label.chars().take(width).enumerate() {
                grid.put(x + offset, y + row + 1, ch, role);
            }
        }
        return;
    }
    let bottom = y + 4;
    grid.put(x, y, '┌', border_role);
    grid.put(x + width - 1, y, '┐', border_role);
    grid.horizontal(x + 1, x + width - 2, y, '─', border_role);
    for (row, label) in labels.iter().enumerate() {
        let y = y + row + 1;
        grid.put(x, y, '│', border_role);
        grid.put(x + width - 1, y, '│', border_role);
        draw_text(grid, x + 1, y, x + width - 1, label, text_role);
    }
    grid.put(x, bottom, '└', border_role);
    grid.put(x + width - 1, bottom, '┘', border_role);
    grid.horizontal(x + 1, x + width - 2, bottom, '─', border_role);
}

fn draw_text(grid: &mut Grid, x: usize, y: usize, end: usize, text: &str, role: TextStyle) {
    let mut at = x;
    for ch in text.chars() {
        let cells = char_w(ch).max(1);
        if at + cells > end {
            break;
        }
        grid.put(at, y, ch, role);
        at += cells;
    }
}

fn draw_edges(grid: &mut Grid, graph: &Graph, positions: &HashMap<&str, Pos>, width: usize) {
    for edge in &graph.edges {
        let (Some(from), Some(to)) = (
            positions.get(edge.from.as_str()),
            positions.get(edge.to.as_str()),
        ) else {
            continue;
        };
        let role = match edge.kind {
            EdgeKind::Parent => TextStyle::Item,
            EdgeKind::Contained => TextStyle::Meta,
            EdgeKind::BlockedBy => TextStyle::ERROR,
            EdgeKind::Related => TextStyle::Badge,
        };
        let sy = from.y + 2;
        let dy = to.y + 2;
        let sx = from.x + from.w;
        let dx = to.x.saturating_sub(1);
        let route = if sx < dx { (sx + dx) / 2 } else { 0 }.min(width.saturating_sub(1));
        let horizontal = match edge.kind {
            EdgeKind::Parent => '─',
            EdgeKind::Contained => '┄',
            EdgeKind::BlockedBy => '━',
            EdgeKind::Related => '·',
        };
        grid.horizontal(sx.min(width - 1), route, sy, horizontal, role);
        grid.vertical(route, sy, dy, '│', role);
        grid.horizontal(route, dx.min(width - 1), dy, horizontal, role);
        let end = dx.min(width.saturating_sub(1));
        grid.put(
            end,
            dy,
            if edge.kind == EdgeKind::BlockedBy {
                '→'
            } else {
                '┤'
            },
            role,
        );
    }
}
