//! The team census: one pure fold over rows the client already holds, the
//! spawn-gate reading and the org tree. No I/O. It answers how many managed
//! sessions are live, working, testing and on a PR, fleet-wide and per Lead,
//! and how full each provider lane is, so the org block renders it without
//! touching a second counting authority.

use crate::org_model::OrgTree;
use crate::org_overlay::GateReading;
use crate::proto::AgentRow;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// One group's answer. A zero segment is omitted at render; an unread one
/// says `unknown` there, never a fabricated zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupCounts {
    pub live: u32,
    pub working: u32,
    pub unknown_age: u32,
    pub testing: u32,
    pub queued: u32,
    pub pr: u32,
    pub merged: u32,
}

impl GroupCounts {
    /// The non-zero segments of one group, in the fleet line's order.
    fn segments(&self) -> Vec<String> {
        let mut segs = Vec::new();
        for (count, label) in [
            (self.working, "working"),
            (self.unknown_age, "unknown age"),
            (self.testing, "testing"),
            (self.queued, "queued"),
            (self.pr, "pr"),
            (self.merged, "merged"),
        ] {
            if count > 0 {
                segs.push(format!("{count} {label}"));
            }
        }
        segs
    }
}

/// One Lead's counts beside the label the sideline already uses for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadCounts {
    pub label: String,
    pub counts: GroupCounts,
}

/// One provider lane's occupancy reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCounts {
    pub name: String,
    pub occupied: u32,
    pub limit: Option<u32>,
    pub working: u32,
    pub unknown: u32,
    pub reserved: u32,
    pub parked: u32,
    pub quota: Option<String>,
}

/// Test-run pids no session owns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unattributed {
    pub running: u32,
    pub queued: u32,
}

/// The whole census answer for one paint. `tests_known` is false when the
/// probe carried no readable tests block: the activity answers still
/// render, and the testing answer reads `unknown`, never zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Team {
    pub counts: GroupCounts,
    pub reserved: u32,
    pub observed: u32,
    pub tests_known: bool,
    pub unattributed: Option<Unattributed>,
    pub leads: Vec<LeadCounts>,
    pub unowned: GroupCounts,
    pub providers: Vec<ProviderCounts>,
}

/// Fold the census. Members are the non-exited rows the probe's registry
/// `slot_rows` name; every other non-exited row is `observed`, never live.
/// A member goes to the OrgTree Lead whose holder it is (its own row
/// wins) or whose node sessions name it, first Lead in OrgTree order when
/// two claim it, so the Lead lines never sum past the fleet. A member no
/// Lead claims is `unowned`. A reservation is counted once, fleet-wide,
/// under no Lead.
pub fn census(rows: &[AgentRow], gate: &GateReading, org: Option<&OrgTree>) -> Team {
    let slot_rows = gate.slot_rows.as_deref().unwrap_or(&[]);
    let mut slot_names: HashSet<&str> = HashSet::new();
    let mut reserved = 0u32;
    for row in slot_rows {
        if row.kind.as_deref() == Some("reservation") {
            reserved += 1;
        } else if let Some(name) = row.name.as_deref() {
            slot_names.insert(name);
        }
    }
    let live_rows: Vec<&AgentRow> = rows.iter().filter(|r| !r.exited).collect();
    let members: Vec<&AgentRow> = live_rows
        .iter()
        .copied()
        .filter(|r| slot_names.contains(r.name.as_str()))
        .collect();
    let observed = (live_rows.len() - members.len()) as u32;

    // The activity split over the members only; the ages the daemon
    // stamped are the one working answer.
    let ages: Vec<Option<u64>> = members.iter().map(|r| r.last_activity_age_s).collect();
    let split = crate::org_overlay::census_split(&ages);

    // The test feed joins by harness_session_id first, then by name. A
    // missing or errored tests block reads unknown, never zero: the
    // activity answers still render.
    let mut testing: HashSet<String> = HashSet::new();
    let mut queued: HashSet<String> = HashSet::new();
    let sessions = gate
        .tests
        .as_ref()
        .and_then(|tests| tests.get("error"))
        .is_none()
        .then(|| {
            gate.tests
                .as_ref()
                .and_then(|tests| tests.get("sessions"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        });
    let tests_known = sessions.is_some();
    for session in sessions.into_iter().flatten() {
        let sid = session.get("session_id").and_then(Value::as_str);
        let name = session.get("name").and_then(Value::as_str);
        let stage = session.get("stage").and_then(Value::as_str);
        let row = sid
            .and_then(|sid| {
                rows.iter()
                    .find(|r| r.harness_session_id.as_deref() == Some(sid))
            })
            .or_else(|| name.and_then(|name| rows.iter().find(|r| r.name == name)));
        let Some(row) = row else { continue };
        match stage {
            Some("test") => {
                testing.insert(row.name.clone());
            }
            Some("test-queued") => {
                queued.insert(row.name.clone());
            }
            _ => {}
        }
    }

    // Lead attribution: holder rows first, then the node-session join, first
    // Lead in OrgTree order where two claim the same row.
    let mut owner: HashMap<&str, usize> = HashMap::new();
    let mut row_pr: HashMap<&str, (bool, bool)> = HashMap::new();
    if let Some(org) = org {
        for (i, lead) in org.leads.iter().enumerate() {
            owner.insert(lead.holder.name.as_str(), i);
        }
        for (i, lead) in org.leads.iter().enumerate() {
            for node in &lead.nodes {
                let has_pr = !node.view.prs.is_empty();
                let merged = node
                    .view
                    .prs
                    .iter()
                    .any(|p| p.merge_status.as_deref() == Some("merged"));
                for session in &node.current {
                    if let Some(joined) = &session.agent {
                        let name = joined.name.as_str();
                        row_pr.entry(name).or_insert((has_pr, merged));
                        owner.entry(name).or_insert(i);
                    }
                }
            }
        }
    }

    let mut groups: Vec<GroupCounts> = org
        .map(|o| vec![GroupCounts::default(); o.leads.len()])
        .unwrap_or_default();
    let mut unowned = GroupCounts::default();
    for row in &members {
        let is_working = row
            .last_activity_age_s
            .is_some_and(|age| age < crate::org_overlay::working_max_s());
        let unknown_age = row.last_activity_age_s.is_none();
        let is_testing = testing.contains(&row.name);
        let is_queued = !is_testing && queued.contains(&row.name);
        let (pr, merged) = match (row_pr.get(row.name.as_str()), row.node.as_deref()) {
            (Some((has_pr, merged)), _) => (*has_pr, *merged),
            (None, Some(node)) => org
                .and_then(|o| {
                    o.leads
                        .iter()
                        .flat_map(|l| &l.nodes)
                        .find(|n| n.view.card.id == node)
                })
                .map(|n| {
                    (
                        !n.view.prs.is_empty(),
                        n.view
                            .prs
                            .iter()
                            .any(|p| p.merge_status.as_deref() == Some("merged")),
                    )
                })
                .unwrap_or((row.pr.is_some(), false)),
            _ => (row.pr.is_some(), false),
        };
        let mut c = GroupCounts {
            live: 1,
            working: u32::from(is_working),
            unknown_age: u32::from(unknown_age),
            testing: u32::from(is_testing),
            queued: u32::from(is_queued),
            pr: u32::from(pr),
            merged: u32::from(merged),
        };
        match owner.get(row.name.as_str()).copied() {
            Some(i) => {
                if let Some(slot) = groups.get_mut(i) {
                    merge_into(slot, &mut c);
                }
            }
            None => merge_into(&mut unowned, &mut c),
        }
    }

    // The Lead lines, sorted by live count the way the expanded block shows
    // them, brightest first.
    let mut leads: Vec<LeadCounts> = org
        .map(|o| {
            o.leads
                .iter()
                .enumerate()
                .map(|(i, lead)| LeadCounts {
                    label: lead_label(lead),
                    counts: groups.get(i).cloned().unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    leads.sort_by(|a, b| {
        b.counts
            .live
            .cmp(&a.counts.live)
            .then(a.label.cmp(&b.label))
    });

    let unattributed = gate.tests.as_ref().and_then(|tests| {
        let un = tests.get("unattributed")?;
        Some(Unattributed {
            running: un.get("running").and_then(Value::as_u64)? as u32,
            queued: un.get("queued").and_then(Value::as_u64)? as u32,
        })
    });

    Team {
        tests_known,
        counts: GroupCounts {
            live: members.len() as u32,
            working: split.working,
            unknown_age: split.unknown_age,
            testing: testing.len() as u32,
            queued: queued.len() as u32,
            pr: unowned.pr + leads.iter().map(|l| l.counts.pr).sum::<u32>(),
            merged: unowned.merged + leads.iter().map(|l| l.counts.merged).sum::<u32>(),
        },
        reserved,
        observed,
        unattributed,
        leads,
        unowned,
        providers: provider_readings(gate, rows),
    }
}

fn merge_into(slot: &mut GroupCounts, row: &mut GroupCounts) {
    slot.live += row.live;
    slot.working += row.working;
    slot.unknown_age += row.unknown_age;
    slot.testing += row.testing;
    slot.queued += row.queued;
    slot.pr += row.pr;
    slot.merged += row.merged;
    // A row counts once: clear it so the fleet total cannot double-count.
    *row = GroupCounts::default();
}

/// The Lead label in the order the sideline's own label walk uses: role
/// title, else scope, else holder name.
fn lead_label(lead: &crate::org_model::OrgLead) -> String {
    lead.holder
        .role_title
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| Some(lead.scope.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| lead.holder.name.clone())
}

/// One provider lane's reading, from the probe's `lanes` object. The
/// `quota_source` key names the read's provenance, not a lane, and is
/// skipped.
fn provider_readings(gate: &GateReading, rows: &[AgentRow]) -> Vec<ProviderCounts> {
    let Some(lanes) = gate.lanes.as_ref().and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let working_max = crate::org_overlay::working_max_s();
    for (provider, lane) in lanes {
        if provider == "quota_source" {
            continue;
        }
        let mut working = 0u32;
        let mut unknown = 0u32;
        for name in lane
            .get("counted")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            match rows
                .iter()
                .find(|r| !r.exited && r.name == name)
                .and_then(|r| r.last_activity_age_s)
            {
                Some(age) if age < working_max => working += 1,
                Some(_) => {}
                None => unknown += 1,
            }
        }
        out.push(ProviderCounts {
            name: provider.clone(),
            occupied: lane.get("live").and_then(Value::as_u64).unwrap_or(0) as u32,
            limit: lane.get("cap").and_then(Value::as_u64).map(|c| c as u32),
            working,
            unknown,
            reserved: lane
                .get("reserved")
                .and_then(Value::as_array)
                .map_or(0, |a| a.len() as u32),
            parked: lane
                .get("parked")
                .and_then(Value::as_array)
                .map_or(0, |a| a.len() as u32),
            quota: lane
                .get("quota")
                .and_then(Value::as_str)
                .filter(|q| *q != "closed")
                .map(str::to_string),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The fleet team line's segments: occupancy first, then the group's
/// non-zero answers, then the fleet-only buckets.
pub fn fleet_segments(team: &Team, gate: &GateReading) -> Vec<String> {
    let live_word = match gate.max_live {
        Some(max) => format!("{}/{} live", team.counts.live, max),
        None => format!("{} live", team.counts.live),
    };
    let mut segs = vec![live_word];
    if !team.tests_known {
        segs.push("testing unknown".to_string());
    }
    segs.extend(team.counts.segments());
    if team.reserved > 0 {
        segs.push(format!("{} reserved", team.reserved));
    }
    if team.observed > 0 {
        segs.push(format!("{} observed", team.observed));
    }
    if let Some(un) = &team.unattributed {
        if un.running > 0 {
            segs.push(format!("{} unattributed", un.running));
        }
    }
    segs
}

/// One labeled line: segments joined with a middle dot, trimmed at a
/// segment boundary with an ellipsis when the next does not fit. A digit
/// group is never cut in half. The label column is 10 columns, and 6 under
/// a 40-column sideline.
pub fn fit(label: &str, segments: &[String], width: usize) -> String {
    let label_w = if width < 40 { 4 } else { 8 };
    let prefix = format!("  {label:<label_w$} ");
    let mut out = prefix.clone();
    let mut used = prefix.chars().count();
    for (i, segment) in segments.iter().enumerate() {
        let piece = if i == 0 {
            segment.clone()
        } else {
            format!(" · {segment}")
        };
        let w = piece.chars().count();
        if i > 0 && used + w > width {
            out.push('…');
            return out;
        }
        out.push_str(&piece);
        used += w;
    }
    out
}

/// The Lead lines under a line budget: the brightest Leads first, and when
/// they do not all fit, the dimmest rest folds into one `+k leads (n live)`
/// line whose live total keeps the books balanced.
pub fn lead_lines(leads: &[LeadCounts], width: usize, budget: usize) -> Vec<String> {
    if leads.is_empty() {
        return Vec::new();
    }
    let budget = budget.max(1);
    let shown = budget.saturating_sub(1).min(leads.len());
    let mut lines: Vec<String> = leads[..shown]
        .iter()
        .map(|lead| {
            let mut segs = vec![format!("{} live", lead.counts.live)];
            segs.extend(lead.counts.segments());
            fit(&lead.label, &segs, width)
        })
        .collect();
    let rest = &leads[shown..];
    if !rest.is_empty() {
        let n: u32 = rest.iter().map(|l| l.counts.live).sum();
        lines.push(format!("  +{} leads ({n} live)", rest.len()));
    }
    lines
}

/// The unowned line, only when it holds someone: an empty bucket renders by
/// being absent, never as a zero.
pub fn unowned_line(unowned: &GroupCounts, width: usize) -> Option<String> {
    if unowned.live == 0 && unowned.pr == 0 {
        return None;
    }
    let mut segs = vec![format!("{} live", unowned.live)];
    segs.extend(unowned.segments());
    Some(fit("unowned", &segs, width))
}

/// One line per provider lane: `zai  13/16 occupied · 7/16 working`, with
/// the lane's unknown, reserved, parked and quota words beside them.
pub fn provider_lines(providers: &[ProviderCounts], width: usize) -> Vec<String> {
    providers
        .iter()
        .map(|lane| {
            let mut segs = Vec::new();
            match lane.limit {
                Some(cap) => segs.push(format!("{}/{} occupied", lane.occupied, cap)),
                None => segs.push(format!("{} occupied (no cap)", lane.occupied)),
            }
            if lane.working > 0 {
                match lane.limit {
                    Some(cap) => segs.push(format!("{}/{} working", lane.working, cap)),
                    None => segs.push(format!("{} working", lane.working)),
                }
            }
            if lane.unknown > 0 {
                segs.push(format!("{} unknown", lane.unknown));
            }
            if lane.reserved > 0 {
                segs.push(format!("{} reserved", lane.reserved));
            }
            if lane.parked > 0 {
                segs.push(format!("{} parked", lane.parked));
            }
            if let Some(quota) = &lane.quota {
                segs.push(format!("quota {quota}"));
            }
            fit(&lane.name, &segs, width)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::AgentRow;

    fn row(name: &str, age_s: Option<u64>) -> AgentRow {
        AgentRow {
            name: name.into(),
            last_activity_age_s: age_s,
            ..Default::default()
        }
    }

    fn slot_row(name: &str, kind: Option<&str>) -> crate::org_overlay::SlotRow {
        crate::org_overlay::SlotRow {
            kind: kind.map(str::to_string),
            name: Some(name.into()),
            node: None,
            provider: None,
        }
    }

    fn gate_with(slot_rows: Vec<crate::org_overlay::SlotRow>) -> GateReading {
        GateReading {
            slots: Some(slot_rows.len() as u32),
            max_live: Some(23),
            slot_rows: Some(slot_rows),
            tests: Some(serde_json::json!({
                "sessions": [],
                "running": 0,
                "queued": 0,
                "unattributed": {"running": 0, "queued": 0}
            })),
            ..GateReading::default()
        }
    }

    fn org_lead(
        holder: &str,
        title: Option<&str>,
        sessions: &[(&str, Option<(u64, Option<&str>)>)],
    ) -> crate::org_model::OrgLead {
        crate::org_model::OrgLead {
            holder: AgentRow {
                name: holder.into(),
                role_title: title.map(str::to_string),
                ..Default::default()
            },
            scope: format!("scope-{holder}"),
            level: 1,
            grantor: None,
            counts: Value::Null,
            owned_counts: Value::Null,
            epics: Value::Null,
            stuck_line: None,
            nodes: sessions
                .iter()
                .map(|(name, pr)| crate::org_model::OrgNode {
                    view: crate::backlog_model::NodeView {
                        card: crate::backlog_model::Card {
                            id: format!("x-{name}"),
                            ..Default::default()
                        },
                        prs: pr.map_or_else(Vec::new, |(number, status)| {
                            vec![crate::backlog_model::Pr {
                                number,
                                url: None,
                                merge_status: status.map(str::to_string),
                            }]
                        }),
                        ..Default::default()
                    },
                    claim_state: String::new(),
                    claim_holder: None,
                    age_s: None,
                    current: vec![crate::org_model::OrgSession {
                        view: crate::backlog_model::SessionView {
                            agent: Some((*name).into()),
                            action: "attach".into(),
                            ..Default::default()
                        },
                        agent: Some(AgentRow {
                            name: (*name).into(),
                            ..Default::default()
                        }),
                    }],
                    former: Vec::new(),
                })
                .collect(),
            left: Vec::new(),
        }
    }

    /// The census's primary owner test at one boundary. AC4-HP: one codex
    /// Lead holding two claude workers, one member no Lead claims,
    /// `slot_rows` naming all four plus one reservation. AC4-PR: the Lead's
    /// sessions sit on nodes whose PRs are merged and open, and the unowned
    /// drifter carries its own `pr`. AC4-PROV: lane zai caps at 16, lane
    /// anthropic has no cap, and a counted name with no row reads unknown.
    /// The books stay honest throughout: the reservation counts once
    /// fleet-wide, pr and merged read the node and the row, and no line
    /// claims a zero or a merge it cannot know.
    #[test]
    fn census_attributes_members_and_keeps_the_books_honest() {
        let mut drifter = row("drifter", Some(20));
        drifter.pr = Some(7);
        let rows = vec![
            row("chief", Some(900)),
            row("worker-a", Some(10)),
            row("worker-b", None),
            drifter,
        ];
        let mut gate = gate_with(vec![
            slot_row("chief", None),
            slot_row("worker-a", None),
            slot_row("worker-b", None),
            slot_row("drifter", None),
            slot_row("res-1", Some("reservation")),
        ]);
        let mut zai = serde_json::Map::new();
        zai.insert("cap".into(), serde_json::json!(16));
        zai.insert("live".into(), serde_json::json!(4));
        zai.insert(
            "counted".into(),
            serde_json::json!(["chief", "worker-a", "worker-b", "drifter", "ghost-a"]),
        );
        let mut anthropic = serde_json::Map::new();
        anthropic.insert("cap".into(), Value::Null);
        anthropic.insert("live".into(), serde_json::json!(5));
        anthropic.insert("counted".into(), serde_json::json!([]));
        let mut lanes = serde_json::Map::new();
        lanes.insert("zai".into(), Value::Object(zai));
        lanes.insert("anthropic".into(), Value::Object(anthropic));
        lanes.insert("quota_source".into(), serde_json::json!("snapshot"));
        gate.lanes = Some(Value::Object(lanes));
        let org = crate::org_model::OrgTree {
            leads: vec![org_lead(
                "chief",
                Some("Fleet Lead"),
                &[
                    ("worker-a", Some((1, Some("merged")))),
                    ("worker-b", Some((2, None))),
                ],
            )],
            unowned: Vec::new(),
            measured_at: 0,
        };
        let team = census(&rows, &gate, Some(&org));

        assert_eq!(team.counts.live, 4, "all four members are live");
        assert_eq!(team.reserved, 1, "the reservation counts once, fleet-wide");
        assert_eq!(team.leads.len(), 1);
        assert_eq!(
            team.leads[0].counts.live, 3,
            "the Lead holds chief + 2 workers"
        );
        assert_eq!(team.leads[0].counts.working, 1, "only worker-a is working");
        assert_eq!(team.leads[0].counts.unknown_age, 1, "worker-b has no age");
        assert_eq!(team.unowned.live, 1, "the drifter is unowned");
        assert_eq!(team.observed, 0, "every live row is a named slot");
        assert!(team.tests_known, "a gate with a tests block is known");
        assert_eq!(team.leads[0].counts.pr, 2, "one pr per session node");
        assert_eq!(
            team.leads[0].counts.merged, 1,
            "merged reads the node's merge status"
        );
        assert_eq!(team.unowned.pr, 1, "the drifter's own pr counts");
        assert_eq!(
            team.unowned.merged, 0,
            "an unowned row's merged reads unknown"
        );

        let text = lead_lines(&team.leads, 200, 10).join("\n");
        assert!(
            text.contains("Fleet Lead 3 live · 1 working · 1 unknown age"),
            "{text}"
        );
        assert!(text.contains("2 pr · 1 merged"), "{text}");
        let unowned_text = unowned_line(&team.unowned, 200).expect("the unowned line renders");
        assert!(
            unowned_text.contains("unowned  1 live · 1 working · 1 pr"),
            "{unowned_text}"
        );
        assert!(
            !unowned_text.contains("merged"),
            "the unowned line never claims a merge: {unowned_text}"
        );
        let fleet = fit("team", &fleet_segments(&team, &gate), 200);
        assert!(fleet.contains("1 reserved"), "{fleet}");
        assert!(!fleet.contains("0 live"), "{fleet}");

        let providers = provider_lines(&team.providers, 200).join("\n");
        assert!(
            providers.contains("4/16 occupied · 2/16 working · 2 unknown"),
            "{providers}"
        );
        assert!(
            providers.contains("anthropic 5 occupied (no cap)"),
            "{providers}"
        );
        assert!(!providers.contains("quota_source"), "{providers}");
    }

    /// The fleet line's render contracts at one boundary: a tests block
    /// that errored reads `testing unknown` while the activity answers still
    /// render; at a 30-column sideline the line trims at a segment boundary
    /// with an ellipsis, never splitting a digit group; and with more Leads
    /// than the budget the dimmest rest folds into one summary line whose
    /// live total keeps the books balanced.
    #[test]
    fn the_fleet_line_reads_unknown_fits_narrow_and_folds_the_rest() {
        let rows = vec![row("w0", Some(10))];
        let gate = GateReading {
            slot_rows: Some(vec![slot_row("w0", None)]),
            max_live: Some(3),
            tests: Some(serde_json::json!({"error": "process table unreadable"})),
            ..GateReading::default()
        };
        let team = census(&rows, &gate, None);
        assert!(!team.tests_known);
        assert_eq!(team.counts.testing, 0);
        let fleet = fit("team", &fleet_segments(&team, &gate), 200);
        assert!(fleet.contains("testing unknown"), "{fleet}");
        assert!(fleet.contains("1/3 live"), "{fleet}");

        let line = fit(
            "team",
            &[
                "21/23 live".to_string(),
                "9 working".to_string(),
                "2 testing".to_string(),
                "1 queued".to_string(),
            ],
            30,
        );
        assert!(line.starts_with("  team "), "{line}");
        assert!(line.ends_with('…'), "{line}");
        assert!(!line.contains("2 te"), "no digit group split: {line}");
        assert!(line.contains("21/23 live"), "{line}");

        // AC5-CAP: with more Leads than the budget, the dimmest rest folds
        // into one summary line and the live books stay balanced.
        let leads: Vec<LeadCounts> = (0..12)
            .map(|i| LeadCounts {
                label: format!("lead-{i}"),
                counts: GroupCounts {
                    live: 1,
                    ..Default::default()
                },
            })
            .collect();
        let lines = lead_lines(&leads, 200, 6);
        assert_eq!(lines.len(), 6, "{lines:?}");
        let summary = lines.last().expect("the summary line renders");
        assert!(summary.contains("+7 leads (7 live)"), "{summary}");
        let shown: u32 = lines[..5]
            .iter()
            .filter_map(|l| l.split(" live").next())
            .filter_map(|head| head.trim().rsplit(' ').next())
            .filter_map(|n| n.parse::<u32>().ok())
            .sum();
        assert_eq!(
            shown + 7,
            12,
            "shown plus folded equals the attributed live"
        );
    }
}
