//! Characterization for the dispatch admission decision (protocol steps 3-4,
//! docs/architecture/dual-implementation-inventory.md). The Rust leg is
//! `fno_agents::backlog_ready::select`, served over the keeper's `ready`
//! command. The goldens under `tests/golden/backlog_ready/` were captured from
//! the PYTHON leg while both legs lived: the differential stage asserted
//! Rust==Python byte-for-byte (modulo the volatile stamps and paths named in
//! `normalize_volatile`) on survivor JSON, drop attribution, and survivor
//! sets, then the Python cascade (`fno.backlog.explain
//! .build_selection_filters` / `run_cascade`) and `cmd_ready`'s inline
//! filters were deleted in the same change that flipped this file.

//! parity-stage: characterization
//! parity-oracle: frozen pre-port goldens

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Canonical repo root, the anchor both legs share: the Python leg runs with
/// cwd = canonical root so `repo_root()` resolves there, and the Rust leg
/// passes the same string as `ReadyOpts.repo_root` for `detect_project`.
fn canonical_repo_root() -> PathBuf {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let out = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo)
        .output()
        .expect("git worktree list");
    let text = String::from_utf8_lossy(&out.stdout);
    let main = text
        .lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .expect("git worktree list names a main worktree");
    PathBuf::from(main)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Materialization context: where plan docs land, the canonical repo root,
/// and the selection instant both legs share.
struct Ctx {
    planroot: PathBuf,
    repo: PathBuf,
    now_ms: i64,
}

impl Ctx {
    /// ISO stamp `days` in the past, second precision, UTC.
    fn iso(&self, days: i64) -> String {
        let ms = self.now_ms - days * 86_400_000;
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
    }
}

struct PlanDoc {
    name: &'static str,
    contents: &'static str,
    /// File mtime set this many days back (the staleness freshness probe).
    age_days: i64,
}

struct Case {
    name: &'static str,
    /// `fno backlog ready` argv tail (e.g. ["-A"]).
    flags: &'static [&'static str],
    entries: fn(&Ctx) -> Vec<Value>,
    plans: &'static [PlanDoc],
    /// Node ids the materializer holds LIVE claims for.
    claims: &'static [&'static str],
    /// --parent resolves to nothing: the command refuses (exit 1).
    expect_err: bool,
}

/// A minimal ready row: `id`, priority, project, created `days` ago.
fn ready_row(id: &str, prio: &str, project: &str, created_days: i64, ctx: &Ctx) -> Value {
    serde_json::json!({
        "id": id, "slug": format!("slug-{id}"), "title": format!("Node {id}"),
        "priority": prio, "project": project, "status": "ready",
        "type": "task",
        "created_at": ctx.iso(created_days),
    })
}

fn cases() -> Vec<Case> {
    vec![
        // Detection with no flags: the node whose cwd is the canonical root
        // names the project; other projects narrow out.
        Case {
            name: "plain_ready",
            flags: &[],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-pr1", "slug": "s1", "title": "P1", "priority": "p1",
                    "project": "fno", "status": "ready", "type": "task",
                    "cwd": c.repo.display().to_string(), "created_at": c.iso(2)}),
                serde_json::json!({"id": "x-pr2", "slug": "s2", "title": "P2", "priority": "p2",
                    "project": "fno", "status": "ready", "type": "task",
                    "cwd": c.repo.display().to_string(), "created_at": c.iso(3)}),
                serde_json::json!({"id": "x-pr3", "slug": "s3", "title": "P3", "priority": "p1",
                    "project": "otherproj", "status": "ready", "type": "task",
                    "cwd": "/somewhere/else", "created_at": c.iso(1)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // --all widens back over every project.
        Case {
            name: "all_projects",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-ap1", "slug": "s1", "title": "P1", "priority": "p1",
                    "project": "fno", "status": "ready", "type": "task", "created_at": c.iso(5)}),
                serde_json::json!({"id": "x-ap2", "slug": "s2", "title": "P2", "priority": "p2",
                    "project": "otherproj", "status": "ready", "type": "task", "created_at": c.iso(4)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // --ideas admits the plan-less idea (rung none, cold-dispatchable);
        // the linked stub (rung idea) is admitted but parks idea-stage.
        Case {
            name: "ideas",
            flags: &["--ideas"],
            entries: |c: &Ctx| vec![
                ready_row("x-id1", "p1", "fno", 2, c),
                serde_json::json!({"id": "x-id2", "slug": "s2", "title": "Planless idea",
                    "priority": "p1", "project": "fno", "status": "idea", "type": "task",
                    "created_at": c.iso(2)}),
                serde_json::json!({"id": "x-id3", "slug": "s3", "title": "Linked stub",
                    "priority": "p1", "project": "fno", "status": "idea", "type": "task",
                    "plan_path": format!("{}/stub-plan.md", c.planroot.display()), "created_at": c.iso(2)}),
            ],
            plans: &[PlanDoc {
                name: "stub-plan.md",
                contents: "---\nstatus: stub\n---\n\n# Stub\n",
                age_days: 0,
            }],
            claims: &[],
            expect_err: false,
        },
        // --include-deferred resurfaces paused rows; a paused PR-bearing row
        // still lists (the PR guard is scoped to ready status).
        Case {
            name: "include_deferred",
            flags: &["--include-deferred"],
            entries: |c: &Ctx| vec![
                ready_row("x-df0", "p0", "fno", 1, c),
                serde_json::json!({"id": "x-df1", "slug": "s1", "title": "Deferred",
                    "priority": "p1", "project": "fno", "status": "deferred", "type": "task",
                    "created_at": c.iso(2)}),
                serde_json::json!({"id": "x-df2", "slug": "s2", "title": "Deferred with PR",
                    "priority": "p2", "project": "fno", "status": "deferred", "type": "task",
                    "pr_number": 42, "created_at": c.iso(2)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // An explicit --project narrows without detection.
        Case {
            name: "project_flag",
            flags: &["-p", "beta"],
            entries: |c: &Ctx| vec![
                ready_row("x-pf1", "p1", "beta", 2, c),
                ready_row("x-pf2", "p0", "alpha", 1, c),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        Case {
            name: "roadmap",
            flags: &["--roadmap-id", "rm-1"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-rm1", "slug": "s1", "title": "On roadmap",
                    "priority": "p2", "project": "fno", "status": "ready", "type": "task",
                    "roadmap_id": "rm-1", "created_at": c.iso(2)}),
                serde_json::json!({"id": "x-rm2", "slug": "s2", "title": "Other roadmap",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "roadmap_id": "rm-2", "created_at": c.iso(1)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        Case {
            name: "mission",
            flags: &["--mission", "m-1"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-ms1", "slug": "s1", "title": "In mission",
                    "priority": "p2", "project": "fno", "status": "ready", "type": "task",
                    "mission_id": "m-1", "created_at": c.iso(2)}),
                serde_json::json!({"id": "x-ms2", "slug": "s2", "title": "Out of mission",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "created_at": c.iso(1)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // Epic scope: transitive children of the parent, deeper levels too.
        Case {
            name: "parent_scope",
            flags: &["--parent", "x-epic1"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-epic1", "slug": "e1", "title": "Epic", "priority": "p1",
                    "project": "fno", "status": "ready", "type": "epic", "created_at": c.iso(9)}),
                serde_json::json!({"id": "x-c1", "slug": "c1", "title": "Child", "priority": "p1",
                    "project": "fno", "status": "ready", "type": "task", "parent": "x-epic1",
                    "created_at": c.iso(2)}),
                serde_json::json!({"id": "x-g1", "slug": "g1", "title": "Grandchild", "priority": "p0",
                    "project": "fno", "status": "ready", "type": "task", "parent": "x-c1",
                    "created_at": c.iso(2)}),
                ready_row("x-out1", "p0", "fno", 1, c),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // A live claim hides the node from selection (same fixture also
        // proves the empty-claims default reads as no claims).
        Case {
            name: "live_claimed",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                ready_row("x-lc1", "p2", "fno", 2, c),
                ready_row("x-lc2", "p1", "fno", 2, c),
            ],
            plans: &[],
            claims: &["x-lc2"],
            expect_err: false,
        },
        // A ready node with an unmerged open PR is work in review, not
        // waiting work.
        Case {
            name: "open_pr",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-op1", "slug": "s1", "title": "Has PR",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "pr_number": 123, "created_at": c.iso(2)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // Containers are never actionable work: the box vs its leaves.
        Case {
            name: "containers",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-ct1", "slug": "e1", "title": "Epic container",
                    "priority": "p0", "project": "fno", "status": "ready", "type": "epic",
                    "created_at": c.iso(9)}),
                serde_json::json!({"id": "x-ct2", "slug": "c1", "title": "Leaf",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "parent": "x-ct1", "created_at": c.iso(2)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // Batch members ship via the batch PR, not as individual ready work.
        Case {
            name: "batched",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-bt1", "slug": "s1", "title": "Batched",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "batch": "bt-1", "created_at": c.iso(2)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // Contained work is delivered inside another node's PR.
        Case {
            name: "contained",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-cd1", "slug": "s1", "title": "Contained",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "contained_in": "x-owner1", "created_at": c.iso(2)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // A superseded/deferred ancestor quarantines the subtree.
        Case {
            name: "dead_ancestor",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-da-p", "slug": "p", "title": "Deferred epic",
                    "priority": "p1", "project": "fno", "status": "deferred", "type": "epic",
                    "created_at": c.iso(9)}),
                serde_json::json!({"id": "x-da-c", "slug": "c", "title": "Child under dead epic",
                    "priority": "p0", "project": "fno", "status": "ready", "type": "task",
                    "parent": "x-da-p", "created_at": c.iso(2)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // A ready-status node whose plan doc says `design` parks.
        Case {
            name: "design_stage",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-ds1", "slug": "s1", "title": "Design doc",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "plan_path": format!("{}/design-plan.md", c.planroot.display()), "created_at": c.iso(2)}),
                // A repo-relative plan_path on a node with no cwd: the probe
                // refuses to guess, the rung answers unreadable, and the node
                // is SELECTED (fail open).
                serde_json::json!({"id": "x-ds2", "slug": "s2", "title": "Unanchored relative plan",
                    "priority": "p2", "project": "fno", "status": "ready", "type": "task",
                    "plan_path": "plans/design-plan.md", "created_at": c.iso(2)}),
            ],
            plans: &[PlanDoc {
                name: "design-plan.md",
                contents: "---\nstatus: design\n---\n\n# Design\n",
                age_days: 0,
            }],
            claims: &[],
            expect_err: false,
        },
        // `status: stub` (the retired scaffold spelling) reads idea and
        // parks exactly like a declared idea rung.
        Case {
            name: "idea_stub",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-is1", "slug": "s1", "title": "Stub doc",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "plan_path": format!("{}/stub-plan.md", c.planroot.display()), "created_at": c.iso(2)}),
            ],
            plans: &[PlanDoc {
                name: "stub-plan.md",
                contents: "---\nstatus: stub\n---\n\n# Stub\n",
                age_days: 0,
            }],
            claims: &[],
            expect_err: false,
        },
        // A plan-less idea is cold-dispatchable: admitted without --ideas.
        // Carries an intake difficulty now: the no-difficulty guard drops an
        // underivable row on the cold path, so the characterization case
        // pins admission WITH a derivable command.
        Case {
            name: "plan_less_idea",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-pl1", "slug": "s1", "title": "Planless",
                    "priority": "p1", "project": "fno", "status": "idea", "type": "task",
                    "difficulty": "low",
                    "created_at": c.iso(2)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // Old, unmoved, plan-bearing: quarantined. The plan mtime is set 400
        // days back so the freshness probe does not read as movement.
        Case {
            name: "stale_ready",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-sr1", "slug": "s1", "title": "Stale",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task",
                    "plan_path": format!("{}/ready-plan.md", c.planroot.display()),
                    "created_at": c.iso(400)}),
            ],
            plans: &[PlanDoc {
                name: "ready-plan.md",
                contents: "---\nstatus: ready\n---\n\n# Ready\n",
                age_days: 400,
            }],
            claims: &[],
            expect_err: false,
        },
        // No parseable created_at: never quarantined on uncertainty.
        Case {
            name: "no_created_at",
            flags: &["-A"],
            entries: |_c: &Ctx| vec![
                serde_json::json!({"id": "x-nc1", "slug": "s1", "title": "No stamp",
                    "priority": "p1", "project": "fno", "status": "ready", "type": "task"}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // A curated rank pin outranks unranked nodes.
        Case {
            name: "ranked",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                ready_row("x-rk1", "p1", "fno", 1, c),
                {
                    let mut e = ready_row("x-rk2", "p3", "fno", 2, c);
                    e["rank"] = serde_json::json!(1.5);
                    e
                },
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // Review-found epic-group ordering: a ranked child must NOT float
        // its group ahead of a ranked loose node (the group's leading band
        // is the EPIC's), and the in-progress probe names the candidate's
        // PARENT (an epic id), never the candidate's own id.
        Case {
            name: "epic_group_leading_band",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                {
                    let mut e = ready_row("x-gb-l", "p2", "fno", 3, c);
                    e["rank"] = serde_json::json!(1.5);
                    e
                },
                serde_json::json!({"id": "x-gb-e", "slug": "e1", "title": "Unranked epic", "priority": "p1",
                    "project": "fno", "status": "ready", "type": "epic", "created_at": c.iso(9)}),
                {
                    let mut e = ready_row("x-gb-rc", "p1", "fno", 2, c);
                    e["rank"] = serde_json::json!(1.5);
                    e["parent"] = serde_json::json!("x-gb-e");
                    e
                },
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        Case {
            name: "epic_progress_probe_parent",
            flags: &["-A"],
            entries: |c: &Ctx| vec![                serde_json::json!({"id": "x-pp-g", "slug": "g", "title": "Grand epic", "priority": "p1",
                    "project": "fno", "status": "ready", "type": "epic", "created_at": c.iso(9)}),
                {
                    let mut e = ready_row("x-pp-h", "p1", "fno", 2, c);
                    e["rank"] = serde_json::json!(1.5);
                    e["parent"] = serde_json::json!("x-pp-g");
                    e
                },
                serde_json::json!({"id": "x-pp-c", "slug": "c", "title": "Mid epic", "priority": "p2",
                    "project": "fno", "status": "ready", "type": "epic", "parent": "x-pp-g",
                    "created_at": c.iso(3)}),
                serde_json::json!({"id": "x-pp-d", "slug": "d", "title": "Done grandchild", "priority": "p2",
                    "project": "fno", "status": "done", "type": "task", "parent": "x-pp-c",
                    "contained_in": "x-pp-c", "completed_at": c.iso(1), "created_at": c.iso(4)}),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // Exact id resolution is format-agnostic (a fixture/legacy id with
        // non-hex characters resolves as --parent), and the hex gate only
        // governs the partial-prefix convenience.
        Case {
            name: "parent_fixture_id",
            flags: &["-A", "--parent", "ab-epic"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "ab-epic", "slug": "e", "title": "Mission", "priority": "p1",
                    "project": "fno", "status": "ready", "type": "epic", "created_at": c.iso(9)}),
                ready_row("x-fx1", "p1", "fno", 2, c),
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // The full sort key over one graph: epics-first tiers, in-progress
        // epic groups, fan-out, orphans last, encounter evidence.
        Case {
            name: "ordering",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                // Epic 1 (p1) with a done child -> child_progress set, epic
                // not in_progress.
                serde_json::json!({"id": "x-oe1", "slug": "e1", "title": "Epic one", "priority": "p1",
                    "project": "fno", "status": "ready", "type": "epic", "created_at": c.iso(9)}),
                serde_json::json!({"id": "x-oc1", "slug": "c1", "title": "Done child", "priority": "p2",
                    "project": "fno", "status": "done", "type": "task", "parent": "x-oe1",
                    "completed_at": c.iso(1), "created_at": c.iso(8)}),
                serde_json::json!({"id": "x-oc2", "slug": "c2", "title": "Ready child of e1", "priority": "p2",
                    "project": "fno", "status": "ready", "type": "task", "parent": "x-oe1",
                    "created_at": c.iso(7)}),
                // Epic 2 (p2) with an in_progress child -> in-progress group
                // leads among unranked epic groups.
                serde_json::json!({"id": "x-oe2", "slug": "e2", "title": "Epic two", "priority": "p2",
                    "project": "fno", "status": "ready", "type": "epic", "created_at": c.iso(8)}),
                serde_json::json!({"id": "x-oc3", "slug": "c3", "title": "Moving child", "priority": "p2",
                    "project": "fno", "status": "in_progress", "type": "task", "parent": "x-oe2",
                    "created_at": c.iso(6)}),
                serde_json::json!({"id": "x-oc4", "slug": "c4", "title": "Ready child of e2", "priority": "p0",
                    "project": "fno", "status": "ready", "type": "task", "parent": "x-oe2",
                    "created_at": c.iso(5)}),
                // Ranked loose node: band 0 beats every unranked node.
                {
                    let mut e = ready_row("x-ol1", "p3", "fno", 3, c);
                    e["rank"] = serde_json::json!(2.0);
                    e
                },
                // Fan-out: an open node waits on x-of1.
                {
                    let mut e = ready_row("x-of1", "p2", "fno", 4, c);
                    e["blocked_by"] = serde_json::json!([]);
                    e
                },
                {
                    let mut e = ready_row("x-of2", "p2", "fno", 4, c);
                    e["blocked_by"] = serde_json::json!(["x-of1"]);
                    e
                },
                // Orphan: feature-typed, no mission edge.
                serde_json::json!({"id": "x-oo1", "slug": "s1", "title": "Orphan feature",
                    "priority": "p2", "project": "fno", "status": "ready", "type": "feature",
                    "created_at": c.iso(4)}),
                // Encounter evidence: two voters, unvoted rows score zero.
                {
                    let mut e = ready_row("x-ov1", "p2", "fno", 4, c);
                    e["encounters"] = serde_json::json!([
                        {"ts": c.iso(3), "session_id": "voter-a", "evidence": "cost a rebase"},
                        {"ts": c.iso(2), "session_id": "voter-b", "evidence": "wedged a review"},
                    ]);
                    e
                },
            ],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        Case {
            name: "empty_graph",
            flags: &["-A"],
            entries: |_c: &Ctx| vec![],
            plans: &[],
            claims: &[],
            expect_err: false,
        },
        // HELD and INVALID holds both park; the guard reason names the owner.
        Case {
            name: "dispatch_hold",
            flags: &["-A"],
            entries: |c: &Ctx| vec![
                serde_json::json!({"id": "x-hd1", "slug": "s1", "title": "Held",
                    "priority": "p0", "project": "fno", "status": "ready", "type": "task",
                    "plan_path": format!("{}/held-plan.md", c.planroot.display()), "created_at": c.iso(2)}),
                serde_json::json!({"id": "x-hd2", "slug": "s2", "title": "Invalid hold",
                    "priority": "p0", "project": "fno", "status": "ready", "type": "task",
                    "plan_path": format!("{}/invalid-hold-plan.md", c.planroot.display()), "created_at": c.iso(2)}),
                serde_json::json!({"id": "x-hd3", "slug": "s3", "title": "Free sibling",
                    "priority": "p2", "project": "fno", "status": "ready", "type": "task",
                    "created_at": c.iso(2)}),
            ],
            plans: &[
                PlanDoc {
                    name: "held-plan.md",
                    contents: "---\nstatus: ready\ndispatch_hold:\n  reason: waiting on legal\n  release_when: legal clears\n  review_on: 2099-01-01\n  set_by: operator\n---\n\n# Held\n",
                    age_days: 0,
                },
                PlanDoc {
                    name: "invalid-hold-plan.md",
                    contents: "---\nstatus: ready\ndispatch_hold:\n  reason: waiting on legal\n---\n\n# Invalid\n",
                    age_days: 0,
                },
            ],
            claims: &[],
            expect_err: false,
        },
        // --parent on an absent id refuses (exit 1, empty stdout).
        Case {
            name: "parent_missing",
            flags: &["--parent", "x-absent"],
            entries: |c: &Ctx| vec![ready_row("x-pm1", "p1", "fno", 2, c)],
            plans: &[],
            claims: &[],
            expect_err: true,
        },
    ]
}

// ---------------------------------------------------------------------------
// Materialization + the two legs
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// One materialized case: temp dir with graph.json + plan docs + claims.
struct Materialized {
    dir: tempfile::TempDir,
}

fn materialize(case: &Case) -> (Materialized, Ctx) {
    let dir = tempfile::tempdir().expect("tempdir");
    let planroot = dir.path().join("plans");
    std::fs::create_dir_all(&planroot).unwrap();
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    std::fs::create_dir_all(dir.path().join("claims-root/.fno/claims")).unwrap();
    let now_ms = now_ms();
    let ctx = Ctx {
        planroot: planroot.clone(),
        repo: canonical_repo_root(),
        now_ms,
    };
    let entries = (case.entries)(&ctx);
    fno_agents::graph_store::seed_rows(&dir.path().join("graph.json"), &entries)
        .expect("seed graph.db");
    for plan in case.plans {
        let path = planroot.join(plan.name);
        std::fs::write(&path, plan.contents).unwrap();
        if plan.age_days > 0 {
            let mtime = std::time::UNIX_EPOCH
                + Duration::from_millis((now_ms - plan.age_days * 86_400_000) as u64);
            let f = std::fs::File::open(&path).unwrap();
            f.set_modified(mtime).unwrap();
        }
    }
    for node_id in case.claims {
        let held = fno_agents::claims::acquire(
            &format!("node:{node_id}"),
            &format!("parity-{node_id}"),
            fno_agents::claims::AcquireOpts {
                pid: Some(std::process::id()),
                ttl_ms: Some(3_600_000),
                root: Some(dir.path().join("claims-root")),
                events_dir: Some(dir.path().to_path_buf()),
                ..Default::default()
            },
        );
        assert!(
            matches!(held, fno_agents::claims::AcquireOutcome::Acquired(_)),
            "{held:?}"
        );
    }
    std::fs::write(
        dir.path().join("config.toml"),
        format!("state_dir = \"{}\"\n", dir.path().display()),
    )
    .unwrap();
    (Materialized { dir }, ctx)
}

/// Env for both Python legs: the fixture graph pinned by config, claims
/// redirected, home sandboxed, hermetic pins off.
fn rust_rows(ctx: &Ctx, case: &Case, dir: &Path) -> Value {
    let graph = dir.join("graph.json");
    let entries = fno_agents::graph_store::read_rows(&graph).expect("rust read");
    let mut claimed = std::collections::BTreeSet::new();
    if !case.claims.is_empty() {
        let dirs = vec![dir.join("claims-root/.fno/claims")];
        for rec in fno_agents::claims::list_in(&dirs, Some("node:"), false).expect("claims read") {
            if let Some(id) = rec.key.strip_prefix("node:") {
                claimed.insert(id.to_string());
            }
        }
    }
    let flag = |name: &str| case.flags.contains(&name);
    let opt_value = |name: &str| -> Option<String> {
        case.flags
            .iter()
            .position(|f| *f == name)
            .map(|i| case.flags[i + 1].to_string())
    };
    let opts = fno_agents::backlog_ready::ReadyOpts {
        project: opt_value("-p"),
        all: flag("-A") || flag("--all"),
        roadmap_id: opt_value("--roadmap-id"),
        parent: opt_value("--parent"),
        mission: opt_value("--mission"),
        include_ideas: flag("--ideas") || flag("--include-ideas") || flag("-I"),
        include_deferred: flag("--include-deferred"),
        repo_root: Some(ctx.repo.display().to_string()),
        staleness_days: None,
        claimed,
        held: Default::default(),
        now_ms: ctx.now_ms,
    };
    match fno_agents::backlog_ready::select(&entries, &opts) {
        Ok(reply) => {
            serde_json::json!({ "rows": reply.rows, "drops": reply.drops.iter().map(|d| serde_json::json!({"id": d.id, "filter": d.filter, "reason": d.reason})).collect::<Vec<_>>() })
        }
        Err(_) => serde_json::json!({ "error": "no-such-parent" }),
    }
}

/// Values that ride the projection but move with the run: the two
/// now()-relative stamps, and the two path fields (tempdir-rooted plan
/// paths, the canonical repo root as cwd). Normalized before comparison so
/// a golden frozen on one machine and clock compares byte-equal.
fn normalize_volatile(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                let volatile = match k.as_str() {
                    "created_at" | "touched_at" | "parented_at" => val.is_string(),
                    "plan_path" | "cwd" => val.is_string(),
                    _ => false,
                };
                if volatile {
                    out.insert(k.clone(), Value::String("<TS>".into()));
                } else {
                    out.insert(k.clone(), normalize_volatile(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize_volatile).collect()),
        other => other.clone(),
    }
}

/// The rows value carried a refusal: the golden surface is the exit code.
fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/backlog_ready")
}

#[test]
fn characterization_ready_selection_matches_the_frozen_goldens() {
    for case in cases() {
        let (fixture, ctx) = materialize(&case);
        let rs = rust_rows(&ctx, &case, fixture.dir.path());
        if case.expect_err {
            assert!(rs.get("error").is_some(), "[{}] must refuse", case.name);
            continue;
        }
        let golden = std::fs::read_to_string(golden_dir().join(format!("{}.out", case.name)))
            .expect("frozen rows");
        assert_eq!(
            normalize_volatile(&rs["rows"]),
            serde_json::from_str::<Value>(&golden).expect("frozen rows JSON"),
            "[{}] survivor contract",
            case.name
        );
        let actual: BTreeMap<String, String> = rs["drops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["id"].as_str().unwrap().to_string(),
                    d["filter"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let expected: BTreeMap<String, String> = serde_json::from_str(
            &std::fs::read_to_string(golden_dir().join(format!("{}.drops.json", case.name)))
                .expect("frozen drops"),
        )
        .unwrap();
        assert_eq!(actual, expected, "[{}] drop attribution", case.name);
    }
}
