//! How sideline agent rows order: the active column's comparator, applied to
//! each contiguous agent run and to each lineage level inside one, shared by
//! the card view and the extended table so both read in the sorted order and
//! the painted age is the value sorted on.

use super::*;

/// The persisted manual-order list the Manual column reads. Borrowed per
/// sort pass.
pub(super) struct AgentOrder<'a> {
    pub manual: &'a [String],
}

/// Pin or unpin the agent row under the selector cursor; a non-agent row
/// notices why.
pub(super) fn toggle_pin(view: &mut View, cur: usize) {
    let agent = match view.display_rows().get(cur) {
        Some(DisplayRow::Agent(a)) => (*a).clone(),
        _ => {
            view.set_notice("pin works on an agent row".into());
            return;
        }
    };
    pin_agent(view, &agent);
}

/// The `fno-agents reap` argv that flips a row's pin. The pin IS the keep
/// mark every reaper reads, keyed by session id, so a row with no session
/// yet has nothing to pin.
pub(super) fn pin_argv(a: &AgentRow) -> Result<Vec<String>, String> {
    let sid = a
        .harness_session_id
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{} has no session id to pin yet", a.name))?;
    let flag = if a.pinned { "--unkeep" } else { "--keep" };
    Ok(vec!["reap".into(), flag.into(), sid])
}

/// Flip a row's pin off the UI loop. The sideline shows the new state when
/// the next layout reads the marker; the notice carries the command's own
/// answer, so a refusal is never painted as a pin.
pub(super) fn pin_agent(view: &mut View, a: &AgentRow) {
    let argv = match pin_argv(a) {
        Ok(argv) => argv,
        Err(why) => {
            view.set_notice(why);
            return;
        }
    };
    let word = if a.pinned { "unpinning" } else { "pinning" };
    view.set_notice(format!("{word} {}", a.name));
    if cfg!(test) {
        return;
    }
    let notice_tx = view.reply_notice_tx.clone();
    tokio::spawn(async move {
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new(crate::digest_overlay::fno_agents_bin())
                .args(&argv)
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await;
        let first = |b: &[u8]| {
            String::from_utf8_lossy(b)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        };
        let text = match outcome {
            Ok(Ok(o)) if o.status.success() => first(&o.stdout),
            Ok(Ok(o)) => format!("pin refused: {}", first(&o.stderr)),
            Ok(Err(e)) => format!("pin did not start: {e}"),
            Err(_) => "pin is still running; the row shows it when it lands".into(),
        };
        if let Some(tx) = notice_tx {
            let _ = tx.send(text);
        }
    });
}

#[allow(clippy::type_complexity)]
pub(super) fn append_sorted_agent_group<'a>(
    out: &mut Vec<(DisplayRow<'a>, usize)>,
    group: &mut Vec<(Vec<(DisplayRow<'a>, usize)>, &'a AgentRow)>,
    sort: AgentSort,
    needs: &HashMap<String, NeedKind>,
    now_secs: u64,
    order: &AgentOrder<'_>,
) {
    let mut subtrees: Vec<(
        Vec<(Vec<(DisplayRow<'a>, usize)>, &'a AgentRow)>,
        &'a AgentRow,
    )> = Vec::new();
    for item in group.drain(..) {
        let depth = item.0.first().map(|(_, depth)| *depth).unwrap_or_default();
        if depth == 0 || subtrees.is_empty() {
            let root = item.1;
            subtrees.push((vec![item], root));
        } else {
            subtrees.last_mut().unwrap().0.push(item);
        }
    }
    subtrees.sort_by(|(_, a), (_, b)| {
        compare_agent_rows(
            a,
            b,
            sort,
            needs.get(a.name.as_str()).copied(),
            needs.get(b.name.as_str()).copied(),
            now_secs,
            order,
        )
    });
    for (items, _) in subtrees {
        let items = sort_lineage_level(items, sort, needs, now_secs, order);
        for (rows, _) in items {
            out.extend(rows);
        }
    }
}

/// Sorts one lineage level in place. `items[0]` is the level's root and keeps
/// its place; every later item starts (one level deeper) or continues (deeper
/// still) a child run, runs order by their leading agent, and each run's own
/// children sort the same way. Sublines ride inside their agent's item, so an
/// agent never separates from its detail rows.
fn sort_lineage_level<'a>(
    mut items: Vec<(Vec<(DisplayRow<'a>, usize)>, &'a AgentRow)>,
    sort: AgentSort,
    needs: &HashMap<String, NeedKind>,
    now_secs: u64,
    order: &AgentOrder<'_>,
) -> Vec<(Vec<(DisplayRow<'a>, usize)>, &'a AgentRow)> {
    let Some(root_depth) = items
        .first()
        .and_then(|(rows, _)| rows.first())
        .map(|(_, d)| *d)
    else {
        return items;
    };
    let mut runs: Vec<Vec<_>> = Vec::new();
    for item in items.drain(1..) {
        let depth = item.0.first().map(|(_, d)| *d).unwrap_or(root_depth);
        if depth <= root_depth + 1 || runs.is_empty() {
            runs.push(vec![item]);
        } else {
            runs.last_mut().expect("non-empty run above").push(item);
        }
    }
    runs.sort_by(|a, b| {
        compare_agent_rows(
            a[0].1,
            b[0].1,
            sort,
            needs.get(a[0].1.name.as_str()).copied(),
            needs.get(b[0].1.name.as_str()).copied(),
            now_secs,
            order,
        )
    });
    // The root survived `drain(1..)` at index 0; the sorted child runs
    // append after it, so the level keeps its head and its order.
    items.extend(
        runs.into_iter()
            .flat_map(|run| sort_lineage_level(run, sort, needs, now_secs, order)),
    );
    items
}

fn compare_agent_rows(
    a: &AgentRow,
    b: &AgentRow,
    sort: AgentSort,
    need_a: Option<NeedKind>,
    need_b: Option<NeedKind>,
    now_secs: u64,
    order: &AgentOrder<'_>,
) -> Ordering {
    // A pinned row leads every column.
    let pin = b.pinned.cmp(&a.pinned);
    let order = match sort.column {
        AgentSortColumn::Status => {
            let a_key = attention_key(a, need_a);
            let b_key = attention_key(b, need_b);
            let a_state = if a.exited {
                u8::MAX
            } else {
                pane_state(a.badge, a.seen, a.pane_activity) as u8
            };
            let b_state = if b.exited {
                u8::MAX
            } else {
                pane_state(b.badge, b.seen, b.pane_activity) as u8
            };
            apply_direction(
                a_state
                    .cmp(&b_state)
                    .then_with(|| a_key.0.cmp(&b_key.0))
                    .then_with(|| a_key.1.cmp(&b_key.1))
                    .then_with(|| a_key.2.cmp(&b_key.2)),
                sort.direction,
            )
        }
        AgentSortColumn::Agent => apply_direction(a.name.cmp(&b.name), sort.direction),
        AgentSortColumn::LastMessage => cmp_optional(
            a.tail.as_deref().filter(|value| !value.is_empty()),
            b.tail.as_deref().filter(|value| !value.is_empty()),
            sort.direction,
        ),
        AgentSortColumn::Pr => cmp_optional(a.pr, b.pr, sort.direction),
        AgentSortColumn::Age => {
            cmp_optional(row_age(a, now_secs), row_age(b, now_secs), sort.direction)
        }
        AgentSortColumn::Created => cmp_optional(a.started_at, b.started_at, sort.direction),
        AgentSortColumn::Modified => cmp_optional(a.updated_at, b.updated_at, sort.direction),
        AgentSortColumn::Manual => {
            order_rank(&a.name, order.manual).cmp(&order_rank(&b.name, order.manual))
        }
    };
    pin.then(order)
}

/// A name's seat in a persisted order list, `usize::MAX` when absent - so
/// listed names sort before unlisted ones and the unlisted keep name order
/// stability behind them.
fn order_rank(name: &str, list: &[String]) -> usize {
    list.iter().position(|n| n == name).unwrap_or(usize::MAX)
}

fn cmp_optional<T: Ord>(a: Option<T>, b: Option<T>, direction: SortDirection) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => apply_direction(a.cmp(&b), direction),
    }
}

fn apply_direction(order: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Ascending => order,
        SortDirection::Descending => order.reverse(),
    }
}

fn row_age(a: &AgentRow, now_secs: u64) -> Option<u64> {
    a.last_activity_age_s
        .or_else(|| a.updated_at.map(|updated| now_secs.saturating_sub(updated)))
}
