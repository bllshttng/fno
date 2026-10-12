use crate::org_model::OrgTree;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::symbols::Marker;
use ratatui_core::widgets::Widget;
use ratatui_widgets::canvas::{Canvas, Line};

const SPACING: f64 = 3.0;

#[derive(Clone)]
pub(crate) struct Dot {
    pub(crate) key: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) kind: DotKind,
    pub(crate) label: String,
    pub(crate) blocked: bool,
    pub(crate) state: Option<crate::lattice::LatticeState>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DotKind {
    Lead,
    Node,
    Worker,
}

#[derive(Clone)]
pub(crate) struct Edge {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) kind: EdgeKind,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum EdgeKind {
    Owns,
    Blocks,
}

#[derive(Clone, Default)]
pub(crate) struct Graph {
    pub(crate) dots: Vec<Dot>,
    pub(crate) edges: Vec<Edge>,
    pub(crate) cluster_labels: Vec<(f64, f64, String)>,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

pub(crate) fn unowned_key(agent: &crate::proto::AgentRow, index: usize) -> String {
    match crate::org_model::agent_key(agent) {
        Some(key) => format!("unowned:{key}:{:?}:{:?}", agent.pane_id, agent.attach_id),
        None => format!("unowned:unknown:{index}"),
    }
}

const TAU: f64 = std::f64::consts::TAU;

fn ring(count: usize) -> f64 {
    if count <= 1 {
        0.0
    } else {
        (SPACING * count as f64 / TAU).max(2.0)
    }
}

pub(crate) fn layout(tree: &OrgTree) -> Graph {
    let mut dots: Vec<Dot> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut cluster_labels: Vec<(f64, f64, String)> = Vec::new();
    let lead_ring = ring(tree.leads.len()).max(4.0);
    let mut max_cluster = 1.5f64;
    for lead in &tree.leads {
        let max_sessions: usize = lead
            .nodes
            .iter()
            .map(|n| n.current.len() + n.former.len())
            .max()
            .unwrap_or(0);
        let workers_r = if max_sessions == 0 { 0.0 } else { 2.0 };
        let node_r = ring(lead.nodes.len());
        // A node never sits on its lead's cell: one worker dot over the
        // lead glyph reads as no lead at all.
        let base_r = node_r.max(2.0);
        let cluster = base_r + workers_r + 1.0;
        if cluster > max_cluster {
            max_cluster = cluster;
        }
    }

    let field = lead_ring + max_cluster + 1.0;
    let w = (field * 2.0).max(8.0);
    let h = (field * 2.0).max(6.0);
    let cx = w / 2.0;
    let cy = h / 2.0;
    let n = tree.leads.len();
    for (i, lead) in tree.leads.iter().enumerate() {
        let angle = -std::f64::consts::FRAC_PI_2 + i as f64 * TAU / n as f64;
        let lx = cx + lead_ring * angle.cos();
        let ly = cy + lead_ring * angle.sin();
        dots.push(Dot {
            key: format!("lead:{}", lead.holder.name),
            x: lx,
            y: ly,
            kind: DotKind::Lead,
            label: lead.holder.name.clone(),
            blocked: false,
            state: None,
        });
        let node_r = ring(lead.nodes.len());
        // Matches the sizing pre-pass: a node on its lead's cell would let
        // the node glyph paint over the lead.
        let base_r = node_r.max(2.0);
        let m = lead.nodes.len();
        for (j, node) in lead.nodes.iter().enumerate() {
            let na = -std::f64::consts::FRAC_PI_2 + j as f64 * TAU / m as f64;
            let nx = lx + base_r * na.cos();
            let ny = ly + base_r * na.sin();
            dots.push(Dot {
                key: format!("node:{}", node.view.card.id),
                x: nx,
                y: ny,
                kind: DotKind::Node,
                label: node.view.card.id.clone(),
                blocked: !node.view.blocked_by.is_empty(),
                state: None,
            });
            edges.push(Edge {
                from: format!("lead:{}", lead.holder.name),
                to: format!("node:{}", node.view.card.id),
                kind: EdgeKind::Owns,
            });
            for blocked in &node.view.blocked_by {
                edges.push(Edge {
                    from: format!("node:{}", node.view.card.id),
                    to: format!("node:{}", blocked.id),
                    kind: EdgeKind::Blocks,
                });
            }

            let workers: Vec<&crate::org_model::OrgSession> =
                node.current.iter().chain(&node.former).collect();
            // Workers chain outward from their node, away from the lead.
            // A radial ring re-aims along the node-lead axis, and a worker
            // landing on the lead's cell erased the lead glyph.
            let axis = (nx - lx, ny - ly);
            let span = (axis.0 * axis.0 + axis.1 * axis.1).sqrt();
            let unit = if span > f64::EPSILON {
                (axis.0 / span, axis.1 / span)
            } else {
                (0.0, -1.0)
            };
            for (k, session) in workers.iter().enumerate() {
                let off = 2.0 + k as f64 * 2.0;
                let wx = nx + unit.0 * off;
                let wy = ny + unit.1 * off;
                let name = session
                    .agent
                    .as_ref()
                    .map(|a| a.name.clone())
                    .or_else(|| session.view.session_id.clone())
                    .unwrap_or_else(|| "unobserved".to_string());
                dots.push(Dot {
                    key: format!("session:{}:{}", node.view.card.id, k),
                    x: wx,
                    y: wy,
                    kind: DotKind::Worker,
                    label: name,
                    blocked: false,
                    state: session
                        .agent
                        .as_ref()
                        .map(|a| super::agent_lattice_state(a)),
                });
                edges.push(Edge {
                    from: format!("node:{}", node.view.card.id),
                    to: format!("session:{}:{}", node.view.card.id, k),
                    kind: EdgeKind::Owns,
                });
            }
        }
    }

    if !tree.unowned.is_empty() {
        let ux = 1.5;
        let uy = h - 1.5;
        let wr = if tree.unowned.len() <= 1 { 0.0 } else { 1.5 };
        let ucount = tree.unowned.len();
        for (k, agent) in tree.unowned.iter().enumerate() {
            let ua = -std::f64::consts::FRAC_PI_2 + k as f64 * TAU / ucount as f64;
            dots.push(Dot {
                key: unowned_key(agent, k),
                x: ux + wr * ua.cos(),
                y: uy + wr * ua.sin(),
                kind: DotKind::Worker,
                label: agent.name.clone(),
                blocked: false,
                state: Some(super::agent_lattice_state(agent)),
            });
        }
        cluster_labels.push((ux, uy - 1.5, "unowned".to_string()));
    }
    Graph {
        dots,
        edges,
        cluster_labels,
        w,
        h,
    }
}

fn projection(
    graph: &Graph,
    width: usize,
    height: usize,
    pan: (usize, usize),
) -> (f64, usize, usize) {
    let fit = if graph.w > 0.0 && graph.h > 0.0 {
        (width as f64 / graph.w).min(height as f64 / graph.h)
    } else {
        1.0
    };
    let scale = fit.max(1.0);
    let max_px = ((graph.w * scale) - width as f64).ceil().max(0.0) as usize;
    let max_py = ((graph.h * scale) - height as f64).ceil().max(0.0) as usize;
    (scale, pan.0.min(max_px), pan.1.min(max_py))
}

pub(crate) fn dot_cells(
    graph: &Graph,
    width: usize,
    height: usize,
    pan: (usize, usize),
) -> Vec<(usize, usize)> {
    let (scale, px, py) = projection(graph, width, height, pan);
    graph
        .dots
        .iter()
        .map(|d| {
            let cx = (d.x * scale) as usize;
            let cy = (d.y * scale) as usize;
            (cx.saturating_sub(px), cy.saturating_sub(py))
        })
        .collect()
}

/// Where each dot's label paints, mirroring [`frame`]'s painter: try right
/// of the dot, then left; a label that would overlap a dot or a painted
/// label is skipped (`None`), and the selected dot's label always paints.
pub(crate) fn label_cells(
    graph: &Graph,
    width: usize,
    height: usize,
    pan: (usize, usize),
    selected: Option<&str>,
) -> Vec<Option<(usize, usize, usize)>> {
    let cells_pos = dot_cells(graph, width, height, pan);
    let mut taken = vec![false; width * height];
    for (cx, cy) in &cells_pos {
        if *cx < width && *cy < height {
            taken[cy * width + cx] = true;
        }
    }
    let mut out = vec![None; graph.dots.len()];
    let mut order: Vec<usize> = (0..graph.dots.len()).collect();
    order.sort_by_key(|i| selected == Some(graph.dots[*i].key.as_str()));
    for i in order {
        let label_len = graph.dots[i].label.chars().count();
        let (cx, cy) = cells_pos[i];
        if cx >= width || cy >= height {
            continue;
        }
        let fits = |start: usize| -> bool {
            start + label_len <= width && !(start..start + label_len).any(|x| taken[cy * width + x])
        };
        let is_selected = selected == Some(graph.dots[i].key.as_str());
        let spot = if fits(cx + 2) {
            Some(cx + 2)
        } else if cx >= label_len + 1 && fits(cx - label_len - 1) {
            Some(cx - label_len - 1)
        } else if is_selected {
            Some(cx + 2)
        } else {
            None
        };
        if let Some(start) = spot {
            let start = start.min(width.saturating_sub(label_len));
            out[i] = Some((start, cy, label_len));
            for x in start..(start + label_len).min(width) {
                taken[cy * width + x] = true;
            }
        }
    }
    out
}

/// The colors the frame paints with, held in the proto space the theme
/// speaks and converted at the cell.
pub(crate) struct GraphColors {
    pub(crate) brand: crate::proto::Color,
    pub(crate) needs_you: crate::proto::Color,
    pub(crate) dim: crate::proto::Color,
}

impl GraphColors {
    pub(crate) fn of(theme: &crate::theme::Theme) -> Self {
        Self {
            brand: theme.brand,
            needs_you: theme.needs_you,
            dim: theme.dim,
        }
    }

    fn dot_color(&self, dot: &Dot) -> crate::proto::Color {
        match dot.kind {
            DotKind::Lead => self.brand,
            DotKind::Node => {
                if dot.blocked {
                    self.needs_you
                } else {
                    self.brand
                }
            }
            DotKind::Worker => match dot.state {
                Some(s) => crate::lattice::lattice_style(s, self.needs_you).fg,
                None => self.dim,
            },
        }
    }
}

/// The rendered graph: braille lines between connected dots in a
/// standalone Buffer, then dot glyphs over the lines, then labels beside
/// them. Deterministic in the layout, the projection and the paint order.
pub(crate) fn frame(
    graph: &Graph,
    width: usize,
    height: usize,
    pan: (usize, usize),
    selected: Option<&str>,
    colors: &GraphColors,
) -> Buffer {
    let area = Rect::new(0, 0, width as u16, height as u16);
    let mut buf = Buffer::empty(area);
    if width == 0 || height == 0 || graph.dots.is_empty() {
        return buf;
    }
    let (scale, px, py) = projection(graph, width, height, pan);
    let sx = scale * 2.0;
    let sy = scale * 4.0;
    let h_sub = height as f64 * 4.0;
    let to_sub = |x: f64, y: f64| -> (f64, f64) {
        let sub_x = x * sx - px as f64 * 2.0;
        let sub_y = h_sub - (y * sy - py as f64 * 4.0);
        (sub_x, sub_y)
    };
    let owns = crate::ratatui_blit::rt_color(crate::proto::Color::Default);
    let cluster_fg = crate::ratatui_blit::rt_color(colors.dim);
    let mut taken = vec![false; width * height];
    for (x, y, label) in &graph.cluster_labels {
        let cx = ((*x * scale) as usize).saturating_sub(px);
        let cy = ((*y * scale) as usize).saturating_sub(py);
        if cy < height && cx < width {
            for (off, ch) in label.chars().enumerate() {
                if cx + off < width {
                    buf[((cx + off) as u16, cy as u16)]
                        .set_char(ch)
                        .set_fg(cluster_fg);
                    taken[cy * width + cx + off] = true;
                }
            }
        }
    }
    let warn = crate::ratatui_blit::rt_color(colors.needs_you);
    let accent = crate::ratatui_blit::rt_color(colors.brand);

    let cells_pos = dot_cells(graph, width, height, pan);
    let canvas = Canvas::default()
        .x_bounds([0.0, width as f64 * 2.0])
        .y_bounds([0.0, h_sub])
        .marker(Marker::Braille)
        .paint(|ctx| {
            for edge in &graph.edges {
                let from = graph.dots.iter().find(|d| d.key == edge.from);
                let to = graph.dots.iter().find(|d| d.key == edge.to);
                if let (Some(a), Some(b)) = (from, to) {
                    let accent_edge = selected.is_some_and(|s| s == edge.from.as_str())
                        || selected.is_some_and(|s| s == edge.to.as_str());
                    let color = if accent_edge {
                        accent
                    } else if edge.kind == EdgeKind::Blocks {
                        warn
                    } else {
                        owns
                    };
                    let (x1, y1) = to_sub(a.x + 0.5, a.y + 0.5);
                    let (x2, y2) = to_sub(b.x + 0.5, b.y + 0.5);
                    ctx.draw(&Line::new(x1, y1, x2, y2, color));
                }
            }
        });
    Widget::render(&canvas, area, &mut buf);

    for (cx, cy) in &cells_pos {
        if *cx < width && *cy < height {
            taken[cy * width + cx] = true;
        }
    }
    // Dots come leads-first, so a cell two dots share keeps the senior
    // glyph; a vanished lead reads as no lead at all.
    let mut held = vec![false; width * height];
    for (dot, (cx, cy)) in graph.dots.iter().zip(cells_pos.iter().copied()) {
        if cx >= width || cy >= height || held[cy * width + cx] {
            continue;
        }
        let glyph = match dot.kind {
            DotKind::Lead => '\u{25c6}',
            DotKind::Node => '\u{25cf}',
            DotKind::Worker => '\u{2022}',
        };
        let fg = crate::ratatui_blit::rt_color(colors.dot_color(dot));
        buf[(cx as u16, cy as u16)].set_char(glyph).set_fg(fg);
        held[cy * width + cx] = true;
    }

    let labels = label_cells(graph, width, height, pan, selected);
    for (dot, label) in graph.dots.iter().zip(labels.iter()) {
        let Some((lx, ly, len)) = *label else {
            continue;
        };
        let is_sel = selected.is_some_and(|s| s == dot.key.as_str());

        let fg = crate::ratatui_blit::rt_color(if is_sel { colors.brand } else { colors.dim });
        for (off, ch) in dot.label.chars().enumerate() {
            if lx + off < width {
                buf[((lx + off) as u16, ly as u16)].set_char(ch).set_fg(fg);
            }
        }
        let _ = len;
    }
    buf
}

pub(crate) fn hit_test(
    graph: &Graph,
    width: usize,
    height: usize,
    pan: (usize, usize),
    selected: Option<&str>,
    x: usize,
    y: usize,
) -> Option<String> {
    let cells_pos = dot_cells(graph, width, height, pan);
    for (dot, (cx, cy)) in graph.dots.iter().zip(&cells_pos) {
        if *cx == x && *cy == y {
            return Some(dot.key.clone());
        }
    }
    let labels = label_cells(graph, width, height, pan, selected);
    for (dot, label) in graph.dots.iter().zip(labels.iter()) {
        if let Some((lx, ly, len)) = *label {
            if ly == y && x >= lx && x < lx + len {
                return Some(dot.key.clone());
            }
        }
    }
    None
}
