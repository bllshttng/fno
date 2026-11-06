//! A bounded family graph over the board's already-filtered cards.
use crate::backlog_model::{Card, Inputs, Unavailable};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

const NODE_CAP: usize = 80;

#[derive(Debug, Clone, Serialize)]
pub struct Graph {
    pub focus: String,
    pub root: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub hidden: GraphHidden,
    pub unavailable: Vec<Unavailable>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub layer: u16,
    pub slot: u16,
    pub stub: Option<Stub>,
    pub card: Option<Card>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stub {
    Done { count: usize },
    More { count: usize },
    Outside,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Parent,
    Contained,
    BlockedBy,
    Related,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GraphHidden {
    pub nodes: usize,
}

/// Build the family rooted at the focus. `cards` already holds the board's
/// scope and filters, so this function cannot introduce a second filter path.
pub fn graph(inp: &Inputs, cards: &[Card], requested_focus: Option<&str>) -> Option<Graph> {
    let row_by_id: HashMap<&str, &Value> = inp
        .rows
        .iter()
        .filter_map(|row| Some((row.get("id")?.as_str()?, row)))
        .collect();
    let card_by_id: HashMap<&str, &Card> =
        cards.iter().map(|card| (card.id.as_str(), card)).collect();
    if card_by_id.is_empty() {
        return None;
    }

    let mut ordered: Vec<&Card> = cards.iter().collect();
    ordered.sort_by_key(|card| card.order);
    let focus = requested_focus
        .filter(|id| card_by_id.contains_key(id))
        .unwrap_or(&ordered[0].id)
        .to_string();

    let tree_parent = |id: &str| -> Option<&str> {
        let row = row_by_id.get(id)?;
        let contained = field_id(row, "contained_in");
        let parent = field_id(row, "parent");
        contained.or(parent)
    };
    let mut root = focus.clone();
    let mut ancestors = HashSet::new();
    while ancestors.insert(root.clone()) {
        let Some(parent) = tree_parent(&root).filter(|id| card_by_id.contains_key(id)) else {
            break;
        };
        root = parent.to_string();
    }

    let mut children: HashMap<&str, Vec<&Card>> = HashMap::new();
    for card in &ordered {
        if let Some(parent) = tree_parent(&card.id).filter(|id| card_by_id.contains_key(id)) {
            children.entry(parent).or_default().push(card);
        }
    }
    for siblings in children.values_mut() {
        siblings.sort_by_key(|card| card.order);
    }

    let mut family_layers: HashMap<&str, u16> = HashMap::new();
    let mut queue = VecDeque::from([(root.as_str(), 0_u16)]);
    while let Some((id, layer)) = queue.pop_front() {
        if family_layers.insert(id, layer).is_some() {
            continue;
        }
        if let Some(descendants) = children.get(id) {
            for child in descendants {
                queue.push_back((child.id.as_str(), layer.saturating_add(1)));
            }
        }
    }

    let mut by_layer: BTreeMap<u16, Vec<GraphNode>> = BTreeMap::new();
    let mut done_counts: HashMap<&str, usize> = HashMap::new();
    let mut done_kinds: HashSet<(String, EdgeKind)> = HashSet::new();
    for card in &ordered {
        let Some(&layer) = family_layers.get(card.id.as_str()) else {
            continue;
        };
        let parent = tree_parent(&card.id);
        if card.column == "Done" && card.id != root {
            if let Some(parent) = parent {
                *done_counts.entry(parent).or_default() += 1;
                let kind = row_by_id
                    .get(card.id.as_str())
                    .map(|row| {
                        if field_id(row, "contained_in") == Some(parent) {
                            EdgeKind::Contained
                        } else {
                            EdgeKind::Parent
                        }
                    })
                    .unwrap_or(EdgeKind::Parent);
                done_kinds.insert((parent.to_string(), kind));
                continue;
            }
        }
        by_layer.entry(layer).or_default().push(GraphNode {
            id: card.id.clone(),
            layer,
            slot: 0,
            stub: None,
            card: Some((*card).clone()),
        });
    }
    for (parent, count) in &done_counts {
        let Some(&parent_layer) = family_layers.get(parent) else {
            continue;
        };
        let layer = parent_layer.saturating_add(1);
        by_layer.entry(layer).or_default().push(GraphNode {
            id: format!("done:{parent}"),
            layer,
            slot: 0,
            stub: Some(Stub::Done { count: *count }),
            card: None,
        });
    }

    let mut edges = Vec::new();
    for card in &ordered {
        if !family_layers.contains_key(card.id.as_str()) {
            continue;
        }
        let Some(row) = row_by_id.get(card.id.as_str()) else {
            continue;
        };
        for (field, kind) in [
            ("parent", EdgeKind::Parent),
            ("contained_in", EdgeKind::Contained),
        ] {
            for parent in field_ids(row, field) {
                if family_layers.contains_key(parent) {
                    edges.push(GraphEdge {
                        from: parent.to_string(),
                        to: card.id.clone(),
                        kind,
                    });
                } else if done_counts.contains_key(parent) {
                    edges.push(GraphEdge {
                        from: parent.to_string(),
                        to: format!("done:{parent}"),
                        kind,
                    });
                }
            }
        }
    }

    let visible_family: HashSet<String> = by_layer
        .values()
        .flat_map(|nodes| {
            nodes
                .iter()
                .filter(|node| node.card.is_some())
                .map(|node| node.id.clone())
        })
        .collect();
    let mut outside_ids: HashMap<String, u16> = HashMap::new();
    for source in ordered
        .iter()
        .filter(|card| visible_family.contains(&card.id))
    {
        let Some(row) = row_by_id.get(source.id.as_str()) else {
            continue;
        };
        for (field, kind) in [
            ("blocked_by", EdgeKind::BlockedBy),
            ("related", EdgeKind::Related),
        ] {
            for target in field_ids(row, field) {
                if family_layers.contains_key(target) {
                    edges.push(GraphEdge {
                        from: source.id.clone(),
                        to: target.to_string(),
                        kind,
                    });
                } else {
                    let id = format!("outside:{target}");
                    let layer = family_layers
                        .get(source.id.as_str())
                        .copied()
                        .unwrap_or_default()
                        .saturating_add(1);
                    outside_ids
                        .entry(id.clone())
                        .and_modify(|existing| *existing = (*existing).min(layer))
                        .or_insert(layer);
                    edges.push(GraphEdge {
                        from: source.id.clone(),
                        to: id,
                        kind,
                    });
                }
            }
        }
    }
    for (id, layer) in outside_ids {
        by_layer.entry(layer).or_default().push(GraphNode {
            id,
            layer,
            slot: 0,
            stub: Some(Stub::Outside),
            card: None,
        });
    }

    for (parent, kind) in done_kinds {
        let target = format!("done:{parent}");
        if !edges
            .iter()
            .any(|edge| edge.to == target && edge.kind == kind)
        {
            edges.push(GraphEdge {
                from: parent,
                to: target,
                kind,
            });
        }
    }

    for nodes in by_layer.values_mut() {
        nodes.sort_by(|left, right| match (&left.card, &right.card) {
            (Some(a), Some(b)) => a.order.cmp(&b.order),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.id.cmp(&right.id),
        });
        for (slot, node) in nodes.iter_mut().enumerate() {
            node.slot = slot.min(u16::MAX as usize) as u16;
        }
    }

    let keys: Vec<u16> = by_layer.keys().copied().collect();
    let mut nodes = Vec::new();
    let mut hidden = GraphHidden::default();
    let overflow_layers = keys.len() > NODE_CAP;
    let retained_layers = if overflow_layers {
        NODE_CAP - 1
    } else {
        keys.len()
    };
    for (index, layer) in keys.iter().take(retained_layers).enumerate() {
        let layer_nodes = by_layer.remove(layer).unwrap_or_default();
        let remaining_layers = retained_layers - index + usize::from(overflow_layers);
        let budget = NODE_CAP.saturating_sub(nodes.len());
        let node_budget = budget.saturating_sub(remaining_layers);
        let keep = layer_nodes.len().min(node_budget);
        nodes.extend(layer_nodes.iter().take(keep).cloned());
        let omitted = layer_nodes.len().saturating_sub(keep);
        if omitted > 0 {
            hidden.nodes += omitted;
            nodes.push(GraphNode {
                id: format!("more:{layer}"),
                layer: *layer,
                slot: keep.min(u16::MAX as usize) as u16,
                stub: Some(Stub::More { count: omitted }),
                card: None,
            });
        }
    }
    if overflow_layers {
        let layer = keys[retained_layers];
        let omitted = keys[retained_layers..]
            .iter()
            .filter_map(|key| by_layer.get(key))
            .map(Vec::len)
            .sum::<usize>();
        hidden.nodes += omitted;
        nodes.push(GraphNode {
            id: format!("more:{layer}"),
            layer,
            slot: 0,
            stub: Some(Stub::More { count: omitted }),
            card: None,
        });
    }
    let visible: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    edges.sort_by(|left, right| {
        (&left.from, &left.to, left.kind).cmp(&(&right.from, &right.to, right.kind))
    });
    let mut edge_set = HashSet::new();
    edges.retain(|edge| {
        visible.contains(edge.from.as_str())
            && visible.contains(edge.to.as_str())
            && edge_set.insert(edge.clone())
    });

    Some(Graph {
        focus,
        root,
        nodes,
        edges,
        hidden,
        unavailable: unavailable_edges(inp),
    })
}

fn field_id<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

fn field_ids<'a>(row: &'a Value, field: &str) -> Vec<&'a str> {
    match row.get(field) {
        Some(Value::String(id)) if !id.is_empty() => vec![id],
        Some(Value::Array(ids)) => ids
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| !id.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn unavailable_edges(inp: &Inputs) -> Vec<Unavailable> {
    if inp.backend == "graph" {
        return Vec::new();
    }
    [
        ("parent edges", "parent"),
        ("contained edges", "contained_in"),
        ("blocked_by edges", "blocked_by"),
        ("related edges", "related"),
    ]
    .into_iter()
    .filter_map(|(feature, field)| {
        let has_field = inp.rows.iter().any(|row| !field_ids(row, field).is_empty());
        (!has_field).then(|| Unavailable {
            feature,
            reason: format!("{} supplies no {field}", inp.backend),
        })
    })
    .collect()
}
