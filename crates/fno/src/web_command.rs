//! `/command/model.json`: the org view's fleet fold as JSON. The bridge keeps
//! the last good read and refreshes it off the request path.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::backlog_model::Inputs;
use crate::client::org_seats::{self, Fleet};
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
    good: Option<(Fleet, Instant)>,
    failure: Option<(String, Instant)>,
    questions: Option<Result<Vec<Value>, String>>,
    prs: Vec<Value>,
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
                st.good = Some((org_seats::fold(&map, backlog, &[], measured_at), now));
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
        st.prs = open_prs(backlog);
        st.read_at = Some(now);
    }

    pub(crate) async fn model(&self) -> Value {
        let st = self.state.lock().await;
        let mut waiting = json!({ "prs": st.prs });
        match &st.questions {
            Some(Ok(items)) => waiting["questions"] = json!(items),
            Some(Err(reason)) => waiting["questions_error"] = json!(reason),
            None => waiting["questions_error"] = json!("not read yet"),
        }
        let fleet = st.good.as_ref().map(|(f, _)| f);
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
            "age_s": st.good.as_ref().map(|(_, at)| at.elapsed().as_secs()),
            "notice": fleet.and_then(|f| f.notice.clone()),
            "seat_error": fleet.and_then(|f| f.seat_error.clone()),
            "fleet_error": fleet_error,
            "machines": fleet.map(|f| json!(f.machines)).unwrap_or_else(|| json!([])),
            "waiting": waiting,
        })
    }
}

/// PRs on in-review nodes the store has not seen merged or closed. The store
/// records no review or conflict word, so `merge_status` is its raw word or null.
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
        .filter(|row| {
            !matches!(
                text(row, "merge_status").as_deref(),
                Some("merged" | "closed")
            )
        })
        .filter_map(|row| {
            let number = row.get("pr_number").and_then(Value::as_u64)?;
            Some(
                json!({"node": text(row, "id"), "title": text(row, "title"), "number": number,
                "url": text(row, "pr_url"), "merge_status": text(row, "merge_status")}),
            )
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
            json!({"id": "x-1", "status": "in_review", "pr_number": 7, "merge_status": ""}),
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
        assert_eq!(m["waiting"]["questions"][1]["host"], "iMac2D2");
        assert_eq!(m["waiting"]["questions"].as_array().unwrap().len(), 3);
        assert_eq!(
            m["waiting"]["prs"],
            json!([{"node": "x-1", "title": null, "number": 7, "url": null, "merge_status": null}])
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
    }
}
