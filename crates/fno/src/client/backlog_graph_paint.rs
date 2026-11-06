use crate::backlog_graph::{EdgeKind, Graph, GraphNode, Stub};
use crate::text_style::{char_w, TLine, TSeg, TextStyle};
use std::collections::{BTreeMap, HashMap};

const NODE_STEP: usize = 4;
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
            if role.is_some_and(|current| current != next_role) {
                segs.push(TSeg {
                    text: std::mem::take(&mut text),
                    role: role.expect("a segment has a role"),
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
        band_h[index / cols] = band_h[index / cols].max(layers[layer].len());
    }
    let mut band_y = vec![2usize; bands];
    for band in 1..bands {
        band_y[band] = band_y[band - 1] + band_h[band - 1] * NODE_STEP + 2;
    }
    let height = band_y[bands - 1] + band_h[bands - 1] * NODE_STEP + 1;
    let mut grid = Grid::new(width, height);
    let mut positions = HashMap::new();
    let mut selected_y = None;

    for (index, layer) in layer_ids.iter().enumerate() {
        let band = index / cols;
        let col = index % cols;
        let x = col * col_w + 1;
        for (slot, node) in layers[layer].iter().enumerate() {
            let y = band_y[band] + slot * NODE_STEP;
            let is_selected = selected == Some(node.id.as_str());
            positions.insert(node.id.as_str(), Pos { x, y, w: node_w });
            if is_selected {
                selected_y = Some(y + 1);
            }
        }
    }
    draw_edges(&mut grid, graph, &positions, width);
    for (index, layer) in layer_ids.iter().enumerate() {
        let band = index / cols;
        let col = index % cols;
        let x = col * col_w + 1;
        for (slot, node) in layers[layer].iter().enumerate() {
            let y = band_y[band] + slot * NODE_STEP;
            let role = node_role(node, selected);
            draw_box(&mut grid, x, y, node_w, &node_label(node), role);
        }
    }
    for (index, layer) in layer_ids.iter().enumerate() {
        let band = index / cols;
        let col = index % cols;
        let x = col * col_w;
        let label = format!("layer {}", layer);
        for (offset, ch) in label.chars().enumerate().take(col_w) {
            grid.put(
                x + offset,
                band_y[band].saturating_sub(1),
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
        let mut line = grid.line(y).trunc(width);
        if selected_y == Some(y) {
            line.band = true;
        }
        lines.push(line);
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
    if let Some(card) = &node.card {
        let live = if card.live { "●" } else { " " };
        let lead = card
            .lead
            .as_ref()
            .map(|lead| format!(" @{}", lead.name))
            .unwrap_or_default();
        return format!("{live} {} {}{lead}", card.id, card.title);
    }
    match &node.stub {
        Some(Stub::Done { count }) => format!("✓ {count} done"),
        Some(Stub::More { count }) => format!("+{count} more"),
        Some(Stub::Outside) => format!("↗ {}", node.id.trim_start_matches("outside:")),
        None => node.id.clone(),
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

fn draw_box(grid: &mut Grid, x: usize, y: usize, width: usize, label: &str, role: TextStyle) {
    if width < 3 {
        for (offset, ch) in label.chars().take(width).enumerate() {
            grid.put(x + offset, y + 1, ch, role);
        }
        return;
    }
    grid.put(x, y, '┌', role);
    grid.put(x + width - 1, y, '┐', role);
    grid.horizontal(x + 1, x + width - 2, y, '─', role);
    grid.put(x, y + 1, '│', role);
    grid.put(x + width - 1, y + 1, '│', role);
    grid.put(x, y + 2, '└', role);
    grid.put(x + width - 1, y + 2, '┘', role);
    grid.horizontal(x + 1, x + width - 2, y + 2, '─', role);
    let mut at = x + 1;
    for ch in label.chars() {
        let cells = char_w(ch).max(1);
        if at + cells > x + width - 1 {
            break;
        }
        grid.put(at, y + 1, ch, role);
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
        let sy = from.y + 1;
        let dy = to.y + 1;
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
