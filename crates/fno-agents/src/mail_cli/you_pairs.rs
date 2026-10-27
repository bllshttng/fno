//! The `mail unread --mine` leg: the caller's own session's You pairs on the
//! terminal, the same data the mux Messages tab's You tab paints (node
//! x-cecf). Resolution rides `resolve_self_identity`, the same ladder
//! `fno whoami` uses; outside a session it refuses with the remedy.

use crate::provenance::BusIndex;
use crate::you_threads::YouStore;

/// `--mine` on the unread leaf: print this session's You pairs.
pub(crate) fn run(json_out: bool) -> i32 {
    let home = crate::paths::AgentsHome::from_env();
    let owned =
        crate::spawn_context::resolve_self_identity(&|k| std::env::var(k).ok(), None, None, &home);
    let raw_sid = owned.session_id.unwrap_or_default();
    let sid = raw_sid.trim().to_string();
    if sid.is_empty() {
        eprintln!("unread: --mine outside a session: run inside the session, or read the You tab in the mux Messages view");
        return 2;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let slug = crate::claude_ask::claude_cwd_slug(&cwd);
    let projects = crate::claude_drive::claude_projects_dir();
    let transcript = projects.join(slug).join(sid.clone() + ".jsonl");
    if !transcript.exists() {
        eprintln!("unread: --mine: no transcript at {}", transcript.display());
        return 2;
    }
    let dot_fno = home.root().parent().unwrap_or_else(|| home.root());
    let bus = BusIndex::load(&crate::intel::bus_log_path(dot_fno));
    let cache = home.root().join("intel").join("you-threads.json");
    let lock = crate::gh_budget::FileLock::try_acquire(&crate::gh_budget::lock_path(&cache));
    let persist = lock.is_ok();
    let mut store = YouStore::open(cache, persist);
    let items = store.advance(&transcript, &sid, &bus);
    if persist {
        store.save();
    }
    if items.is_empty() {
        if json_out {
            let doc = serde_json::to_string(&serde_json::json!({
                "session_id": sid,
                "items": [],
            }));
            println!("{}", doc.unwrap_or_default());
        } else {
            println!("no You pairs yet: type in this session and the replies pair up");
        }
        return 0;
    }
    if json_out {
        let doc = serde_json::to_string(&serde_json::json!({
            "session_id": sid.clone(),
            "items": items,
        }));
        println!("{}", doc.unwrap_or_default());
        return 0;
    }
    println!("You pairs for {sid}, oldest first:");
    for item in &items {
        let typed = item.typed_text.lines().next().unwrap_or("");
        let reply = item.reply_text.lines().next().unwrap_or("");
        println!("[{ts}] {typed}", ts = item.ts);
        if !reply.is_empty() {
            println!("  {reply}");
        }
    }
    0
}
