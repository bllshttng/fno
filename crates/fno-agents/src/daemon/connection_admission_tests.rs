use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;

fn ticket_count(path: &std::path::Path) -> usize {
    std::fs::read_dir(path)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

#[test]
fn stop_reaches_handler_while_ordinary_reports_wait_on_graph_lock() {
    const ORDINARY: usize = connection::ORDINARY_REQUEST_SLOTS;
    const BLOCKING_THREADS: usize = ORDINARY + connection::STOP_REQUEST_SLOTS;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(BLOCKING_THREADS)
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let root = std::env::temp_dir().join(format!(
            "fno-stop-admission-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = AgentsHome::at(&root);
        home.ensure_root().unwrap();
        state::update_registry(&home.registry_json(), |_| ()).unwrap();
        tests::seed_stream_row(&home, "stop-target", "stp12345");
        state::update_registry(&home.registry_json(), |registry| {
            let entry = registry.find_mut("stop-target").unwrap();
            entry.status = crate::AgentStatus::Exited;
            entry.exited_at = Some("2026-11-06T00:00:00Z".into());
        })
        .unwrap();
        let ctx = Arc::new(tests::test_ctx(
            home.clone(),
            PathBuf::from("fno-agents-worker"),
        ));
        let admission = Arc::new(connection::RequestAdmission::default());

        let graph = crate::state_layout::graph_anchor(home.root());
        let graph_lock_path = crate::graph_store::graph_lock_path(&graph);
        let queue = crate::lock_queue::queue_dir(&graph_lock_path);
        let held = crate::graph_store::BoundedLock::acquire(&graph, Duration::from_secs(1))
            .expect("test holds the graph lock");
        let baseline_tickets = ticket_count(&queue);

        let busy = Arc::new(AtomicUsize::new(0));
        let mut report_readers = Vec::new();
        let mut report_servers = Vec::new();
        for id in 0..BLOCKING_THREADS {
            let (mut client, server) = UnixStream::pair().unwrap();
            let server_ctx = Arc::clone(&ctx);
            let server_admission = Arc::clone(&admission);
            report_servers.push(tokio::spawn(async move {
                connection::serve_connection(server_ctx, server_admission, server).await;
            }));
            crate::protocol::write_request(
                &mut client,
                &Request::new(
                    id as u64,
                    "agent.report",
                    json!({
                        "session_id": format!("missing-{id}"),
                        "seq": 1,
                        "state": "working"
                    }),
                ),
            )
            .await
            .unwrap();
            let busy_counter = Arc::clone(&busy);
            report_readers.push(tokio::spawn(async move {
                let response = crate::protocol::read_response(&mut client).await.unwrap();
                if response.error().map(|e| e.code) == Some(ErrorCode::Busy) {
                    busy_counter.fetch_add(1, Ordering::SeqCst);
                }
                response
            }));
        }

        let admission_reached = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let tickets = ticket_count(&queue).saturating_sub(baseline_tickets);
                let rejected = busy.load(Ordering::SeqCst);
                if tickets >= BLOCKING_THREADS || (tickets >= ORDINARY && rejected >= 4) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();

        let (mut stop_client, stop_server) = UnixStream::pair().unwrap();
        let stop_ctx = Arc::clone(&ctx);
        let stop_admission = Arc::clone(&admission);
        let stop_server_task = tokio::spawn(async move {
            connection::serve_connection(stop_ctx, stop_admission, stop_server).await;
        });
        crate::protocol::write_request(
            &mut stop_client,
            &Request::new(1000, "agent.stop", json!({"name": "stop-target"})),
        )
        .await
        .unwrap();
        let stop_result = tokio::time::timeout(
            Duration::from_millis(500),
            crate::protocol::read_response(&mut stop_client),
        )
        .await;

        drop(held);
        let _ = tokio::time::timeout(Duration::from_secs(5), stop_server_task).await;
        let mut report_responses = Vec::new();
        for reader in report_readers {
            report_responses.push(tokio::time::timeout(Duration::from_secs(5), reader).await);
        }
        for server in report_servers {
            let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
        }
        let _ = std::fs::remove_dir_all(&root);

        assert!(
            admission_reached,
            "report requests did not reach the held lock"
        );
        let stop_response = stop_result
            .expect("stop response must not queue behind ordinary reports")
            .expect("stop response frame is valid");
        assert_eq!(
            stop_response.result().unwrap()["already_exited"],
            true,
            "stop reached the handler and returned the already-exited receipt"
        );
        let busy_responses = report_responses
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .filter_map(|r| r.as_ref().ok())
            .filter(|r| r.error().map(|e| e.code) == Some(ErrorCode::Busy))
            .count();
        assert_eq!(
            busy_responses, 4,
            "requests above the ordinary cap are refused"
        );
    });
}
