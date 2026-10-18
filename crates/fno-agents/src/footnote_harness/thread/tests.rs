use super::*;

/// An ask nobody answers rejects when the window closes, never allows. An
/// answer request resolves a parked ask; with none parked, or an option
/// outside allow_once and reject_once, the host refuses. A prompt with no
/// text refuses; one with text queues with its `_meta`.
#[test]
fn an_unanswered_ask_rejects_and_an_answer_resolves_it() {
    let host = Arc::new(Host::default());
    let request = json!({"toolCall": {"title": "Write: a.txt"}});
    let mut quick = ask_answerer(&host, Duration::from_millis(50));
    assert_eq!(quick(&request), PermissionPolicy::RejectOnce);
    assert!(lock(&host.ask).is_none());
    let none = host.reply("s1", &json!({"op": "answer", "option": "allow_once"}));
    assert_eq!(none["ok"], false);

    let mut ask = ask_answerer(&host, Duration::from_secs(10));
    let waiter = std::thread::spawn(move || ask(&request));
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.reply("s1", &json!({"op": "status"}))["ask"].is_null() {
        assert!(Instant::now() < deadline, "the ask never parked");
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = host.reply("s1", &json!({"op": "status"}));
    assert_eq!(status["ask"]["toolCall"]["title"], "Write: a.txt");
    assert_eq!(status["session_id"], "s1");
    let bad = host.reply("s1", &json!({"op": "answer", "option": "allow_always"}));
    assert_eq!(bad["ok"], false);
    let ok = host.reply("s1", &json!({"op": "answer", "option": "allow_once"}));
    assert_eq!(ok["ok"], true);
    assert_eq!(waiter.join().unwrap(), PermissionPolicy::AllowOnce);

    let empty = host.reply("s1", &json!({"op": "prompt", "text": ""}));
    assert_eq!(empty["ok"], false);
    let meta = json!({"footnote/origin": {"kind": "mail"}});
    let queued = host.reply(
        "s1",
        &json!({"op": "prompt", "text": "hi", "meta": meta.clone()}),
    );
    assert_eq!(queued["queued"], 1);
    assert_eq!(lock(&host.queue)[0].meta, Some(meta));
}
