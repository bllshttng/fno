use fno_agents::claims::{self, AcquireOpts, AcquireOutcome};
use rusqlite::Connection;
use std::sync::{Arc, Barrier};

fn options(root: &std::path::Path) -> AcquireOpts {
    AcquireOpts {
        root: Some(root.to_path_buf()),
        events_dir: Some(root.to_path_buf()),
        pid: Some(std::process::id()),
        ttl_ms: Some(60_000),
        identity: Some(("store-owner".into(), "claude".into())),
        ..Default::default()
    }
}

#[test]
fn independent_claimants_share_one_table_and_stale_owners_cannot_release_a_successor() {
    let temp = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["owner-a", "owner-b"]
        .into_iter()
        .map(|holder| {
            let root = temp.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                claims::acquire("node:contended", holder, options(&root))
            })
        })
        .collect();
    let outcomes: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| matches!(r, AcquireOutcome::Acquired(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| matches!(r, AcquireOutcome::HeldByOther { .. }))
            .count(),
        1
    );

    let connection = Connection::open(temp.path().join(".fno/db/graph.db")).unwrap();
    let holder: String = connection
        .query_row(
            "SELECT holder FROM claims WHERE key = 'node:contended'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE claims SET expires_at = 1 WHERE key = 'node:contended'",
            [],
        )
        .unwrap();
    assert!(matches!(
        claims::acquire("node:contended", "successor", options(temp.path())),
        AcquireOutcome::Acquired(_)
    ));
    claims::release(
        "node:contended",
        &holder,
        Some(temp.path()),
        Some(temp.path()),
    )
    .unwrap();
    assert!(!claims::renew("node:contended", &holder, 60_000, Some(temp.path())).unwrap());
    assert_eq!(
        claims::status("node:contended", Some(temp.path()))
            .1
            .unwrap()
            .holder,
        "successor"
    );
    assert!(!claims::claim_path("node:contended", Some(temp.path()))
        .unwrap()
        .exists());
}

#[test]
fn a_bad_legacy_claim_refuses_migration_without_importing_a_partial_store() {
    let temp = tempfile::tempdir().unwrap();
    let path = claims::claim_path("node:broken", Some(temp.path())).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "this is not a claim").unwrap();
    assert!(matches!(
        claims::acquire("node:new", "owner", options(temp.path())),
        AcquireOutcome::Error(_)
    ));
    let connection = Connection::open(temp.path().join(".fno/db/graph.db")).unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM claims", [], |r| r.get(0))
        .unwrap();
    let imported: i64 = connection
        .query_row(
            "SELECT count(*) FROM claim_meta WHERE key = 'lockfiles_imported'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((count, imported), (0, 0));
    assert!(path.is_file());
}
