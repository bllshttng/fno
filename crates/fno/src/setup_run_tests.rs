//! Contracts for `fno config setup run`, replacing the deleted Python
//! wizard tests (cli/tests/unit/test_setup_wizard.py): the argv claim, the
//! no-prompt decision, the step table's askability, the report shape, the
//! markers, and the recommendation shaping. One table-driven fn per
//! contract family, so the suite stays under the test-delta cap.

use super::*;

fn oss(pieces: &[&str]) -> Vec<OsString> {
    pieces.iter().map(OsString::from).collect()
}

#[test]
fn the_argv_surface_parses_and_refuses() {
    // The lexical claim: run is ours; the siblings stay with their owners.
    assert!(classify(&oss(&["config", "setup", "run", "--yes"])).is_some());
    assert!(classify(&oss(&["config", "setup", "run"])).is_some());
    for sibling in [
        vec!["config", "setup", "auto-wire"],
        vec!["config", "setup", "plan"],
        vec!["config", "setup", "wizard"],
        vec!["config", "setup"],
    ] {
        assert!(classify(&oss(&sibling)).is_none());
    }
    // Defaults: the whole run, both layers, no flags, no preview.
    let o = parse_opts(&oss(&[])).unwrap();
    assert!(
        !o.yes
            && !o.once
            && !o.list
            && !o.json
            && !o.dry_run
            && !o.scan
            && o.scan_dirs.is_empty()
            && o.only.is_empty()
    );
    assert_eq!(o.scope, Scope::Both);
    // Every flag reads.
    let o = parse_opts(&oss(&[
        "--yes",
        "--once",
        "--scope",
        "global",
        "--only",
        "gh-auth,backlog-prefix",
        "--list",
        "--json",
        "--dry-run",
        "--scan-repos",
        "--scan-dirs",
        "code,src",
    ]))
    .unwrap();
    assert!(o.yes && o.once && o.list && o.json && o.dry_run && o.scan);
    assert_eq!(o.scan_dirs, vec!["code".to_string(), "src".to_string()]);
    assert_eq!(o.scope, Scope::Global);
    assert_eq!(o.only, vec!["gh-auth", "backlog-prefix"]);
    // Refusals: unknown flag, unknown scope, missing values.
    assert!(parse_opts(&oss(&["--bogus"])).is_err());
    assert!(parse_opts(&oss(&["--scope", "side"])).is_err());
    assert!(parse_opts(&oss(&["--scope"])).is_err());
    assert!(parse_opts(&oss(&["--only"])).is_err());
}

#[test]
fn the_no_prompt_decision_never_depends_on_one_signal() {
    // Explicit --yes wins even on a TTY.
    assert!(no_prompt(true, true, false));
    // A pipe on stdin takes the defaults without reading them.
    assert!(no_prompt(false, false, false));
    // An agent session env never sees a prompt.
    assert!(no_prompt(false, true, true));
    // A human on a TTY with no --yes is the only prompting shape.
    assert!(!no_prompt(false, true, false));
}

#[test]
fn step_selection_offers_both_layers_and_filters() {
    // The table is askable: unique ids, real question text, both layers
    // always offered (the plan forbids a choose-one).
    let mut ids: Vec<&str> = STEPS.iter().map(|s| s.id).collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate step id");
    for s in STEPS {
        assert!(!s.question.is_empty());
        assert!(!s.effect.is_empty());
    }
    assert!(STEPS.iter().any(|s| s.layer == Layer::Global));
    assert!(STEPS.iter().any(|s| s.layer == Layer::Project));
    // The service and report rows: the approve gate owns the first five's
    // proposals, the last two only ever report.
    for id in [
        "pr-watcher",
        "auto-heal",
        "groom-agent",
        "projects",
        "review-merge",
    ] {
        assert!(STEPS
            .iter()
            .any(|s| s.id == id && s.kind == Kind::Approve && s.layer == Layer::Global));
    }
    for id in ["machine-tools", "permission-rules"] {
        assert!(STEPS
            .iter()
            .any(|s| s.id == id && s.kind == Kind::Report && s.layer == Layer::Global));
    }
    // --only selects known ids and refuses unknown ones.
    let mut opts = Opts::default();
    opts.only = vec!["gh-auth".into(), "backlog-prefix".into()];
    assert_eq!(select_steps(&opts, Path::new(".")).unwrap().len(), 2);
    let mut bad = Opts::default();
    bad.only = vec!["no-such-step".into()];
    assert!(select_steps(&bad, Path::new("."))
        .unwrap_err()
        .contains("no-such-step"));
    // A global scope never carries project or contributor rows.
    let mut global = Opts::default();
    global.scope = Scope::Global;
    assert!(select_steps(&global, Path::new("."))
        .unwrap()
        .iter()
        .all(|s| s.layer == Layer::Global));
    // Every kind renders its stable name, approve included.
    assert_eq!(kind_name(Kind::Act), "act");
    assert_eq!(kind_name(Kind::Approve), "approve");
    assert_eq!(kind_name(Kind::Report), "report");
    assert_eq!(kind_name(Kind::Human), "human");
}

#[test]
fn the_report_shape_and_wire_fold_hold() {
    // The JSON report carries exactly the thirteen fields an agent parses.
    let mut rep = Report::default();
    rep.done
        .push("harness-wiring: Claude Code: installed".into());
    rep.skipped
        .push("accounts: review with `fno config accounts`".into());
    rep.needs_human.push("gh-auth: run `gh auth login`".into());
    rep.paths.push("/Users/x/.fno/config.toml".into());
    rep.restart_needed = true;
    let v = rep.to_json();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(keys.len(), 13);
    for k in [
        "schema",
        "run_id",
        "host",
        "status",
        "done",
        "skipped",
        "needs_human",
        "paths",
        "restart_needed",
        "proposed",
        "verified",
        "rows",
        "next_action",
    ] {
        assert!(keys.contains(&k), "missing {k}");
    }
    assert_eq!(v["schema"], json!(1));
    assert_eq!(v["host"], "agent");
    assert_eq!(v["status"], "finished");
    assert_eq!(v["restart_needed"], true);
    // Wire lines fold by their status words: an install is done plus a
    // restart ask, already-installed is done without one, and a manual
    // finish or failure blocks on a human.
    let mut rep = Report::default();
    fold_wire_line("wired the fno plugin into your agent CLIs:", &mut rep);
    fold_wire_line("  Claude Code: installed", &mut rep);
    fold_wire_line("  Codex CLI: already installed", &mut rep);
    fold_wire_line(
        "  pi: needs a manual finish - locate the extension",
        &mut rep,
    );
    fold_wire_line("  agy: FAILED (no hooks file)", &mut rep);
    fold_wire_line(
        "  no agent CLIs detected on PATH - nothing to wire.",
        &mut rep,
    );
    assert_eq!(rep.done.len(), 2);
    assert_eq!(rep.needs_human.len(), 2);
    assert_eq!(rep.skipped.len(), 1);
    assert!(rep.restart_needed, "a fresh install must ask for a restart");
    // Every blocking wire line carries a stable code, its fix, and the
    // needs_human list, in line order.
    let codes: Vec<&str> = rep.rows.iter().map(|r| r.code).collect();
    assert_eq!(codes, vec!["wiring_manual", "wiring_failed"]);
    assert!(rep.rows.iter().all(|r| !r.fix.is_empty()));
    assert!(rep.rows.iter().all(|r| r.list == "needs_human"));
    let mut rep = Report::default();
    fold_wire_line("  Codex CLI: already installed", &mut rep);
    assert!(
        !rep.restart_needed,
        "an already-installed line changed nothing"
    );
    // The dry-run envelope: seven keys, the first red row's fix as the
    // next action, the fallback when nothing blocks, and `services`
    // lifted out of detection (absent here, so null).
    assert!(!run_id().is_empty());
    assert!(CODES.iter().any(|(c, _)| *c == "gh_not_signed_in"));
    let rows = vec![row("needs_human", "gh_not_signed_in", "sign in".into())];
    assert_eq!(next_action(&rows, "fallback"), "gh auth login");
    assert_eq!(next_action(&[], "fallback"), "fallback");
    let proposed = vec![Proposal {
        id: "concurrency-band",
        key: "agents.max_live",
        value: "4".into(),
        scope: "global",
        reason: "the measured band".into(),
        source: "sample",
    }];
    let body = dry_run_json(json!({}), &proposed, &rows, &["sign in".into()], "fallback");
    let keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(keys.len(), 7);
    for k in [
        "detected",
        "proposed",
        "services",
        "needs_human",
        "key_gap",
        "rows",
        "next_action",
    ] {
        assert!(keys.contains(&k), "missing {k}");
    }
    assert_eq!(body["key_gap"], json!(1));
    assert_eq!(body["next_action"], "gh auth login");
    assert!(body["services"].is_null());
    let body = dry_run_json(json!({}), &[], &[], &[], "fallback");
    assert_eq!(body["next_action"], "fallback");
    assert_eq!(body["key_gap"], json!(0));
}

#[test]
fn markers_name_owned_dirs_and_stamp_iso() {
    // The global marker rides the already-inventoried sidecar/ subfolder:
    // the state root itself grows no new row (the freeze gate).
    assert_eq!(global_marker().file_name().unwrap(), "setup-done");
    assert_eq!(
        global_marker().parent().unwrap().file_name().unwrap(),
        "sidecar"
    );
    // The project marker sits beside the repo's config, not in the state root.
    assert_eq!(
        project_marker(Path::new("/tmp/some-repo")),
        Path::new("/tmp/some-repo/.fno/setup.done")
    );
    // The marker line carries the version and an ISO UTC stamp.
    let line = marker_line();
    assert!(line.starts_with("fno "));
    assert!(line.contains(env!("CARGO_PKG_VERSION")));
    assert!(line.ends_with('Z'));
    let stamp = utc_now();
    assert_eq!(stamp.len(), 20);
    let b = stamp.as_bytes();
    assert_eq!(b[4], b'-');
    assert_eq!(b[7], b'-');
    assert_eq!(b[10], b'T');
    assert_eq!(b[13], b':');
    assert_eq!(b[16], b':');
    assert_eq!(b[19], b'Z');
}

#[test]
fn the_recommendation_shapers_hold() {
    // The band-clause parser is the one both hosts share: the integer
    // after `budget max_live`, nothing else.
    assert_eq!(
        crate::setup_detect::parse_budget_clause("x · budget max_live 4 (2 leads + 2 workers)"),
        Some(4)
    );
    assert_eq!(
        crate::setup_detect::parse_budget_clause("no clause here"),
        None
    );
    // The guardrail proposal: the shipped default already reads standard,
    // so it never proposes (deterministic - the example ships compiled in).
    let mut skips = Vec::new();
    assert!(propose_guardrails(&mut skips).is_none());
    assert_eq!(skips, vec!["guardrails: guards.preset already standard"]);
    // A footnote checkout is the fno crate manifest under crates/.
    let dir = std::env::temp_dir().join(format!("fno-setup-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("crates/fno")).unwrap();
    assert!(!in_source_checkout(&dir), "no crate manifest yet");
    std::fs::write(dir.join("crates/fno/Cargo.toml"), "[package]\n").unwrap();
    assert!(in_source_checkout(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn once_needs_every_covered_layers_marker() {
    // A layer outside the scope never blocks --once; a covered layer
    // needs its own marker. This pins the inverted-condition fix.
    assert!(all_markers_present_in(Scope::Global, true, false));
    assert!(!all_markers_present_in(Scope::Global, false, true));
    assert!(all_markers_present_in(Scope::Project, false, true));
    assert!(!all_markers_present_in(Scope::Project, true, false));
    assert!(all_markers_present_in(Scope::Both, true, true));
    assert!(!all_markers_present_in(Scope::Both, true, false));
}
