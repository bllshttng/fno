//! `/command/model.json`: the org view's fleet fold as JSON. The bridge keeps
//! the last good read and refreshes it off the request path.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::backlog_model::Inputs;
use crate::client::org_seats::{self, Fleet};
use crate::digest_overlay::ObsidianCfg;
use crate::link::{plan_link, PlanLink};
use crate::needs_overlay::QuestionsFold;

pub(crate) const REFRESH_AFTER: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct CommandCache {
    state: tokio::sync::Mutex<Cached>,
    /// The first read: every request before it waits on the same one.
    pub(crate) first: tokio::sync::OnceCell<()>,
}

#[derive(Default)]
struct Cached {
    /// The fold, its machines as JSON, and when it was read.
    good: Option<(Fleet, Value, Instant)>,
    failure: Option<(String, Instant)>,
    questions: Option<Result<Vec<Value>, String>>,
    prs: Vec<Value>,
    /// The last backlog read failed; `prs` is the last good list.
    prs_error: Option<String>,
    read_at: Option<Instant>,
}

impl CommandCache {
    /// True when the caller should start a refresh. Claiming stamps the
    /// attempt, so a burst of reads starts one and a lost refresh retries.
    pub(crate) async fn claim_refresh(&self) -> bool {
        let mut st = self.state.lock().await;
        if st.read_at.is_some_and(|at| at.elapsed() < REFRESH_AFTER) {
            return false;
        }
        st.read_at = Some(Instant::now());
        true
    }

    /// Store one refresh. A failed fleet read keeps the last good fold.
    pub(crate) async fn record(
        &self,
        fleet: Result<Value, String>,
        questions: Result<QuestionsFold, String>,
        backlog: &Inputs,
        measured_at: u64,
    ) {
        let now = Instant::now();
        let mut st = self.state.lock().await;
        match fleet {
            Ok(map) => {
                let fleet = org_seats::fold(&map, backlog, &[], measured_at);
                let cwd = std::env::current_dir().unwrap_or_default();
                let machines = machines_json(&fleet, &ObsidianCfg::read(&cwd));
                st.good = Some((fleet, machines, now));
                st.failure = None;
            }
            Err(reason) => st.failure = Some((reason, now)),
        }
        st.questions = Some(questions.map(|fold| {
            fold.items
                .iter()
                .filter(|q| q.state == "open" && !q.settled)
                .map(|q| {
                    json!({
                        "id": q.id,
                        "kind": q.kind,
                        "title": q.title,
                        "node": q.node,
                        "host": q.host,
                        "created_at": q.created_at,
                        "priority": q.priority,
                    })
                })
                .collect()
        }));
        match &backlog.rows_error {
            Some(reason) => st.prs_error = Some(reason.clone()),
            None => (st.prs, st.prs_error) = (open_prs(backlog), None),
        }
        st.read_at = Some(now);
    }

    pub(crate) async fn model(&self) -> Value {
        let st = self.state.lock().await;
        let mut waiting = json!({ "prs": st.prs, "prs_error": st.prs_error });
        match &st.questions {
            Some(Ok(items)) => waiting["questions"] = json!(items),
            Some(Err(reason)) => waiting["questions_error"] = json!(reason),
            None => waiting["questions_error"] = json!("not read yet"),
        }
        let fleet = st.good.as_ref().map(|(f, _, _)| f);
        let fleet_error = match (&st.failure, &st.good) {
            (Some((reason, at)), _) => {
                json!({"reason": reason, "failed_age_s": at.elapsed().as_secs()})
            }
            (None, None) => json!({ "reason": "fleet not read yet", "failed_age_s": null }),
            (None, Some(_)) => Value::Null,
        };
        json!({
            "schema": 1,
            "measured_at": fleet.map(|f| f.measured_at),
            "age_s": st.good.as_ref().map(|(_, _, at)| at.elapsed().as_secs()),
            "notice": fleet.and_then(|f| f.notice.clone()),
            "seat_error": fleet.and_then(|f| f.seat_error.clone()),
            "fleet_error": fleet_error,
            "machines": st.good.as_ref().map_or_else(|| json!([]), |(_, m, _)| m.clone()),
            "waiting": waiting,
        })
    }
}

/// The machines with each seat's plan link, so the page builds none. With obsidian on, a plan in the vault gets its
/// `obsidian://` link; any other plan gets a plain `file://` link.
fn machines_json(fleet: &Fleet, obsidian: &ObsidianCfg) -> Value {
    let mut machines = json!(fleet.machines);
    let rows = machines
        .as_array_mut()
        .into_iter()
        .flatten()
        .flat_map(|m| m["leads"].as_array_mut().into_iter().flatten())
        .flat_map(|l| l["seats"].as_array_mut().into_iter().flatten());
    for (row, seat) in rows.zip(fleet.seats()) {
        let plan = seat.work.as_ref().and_then(|w| w.plan.as_deref());
        match (plan_link(plan.map(Path::new), obsidian), plan) {
            (PlanLink::Obsidian { uri }, _) => row["plan_uri"] = json!(uri),
            (_, Some(plan)) if Path::new(plan).is_absolute() => {
                row["plan_file"] = json!(format!(
                    "file://{}",
                    crate::link::percent_encode_query(plan)
                ))
            }
            _ => {}
        }
    }
    machines
}

/// PRs on in-review nodes, the primary binding and `additional_prs`, that the
/// store has not seen merged or closed. The store records no review or
/// conflict word, so `merge_status` is its raw word or null.
fn open_prs(backlog: &Inputs) -> Vec<Value> {
    let text = |row: &Value, k: &str| {
        row.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    backlog
        .rows
        .iter()
        .filter(|row| text(row, "status").as_deref() == Some("in_review"))
        .flat_map(|row| {
            let primary = row.get("pr_number").and_then(Value::as_u64);
            let primary = primary.map(|n| (n, text(row, "pr_url"), text(row, "merge_status")));
            let extra = row.get("additional_prs").and_then(Value::as_array);
            let extra = extra.into_iter().flatten().filter_map(|p| {
                let n = p.get("number").and_then(Value::as_u64)?;
                Some((n, text(p, "url"), text(p, "merge_status")))
            });
            primary
                .into_iter()
                .chain(extra)
                .filter(|(_, _, m)| !matches!(m.as_deref(), Some("merged" | "closed")))
                .map(move |(number, url, merge_status)| {
                    json!({"node": text(row, "id"), "title": text(row, "title"), "number": number,
                    "url": url, "merge_status": merge_status})
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::needs_overlay::QuestionItem;

    fn two_machines() -> Value {
        json!({"machines": [
            {"host": "bb16", "local": true, "fresh": true, "sessions": [
                {"name": "kara", "session_id": "976b7b64", "harness": "claude", "role": "lead"},
                {"name": "t-one", "session_id": "aaaa1111", "parent": "976b", "harness": "claude",
                 "node": "x-1", "status": "writing"}
            ]},
            {"host": "iMac2D2", "fresh": false, "age_s": 900, "sessions": [
                {"name": "t-two", "harness": "codex"}
            ]}
        ]})
    }

    fn questions(hosts: &[Option<&str>]) -> QuestionsFold {
        QuestionsFold {
            items: hosts
                .iter()
                .enumerate()
                .map(|(i, h)| QuestionItem {
                    id: format!("q-{i}"),
                    host: h.map(str::to_string),
                    state: ["open", "answered"][usize::from(*h == Some("answered"))].into(),
                    ..QuestionItem::default()
                })
                .collect(),
            ..QuestionsFold::default()
        }
    }

    async fn read(
        cache: &CommandCache,
        fleet: Result<Value, String>,
        qs: Result<QuestionsFold, String>,
        at: u64,
    ) -> Value {
        let rows = vec![
            json!({"id": "x-1", "status": "in_review", "pr_number": 7, "merge_status": "",
                "additional_prs": [{"number": 9}, {"number": 10, "merge_status": "closed"}]}),
            json!({"id": "x-2", "status": "in_review", "pr_number": 8, "merge_status": "merged"}),
        ];
        let backlog = Inputs {
            rows,
            ..Inputs::default()
        };
        cache.record(fleet, qs, &backlog, at).await;
        cache.model().await
    }

    #[tokio::test]
    async fn two_machine_fold_serializes_every_seat_and_question_host() {
        let qs = questions(&[None, Some("iMac2D2"), Some("iMac2D2"), Some("answered")]);
        let m = read(&CommandCache::default(), Ok(two_machines()), Ok(qs), 100).await;
        let seats = |i: usize| {
            m["machines"][i]["leads"][0]["seats"]
                .as_array()
                .map(Vec::len)
        };
        assert_eq!((seats(0), seats(1), seats(2)), (Some(2), Some(1), None));
        assert!(m["machines"][0]["leads"][0]["seats"][0]
            .get("local")
            .is_none());
        let imac = &m["machines"][1]["leads"][0]["seats"][0];
        assert_eq!(imac["name"], "t-two");
        assert!(
            imac.get("attach").is_none(),
            "the page hands out no command"
        );
        assert_eq!(m["waiting"]["questions"][1]["host"], "iMac2D2");
        assert_eq!(m["waiting"]["questions"].as_array().unwrap().len(), 3);
        assert_eq!(
            m["waiting"]["prs"],
            json!([
                {"node": "x-1", "title": null, "number": 7, "url": null, "merge_status": null},
                {"node": "x-1", "title": null, "number": 9, "url": null, "merge_status": null}
            ])
        );
        assert_eq!(
            (&m["measured_at"], &m["fleet_error"]),
            (&json!(100), &Value::Null)
        );
    }

    #[tokio::test]
    async fn a_failed_read_names_its_reason_and_keeps_the_last_good_fold() {
        let cache = CommandCache::default();
        let fail = |r: &str| Err::<Value, String>(r.into());
        let m = read(&cache, fail("no primary"), Ok(questions(&[None])), 1).await;
        assert_eq!(m["machines"], json!([]));
        assert_eq!(m["fleet_error"]["reason"], "no primary");
        assert_eq!(m["waiting"]["questions"].as_array().unwrap().len(), 1);
        read(&cache, Ok(two_machines()), Err("timed out".into()), 100).await;
        let m = read(
            &cache,
            fail("fleet top timed out"),
            Err("timed out".into()),
            200,
        )
        .await;
        assert_eq!(m["machines"].as_array().unwrap().len(), 2);
        assert_eq!(m["measured_at"], 100);
        assert_eq!(m["fleet_error"]["reason"], "fleet top timed out");
        assert!(m["age_s"].is_u64());
        assert!(m["waiting"].get("questions").is_none());
        assert_eq!(m["waiting"]["questions_error"], "timed out");
        let locked = Inputs {
            rows_error: Some("graph locked".into()),
            ..Inputs::default()
        };
        cache.record(fail("x"), Err("x".into()), &locked, 300).await;
        let m = cache.model().await;
        assert_eq!(m["waiting"]["prs"].as_array().map(Vec::len), Some(2));
        assert_eq!(m["waiting"]["prs_error"], "graph locked");
    }
}
