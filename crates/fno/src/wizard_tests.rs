//! Contracts for the pure wizard flow: one scripted prompter drives all
//! four acceptance shapes - the screen order and skips across the three
//! presets, back navigation with its pre-selected answers, the write
//! latch, and the harness gate's retry loop and skip-for-now exit.

use super::*;

use std::collections::VecDeque;

enum Que {
    C(Conf),
    P(Pick),
    T(Ticks),
}

struct Offer {
    screen: &'static str,
    kind: &'static str,
    yes: Option<bool>,
    index: Option<usize>,
    ticks: Vec<usize>,
    options: usize,
}

struct Script {
    que: VecDeque<Que>,
    offers: Vec<Offer>,
    notes: Vec<String>,
}

impl Script {
    fn auto() -> Script {
        Script {
            que: VecDeque::new(),
            offers: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn queued(items: Vec<Que>) -> Script {
        Script {
            que: items.into(),
            offers: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn confirms(&self, screen: &str) -> Vec<bool> {
        self.offers
            .iter()
            .filter(|o| o.kind == "confirm" && o.screen == screen)
            .map(|o| o.yes.unwrap_or(false))
            .collect()
    }

    fn asks(&self) -> usize {
        self.offers.len()
    }
}

impl Prompter for Script {
    fn confirm(&mut self, screen: &'static str, _prompt: &str, default: bool) -> Conf {
        self.offers.push(Offer {
            screen,
            kind: "confirm",
            yes: Some(default),
            index: None,
            ticks: Vec::new(),
            options: 0,
        });
        match self.que.pop_front() {
            Some(Que::C(c)) => c,
            Some(_) => panic!("queue mismatch at {screen}: expected a confirm"),
            None => {
                if default {
                    Conf::Yes
                } else {
                    Conf::No
                }
            }
        }
    }

    fn select(
        &mut self,
        screen: &'static str,
        _prompt: &str,
        options: &[&str],
        default: usize,
    ) -> Pick {
        self.offers.push(Offer {
            screen,
            kind: "select",
            yes: None,
            index: Some(default),
            ticks: Vec::new(),
            options: options.len(),
        });
        match self.que.pop_front() {
            Some(Que::P(p)) => p,
            Some(_) => panic!("queue mismatch at {screen}: expected a pick"),
            None => Pick::Ok(default),
        }
    }

    fn checklist(
        &mut self,
        screen: &'static str,
        _prompt: &str,
        _options: &[&str],
        defaults: &[usize],
    ) -> Ticks {
        self.offers.push(Offer {
            screen,
            kind: "checklist",
            yes: None,
            index: None,
            ticks: defaults.to_vec(),
            options: 0,
        });
        match self.que.pop_front() {
            Some(Que::T(t)) => t,
            Some(_) => panic!("queue mismatch at {screen}: expected ticks"),
            None => Ticks::Ok(defaults.to_vec()),
        }
    }

    fn secret(&mut self, screen: &'static str, _prompt: &str) -> Hidden {
        panic!("the pure flow never reads a secret (asked at {screen})")
    }

    fn note(&mut self, screen: &'static str, text: &str) {
        self.notes.push(format!("{screen}: {text}"));
    }

    fn progress(&mut self, screen: &'static str, label: &str) {
        self.notes.push(format!("{screen}: [step] {label}"));
    }
}

struct FixedProbes {
    que: VecDeque<Probe>,
}

impl Probes for FixedProbes {
    fn harness_check(&mut self, _harness: &str) -> Probe {
        self.que.pop_front().unwrap_or(Probe::Pass { ms: 5 })
    }

    fn model_check(&mut self, _harness: &str, _model: &str) -> Probe {
        self.que.pop_front().unwrap_or(Probe::Pass { ms: 5 })
    }
}

fn fresh_detect() -> Detect {
    Detect {
        markers_present: false,
        band: Some(4),
        harnesses: vec![
            Found {
                name: "claude",
                by: "on PATH",
            },
            Found {
                name: "codex",
                by: "on PATH",
            },
        ],
        env_keys: vec![("claude", "ANTHROPIC_API_KEY"), ("codex", "OPENAI_API_KEY")],
        models: vec!["model-a".into(), "model-b".into()],
    }
}

fn drive(state: &mut State, pr: &mut Script, pb: &mut FixedProbes) -> Next {
    for _ in 0..200 {
        let next = step(state, pr, pb);
        if matches!(next, Next::Done | Next::Cancel | Next::SkipToDone) {
            return next;
        }
    }
    panic!("the flow did not settle in 200 steps");
}

fn shown(state: &State) -> Vec<&'static str> {
    state
        .screens
        .iter()
        .filter(|(_, mark)| *mark == "shown")
        .map(|(id, _)| *id)
        .collect()
}

fn skipped(state: &State) -> Vec<&'static str> {
    state
        .screens
        .iter()
        .filter(|(_, mark)| *mark == "skipped")
        .map(|(id, _)| *id)
        .collect()
}

#[test]
fn screen_order_and_skips_follow_the_presets() {
    // Balanced with every default accepted: the twelve screens run in
    // order, three skip, and the proposal set is the balanced one.
    let mut state = State::new(None, fresh_detect(), false);
    let mut pr = Script::auto();
    let mut pb = FixedProbes {
        que: VecDeque::new(),
    };
    assert_eq!(drive(&mut state, &mut pr, &mut pb), Next::Done);
    assert_eq!(
        shown(&state),
        vec![
            "preflight",
            "preset",
            "look",
            "harnesses",
            "models",
            "budget",
            "review-merge",
            "services",
            "projects",
            "review",
            "apply",
            "done",
        ]
    );
    assert_eq!(skipped(&state), vec!["rerun", "keys", "join"]);
    let rows = pending(&state);
    assert!(rows
        .iter()
        .any(|r| r.id == "pr-watcher" && r.key == "pr_watch.enabled"));
    assert!(rows.iter().any(|r| r.id == "groom-agent"));
    assert!(rows.iter().all(|r| !r.key.starts_with("auto_merge")));
    // The preset pick offers three postures with balanced defaulted; the
    // model pick appends Leave unchanged last and defaults to it.
    let preset_offer = pr
        .offers
        .iter()
        .find(|o| o.kind == "select" && o.screen == "preset")
        .unwrap();
    assert_eq!(preset_offer.options, 3);
    assert_eq!(preset_offer.index, Some(1));
    let model_offer = pr
        .offers
        .iter()
        .find(|o| o.kind == "select" && o.screen == "models")
        .unwrap();
    assert_eq!(model_offer.options, 3);
    assert_eq!(model_offer.index, Some(2));

    // User-reviewed: every stop is asked, the review screen offers No by
    // default, the flow ends in Cancel, and no service or auto-merge row
    // exists (the posture row may).
    let mut state = State::new(Some(Preset::UserReviewed), fresh_detect(), false);
    let mut pr = Script::auto();
    let mut pb = FixedProbes {
        que: VecDeque::new(),
    };
    assert_eq!(drive(&mut state, &mut pr, &mut pb), Next::Cancel);
    let last = pr.offers.last().unwrap();
    assert_eq!(last.screen, "review");
    assert_eq!(last.kind, "confirm");
    assert_eq!(last.yes, Some(false));
    let rows = pending(&state);
    assert!(rows
        .iter()
        .all(|r| r.id != "pr-watcher" && r.id != "auto-heal" && r.id != "groom-agent"));
    assert!(rows.iter().all(|r| !r.key.starts_with("auto_merge")));
    assert!(rows.iter().any(|r| r.key == "review.posture"));

    // Aggressive: the flow runs to done with zero asks and the full
    // aggressive proposal set.
    let mut state = State::new(Some(Preset::Aggressive), fresh_detect(), false);
    let mut pr = Script::auto();
    let mut pb = FixedProbes {
        que: VecDeque::new(),
    };
    assert_eq!(drive(&mut state, &mut pr, &mut pb), Next::Done);
    assert_eq!(pr.asks(), 0, "aggressive stops nowhere");
    let rows = pending(&state);
    assert!(rows.iter().any(|r| r.key == "auto_merge.enabled"));
    assert!(rows.iter().any(|r| r.key == "auto_merge.grant"));
    for id in ["pr-watcher", "auto-heal", "groom-agent"] {
        assert!(rows.iter().any(|r| r.id == id));
    }
}

#[test]
fn back_navigation_and_the_write_latch() {
    // A balanced run that goes back twice: once from services, once from
    // the review screen. Re-shown screens offer the committed answer as
    // the default - the merge ask flips from false to true.
    let mut state = State::new(Some(Preset::Balanced), fresh_detect(), false);
    let mut pr = Script::queued(vec![
        Que::T(Ticks::Ok(vec![0, 2])), // harnesses: claude + codex
        Que::P(Pick::Ok(2)),           // models: claude keeps its model
        Que::P(Pick::Ok(2)),           // models: codex keeps its model
        Que::C(Conf::Yes),             // budget: apply the band
        Que::C(Conf::Yes),             // review-merge: keep the posture
        Que::C(Conf::Yes),             // review-merge: merge (non-default)
        Que::T(Ticks::Back),           // services: back
        Que::C(Conf::Yes),             // review-merge again: posture
        Que::C(Conf::No),              // review-merge again: merge (offered true now)
        Que::T(Ticks::Ok(vec![0, 2])), // services again
        Que::C(Conf::Yes),             // projects: register
        Que::C(Conf::No),              // projects: no scan
        Que::C(Conf::Back),            // review: back
        Que::C(Conf::Yes),             // projects again: register
        Que::C(Conf::No),              // projects again: no scan
        Que::C(Conf::Yes),             // review again: write (the latch)
    ]);
    let mut pb = FixedProbes {
        que: VecDeque::new(),
    };
    assert_eq!(drive(&mut state, &mut pr, &mut pb), Next::Done);
    // The merge default reads the committed answer: false first, then
    // true on the re-shown screen. Posture stays true throughout.
    assert_eq!(pr.confirms("review-merge"), vec![true, false, true, true]);
    // Projects re-offers the committed register yes and the committed
    // scan no.
    assert_eq!(pr.confirms("projects"), vec![true, false, true, false]);
    assert!(!state.back_allowed(), "the review yes latched writes");

    // A host restoring a mid-run session: writes already started, so the
    // first back is refused with a note and the screen asks again.
    let mut state = State::new(Some(Preset::Balanced), fresh_detect(), false);
    state.writes_started = true;
    let mut pr = Script::queued(vec![
        Que::T(Ticks::Back),           // harnesses: refused
        Que::T(Ticks::Ok(vec![0, 2])), // harnesses again
        Que::P(Pick::Ok(2)),           // models: claude
        Que::P(Pick::Ok(2)),           // models: codex
        Que::C(Conf::Yes),             // budget
        Que::C(Conf::Yes),             // posture
        Que::C(Conf::No),              // merge
        Que::T(Ticks::Ok(vec![0, 2])), // services
        Que::C(Conf::Yes),             // register
        Que::C(Conf::No),              // scan
    ]);
    let mut pb = FixedProbes {
        que: VecDeque::new(),
    };
    assert_eq!(drive(&mut state, &mut pr, &mut pb), Next::Done);
    assert_eq!(
        pr.offers
            .iter()
            .filter(|o| o.kind == "checklist" && o.screen == "harnesses")
            .count(),
        2,
        "the refused back re-asked the checklist"
    );
    assert!(pr.notes.iter().any(|n| n.contains("back is off")));

    // A declined posture on back clears the stale answer: Yes at
    // review-merge seeds self_review, then Back from services re-asks the
    // screen. A No there must clear the seeded answer, so no
    // review.posture proposal is built and the note names the kept
    // posture.
    let mut state = State::new(Some(Preset::Balanced), fresh_detect(), false);
    let mut pr = Script::queued(vec![
        Que::T(Ticks::Ok(vec![0, 2])), // harnesses: claude + codex
        Que::P(Pick::Ok(2)),           // models: claude keeps its model
        Que::P(Pick::Ok(2)),           // models: codex keeps its model
        Que::C(Conf::Yes),             // budget: apply the band
        Que::C(Conf::Yes),             // review-merge: keep the posture
        Que::C(Conf::Yes),             // review-merge: merge
        Que::T(Ticks::Back),           // services: back
        Que::C(Conf::No),              // review-merge again: decline the posture
        Que::C(Conf::No),              // review-merge again: no merge
        Que::T(Ticks::Ok(vec![0, 2])), // services again
        Que::C(Conf::Yes),             // projects: register
        Que::C(Conf::No),              // projects: no scan
        Que::C(Conf::Yes),             // review: write (the latch)
    ]);
    let mut pb = FixedProbes {
        que: VecDeque::new(),
    };
    assert_eq!(drive(&mut state, &mut pr, &mut pb), Next::Done);
    assert!(pr
        .notes
        .iter()
        .any(|n| n.contains("the review posture stays as configured")));
    assert!(
        pending(&state).iter().all(|r| r.key != "review.posture"),
        "the declined posture must not stay proposed"
    );
    assert!(pending(&state)
        .iter()
        .all(|r| !r.key.starts_with("auto_merge")));
}

#[test]
fn the_harness_gate_loops_until_a_pass_or_the_skip() {
    // Both default ticks fail twice: retry once, then take Skip for now,
    // which lands on done with an empty proposal set and nothing written.
    let mut state = State::new(Some(Preset::Balanced), fresh_detect(), false);
    let mut pr = Script::queued(vec![
        Que::P(Pick::Ok(0)), // Retry
        Que::P(Pick::Ok(3)), // Skip for now
    ]);
    let mut pb = FixedProbes {
        que: [Probe::Fail("auth"), Probe::Fail("timeout")].into(),
    };
    assert_eq!(drive(&mut state, &mut pr, &mut pb), Next::SkipToDone);
    assert!(state.skip_all);
    assert!(pending(&state).is_empty(), "skip for now writes nothing");
    let seen: Vec<(&str, &str)> = state
        .checks
        .iter()
        .map(|(name, outcome, _)| (*name, outcome.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![("claude", "fail:auth"), ("claude", "fail:timeout")]
    );
    // Zero checks passing, so both failure picks offer the skip-for-now
    // exit.
    let option_counts: Vec<usize> = pr
        .offers
        .iter()
        .filter(|o| o.kind == "select" && o.screen == "harnesses")
        .map(|o| o.options)
        .collect();
    assert_eq!(option_counts, vec![4, 4]);
}
