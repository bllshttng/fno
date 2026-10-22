//! The placement shapes the spine answers for every arm: the group-tab
//! selector split (`_resolve_group_tab`, mux_spawn.py:1486), the byte-equal
//! group-tab refusal (`dispatch_spawn_pane`, mux_spawn.py:3455), the
//! act-then-place post-run move (`place_pane_in_group_tab`, :1586) and the
//! existing-pane attach checks (`resolve_existing_pane`/:98,
//! `start_existing_pane`/:111).
//!
//! Ordering note: a group `--tab` puts NO tab on the `pane run` transport
//! (the pane lands in its own tab and joins after), while a selector tab
//! rides the transport like Python's `placement_args`.

use super::PaneRefusal;
use super::{MuxCli, EXIT_REFUSAL};
use crate::claude_ask::py_repr;
use serde_json::Value;

/// `_resolve_group_tab`: an explicit selector (active/new, `id:`/`name:`/
/// `ordinal:`/a bare integer) rides the placement path untouched; a bare
/// name is a GROUP, placed after the spawn rather than before it.
pub fn resolve_group_tab(requested: &str) -> (Option<String>, Option<String>) {
    let selector = requested == "active"
        || requested == "new"
        || requested.starts_with("id:")
        || requested.starts_with("name:")
        || requested.starts_with("ordinal:")
        || requested.chars().all(|c| c.is_ascii_digit());
    if selector {
        (Some(requested.to_string()), None)
    } else {
        (None, Some(requested.to_string()))
    }
}

/// The group-tab + split/at combination refusal (AC3), byte-equal to the
/// Python leg's DispatchAskError line.
pub fn group_tab_conflict_line(tab: &str) -> String {
    format!(
        "--tab {} is a pane GROUP, which is placed by moving the pane's own tab. \
         It cannot combine with --split/--at, which put the pane in a tab it does not \
         own; the move would take that tab's other panes with it. Pass an explicit \
         --tab active|new|id:<n>|name:<s> to place into a specific tab, or drop \
         --split/--at to use the group.",
        py_repr(tab)
    )
}

/// The refusal carrier for a pre-spawn placement problem.
pub fn placement_refusal(line: String) -> PaneRefusal {
    PaneRefusal {
        line,
        exit_code: EXIT_REFUSAL,
        after_pane: None,
    }
}

/// `resolve_existing_pane` (mux_spawn.py:98): the four checks a `--pane`
/// target must pass before the worker start text is sent.
pub fn resolve_existing_pane(
    session: &str,
    pane_id: u64,
    rows: &[Value],
) -> Result<(), PaneRefusal> {
    if pane_id < 1 {
        return Err(placement_refusal(format!(
            "--pane needs a positive pane id, got {pane_id}"
        )));
    }
    let Some(row) = rows
        .iter()
        .find(|row| row.get("pane_id").and_then(Value::as_u64) == Some(pane_id))
    else {
        return Err(placement_refusal(format!(
            "--pane {pane_id} was not found in mux session {}",
            py_repr(session)
        )));
    };
    if row.get("fno_id").is_some_and(|v| !v.is_null()) {
        return Err(placement_refusal(format!(
            "--pane {pane_id} is occupied by worker {}",
            py_repr(row.get("fno_id").and_then(Value::as_str).unwrap_or("?"))
        )));
    }
    if row.get("pristine_idle_shell").and_then(Value::as_bool) != Some(true) {
        return Err(placement_refusal(format!(
            "--pane {pane_id} is not a confirmed pristine idle shell"
        )));
    }
    Ok(())
}

/// `place_pane_in_group_tab` (mux_spawn.py:1586): act, then place. The pane
/// names its own squad and tab; the tab read is scoped to that squad; the
/// join derives its squad from the anchor pane. A failure is cosmetic by
/// the Python leg's own account (the seed already rode in and the worker
/// may be live), so the caller prints a named warning and keeps the pane -
/// this function never reaps.
pub fn place_pane_in_group_tab(
    session: &str,
    pane_id: u64,
    group: &str,
    max_panes: u64,
    mux: &dyn MuxCli,
) -> Result<(), String> {
    let (rows, _detail) = super::pane_ls_detailed(session, mux);
    let rows = rows.unwrap_or_default();
    let own = rows
        .iter()
        .find(|row| row.get("pane_id").and_then(Value::as_u64) == Some(pane_id));
    let Some(own) = own else {
        return Err(format!(
            "pane group {group:?}: pane {pane_id} was created but reports no resolvable squad/tab id"
        ));
    };
    let squad_id = own.get("squad_id").and_then(Value::as_u64);
    let own_tab = own
        .get("tab_id")
        .and_then(Value::as_str)
        .and_then(|t| t.strip_prefix("id:"))
        .and_then(|t| t.parse::<u64>().ok())
        .or_else(|| own.get("tab_id").and_then(Value::as_u64));
    let (Some(squad_id), Some(own_tab)) = (squad_id, own_tab) else {
        return Err(format!(
            "pane group {group:?}: pane {pane_id} was created but reports no resolvable squad/tab id"
        ));
    };
    let listed = mux.run(
        &[
            "mux".into(),
            "tab".into(),
            "ls".into(),
            "--server".into(),
            session.into(),
            "--workspace".into(),
            format!("id:{squad_id}"),
            "--json".into(),
        ],
        &[],
    );
    let Some(listed) = listed.ok() else {
        return Err(format!(
            "cannot resolve pane group {group:?}: mux tab ls could not run"
        ));
    };
    if !listed.status.success() {
        return Err(format!(
            "cannot resolve pane group {group:?}: mux tab ls failed: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        ));
    }
    let parsed: Vec<Value> = serde_json::from_slice(&listed.stdout).unwrap_or_default();
    let by_name: Vec<&Value> = parsed
        .iter()
        .filter(|row| row.get("name").and_then(Value::as_str) == Some(group))
        .collect();
    for suffix in 1.. {
        let name = if suffix == 1 {
            group.to_string()
        } else {
            format!("{group}-{suffix}")
        };
        let row = by_name
            .iter()
            .find(|row| row.get("name").and_then(Value::as_str) == Some(name.as_str()));
        let Some(row) = row else {
            return rename_own_tab(session, squad_id, own_tab, &name, mux);
        };
        if row.get("tab_id").and_then(Value::as_u64) == Some(own_tab) {
            return Ok(());
        }
        let fits = row
            .get("pane_ids")
            .and_then(Value::as_array)
            .map(|ids| !ids.is_empty() && (ids.len() as u64) < max_panes)
            .unwrap_or(false);
        if fits {
            if let Some(anchor) = row
                .get("pane_ids")
                .and_then(Value::as_array)
                .and_then(|ids| ids.first())
                .and_then(Value::as_u64)
            {
                return join_own_tab(session, own_tab, anchor, &name, mux);
            }
        }
    }
    Ok(())
}

fn rename_own_tab(
    session: &str,
    squad_id: u64,
    own_tab: u64,
    name: &str,
    mux: &dyn MuxCli,
) -> Result<(), String> {
    let proc = mux.run(
        &[
            "mux".into(),
            "tab".into(),
            "rename".into(),
            "--server".into(),
            session.into(),
            "--workspace".into(),
            format!("id:{squad_id}"),
            "--tab".into(),
            format!("id:{own_tab}"),
            "--name".into(),
            name.into(),
        ],
        &[],
    );
    match proc {
        Ok(p) if p.status.success() => Ok(()),
        Ok(p) => Err(format!(
            "pane group tab rename into {name:?} failed: {}",
            String::from_utf8_lossy(&p.stderr).trim()
        )),
        Err(e) => Err(format!("pane group tab rename into {name:?} failed: {e}")),
    }
}

fn join_own_tab(
    session: &str,
    own_tab: u64,
    anchor_pane: u64,
    name: &str,
    mux: &dyn MuxCli,
) -> Result<(), String> {
    let proc = mux.run(
        &[
            "mux".into(),
            "tab".into(),
            "join".into(),
            "--server".into(),
            session.into(),
            "--src".into(),
            format!("id:{own_tab}"),
            "--at".into(),
            anchor_pane.to_string(),
            "--dir".into(),
            "right".into(),
        ],
        &[],
    );
    match proc {
        Ok(p) if p.status.success() => Ok(()),
        Ok(p) => Err(format!(
            "pane group tab join into {name:?} failed: {}",
            String::from_utf8_lossy(&p.stderr).trim()
        )),
        Err(e) => Err(format!("pane group tab join into {name:?} failed: {e}")),
    }
}

/// `pane_placement_conflict` (mux_spawn.py:90): `--pane` targets an
/// already-placed pane, so it never combines with a placement flag.
pub fn pane_placement_conflict(
    pane: Option<u64>,
    workspace: Option<&str>,
    split: Option<&str>,
    at: Option<&str>,
    tab: Option<&str>,
    tab_id: Option<&str>,
    bounded: bool,
) -> Option<String> {
    pane?;
    let labels = [
        (workspace.is_some(), "--workspace"),
        (split.is_some(), "--split"),
        (at.is_some(), "--at"),
        (tab.is_some(), "--tab"),
        (bounded, "--bounded-placement"),
        (tab_id.is_some(), "--tab-id"),
    ];
    let flag = labels
        .iter()
        .find(|(present, _)| *present)
        .map(|(_, label)| *label)?;
    Some(format!(
        "--pane cannot be combined with {flag}; it targets an already-placed pane"
    ))
}

/// POSIX `shlex.quote`: a token of shell-safe characters stays bare,
/// everything else single-quotes with the `'\\''` escape.
fn shlex_quote(token: &str) -> String {
    let safe = token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe && !token.is_empty() {
        return token.to_string();
    }
    format!("'{}'", token.replace('\'', "'\\''"))
}

/// `start_existing_pane` (mux_spawn.py:111): the worker start text sent into
/// the pristine shell, answered by `pane send --submit --raw --guarded`.
pub fn start_existing_pane(
    session: &str,
    pane_id: u64,
    cwd: &std::path::Path,
    wrapped: &[String],
    mux: &dyn MuxCli,
) -> Result<(), String> {
    let joined: Vec<String> = wrapped.iter().map(|t| shlex_quote(t)).collect();
    let text = format!(
        "cd -- {} && exec {}",
        shlex_quote(&cwd.to_string_lossy()),
        joined.join(" ")
    );
    let proc = mux.run(
        &[
            "mux".into(),
            "pane".into(),
            "send".into(),
            "--server".into(),
            session.into(),
            pane_id.to_string(),
            "--text".into(),
            text,
            "--submit".into(),
            "--raw".into(),
            "--guarded".into(),
        ],
        &[],
    );
    match proc {
        Ok(p) if p.status.success() => Ok(()),
        Ok(p) => {
            let detail = if p.stderr.is_empty() {
                String::from_utf8_lossy(&p.stdout).trim().to_string()
            } else {
                String::from_utf8_lossy(&p.stderr).trim().to_string()
            };
            Err(format!(
                "existing pane {pane_id} rejected the worker start in session {}: {}",
                py_repr(session),
                if detail.is_empty() {
                    "no output"
                } else {
                    &detail
                }
            ))
        }
        Err(e) => Err(format!(
            "existing pane {pane_id} rejected the worker start in session {}: {e}",
            py_repr(session)
        )),
    }
}
