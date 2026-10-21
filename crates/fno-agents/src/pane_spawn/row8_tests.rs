//! Row-8 golden parity: the six simple pane arms replayed against the
//! committed Python-leg captures in `tests/golden/pane_spawn/`. The provider
//! argv is compared from the first bare harness token onward, so the env
//! wrapper and the config-dependent qos prefix never gate the compare.
//!
//! Two legs touch the ambient machine the way the capture itself did: the
//! agy plan upserts its cwd key in `~/.gemini/trustedFolders.json`, and the
//! footnote plan writes its spec into the sessions root. The cursor-agent
//! mint and the footnote endpoint resolve need their ambient tools or env;
//! where they are absent the test names the skip instead of failing.
use super::env_wrap::enrich_spawn_payload;
use super::simple::{
    make_plan, SimpleArm, AGY_ARM, CURSOR_ARM, FOOTNOTE_ARM, GROK_ARM, OPENCODE_ARM, PI_ARM,
};
use super::tests::FakeMux;
use super::*;

const SPINE: &str = include_str!("../../tests/golden/pane_spawn/spine_python_leg.json");
const OPENCODE: &str = include_str!("../../tests/golden/pane_spawn/opencode_python_leg.json");
const PI: &str = include_str!("../../tests/golden/pane_spawn/pi_python_leg.json");
const CURSOR: &str = include_str!("../../tests/golden/pane_spawn/cursor-agent_python_leg.json");
const GROK: &str = include_str!("../../tests/golden/pane_spawn/grok_python_leg.json");
const AGY: &str = include_str!("../../tests/golden/pane_spawn/agy_python_leg.json");
const FOOTNOTE: &str = include_str!("../../tests/golden/pane_spawn/footnote_python_leg.json");

fn golden(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("expected a token list, got {v}"))
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

/// The provider argv inside a full `pane run` transport: everything from
/// the first bare harness token onward. The wrapper's tokens are all `-u`
/// unsets or `K=V` pairs, so a bare harness token can only be argv[0] of
/// the provider command, whatever the qos prefix did before it.
fn tail_from(argv: &[String], harness: &str) -> Vec<String> {
    let at = argv
        .iter()
        .position(|t| t == harness)
        .unwrap_or_else(|| panic!("no bare {harness} token in {argv:?}"));
    argv[at..].to_vec()
}

/// Drop the value token after each `flag`: the `--add-dir` grant names the
/// ambient state dirs the capture sandbox held.
fn strip_flag_values(tail: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(tail.len());
    let mut skip = false;
    for t in tail {
        if skip {
            skip = false;
            continue;
        }
        skip = t == flag;
        out.push(t.clone());
    }
    out
}

/// Swap the capture's `<UUID>` placeholder for the id this plan minted.
fn pin_uuid(tail: &[String], uuid: &str) -> Vec<String> {
    tail.iter()
        .map(|t| {
            if t == "<UUID>" {
                uuid.to_string()
            } else {
                t.clone()
            }
        })
        .collect()
}

/// The value following the first `flag` occurrence in an argv tail.
fn value_of(tail: &[String], flag: &str) -> Option<&String> {
    tail.iter()
        .position(|t| t == flag)
        .and_then(|i| tail.get(i + 1))
}

/// The plain spawn shape the goldens captured: cwd /repo, seed `do x`,
/// worker name `w`.
fn seeded(harness: &str) -> PaneContext {
    let mut ctx = PaneContext::simple(harness, "/repo");
    ctx.message = "do x".to_string();
    ctx.worker_name = "w".to_string();
    ctx
}

fn plan_of(arm: &SimpleArm, ctx: &PaneContext) -> ArmPlan {
    make_plan(arm, ctx).unwrap_or_else(|r| panic!("{} plan refused: {}", arm.harness, r.line))
}

#[test]
fn owns_cross_harness() {
    let arms: [&SimpleArm; 6] = [
        &OPENCODE_ARM,
        &PI_ARM,
        &CURSOR_ARM,
        &GROK_ARM,
        &AGY_ARM,
        &FOOTNOTE_ARM,
    ];
    for arm in arms {
        assert!(
            arm.owns(&PaneContext::simple(arm.harness, "/repo")),
            "{} must own its own provider",
            arm.harness
        );
        assert!(
            !arm.owns(&PaneContext::simple("claude", "/repo")),
            "{} must not own claude",
            arm.harness
        );
    }
}

#[test]
fn argv_parity_row8_goldens() {
    let enriched = enrich_spawn_payload("do x");

    // opencode: byte parity of the provider argv, plus the mesh wrapper
    // head the golden pins: `env` first, `FNO_HARNESS_NAME` the first
    // unset, the identity and substrate pairs, and an argv-committed seed
    // with no preassigned id.
    let opencode = golden(OPENCODE);
    let plan = plan_of(&OPENCODE_ARM, &seeded("opencode"));
    assert_eq!(
        tail_from(&plan.wrapped, "opencode"),
        tail_from(
            &str_list(&opencode["cases"]["opencode"]["pane_run_argv"]),
            "opencode"
        )
    );
    assert_eq!(plan.wrapped[0], "env");
    assert_eq!(plan.wrapped[1], "-u");
    assert_eq!(plan.wrapped[2], "FNO_HARNESS_NAME");
    for pair in [
        "FNO_AGENT_SELF=w",
        "FNO_AGENT_HARNESS=opencode",
        "FNO_AGENT_ROW_PENDING=w",
        "FNO_AGENT_SUBSTRATE=pane",
    ] {
        assert!(plan.wrapped.contains(&pair.to_string()), "missing {pair}");
    }
    assert_eq!(plan.held_seed, None);
    assert_eq!(plan.preassigned_id, None);

    // grok: the plain TUI takes no seed, so the argv is exactly ["grok"]
    // and the typed seed waits in the plan.
    let grok = golden(GROK);
    let plan = plan_of(&GROK_ARM, &seeded("grok"));
    assert_eq!(
        tail_from(&plan.wrapped, "grok"),
        tail_from(&str_list(&grok["cases"]["grok"]["pane_run_argv"]), "grok")
    );
    assert_eq!(
        tail_from(&plan.wrapped, "grok"),
        str_list(&grok["cases"]["argv"]["plain"])
    );
    assert_eq!(plan.held_seed.as_deref(), Some(enriched.as_str()));
    assert_eq!(plan.preassigned_id, None);

    // pi: the route tokens are ambient (pi's own settings may name the
    // provider), so the identity pair values, the pinned session pair and
    // the seed positional are pinned instead of the whole tail.
    let pi = golden(PI);
    let plan = plan_of(&PI_ARM, &seeded("pi"));
    let uuid = plan.preassigned_id.clone().unwrap_or_default();
    let tail = tail_from(&plan.wrapped, "pi");
    let expected = pin_uuid(
        &tail_from(&str_list(&pi["cases"]["pi"]["pane_run_argv"]), "pi"),
        &uuid,
    );
    for flag in ["--session-id", "--provider", "--model"] {
        assert_eq!(
            value_of(&tail, flag),
            value_of(&expected, flag),
            "flag {flag}"
        );
    }
    assert_eq!(tail.last(), expected.last());
    assert!(plan
        .wrapped
        .contains(&format!("FNO_HARNESS_SESSION_ID={uuid}")));

    // agy: the lane-default bypass is the only fixed token; the state-dir
    // grant values are ambient and stripped from both sides.
    let agy = golden(AGY);
    let plan = plan_of(&AGY_ARM, &seeded("agy"));
    assert_eq!(
        strip_flag_values(&tail_from(&plan.wrapped, "agy"), "--add-dir"),
        strip_flag_values(
            &tail_from(&str_list(&agy["cases"]["agy"]["pane_run_argv"]), "agy"),
            "--add-dir"
        )
    );
    assert_eq!(plan.held_seed.as_deref(), Some(enriched.as_str()));
    assert_eq!(plan.preassigned_id, None);

    // footnote: one launch spec, no session flag; the spec file name
    // carries the pinned uuid. The endpoint leg is ambient: a bare tree
    // has no provider record and no ANTHROPIC_BASE_URL, and the spec
    // write then degrades to that named refusal.
    let footnote = golden(FOOTNOTE);
    let mut modeled = PaneContext::simple("footnote", "/repo");
    modeled.passthrough = vec!["-m".to_string(), "glm-5.3".to_string()];
    match make_plan(&FOOTNOTE_ARM, &modeled) {
        Ok(plan) => {
            let tail = tail_from(&plan.wrapped, "footnote");
            assert_eq!(tail[0], "footnote");
            assert_eq!(tail[1], "--spec");
            let spec = tail.last().unwrap();
            assert!(spec.contains(".pane-spec-"), "{spec}");
            assert!(spec.ends_with(".json"), "{spec}");
            let uuid = plan.preassigned_id.clone().unwrap_or_default();
            assert!(
                spec.contains(&uuid),
                "spec {spec} must carry the pinned {uuid}"
            );
            assert!(plan
                .wrapped
                .contains(&format!("FNO_HARNESS_SESSION_ID={uuid}")));
        }
        Err(r) => {
            let ambient = r.line.contains("no model endpoint") || r.line.contains("no API key");
            assert!(
                ambient,
                "footnote plan refused for a non-ambient reason: {}",
                r.line
            );
            eprintln!(
                "skip: footnote spec shape needs a resolvable endpoint: {}",
                r.line
            );
        }
    }
    // The no-model refusal is byte-equal to the golden on every tree.
    let refused = match make_plan(&FOOTNOTE_ARM, &PaneContext::simple("footnote", "/repo")) {
        Ok(_) => panic!("footnote without a model must refuse"),
        Err(r) => r,
    };
    assert_eq!(
        refused.line,
        footnote["cases"]["argv"]["no_model"]["line"]
            .as_str()
            .unwrap()
    );
    assert_eq!(
        refused.exit_code,
        footnote["cases"]["argv"]["no_model"]["exit"]
            .as_i64()
            .unwrap() as i32
    );

    // cursor-agent: the argv parity runs wherever the create-chat mint
    // can run; a tree without the binary skips only this leg, last, so
    // the deterministic halves above always ran.
    let cursor = golden(CURSOR);
    match make_plan(&CURSOR_ARM, &seeded("cursor-agent")) {
        Ok(plan) => {
            let uuid = plan.preassigned_id.clone().unwrap_or_default();
            let got = strip_flag_values(&tail_from(&plan.wrapped, "cursor-agent"), "--add-dir");
            let want = strip_flag_values(
                &pin_uuid(
                    &tail_from(
                        &str_list(&cursor["cases"]["cursor-agent"]["pane_run_argv"]),
                        "cursor-agent",
                    ),
                    &uuid,
                ),
                "--add-dir",
            );
            assert_eq!(got, want);
            assert_eq!(plan.held_seed.as_deref(), Some(enriched.as_str()));
        }
        Err(r) => {
            assert!(
                r.line.contains("create-chat could not"),
                "cursor-agent plan refused for a non-ambient reason: {}",
                r.line
            );
            eprintln!(
                "skip: cursor-agent argv parity needs the create-chat mint: {}",
                r.line
            );
        }
    }
}

#[test]
fn gemini_refusal_names_the_deprecation() {
    let line = pane_refusal_for("gemini").expect("gemini must refuse with a named line");
    assert!(line.contains("deprecated 2026-07-27"), "{line}");
    assert!(line.contains("agy"), "{line}");
    assert!(pane_refusal_for("opencode").is_none());
    assert!(pane_refusal_for("agy").is_none());
}

#[test]
fn group_tab_split_and_conflict_line() {
    assert_eq!(
        placement::resolve_group_tab("delivery"),
        (None, Some("delivery".to_string()))
    );
    assert_eq!(
        placement::resolve_group_tab("id:3"),
        (Some("id:3".to_string()), None)
    );
    let spine = golden(SPINE);
    assert_eq!(
        placement::group_tab_conflict_line("delivery"),
        spine["cases"]["group_tab_refusal"]["line"]
            .as_str()
            .unwrap()
    );
    assert_eq!(
        spine["cases"]["group_tab_refusal"]["exit"].as_i64(),
        Some(EXIT_REFUSAL as i64)
    );
}

#[test]
fn attach_send_on_pristine_pane() {
    let wrapped = vec![
        "env".to_string(),
        "-u".to_string(),
        "FNO_HARNESS_NAME".to_string(),
        "FNO_AGENT_SUBSTRATE=pane".to_string(),
        "claude".to_string(),
        "--name".to_string(),
        "w".to_string(),
    ];
    placement::start_existing_pane("main", 5, Path::new("/repo"), &wrapped, &FakeMux::default())
        .unwrap_or_else(|e| panic!("the pristine-pane start failed: {e}"));
}

#[test]
fn normalize_command_and_verb_seed() {
    use super::env_wrap::{is_verb_seed, normalize_command};
    assert_eq!(normalize_command("/fno:target x", "claude"), "/target x");
    assert_eq!(normalize_command("/target x", "opencode"), "/fno:target x");
    assert_eq!(normalize_command("hello", "opencode"), "hello");
    assert_eq!(
        normalize_command("/usr/bin/script", "claude"),
        "/usr/bin/script"
    );
    assert!(is_verb_seed("/target x"));
    assert!(!is_verb_seed("hello"));
}
