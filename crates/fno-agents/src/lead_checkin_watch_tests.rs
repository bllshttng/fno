use super::*;

#[test]
fn failed_reader_prints_own_line_and_beat_continues() {
    let mut readings = sample_readings(Value::Null, org4(), cap_ok(), workers3());
    set_reading(
        &mut readings,
        Reading::took(
            "workers",
            json!({
                "live_workers": 3,
                "oldest_worker_seen": "90s w1",
                "live_subagents": null,
            }),
        ),
    );
    set_reading(
        &mut readings,
        Reading::failed(
            "watch_expiry",
            "watch expiry event has no valid timestamp".into(),
        ),
    );
    set_reading(
        &mut readings,
        Reading::failed("board", "board payload names no undriven_pr queue".into()),
    );
    let data = build_data(&readings, "x-bbbb");
    let change = derive_change(None, &data, "");
    let lines = render_lines("x-bbbb", &readings, &data, &None, "", &change);
    assert!(lines.iter().any(|l| l.starts_with("READER FAILED board:")));
    assert!(lines
        .iter()
        .any(|l| l == "READER FAILED watch expiry: watch expiry event has no valid timestamp"));
    assert!(lines
        .iter()
        .any(|l| l.starts_with("workers:") && l.contains("overdue watches unmeasured")));
    assert!(lines
        .iter()
        .any(|l| l.starts_with("coverage: 21 of 23 readings ok")));
    assert!(lines.iter().any(|l| l.contains("failed readers: board (")
        && l.contains(", watch_expiry (watch expiry event has no valid timestamp)")));
    assert_eq!(
        change,
        "no numeric movement; readings failed: board, watch_expiry"
    );
    assert_eq!(data.get("open_prs"), None);
    assert_eq!(
        data.get("readers_failed"),
        Some(&json!(["board", "watch_expiry"]))
    );
}
