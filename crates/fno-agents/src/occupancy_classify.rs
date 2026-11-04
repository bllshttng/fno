//! Classify the processes rooted in a worktree: holder or inert.
//!
//! The sweep's process step enumerates truthfully and decides coarsely: any
//! pid keeps the tree forever, because nothing retires a leaked keeper, an
//! orphaned Bash-tool shell, or a bg session whose job already finished. This
//! names each hit. `fno-agents occupancy-login --classify <wt> <pid>...`
//! prints one `pid verdict action job reason cmd` row per pid; the shell
//! bridge (`scripts/lib/worktree-occupancy.sh`) fails closed on anything less.
//!
//! First match wins: R6 idle login shell (occupancy_login), R0 no ps row, R1
//! keeper, R2 claude bg session, R3 orphaned tool shell, R4 descendant of any
//! of these, R5 unclassified holds. Absence of a recognised holder is never
//! proof a tree is free.

use std::collections::BTreeMap;
use std::path::Path;

const KEEPER_BIN: &str = "fno-agents-worker";
const TERMINAL_JOB_STATES: [&str; 3] = ["done", "stopped", "failed"];
const SHELLS: [&str; 3] = ["zsh", "bash", "sh"];
const DESCENT_CAP: usize = 32;
/// argv[1] tokens the Claude Code background daemon gives its pre-warm pool;
/// both spellings are live.
const SPARE_POOL_ARGS: [&str; 4] = ["bg-spare", "bg-pty-host", "--bg-spare", "--bg-pty-host"];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Hit {
    pub pid: u32,
    pub holds: bool,
    pub action: &'static str,
    pub job: String,
    pub reason: String,
    pub cmd: String,
}

impl Hit {
    fn new(pid: u32, holds: bool, action: &'static str, reason: String, cmd: &str) -> Self {
        Hit {
            pid,
            holds,
            action,
            job: "-".into(),
            reason,
            cmd: cmd.to_string(),
        }
    }

    fn row(&self) -> String {
        let verdict = if self.holds { "holds" } else { "inert" };
        format!(
            "{}\t{verdict}\t{}\t{}\t{}\t{}",
            self.pid, self.action, self.job, self.reason, self.cmd
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Proc {
    pub ppid: u32,
    pub argv: Vec<String>,
}

impl Proc {
    fn base(&self, i: usize) -> &str {
        self.argv
            .get(i)
            .map(|a| a.rsplit('/').next().unwrap_or(a))
            .unwrap_or("")
    }

    /// One physical line: a `zsh -c <script>` argv embeds real newlines, and
    /// a row-counting bridge would read each as another row.
    fn cmd(&self) -> String {
        self.argv.join(" ")
    }
}

pub(crate) type JobState<'a> = &'a dyn Fn(&str) -> Option<(String, Option<f64>)>;

pub(crate) struct Inputs<'a> {
    pub procs: &'a BTreeMap<u32, Proc>,
    /// The keeper lane's verdict per keeper pid; None when it cannot answer.
    pub keepers: Option<&'a BTreeMap<u32, (String, String)>>,
    /// The rv socket farm's pid to job join; None when it cannot answer.
    pub job_of_pid: Option<&'a BTreeMap<u32, String>>,
    pub job_state: JobState<'a>,
    pub home: &'a str,
}

/// A Claude Code background-daemon pre-warm process, matched on argv[0] and
/// argv[1] only: a session whose prompt says bg-spare is a session.
pub(crate) fn is_spare_pool(proc: &Proc) -> bool {
    proc.base(0) == "claude"
        && SPARE_POOL_ARGS.contains(&proc.argv.get(1).map(String::as_str).unwrap_or(""))
}

/// R3 shape: an orphaned claude Bash-tool shell sourcing a shell snapshot.
fn snapshot_orphan(proc: &Proc, home: &str) -> bool {
    if proc.argv.len() < 3 || !SHELLS.contains(&proc.base(0)) || proc.argv[1] != "-c" {
        return false;
    }
    let script = proc.argv[2..].join(" ");
    let home_prefix = format!(
        "source {}/.claude/shell-snapshots/",
        home.trim_end_matches('/')
    );
    script.starts_with(&home_prefix) || script.starts_with("source ~/.claude/shell-snapshots/")
}

fn direct(pid: u32, proc: &Proc, inputs: &Inputs) -> Option<Hit> {
    let cmd = proc.cmd();
    if proc.base(0) == KEEPER_BIN {
        return Some(match inputs.keepers.and_then(|k| k.get(&pid)) {
            None => Hit::new(
                pid,
                true,
                "keep",
                "keeper lane unreadable, so the reap arm cannot read - no reap".into(),
                &cmd,
            ),
            Some((verdict, reason)) if verdict == "reap" => {
                Hit::new(pid, false, "terminate", reason.clone(), &cmd)
            }
            Some((_, reason)) => Hit::new(pid, true, "keep", reason.clone(), &cmd),
        });
    }
    if is_spare_pool(proc) {
        // The identity join is the daemon's rv socket farm, never a recorded
        // cwd: the registry and job state.json cwd are the SPAWN directory.
        let Some(jobs) = inputs.job_of_pid else {
            return Some(Hit::new(
                pid,
                true,
                "keep",
                "claude session: rv socket map unreadable".into(),
                &cmd,
            ));
        };
        let Some(job) = jobs.get(&pid) else {
            return Some(Hit::new(pid, true, "keep", "claude session: no rv socket join (argv bg-spare is identical for a live session and an idle spare)".into(), &cmd));
        };
        let mut hit = match (inputs.job_state)(job) {
            None => Hit::new(
                pid,
                true,
                "keep",
                format!("claude job {job}: state unreadable"),
                &cmd,
            ),
            Some((state, age)) => {
                let age_txt = age
                    .map(|a| format!("{}s", a as i64))
                    .unwrap_or_else(|| "unreadable".into());
                let stalled = age.is_some_and(|a| a > crate::session_truth::STALLED_AFTER_S);
                if TERMINAL_JOB_STATES.contains(&state.as_str()) && stalled {
                    Hit::new(
                        pid,
                        false,
                        "retire",
                        format!("claude job {job} {state}, transcript silent {age_txt}"),
                        &cmd,
                    )
                } else {
                    Hit::new(
                        pid,
                        true,
                        "keep",
                        format!("claude job {job} {state}, transcript {age_txt}"),
                        &cmd,
                    )
                }
            }
        };
        hit.job = job.clone();
        return Some(hit);
    }
    if proc.ppid == 1 && snapshot_orphan(proc, inputs.home) {
        return Some(Hit::new(
            pid,
            false,
            "terminate",
            "claude tool shell, owning session exited".into(),
            &cmd,
        ));
    }
    None
}

/// R4: inherit the first classified ancestor's verdict and action.
fn descendant(pid: u32, proc: &Proc, inputs: &Inputs) -> Option<Hit> {
    let mut cur = proc.ppid;
    for _ in 0..DESCENT_CAP {
        if cur == 0 || cur == pid {
            return None;
        }
        let ancestor = inputs.procs.get(&cur)?;
        if let Some(hit) = direct(cur, ancestor, inputs) {
            return Some(Hit {
                pid,
                reason: format!("child of {cur}: {}", hit.reason),
                cmd: proc.cmd(),
                ..hit
            });
        }
        cur = ancestor.ppid;
    }
    None
}

pub(crate) fn classify(pids: &[u32], inputs: &Inputs) -> Vec<Hit> {
    pids.iter()
        .map(|&pid| match inputs.procs.get(&pid) {
            None => Hit::new(pid, true, "keep", "no ps row".into(), ""),
            Some(proc) => direct(pid, proc, inputs)
                .or_else(|| descendant(pid, proc, inputs))
                .unwrap_or_else(|| {
                    let name = if proc.argv.is_empty() {
                        "unknown"
                    } else {
                        proc.base(0)
                    };
                    Hit::new(
                        pid,
                        true,
                        "keep",
                        format!("unclassified: {name}"),
                        &proc.cmd(),
                    )
                }),
        })
        .collect()
}

fn job_state_reader(home: &str) -> impl Fn(&str) -> Option<(String, Option<f64>)> + '_ {
    move |job: &str| {
        let path = Path::new(home)
            .join(".claude/jobs")
            .join(job)
            .join("state.json");
        let data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let state = data.get("state")?.as_str()?.to_string();
        let age = data
            .get("linkScanPath")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .and_then(|scan| {
                std::fs::metadata(scan)
                    .ok()?
                    .modified()
                    .ok()?
                    .elapsed()
                    .ok()
            })
            .map(|d| d.as_secs_f64());
        Some((state, age))
    }
}

/// `occupancy-login --classify <wt> <pid>...`. A pid that is not a number
/// exits 2 with nothing printed, so the bridge fails closed.
pub fn run(args: &[String]) -> i32 {
    let Some((_worktree, rest)) = args.split_first() else {
        eprintln!("usage: fno-agents occupancy-login --classify <worktree> <pid>...");
        return 2;
    };
    let Ok(pids) = rest
        .iter()
        .map(|p| p.parse::<u32>())
        .collect::<Result<Vec<_>, _>>()
    else {
        return 2;
    };
    let mut rows: BTreeMap<u32, String> = BTreeMap::new();
    let mut left = Vec::new();
    for pid in &pids {
        match crate::occupancy_login::login_row(*pid) {
            Some(row) => {
                rows.insert(*pid, row);
            }
            None => left.push(*pid),
        }
    }
    if !left.is_empty() {
        let (table, _) = crate::census::process_table();
        let procs: BTreeMap<u32, Proc> = table
            .into_iter()
            .map(|r| {
                (
                    r.pid,
                    Proc {
                        ppid: r.ppid,
                        argv: r.command.split_whitespace().map(str::to_string).collect(),
                    },
                )
            })
            .collect();
        let lane = crate::watchdog::keeper::discover();
        let keepers: Option<BTreeMap<u32, (String, String)>> = (!lane.broken()).then(|| {
            lane.to_json()["keepers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|k| {
                    Some((
                        k["pid"].as_u64()? as u32,
                        (
                            k["verdict"].as_str()?.to_string(),
                            k["reason"].as_str().unwrap_or("").to_string(),
                        ),
                    ))
                })
                .collect()
        });
        let sockets = crate::footprint_cause::bg_socket_pid_map(None);
        let job_of_pid: Option<BTreeMap<u32, String>> = (!sockets.is_empty())
            .then(|| sockets.into_iter().map(|(job, pid)| (pid, job)).collect());
        let home = std::env::var("HOME").unwrap_or_default();
        let reader = job_state_reader(&home);
        let inputs = Inputs {
            procs: &procs,
            keepers: keepers.as_ref(),
            job_of_pid: job_of_pid.as_ref(),
            job_state: &reader,
            home: &home,
        };
        for hit in classify(&left, &inputs) {
            rows.insert(hit.pid, hit.row());
        }
    }
    for pid in &pids {
        if let Some(row) = rows.get(pid) {
            println!("{row}");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(ppid: u32, cmd: &str) -> Proc {
        Proc {
            ppid,
            argv: cmd.split_whitespace().map(str::to_string).collect(),
        }
    }

    #[test]
    fn each_rule_names_its_hit_and_fails_closed() {
        let mut procs = BTreeMap::new();
        procs.insert(10, proc(1, "/x/fno-agents-worker --pane"));
        procs.insert(11, proc(10, "node child.js"));
        procs.insert(20, proc(1, "/usr/bin/claude bg-spare"));
        procs.insert(21, proc(1, "/usr/bin/claude --bg-pty-host"));
        procs.insert(
            30,
            proc(
                1,
                "/bin/zsh -c source /h/.claude/shell-snapshots/s.sh && ls",
            ),
        );
        procs.insert(
            31,
            proc(99, "/bin/zsh -c source /h/.claude/shell-snapshots/s.sh"),
        );
        procs.insert(40, proc(1, "vim notes.md"));
        let keepers = BTreeMap::from([(10, ("reap".to_string(), "socket dead".to_string()))]);
        let jobs = BTreeMap::from([(20, "j1".to_string())]);
        let state = |job: &str| (job == "j1").then(|| ("done".to_string(), Some(9000.0)));
        let inputs = Inputs {
            procs: &procs,
            keepers: Some(&keepers),
            job_of_pid: Some(&jobs),
            job_state: &state,
            home: "/h",
        };
        let rows: Vec<String> = classify(&[10, 11, 20, 21, 30, 31, 40, 50], &inputs)
            .iter()
            .map(Hit::row)
            .collect();
        assert_eq!(
            rows[0],
            "10\tinert\tterminate\t-\tsocket dead\t/x/fno-agents-worker --pane"
        );
        assert_eq!(
            rows[1],
            "11\tinert\tterminate\t-\tchild of 10: socket dead\tnode child.js"
        );
        assert!(
            rows[2]
                .starts_with("20\tinert\tretire\tj1\tclaude job j1 done, transcript silent 9000s"),
            "{}",
            rows[2]
        );
        assert!(
            rows[3].starts_with("21\tholds\tkeep\t-\tclaude session: no rv socket join"),
            "{}",
            rows[3]
        );
        assert!(
            rows[4].starts_with("30\tinert\tterminate\t-\tclaude tool shell"),
            "{}",
            rows[4]
        );
        assert!(
            rows[5].starts_with("31\tholds\tkeep\t-\tunclassified: zsh"),
            "{}",
            rows[5]
        );
        assert!(
            rows[6].starts_with("40\tholds\tkeep\t-\tunclassified: vim"),
            "{}",
            rows[6]
        );
        assert_eq!(rows[7], "50\tholds\tkeep\t-\tno ps row\t");
        let blind = Inputs {
            keepers: None,
            job_of_pid: None,
            ..inputs
        };
        let rows: Vec<String> = classify(&[10, 20], &blind).iter().map(Hit::row).collect();
        assert!(rows[0].contains("holds\tkeep\t-\tkeeper lane unreadable"));
        assert!(rows[1].contains("holds\tkeep\t-\tclaude session: rv socket map unreadable"));
    }
}
