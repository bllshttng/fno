//! The user's time-boxed lift of the learned spawn cap. One small file beside
//! the capacity state: `<agents home>/capacity/<machine-key>.grant.json`. The
//! decide door gives the authority and the journal row (an agent session is
//! refused there); this file is the hot read the spawn gate takes on every
//! queue pass, so the gate never shells out to read a decision. It is plain
//! state at the same trust level as the capacity file itself.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::capacity::Effective;
use crate::decide_door::{decide_door_record, parse_decide_door, DecideIdentity};
use crate::paths::AgentsHome;

/// The longest window one grant may cover.
pub const MAX_WINDOW_SECS: i64 = 8 * 3600;
/// How long after its end a grant still reads as ended, so the user sees it
/// ended and was not lost. The tick deletes the file after this.
pub const ENDED_NOTE_SECS: i64 = 900;

/// The persisted grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub cap: u32,
    pub granted_epoch: i64,
    pub expires_epoch: i64,
    pub decision_id: String,
    pub learned_at_grant: u32,
}

/// What a cap reader sees of a grant: `active` inside the window, inactive for
/// [`ENDED_NOTE_SECS`] after it. `learned` is the cap before the lift.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantView {
    pub cap: usize,
    pub learned: usize,
    pub expires_epoch: i64,
    pub active: bool,
    pub decision_id: String,
}

pub fn path(home: &AgentsHome) -> PathBuf {
    home.root()
        .join("capacity")
        .join(format!("{}.grant.json", crate::capacity::machine_key()))
}

/// The grant on disk, `None` when missing or unparseable. A reader never
/// repairs a broken file.
pub fn read(home: &AgentsHome) -> Option<Grant> {
    serde_json::from_str(&std::fs::read_to_string(path(home)).ok()?).ok()
}

/// Tempfile and rename in the same directory, so a reader sees the old file
/// or the new one and never half of either.
pub fn write(home: &AgentsHome, grant: &Grant) -> io::Result<()> {
    let path = path(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(grant)?)?;
    std::fs::rename(&tmp, &path)
}

/// Remove the grant file. A missing file is already clear.
pub fn clear(home: &AgentsHome) -> io::Result<()> {
    match std::fs::remove_file(path(home)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The tick's cleaner: drop a grant whose end is past the ended note window.
pub fn sweep(home: &AgentsHome, now: i64) {
    if read(home).is_some_and(|g| now - g.expires_epoch > ENDED_NOTE_SECS) {
        let _ = clear(home);
    }
}

/// Fold a grant into a cap answer. Inside the window a grant above the
/// learned cap lifts the cap to `grant.cap`, never past the ceiling. A grant
/// at or under the learned cap lifts nothing and leaves no trace. For
/// [`ENDED_NOTE_SECS`] after its end the answer keeps the learned cap and
/// carries an inactive view, so the clause says the grant ended.
pub(crate) fn apply(mut eff: Effective, grant: Option<Grant>, now: i64) -> Effective {
    let Some(grant) = grant else { return eff };
    let learned = eff.cap;
    let lifted = (grant.cap as usize).min(eff.ceiling);
    let active = now < grant.expires_epoch;
    if active && lifted > learned {
        eff.cap = lifted;
        eff.known = true;
    } else if active || now - grant.expires_epoch >= ENDED_NOTE_SECS {
        return eff;
    }
    eff.grant = Some(GrantView {
        cap: lifted,
        learned,
        expires_epoch: grant.expires_epoch,
        active,
        decision_id: grant.decision_id,
    });
    eff
}

const USAGE: &str = "usage: fno agents spawn-gate grant [--cap <n>] [--for <duration>] | --clear\n\
    no argument: print the grant state and the floor the gate read.\n\
    --cap defaults to agents.max_live and never passes it. --for defaults to 1h and is 8h at most.";

/// What `grant` reads from its surroundings. [`run`] fills it from config.
pub(crate) struct Ctx<'a> {
    pub home: &'a AgentsHome,
    pub ceiling: usize,
    pub floor: usize,
    pub floor_set: bool,
    pub id: DecideIdentity,
}

/// `fno agents spawn-gate grant [--cap <n>] [--for <duration>] | --clear`
///
/// With no argument it reads. Otherwise it records the user's override at the
/// decide door (operator authority, refused for any agent session) and only
/// then writes the grant file the gate reads.
pub fn run(cwd: &Path, args: &[String]) -> i32 {
    let home = AgentsHome::from_env();
    let floor_key = crate::agents_config::config_lookup(cwd, &["agents", "capacity", "floor"]);
    let ctx = Ctx {
        home: &home,
        ceiling: crate::agents_config::max_live(cwd) as usize,
        floor: crate::capacity::thresholds(cwd).floor as usize,
        floor_set: floor_key.is_some(),
        id: DecideIdentity::Ambient,
    };
    run_at(&ctx, args)
}

pub(crate) fn run_at(ctx: &Ctx, args: &[String]) -> i32 {
    let mut cap_arg: Option<&str> = None;
    let mut for_arg: Option<&str> = None;
    let mut clear_arg = false;
    let mut it = args.iter().map(String::as_str);
    while let Some(arg) = it.next() {
        match arg {
            "--help" | "-h" => {
                println!("{USAGE}");
                return 0;
            }
            "--clear" => clear_arg = true,
            "--cap" | "--for" => {
                let Some(value) = it.next() else {
                    eprintln!("spawn-gate grant: {arg} needs a value");
                    return 2;
                };
                match arg {
                    "--cap" => cap_arg = Some(value),
                    _ => for_arg = Some(value),
                }
            }
            other => {
                eprintln!("spawn-gate grant: unknown argument {other:?}\n{USAGE}");
                return 2;
            }
        }
    }
    let now = chrono::Utc::now().timestamp();
    if clear_arg {
        return clear_grant(ctx, now);
    }
    if cap_arg.is_none() && for_arg.is_none() {
        return show(ctx, now);
    }
    set_grant(ctx, now, cap_arg, for_arg)
}

/// "1h42m" style time left.
fn left(secs: i64) -> String {
    let mins = secs.max(0) / 60;
    match mins >= 60 {
        true => format!("{}h{:02}m", mins / 60, mins % 60),
        false => format!("{mins}m"),
    }
}

fn subject() -> String {
    format!("spawn-cap-grant:{}", crate::capacity::machine_key())
}

/// The read: the grant when one is active, else the cap the gate reads and
/// the floor it applied. This is also the marker a Python-fronted verb lacks
/// for the unmodeled `[agents.capacity]` block.
fn show(ctx: &Ctx, now: i64) -> i32 {
    let eff = crate::capacity::effective(ctx.home, ctx.ceiling, ctx.floor);
    let key = match ctx.floor_set {
        true => "agents.capacity.floor",
        false => "agents.capacity.floor unset",
    };
    let floor = format!("floor {} ({key})", ctx.floor);
    match eff.grant.as_ref().filter(|g| g.active) {
        Some(g) => println!(
            "spawn-gate grant: active. cap {} until {} ({} left), learned {}, {floor}, journal {}.",
            g.cap,
            crate::capacity::render_clock(g.expires_epoch),
            left(g.expires_epoch - now),
            g.learned,
            g.decision_id
        ),
        None => println!(
            "spawn-gate grant: none. {}, {floor}. Lift it: {}",
            eff.clause(),
            eff.grant_command()
        ),
    }
    0
}

fn door_argv(decision: String, rationale: String) -> Vec<String> {
    vec![
        subject(),
        decision,
        "--authority".into(),
        "operator".into(),
        "--rationale".into(),
        rationale,
    ]
}

/// Record at the decide door and return the minted decision id. The door is
/// the one authority: no second identity check lives here. Exit 3 is its
/// refusal, which for an agent session means "ask the user to type this".
fn record(ctx: &Ctx, argv: &[String], ask: &str) -> Result<String, i32> {
    let door = parse_decide_door(argv).map_err(|e| {
        eprintln!("spawn-gate grant: {e}");
        2
    })?;
    let mut recorded = None;
    match decide_door_record(door, ctx.id, &mut recorded) {
        0 => recorded.ok_or(1),
        3 => {
            eprintln!(
                "spawn-gate grant: refused. A cap grant is the user's own override and the \
                 decide door refused this session, so nothing was written. Ask the user to \
                 type: {ask}"
            );
            Err(3)
        }
        code => Err(code),
    }
}

fn set_grant(ctx: &Ctx, now: i64, cap_arg: Option<&str>, for_arg: Option<&str>) -> i32 {
    let cap = match cap_arg {
        None => ctx.ceiling,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => {
                eprintln!(
                    "spawn-gate grant: refused. --cap {raw:?} is not a slot count of 1 or more. \
                     Nothing was written."
                );
                return 2;
            }
        },
    };
    if cap > ctx.ceiling {
        eprintln!(
            "spawn-gate grant: refused. --cap {cap} is over agents.max_live {}, the ceiling this \
             grant cannot pass. Raise agents.max_live in config.toml first. Nothing was written.",
            ctx.ceiling
        );
        return 2;
    }
    let window = for_arg.unwrap_or("1h");
    let secs = match crate::claims::parse_ttl_ms(window) {
        Some(ms) if ms / 1000 <= MAX_WINDOW_SECS => ms / 1000,
        Some(_) => {
            eprintln!(
                "spawn-gate grant: refused. --for {window} is over the 8h ceiling. Nothing was \
                 written."
            );
            return 2;
        }
        None => {
            eprintln!(
                "spawn-gate grant: refused. --for {window:?} is not a duration; want 90m, 2h or \
                 3600. Nothing was written."
            );
            return 2;
        }
    };
    let eff = crate::capacity::effective(ctx.home, ctx.ceiling, ctx.floor);
    let learned = eff.grant.as_ref().map_or(eff.cap, |g| g.learned);
    let end = now + secs;
    let argv = door_argv(
        format!(
            "cap {cap} until {}",
            crate::provider_cap::epoch_to_rfc3339(end)
        ),
        format!("the user lifted the learned spawn cap to {cap} for {window}"),
    );
    let ask = format!("fno agents spawn-gate grant --cap {cap} --for {window}");
    let id = match record(ctx, &argv, &ask) {
        Ok(id) => id,
        Err(code) => return code,
    };
    let grant = Grant {
        cap: cap as u32,
        granted_epoch: now,
        expires_epoch: end,
        decision_id: id.clone(),
        learned_at_grant: learned as u32,
    };
    if let Err(e) = write(ctx.home, &grant) {
        eprintln!(
            "spawn-gate grant: journaled as {id}, but the grant file was not written ({e}), so \
             the cap did not change. Run the command again."
        );
        return 1;
    }
    let until = crate::capacity::render_clock(end);
    println!(
        "spawn-gate grant: working cap lifted {learned} -> {cap} until {until} ({window}). \
         Journaled as {id} at subject {}.\n  lifts: the learned cap and the fleet CPU-share hold, \
         for every spawner on this machine.\n  does not lift: RAM floor, swap cap, provider lane \
         caps, territory and blueprint caps, the incident stop, the machine-runaway brake.\n  \
         reverts on its own at {until}. Workers already running keep running. End it early: \
         fno agents spawn-gate grant --clear",
        subject()
    );
    0
}

/// End a live grant early. The journal row goes first and through the same
/// door, so an agent cannot end the user's grant either.
fn clear_grant(ctx: &Ctx, now: i64) -> i32 {
    let Some(grant) = read(ctx.home).filter(|g| now < g.expires_epoch) else {
        println!("spawn-gate grant: none; nothing to clear.");
        return 0;
    };
    let argv = door_argv(
        "cap grant ended early".into(),
        format!("the user ended the cap {} grant before its end", grant.cap),
    );
    if let Err(code) = record(ctx, &argv, "fno agents spawn-gate grant --clear") {
        return code;
    }
    match clear(ctx.home) {
        Ok(()) => {
            println!("spawn-gate grant: cleared. The learned cap applies again.");
            0
        }
        Err(e) => {
            eprintln!("spawn-gate grant: journaled the end, but could not remove the file: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capacity::{effective, write_state, CapacityState};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_grant_lifts_the_cap_for_its_window_and_only_the_user_sets_it() {
        let dir = std::env::temp_dir().join(format!("fno-grant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let home = AgentsHome::at(dir);
        let now = chrono::Utc::now().timestamp();
        let learned_17 = CapacityState {
            ceiling: 24,
            effective: 17,
            reason: Some("CPU-bound".into()),
            since: Some(now - 3600),
            slots_learned: None,
            over_since: None,
            calm_since: None,
            memory: None,
            accounts: Vec::new(),
            workers_live: None,
            updated_epoch: now - 60,
        };
        write_state(&home, &learned_17).unwrap();
        assert_eq!(effective(&home, 24, 1).cap, 17, "no file, no lift");

        // Inside the window the cap is the grant and the clause says so.
        let grant = Grant {
            cap: 24,
            granted_epoch: now,
            expires_epoch: now + 3600,
            decision_id: "d-test".into(),
            learned_at_grant: 17,
        };
        write(&home, &grant).unwrap();
        let lifted = effective(&home, 24, 1);
        assert_eq!(lifted.cap, 24);
        assert!(lifted.granted());
        assert!(lifted
            .clause()
            .starts_with("effective cap 24 of 24 (operator grant until "));
        assert!(lifted.clause().ends_with(", learned 17)"));
        assert_eq!(
            lifted.override_hint(),
            None,
            "no lift exists at the ceiling"
        );

        // Just past its end the learned cap returns and the clause says ended.
        let ended = Grant {
            expires_epoch: now - 60,
            ..grant.clone()
        };
        write(&home, &ended).unwrap();
        let after = effective(&home, 24, 1);
        assert_eq!(after.cap, 17);
        assert!(!after.granted());
        assert!(after.clause().contains("(operator grant ended "));
        assert!(after.override_hint().unwrap().contains("--cap 24 --for 2h"));

        // Past the note window the answer is plain and the tick removes the file.
        let stale = Grant {
            expires_epoch: now - ENDED_NOTE_SECS - 1,
            ..grant.clone()
        };
        write(&home, &stale).unwrap();
        assert!(effective(&home, 24, 1).grant.is_none());
        sweep(&home, now);
        assert!(read(&home).is_none());

        // An unparseable file reads as no file.
        std::fs::write(path(&home), "{not json").unwrap();
        assert_eq!(effective(&home, 24, 1).cap, 17);
        clear(&home).unwrap();

        // An agent session writes nothing, and a bad window or a cap over the
        // ceiling refuses before the door is called.
        let agent = Ctx {
            home: &home,
            ceiling: 24,
            floor: 1,
            floor_set: false,
            id: DecideIdentity::Forced(Some("rowan")),
        };
        assert_eq!(run_at(&agent, &args(&["--cap", "24", "--for", "2h"])), 3);
        assert_eq!(run_at(&agent, &args(&["--cap", "30"])), 2);
        assert_eq!(run_at(&agent, &args(&["--for", "9h"])), 2);
        assert!(read(&home).is_none());
        let _ = std::fs::remove_dir_all(home.root());
    }
}
