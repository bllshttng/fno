//! `fno-agents backlog <group> <action> ...` - one grouped backlog
//! dispatcher, the fold of the four direct actions (`backlog-update`,
//! `backlog-note`, `backlog-notes`, `graph-get`) into the canonical catalog.
//!
//! Transport-only, like `sync-canonical`: it registers no client action in
//! `ALL_CLIENT_ACTIONS` (the shrink law allows no new one), and the `fno`
//! front door execs this binary with `backlog` as the first token, so the
//! public `fno backlog` namespace lands here natively. The catalog in
//! [`super::commands`] owns every accepted spelling: grouped
//! (`backlog add node ...`) and legacy (`backlog add ...`) both resolve to
//! the legacy command the actions historically ran under.
//!
//! Every catalog command has a native arm. The folded engines keep their exact
//! argv contracts, so internal callers (the mux snapshot read) move by swapping
//! the first tokens. A word no arm owns answers from the tombstones.

use super::commands::{self, GROUPS};

/// The engine doors' argv shape: every native-engine caller (the Python
/// bridges, the note passthrough, the status fanout) passes `--graph` as
/// the FIRST tail token, and the public full-command surface never leads
/// with it - `note`'s extra-tolerant parser would otherwise swallow a stray
/// engine spelling as an ignored extra, so the first token is the one
/// discriminator that cannot collide with a user invocation.
const ENGINE_DOOR_HEAD: &str = "--graph";

/// One resolved invocation: the legacy command the catalog maps it to (a
/// `'static` catalog name), and the untouched tail after the command word.
struct Resolved<'a> {
    legacy: &'static str,
    tail: &'a [String],
}

/// Native auxiliary vocabulary: commands the dispatcher routes by engine
/// contract that the Python catalog never carried (the note corpus/history
/// reader rode the folded `backlog-notes` action, not a catalog command).
const NATIVE_AUX: &[&str] = &[
    "freshness",
    "notes",
    "target-binding",
    "birth-hook",
    "script-door",
];

const DECIDE_RETRACT_HELP: &str = "Usage: fno backlog decide-retract <decision-id> --reason TEXT

Retract one recorded decision. A law stays operator-only. The record stays; the
retraction is a new row beside it.
";

/// Resolve the head of `args` to a legacy command. Grouped spelling first
/// (`<group> <action>`, only when the second token IS one of that group's
/// actions - several group names are also legacy commands: `fno backlog get
/// x` is the legacy `get`, not a malformed grouped call), then the bare
/// legacy command itself, then the native aux vocabulary.
fn resolve_head(args: &[String]) -> Result<Resolved<'_>, String> {
    let first = match args.first() {
        Some(a) => a.as_str(),
        None => return Err(help_text()),
    };
    if let Some(aux) = NATIVE_AUX.iter().find(|a| **a == first) {
        return Ok(Resolved {
            legacy: aux,
            tail: &args[1..],
        });
    }
    if let Some(group) = GROUPS.iter().find(|g| g.name == first) {
        if let Some(action) = group
            .actions
            .iter()
            .find(|(a, _)| Some(*a) == args.get(1).map(String::as_str))
            .map(|(_, legacy)| *legacy)
        {
            return Ok(Resolved {
                legacy: action,
                tail: &args[2..],
            });
        }
    }
    match commands::from_legacy(first) {
        Some((group, action)) => Ok(Resolved {
            legacy: commands::resolve(group, action).expect("catalog round-trip"),
            tail: &args[1..],
        }),
        // Unknown and retired names fall to the tombstone arm. The runtime
        // name leaks into the 'static slot - at most once per process.
        None => Ok(Resolved {
            legacy: Box::leak(first.to_string().into_boxed_str()),
            tail: &args[1..],
        }),
    }
}

fn help_text() -> String {
    let mut out = String::from("fno backlog <group> <action> [args...]\n");
    out.push_str("       fno backlog <legacy-command> [args...]  (compatibility spelling)\n\n");
    for g in GROUPS {
        out.push_str(&format!(
            "  {:<8} {:<32} {}\n",
            g.name,
            g.purpose,
            g.actions
                .iter()
                .map(|(a, _)| *a)
                .collect::<Vec<_>>()
                .join("|")
        ));
    }
    out
}

/// Whether any engine marker appears in `tail`.
fn carries(tail: &[String], markers: &[&str]) -> bool {
    tail.iter().any(|a| markers.contains(&a.as_str()))
}

/// The run entry: returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let head = args.first().map(String::as_str);
    if head.is_none() || head == Some("-h") || head == Some("--help") {
        println!("{}", help_text());
        return 0;
    }
    // A leading option is the backlog app's own flag (`fno backlog --json
    // decompose ...`): the one app flag rides to the command's own tail.
    if is_app_option_head(args) {
        let flag = args[0].as_str();
        if flag != "--json" && flag != "-J" {
            eprintln!("Error: No such option: {flag}");
            return 2;
        }
        let mut native = args[1..].to_vec();
        if native.is_empty() {
            eprintln!("{}", help_text());
            return 2;
        }
        native.push(args[0].clone());
        return run(&native);
    }
    let resolved = match resolve_head(args) {
        Ok(r) => r,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    // The ported tracker-owned commands own graph state: under an external
    // backend they refuse before any read or write. Help still answers.
    if matches!(
        resolved.legacy,
        "archive"
            | "unarchive"
            | "maintain"
            | "migrate-priorities"
            | "migrate-difficulty"
            | "migrate-updated-at"
            | "retro"
    ) && !carries(resolved.tail, &["--help", "-h"])
    {
        if let Some(code) = super::workflows::refuse_non_graph_backend(resolved.legacy) {
            return code;
        }
    }
    match resolved.legacy {
        // The folded corpus/history reader: fully native, engine owns the
        // whole surface (inventory, migrate, history, stale, findings).
        "notes" => super::note_migrate::run_notes(resolved.tail),
        // Internal: target init and blueprint step 2a ask whether a node's
        // premise, or its plan, still holds against current main. Advisory:
        // every verdict exits 0 and the one write is the verdict event.
        // Not a catalog command, so it adds nothing to the menu.
        "freshness" => super::freshness::run(resolved.tail),
        // Internal: target init/start and the init hook ask which node a run
        // may bind. Not a catalog command, so it adds nothing to the menu.
        "target-binding" => super::target_binding::run(resolved.tail),
        // Internal: the Python birth forwarder execs the one birth hook here
        // (law d-e11b2b3e: the twin is gone, the door is the seam). Not a
        // catalog command, so it adds nothing to the menu.
        "birth-hook" => super::birth::run_birth_hook(resolved.tail),
        // A leading `--graph` is the engine door's shape (passthrough, status
        // fanout); the plain shape is the public face.
        "note" if leads_with_engine_door(resolved.tail) => super::note_cli::run_note(resolved.tail),
        "note" => super::note_public::run(resolved.tail),
        // The comment thread: the catalog folds `note comment` to the
        // legacy word `comment` (commands.rs), so this arm, not a `note`
        // tail probe, is what the receipts' own suggested command rides.
        "comment" => super::note_cli::run_comment(resolved.tail),
        // The folded patch door: the same leading-`--graph` contract the
        // lifecycle door speaks. The full public flag surface stays
        // Python-owned until its port.
        "update" if leads_with_engine_door(resolved.tail) => {
            super::patch::run_update(resolved.tail)
        }
        // The legacy field-flag surface: native when every flag in the tail
        // is one the port owns, else the compat forward with the original
        // argv, so the not-yet-ported flags keep their Python answers until
        // their wave lands.
        "update" => super::update_cli::run(resolved.tail),
        // The folded batch read: the engine contract is a leading or
        // trailing `--graph`, several ids without the single-id render
        // flags, or the stdin tracker door (zero positionals: the engine's
        // own stdin arm decides). The single-id render ladder (field / grouped
        // / tiers / archive walk) answers everything else, help included.
        "get" if is_engine_get(resolved.tail) => crate::graph_get::run_graph_get(resolved.tail),
        "get" => super::get_cli::run(resolved.tail),
        // The search ladder is native; the FTS cache and the external
        // backend reads forward from inside.
        "find" => super::find_cli::run(resolved.tail),
        // The planned-vs-actual read is native end to end: no Python twin
        // ever existed (new Python surface is barred, law d-450caaeb).
        "calibration" => super::calibration_cli::run(resolved.tail),
        // The lane pin is native: id gate, operator fence, lane/epic scoping
        // and midpoint arithmetic over the single-row write seam.
        "rank" => super::rank_cli::run(resolved.tail),
        // The vote is native: identity, style gate and the one same-voter
        // refusal over the single-row write seam. No Python twin remains.
        "encounter" => super::encounter_cli::run(resolved.tail),
        // The create commands are native end to end as of this wave: parse,
        // gates, locked write with the rollup inside it, receipts.
        "add" => super::create_cli::run(resolved.tail),
        "idea" => super::create_cli::run_idea(resolved.tail),
        // The retraction door is native: every lane through the same door
        // `fno inbox law retract` uses, laws still operator-only. The
        // Python twin this arm replaces is deleted in the same change
        // (d-e11b2b3e).
        "decide-retract" if carries(resolved.tail, &["--help", "-h"]) => {
            print!("{DECIDE_RETRACT_HELP}");
            0
        }
        "decide-retract" => crate::law_match::run_backlog_retract(resolved.tail),
        // The decisions listing is native: the Python twin is deleted in the
        // same change (d-e11b2b3e), so the door owns every shape, help
        // included.
        "decisions" => super::decisions_cli::run(resolved.tail),
        // The decide record command is native end to end: the same door
        // `fno inbox decide` uses, every lane, the Python twin deleted in
        // the same change.
        "decide" => crate::decide_door::run_decide_door(resolved.tail),
        // The recovery backfill is native: compact + journal fold, the port
        // of the deleted Python `reindex`.
        "decide-reindex" => crate::decide_door::run_decide_reindex(resolved.tail),
        // The blueprint session lifecycle is native end to end: identity,
        // claims substrate, session rows, receipts. The Python twin is gone.
        "session" => super::session_cli::run(resolved.tail),
        // The canonical close and its inverse; `done` hands the completion
        // flags to `done_rich` from inside the arm.
        "done" => super::workflows::run_done(resolved.tail),
        "reopen" => super::workflows::run_reopen(resolved.tail),
        "queue" => super::workflows::run_queue(resolved.tail),
        "unqueue" => super::workflows::run_unqueue(resolved.tail),
        "queued" => super::workflows::run_queued(resolved.tail),
        "contain" => super::workflows::run_contain(resolved.tail),
        "supersede" => super::workflows::run_supersede(resolved.tail),
        // The worked authority is native end to end: fleet rows, roster
        // reading, reachability, and the session-row fold. The Python twin
        // (statuses.live_worked_node_ids over graph/worked.py) keeps its
        // callers (the next/undispatched occupancy legs) until their doors
        // go native.
        "undispatched" => super::undispatched::run(resolved.tail),
        "next" => super::next::run(resolved.tail),
        // The drift closer is native end to end: scan, promise gate, one
        // locked close, the self-heals, and the shelled legs (auto-continue
        // rides the wheel until the advance port).
        "reconcile" => super::reconcile_cli::run(resolved.tail),
        "advance" => super::advance_cli::run(resolved.tail),
        "join" => super::advance_join::run(resolved.tail),
        "dispatch-lanes" => super::advance_lanes::run_lanes(resolved.tail),
        // The parallel fill: native selection over the lane world (peer
        // slots, domain annotation, the file-collision gate).
        "lane-fill" => super::advance_fill::run_lane_fill(resolved.tail),
        "schedule-shadow" => super::advance_fill::run_schedule_shadow(resolved.tail),
        "worked" => super::worked::run(resolved.tail),
        // The launch-edge writers (the first-launch stamp and the receipted
        // backfill) beside the read shape.
        "provenance" if super::provenance_cli::owns(resolved.tail) => {
            super::provenance_cli::run(resolved.tail)
        }
        "provenance" => super::provenance_read::run(resolved.tail),
        // The triage diagnostics group is native end to end: the nine
        // actions over the engine, health, and the locked apply. The door
        // owns every triage tail: the wheel's triage route forwards here,
        // so a forward back would loop.
        "triage" => super::triage_cli::run(resolved.tail),
        // The projections: the shared kanban classifier, the post-publish
        // view pass, and the read commands over it.
        "render-views" => super::projections::run_render_views(resolved.tail),
        "view" => super::projections::run_view(resolved.tail),
        "board" => super::projections::run_board(resolved.tail),
        "bases" => super::projections::run_bases(resolved.tail),
        "roadmap" => super::projections::run_roadmap(resolved.tail),
        "album" => super::projections::run_album(resolved.tail),
        // Archive residency, the hygiene pass, the daily groom, and the
        // one-shot row migrations.
        "archive" => super::maintenance_archive::run_archive(resolved.tail),
        "unarchive" => super::maintenance_archive::run_unarchive(resolved.tail),
        "maintain" => super::maintenance::run_maintain(resolved.tail),
        "groom" => super::groom::run_groom(resolved.tail),
        "migrate-priorities" => super::migrations::run_migrate_priorities(resolved.tail),
        "migrate-difficulty" => super::migrations::run_migrate_difficulty(resolved.tail),
        "migrate-updated-at" => super::migrations::run_migrate_updated_at(resolved.tail),
        // The publish command is native end to end: the public projection
        // renders through `fno board-render`, and the three targets (static
        // dir, uploader command, GitHub Pages) are the command's own. The
        // catalog row sits in the board group.
        "publish" => super::publish::run(resolved.tail),
        // The owner-run import beside the publish command it serves: approved
        // issues in, quarantined intake rows out, dry run by default.
        "import-requests" => super::requests::run(resolved.tail),
        // The wave-4 cutover arms: catalog commands whose Python bodies were
        // thin clients of native machinery, served with no compat forward.
        // The full list of dispositions lives on the cutover module doc.
        "version" => super::cutover::run_version(resolved.tail),
        "status" => super::cutover::run_status(resolved.tail),
        "cost" => super::cutover::run_cost(resolved.tail),
        "demand" => super::cutover::run_demand(resolved.tail),
        "collisions" => super::cutover::run_collisions(resolved.tail),
        "backfill-deferred-kind" => super::cutover::run_backfill_deferred_kind(resolved.tail),
        "unclaim" => super::cutover::run_unclaim(resolved.tail),
        "defer" => super::cutover::run_defer(resolved.tail),
        "undefer" => super::cutover::run_undefer(resolved.tail),
        "retract" => super::cutover::run_retract(resolved.tail),
        "unsupersede" => super::cutover::run_unsupersede(resolved.tail),
        "relatedness" => super::cutover::run_relatedness(resolved.tail),
        "archive-dedupe-ids" => super::cutover::run_archive_dedupe_ids(resolved.tail),
        "ready" => super::cutover::run_ready(resolved.tail),
        "remove" => super::cutover::run_remove(resolved.tail),
        "project-root" => super::cutover::run_project_root(resolved.tail),
        // The finale arms (this PR): the remaining heavy commands, served with
        // no compat forward. The catalog-vs-arms gate (check-backlog-native
        // check 2) derives the unclaimed set mechanically and fails the tree
        // while any command still rides the catch-all.
        "new" => super::finale::run_new(resolved.tail),
        // The epic status table: children, receipts, streaks, verify-merges,
        // and the scope-growth metric over the archive-joined read.
        "epic" => super::epic::run_epic(resolved.tail),
        // The task grain: claim-gated status transitions over the rows the
        // bound plan's strategy declares.
        "task" => super::task::run_task(resolved.tail),
        // The batch lane: flock'd per-domain state, the daemon's prepare and
        // ship-closeable doors, and the one-PR ship orchestrator.
        "batch" => super::batch::run_batch(resolved.tail),
        "capture" => super::capture::run_capture(resolved.tail),
        "intake" => super::intake::run_intake(resolved.tail),
        "decompose" => super::decompose::run_decompose(resolved.tail),
        "lanes" => super::finale::run_lanes(resolved.tail),
        "stuck-epics" => super::finale::run_stuck_epics(resolved.tail),
        "wave-notes" => super::wave_notes::run_list(resolved.tail),
        "wave-dispose" => super::wave_notes::run_dispose(resolved.tail),
        "reprioritize" => super::finale::run_reprioritize(resolved.tail),
        "discover" => super::finale::run_discover(resolved.tail),
        "pick" => super::finale::run_pick(resolved.tail),
        "requeue" => super::finale::run_requeue(resolved.tail),
        "retro" => crate::retro::run(resolved.tail),
        "carveout" => super::carveout::run_carveout(resolved.tail),
        // Internal: the small facts scripts read. Not a catalog command.
        "script-door" => super::script_door::run(resolved.tail),
        _ => super::tombstones::refuse(args),
    }
}

/// Whether the invocation opens with the backlog app's own option rather
/// than a command (`fno backlog --json decompose ...`). Help was already
/// answered by the caller; the predicate stays honest standalone.
fn is_app_option_head(args: &[String]) -> bool {
    args.first()
        .is_some_and(|a| a.starts_with('-') && a != "-h" && a != "--help")
}

/// The engine door's get contract, help excluded: a leading or trailing
/// `--graph`, zero positionals (the stdin tracker door), or the batch shape
/// (several ids, no render flags). `--help`/`-h` anywhere in the tail keeps
/// the single-id path, whose parse-fail forward reaches the Python command's
/// real argument help.
fn is_engine_get(tail: &[String]) -> bool {
    !carries(tail, &["--help", "-h"])
        && (leads_with_engine_door(tail)
            || carries(tail, &["--graph"])
            || positional_count(tail) == 0
            || (positional_count(tail) > 1
                && !carries(tail, &["--field", "--grouped", "--strict"])))
}

/// Whether the tail opens with the engine door's `--graph` token.
fn leads_with_engine_door(tail: &[String]) -> bool {
    tail.first().map(String::as_str) == Some(ENGINE_DOOR_HEAD)
}

fn positional_count(tail: &[String]) -> usize {
    tail.iter().filter(|a| !a.starts_with('-')).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routed(args: &[&str]) -> Result<&'static str, String> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match resolve_head(&owned) {
            Ok(r) => Ok(r.legacy),
            Err(e) => Err(e),
        }
    }

    #[test]
    fn grouped_spellings_resolve_through_the_catalog() {
        assert_eq!(routed(&["add", "node", "t"]), Ok("add"));
        assert_eq!(routed(&["get", "node", "x-abc"]), Ok("get"));
        assert_eq!(routed(&["note", "state", "x"]), Ok("note"));
        assert_eq!(routed(&["update", "rank", "x"]), Ok("rank"));
    }

    #[test]
    fn an_unknown_command_keeps_its_name_for_the_tombstone_arm() {
        let owned: Vec<String> = ["frobnicate", "x-abc"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let r = resolve_head(&owned).unwrap();
        assert_eq!(r.legacy, "frobnicate");
        assert_eq!(r.tail, &["x-abc".to_string()]);
    }
}
