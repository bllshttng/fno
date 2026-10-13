use crate::org_model::OrgTree;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;

/// One box in the layered flowchart: a lead, a node or a worker drawn as
/// box-drawing lines with its label inside and its status as the line color.
#[derive(Clone)]
pub(crate) struct GBox {
    pub(crate) key: String,
    pub(crate) kind: BoxKind,
    pub(crate) label: String,
    pub(crate) blocked: bool,
    pub(crate) state: Option<crate::lattice::LatticeState>,
    /// Top-left corner in the flowchart grid.
    pub(crate) col: usize,
    pub(crate) row: usize,
    pub(crate) w: usize,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum BoxKind {
    Lead,
    Node,
    Worker,
}

/// A parent-to-child connector: `│` under one child, a `┴`/`┬` distribution
/// bar under several.
#[derive(Clone)]
pub(crate) struct Link {
    pub(crate) from: String,
    pub(crate) to: String,
}

/// A blocked-by relation between two node boxes, drawn as a horizontal run
/// in the warning color on the layer's label line.
#[derive(Clone)]
pub(crate) struct BlocksEdge {
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Clone, Default)]
pub(crate) struct Graph {
    pub(crate) boxes: Vec<GBox>,
    pub(crate) links: Vec<Link>,
    pub(crate) blocks: Vec<BlocksEdge>,
}

pub(crate) fn unowned_key(agent: &crate::proto::AgentRow, index: usize) -> String {
    match crate::org_model::agent_key(agent) {
        Some(key) => format!("unowned:{key}:{:?}:{:?}", agent.pane_id, agent.attach_id),
        None => format!("unowned:unknown:{index}"),
    }
}

const GAP: usize = 3;
/// Box lines per entity, plus the one connector row between layers.
const BOX_H: usize = 3;
const LAYER_STRIDE: usize = 4;
const LEAD_ROW: usize = 0;
const NODE_ROW: usize = LEAD_ROW + LAYER_STRIDE;
const WORKER_ROW: usize = NODE_ROW + LAYER_STRIDE;
const UNOWNED_ROW: usize = WORKER_ROW + LAYER_STRIDE + 1;
const LABEL_CAP: usize = 28;

fn box_w(label: &str) -> usize {
    label.chars().count().min(LABEL_CAP) + 4
}

/// The label as it paints inside the box: capped, with an ellipsis when the
/// cap cut something.
fn inside_label(label: &str) -> String {
    let count = label.chars().count();
    if count <= LABEL_CAP {
        return label.to_string();
    }
    let mut s: String = label.chars().take(LABEL_CAP - 1).collect();
    s.push('…');
    s
}

struct Layer {
    key: String,
    label: String,
    blocked: bool,
    state: Option<crate::lattice::LatticeState>,
    w: usize,
    span: usize,
    workers_span: usize,
}

/// Layered placement. Spans compute bottom-up (a node's span covers its
/// workers, a lead's span covers its nodes), then each layer places
/// left-to-right with every parent centered over its own span.
pub(crate) fn layout(tree: &OrgTree) -> Graph {
    let mut boxes: Vec<GBox> = Vec::new();
    let mut links: Vec<Link> = Vec::new();
    let mut blocks: Vec<BlocksEdge> = Vec::new();
    let mut cx = 0usize;
    let mut max_w = 0usize;

    for lead in &tree.leads {
        let lead_w = box_w(&lead.holder.name);
        let lead_key = format!("lead:{}", lead.holder.name);
        let mut node_layers: Vec<Layer> = Vec::new();
        let mut lead_span = lead_w;
        for node in &lead.nodes {
            let node_key = format!("node:{}", node.view.card.id);
            let mut worker_layers: Vec<Layer> = Vec::new();
            let mut workers_span = 0usize;
            let workers: Vec<&crate::org_model::OrgSession> =
                node.current.iter().chain(&node.former).collect();
            for (k, session) in workers.iter().enumerate() {
                let name = session
                    .agent
                    .as_ref()
                    .map(|a| a.name.clone())
                    .or_else(|| session.view.session_id.clone())
                    .unwrap_or_else(|| "unobserved".to_string());
                let w = box_w(&name);
                worker_layers.push(Layer {
                    key: format!("session:{}:{}", node.view.card.id, k),
                    label: name,
                    blocked: false,
                    state: session
                        .agent
                        .as_ref()
                        .map(|a| super::agent_lattice_state(a)),
                    w,
                    span: w,
                    workers_span: w,
                });
                workers_span += w;
            }
            if worker_layers.len() > 1 {
                workers_span += GAP * (worker_layers.len() - 1);
            }
            let node_w = box_w(&node.view.card.id);
            let node_span = node_w.max(workers_span);
            for blocked in &node.view.blocked_by {
                blocks.push(BlocksEdge {
                    from: node_key.clone(),
                    to: format!("node:{}", blocked.id),
                });
            }
            node_layers.push(Layer {
                key: node_key,
                label: node.view.card.id.clone(),
                blocked: !node.view.blocked_by.is_empty(),
                state: None,
                w: node_w,
                span: node_span,
                workers_span,
            });
            links.push(Link {
                from: lead_key.clone(),
                to: format!("node:{}", node.view.card.id),
            });
            lead_span += node_span;
        }
        if node_layers.len() > 1 {
            lead_span += GAP * (node_layers.len() - 1);
        }
        // Place this lead's whole subtree at cx.
        let lead_x = cx + (lead_span - lead_w) / 2;
        boxes.push(GBox {
            key: lead_key.clone(),
            kind: BoxKind::Lead,
            label: lead.holder.name.clone(),
            blocked: false,
            state: None,
            col: lead_x,
            row: LEAD_ROW,
            w: lead_w,
        });
        let mut ncx = cx;
        for node in &node_layers {
            let node_x = ncx + (node.span - node.w) / 2;
            boxes.push(GBox {
                key: node.key.clone(),
                kind: BoxKind::Node,
                label: node.label.clone(),
                blocked: node.blocked,
                state: None,
                col: node_x,
                row: NODE_ROW,
                w: node.w,
            });
            let mut wcx = ncx + node.span.saturating_sub(node.workers_span) / 2;
            for worker in &worker_layers {
                boxes.push(GBox {
                    key: worker.key.clone(),
                    kind: BoxKind::Worker,
                    label: worker.label.clone(),
                    blocked: false,
                    state: worker.state,
                    col: wcx,
                    row: WORKER_ROW,
                    w: worker.w,
                });
                links.push(Link {
                    from: node.key.clone(),
                    to: worker.key.clone(),
                });
                wcx += worker.w + GAP;
            }
            ncx += node.span + GAP;
        }
        cx += lead_span + GAP;
        max_w = max_w.max(cx.saturating_sub(GAP));
    }

    if !tree.unowned.is_empty() {
        let mut ucx = 0usize;
        for (i, agent) in tree.unowned.iter().enumerate() {
            let name = agent.name.clone();
            let w = box_w(&name);
            boxes.push(GBox {
                key: unowned_key(agent, i),
                kind: BoxKind::Worker,
                label: name,
                blocked: false,
                state: Some(super::agent_lattice_state(agent)),
                col: ucx,
                row: UNOWNED_ROW,
                w,
            });
            ucx += w + GAP;
        }
        max_w = max_w.max(ucx.saturating_sub(GAP));
    }

    let _ = max_w;
    Graph {
        boxes,
        links,
        blocks,
    }
}

/// The rendered flowchart: box-drawing boxes on their layers, `│`/`┴┬`
/// connector rows between them, blocked-by runs between node boxes, every
/// line wearing its status color. Fits the view width; content past the
/// width is cut (the chart is wider than any one pane only for fleets this
/// pane cannot hold).
pub(crate) fn frame(
    graph: &Graph,
    width: usize,
    height: usize,
    selected: Option<&str>,
    colors: &GraphColors,
) -> Buffer {
    let area = Rect::new(0, 0, width as u16, height as u16);
    let mut buf = Buffer::empty(area);
    if width == 0 || height == 0 {
        return buf;
    }
    let dim = crate::ratatui_blit::rt_color(colors.dim);
    let warn = crate::ratatui_blit::rt_color(colors.needs_you);
    for b in &graph.boxes {
        if b.row >= height {
            continue;
        }
        let color = colors.box_color(b);
        let accent = selected == Some(b.key.as_str());
        let accent_color = crate::ratatui_blit::rt_color(colors.needs_you);
        draw_box(
            &mut buf,
            width,
            b.col,
            b.row,
            b.w,
            &inside_label(&b.label),
            color,
            if accent { Some(accent_color) } else { None },
        );
    }
    // Connectors: one row between layers. A single child hangs straight
    // down; several children spread a bar with a drop to each child.
    for link in &graph.links {
        let Some(parent) = graph.boxes.iter().find(|b| b.key == link.from) else {
            continue;
        };
        let Some(child) = graph.boxes.iter().find(|b| b.key == link.to) else {
            continue;
        };
        let row = child.row - 1;
        if row == 0 || row >= height {
            continue;
        }
        let siblings: Vec<&GBox> = graph
            .links
            .iter()
            .filter(|l| l.from == link.from)
            .filter_map(|l| graph.boxes.iter().find(|b| b.key == l.to))
            .collect();
        let pc = parent.col + parent.w / 2;
        if siblings.len() <= 1 {
            put(&mut buf, width, pc, row, "│", dim);
            continue;
        }
        let first = siblings.iter().map(|b| b.col + b.w / 2).min().unwrap_or(0);
        let last = siblings.iter().map(|b| b.col + b.w / 2).max().unwrap_or(0);
        for c in first..=last {
            let ch = if c == first {
                "┌"
            } else if c == last {
                "┐"
            } else {
                "─"
            };
            put(&mut buf, width, c, row, ch, dim);
        }
        if pc > first && pc < last {
            put(&mut buf, width, pc, row, "┴", dim);
        }
        for s in &siblings {
            let cc = s.col + s.w / 2;
            let ch = if cc == first || cc == last {
                "┌"
            } else {
                "┬"
            };
            put(&mut buf, width, cc, row, ch, dim);
        }
    }
    // Blocked-by runs between node boxes on the label line, arrow into the
    // blocker.
    for e in &graph.blocks {
        let Some(a) = graph.boxes.iter().find(|b| b.key == e.from) else {
            continue;
        };
        let Some(b) = graph.boxes.iter().find(|b| b.key == e.to) else {
            continue;
        };
        if a.row != b.row || a.row + 1 >= height {
            continue;
        }
        let row = a.row + 1;
        let (left, right) = if a.col < b.col { (a, b) } else { (b, a) };
        let start = left.col + left.w;
        let end = right.col;
        if end <= start + 1 {
            continue;
        }
        for c in start..end - 1 {
            put(&mut buf, width, c, row, "─", warn);
        }
        put(&mut buf, width, end - 1, row, "▶", warn);
    }
    if graph.boxes.iter().any(|b| b.row == UNOWNED_ROW) && UNOWNED_ROW + 1 < height {
        put_str(&mut buf, width, 0, UNOWNED_ROW - 1, "unowned", dim);
    }
    buf
}

fn draw_box(
    buf: &mut Buffer,
    width: usize,
    col: usize,
    row: usize,
    w: usize,
    label: &str,
    color: ratatui_core::style::Color,
    accent: Option<ratatui_core::style::Color>,
) {
    let border = accent.unwrap_or(color);
    let top: String = format!("┌{}┐", "─".repeat(w.saturating_sub(2)));
    let bottom: String = format!("└{}┘", "─".repeat(w.saturating_sub(2)));
    let pad = w.saturating_sub(2 + label.chars().count());
    let left_pad = pad / 2;
    let mid = format!(
        "│{}{}{}│",
        " ".repeat(left_pad),
        label,
        " ".repeat(pad - left_pad)
    );
    for (i, s) in [top, mid, bottom].iter().enumerate() {
        put_str(buf, width, col, row + i, s, border);
    }
}

fn put(
    buf: &mut Buffer,
    width: usize,
    x: usize,
    y: usize,
    ch: &str,
    color: ratatui_core::style::Color,
) {
    if x >= width || y >= buf.area.height as usize {
        return;
    }
    buf[(x as u16, y as u16)]
        .set_char(ch.chars().next().unwrap_or(' '))
        .set_fg(color);
}

fn put_str(
    buf: &mut Buffer,
    width: usize,
    x: usize,
    y: usize,
    s: &str,
    color: ratatui_core::style::Color,
) {
    for (i, ch) in s.chars().enumerate() {
        put(buf, width, x + i, y, &ch.to_string(), color);
    }
}

/// Where a tap lands: the key of the box containing the point, if any.
pub(crate) fn hit_test(
    graph: &Graph,
    width: usize,
    height: usize,
    x: usize,
    y: usize,
) -> Option<String> {
    let _ = (width, height);
    graph
        .boxes
        .iter()
        .find(|b| x >= b.col && x < b.col + b.w && y >= b.row && y < b.row + BOX_H)
        .map(|b| b.key.clone())
}
/// The colors the flowchart paints with, held in the proto space the theme
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

    fn box_color(&self, b: &GBox) -> ratatui_core::style::Color {
        let proto = match b.kind {
            BoxKind::Lead => self.brand,
            BoxKind::Node => {
                if b.blocked {
                    self.needs_you
                } else {
                    self.brand
                }
            }
            BoxKind::Worker => match b.state {
                Some(s) => crate::lattice::lattice_style(s, self.needs_you).fg,
                None => self.dim,
            },
        };
        crate::ratatui_blit::rt_color(proto)
    }
}
