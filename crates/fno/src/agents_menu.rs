//! The `fno agents` menu: twelve entries, each holding at most twelve commands,
//! and nothing hidden. A nested spelling is rewritten lexically to the argv
//! that answers today, so no command's behavior, flags or output change. An old
//! flat spelling still answers for one release: it prints one stderr line
//! naming the nested form, then runs as before (stderr, because callers
//! parse stdout). `fno doctor lint menu-caps` counts this table as the
//! `agents` group.

use std::ffi::OsString;

/// One entry, group or command, of the agents menu.
#[derive(Debug)]
pub struct Node {
    pub name: &'static str,
    pub about: &'static str,
    /// The words after `agents` this node forwards to. Empty for a group
    /// only this table defines.
    pub legacy: &'static [&'static str],
    /// Older spellings (words after `agents`) that still answer, with the
    /// notice, for one release.
    pub old: &'static [&'static [&'static str]],
    pub children: &'static [Node],
    /// A group whose bare or flag-led spelling forwards its legacy argv.
    pub bare: bool,
    /// A group whose unknown word refuses here. Otherwise the word goes on
    /// to the doors that owned the group before this table.
    pub strict: bool,
    /// `org chart rundown`: add the default `--out` when the caller gave none.
    pub pages_out: bool,
}

const fn command(name: &'static str, legacy: &'static [&'static str], about: &'static str) -> Node {
    Node {
        name,
        about,
        legacy,
        old: &[],
        children: &[],
        bare: false,
        strict: true,
        pages_out: false,
    }
}

const fn moved(
    name: &'static str,
    legacy: &'static [&'static str],
    old: &'static [&'static [&'static str]],
    about: &'static str,
) -> Node {
    Node {
        old,
        ..command(name, legacy, about)
    }
}

/// A group this table defines: an unknown word refuses.
const fn group(name: &'static str, about: &'static str, children: &'static [Node]) -> Node {
    Node {
        children,
        ..command(name, &[], about)
    }
}

/// A group that existed before this table, under the same name: an unknown
/// word goes on to its old doors.
const fn kept(
    name: &'static str,
    legacy: &'static [&'static str],
    about: &'static str,
    children: &'static [Node],
) -> Node {
    Node {
        children,
        strict: false,
        ..command(name, legacy, about)
    }
}

#[rustfmt::skip]
const ADMIT: &[Node] = &[
    command("gate", &["spawn-gate"], "The one spawn gate: stdin JSON payload in, admitted or refused out."),
    command("guard", &["spawn-guard"], "The shared bg-dispatch guard and its mutex."),
    command("overlay", &["spawn-overlay"], "Harness-keyed spawn-defaults resolver over a stdin payload."),
    command("axes", &["spawn-axes"], "Spawn billing axes (route, account, model) over a stdin payload."),
    command("status", &["gate-status"], "The spawn gate's read-only capacity verdict as JSON."),
    command("revival", &["revival-check"], "Whether a spawn --resume revives an existing row."),
    command("fallback", &["fallback-chain"], "Failover chain walk over a stdin payload."),
    command("slot", &["route-slot"], "Delivery-slot resolver over a stdin payload."),
    command("provider-cap", &["provider-cap"], "The provider-cap actor."),
    command("capabilities", &["capabilities"], "One harness's config-independent capability contract."),
    command("liveness", &["liveness"], "One 'is this worker alive' answer per handle."),
    command("autonomy", &["autonomy"], "Every path that can start a session without an operator."),
];

#[rustfmt::skip]
const MAIL_ADMIN: &[Node] = &[
    command("drain-self", &["mail", "drain-self"], "Drain this session's own cross-harness inbox."),
    command("hold", &["mail", "hold"], "Busy mode: hold incoming mail and drain it on a timer."),
    command("hold-release", &["mail", "hold-release"], "Sleep until a hold expires, then release it."),
    command("pane-prepare", &["mail", "pane-prepare"], "The pane-send renderer in the fno-agents binary."),
    command("seed-provenance", &["mail", "seed-provenance"], "The seed-provenance command in the fno-agents binary."),
    command("sent", &["mail", "sent"], "List mail this session sent, with claim state."),
    command("team", &["mail", "team"], "Announce one body to a fleet scope as one bus line."),
    command("withdraw", &["mail", "withdraw"], "Retract a message the recipient has not picked up."),
    command("teach", &["mail-teach"], "The fmail read-command lesson state."),
    command("mcp", &["mcp"], "MCP sidecar client commands."),
];

#[rustfmt::skip]
const MAIL: &[Node] = &[
    command("send", &["mail", "send"], "Send a message to a registered agent or a project."),
    command("reply", &["mail", "reply"], "Reply to a message, routed by the answered message's lane."),
    command("unread", &["mail", "unread"], "Show unread bus messages addressed to a name."),
    command("show", &["mail", "show"], "Print one message."),
    command("ack", &["mail", "ack"], "Advance a read cursor to a message id."),
    command("list", &["mail", "list"], "Retired: prints its replacement, unread."),
    command("drain", &["mail", "drain"], "Retired: prints its replacement, drain-self."),
    command("status", &["mail", "status"], "Retired: prints its replacement, unread."),
    command("triage", &["mail", "triage"], "Retired: prints its replacement, show."),
    command("ask", &["ask"], "Send a follow-up message to a registered agent."),
    command("announce", &["announce"], "Fleet announcements on one scope."),
    group("admin", "Mailbox upkeep: holds, sends, retractions, lessons.", MAIL_ADMIN),
];

#[rustfmt::skip]
const ORG_CHART: &[Node] = &[
    moved("fold", &["org-fold"], &[&["org", "fold"]], "The role scope fold."),
    moved("vacancies", &["org-vacancies"], &[&["org", "vacancies"]], "Roles whose holder is gone."),
    moved("shape", &["lead", "shape"], &[&["org", "shape"]], "Declare this term's shape: a pure pass, or a team."),
    moved("ledger", &["lead", "ledger"], &[&["org", "ledger"]], "Render the term ledger page."),
    Node {
        pages_out: true,
        ..moved(
            "rundown",
            &["lead", "ledger"],
            &[&["org", "rundown"]],
            "The per-team rundown page.",
        )
    },
    moved("manifest-path", &["lead", "manifest-path"], &[&["org", "manifest-path"]], "This promoted session's scope manifest path."),
    moved("drain", &["lead", "drain"], &[&["org", "drain"]], "How many scope nodes are not done."),
    moved("term", &["lead", "term"], &[&["org", "term"]], "Declare or extend this term."),
    command("checkin-raw", &["lead-checkin"], "The check-in body over explicit scope and event paths."),
    command("history-raw", &["lead-history"], "The check-in readback over explicit scope and event paths."),
    command("rundown-raw", &["lead-rundown"], "The rundown page renderer over explicit inputs."),
];

#[rustfmt::skip]
const ORG: &[Node] = &[
    command("init", &["lead", "init"], "Write the role manifest (step 1 of the lead skill)."),
    command("checkin", &["lead", "checkin"], "One lead beat: gather, print, diff, journal."),
    command("refresh-canon", &["lead", "refresh-canon"], "Refresh the beat's canon doc, resolving the plugin root itself."),
    command("done", &["lead", "done"], "Expire this role: vacate the row and clear the manifest."),
    command("cancel", &["lead", "cancel"], "Set or clear the cancel signal beside a role manifest."),
    command("escalate", &["lead", "escalate"], "Escalate to the presiding role with work pending."),
    command("verdict", &["lead", "verdict"], "Read the term tenure verdict."),
    command("history", &["lead", "history"], "This role's recorded term, newest first."),
    command("promote", &["role"], "Grant a role to a live session: <session> --scope <scope>."),
    command("faq", &["lead", "faq"], "Durable answers a promoted session leaves behind."),
    command("roles", &["roles"], "Bounded business-role definitions and resolutions."),
    group("chart", "The org chart's folds, pages and term records.", ORG_CHART),
];

#[rustfmt::skip]
const DISTRESS: &[Node] = &[
    command("scan", &["distress-scan"], "Read a transcript for a <help> tag; record a hit."),
    command("verdicts", &["distress-verdicts"], "Print each session's distress verdict."),
];

#[rustfmt::skip]
const LOOP: &[Node] = &[
    command("run", &["loop", "run"], "The unified driver loop: run --driver target."),
    command("check", &["loop-check"], "The stop-hook decision over external truth (read-only)."),
    command("control", &["loops"], "Every scheduled loop: the arms table, pause-all, resume-all."),
    command("finalize", &["finalize"], "The terminal-only side-effect writer."),
    command("probe", &["probe-run"], "Evaluate a plan's named probe list."),
    command("kill-check", &["kill-check"], "Evaluate a plan's kill_criteria."),
    command("verify-evidence", &["verify-evidence"], "Verify child-promise event evidence."),
    command("prove-it", &["prove-it-verdicts"], "Read terminal prove-it records."),
    command("review-coverage", &["review-coverage"], "Emit the review_coverage event for a PR."),
    command("judge", &["judge"], "Grade a plan against the five product questions."),
    group("distress", "The <help> tag scan and its verdicts.", DISTRESS),
    command("attest", &["attest"], "The review-attestation carrier over the shared primary."),
];

#[rustfmt::skip]
const FLEET: &[Node] = &[
    command("census", &["census"], "One JSON row per long-lived process, with its build drift."),
    command("top", &["top"], "Every live worker process, fno-spawned and foreign."),
    command("orphans", &["orphans"], "Processes that outlived whatever started them."),
    command("sweep", &["sweep"], "Workers whose transcripts went quiet past their deadline."),
    command("watchdog", &["watchdog"], "Unfinished work: started nodes nobody holds."),
    command("truth", &["truth"], "A worker's supervision state from its transcript tail."),
    command("trace", &["trace"], "An agent's dispatch lifecycle from the event store."),
    command("history", &["history"], "Each matching session's whole story."),
    command("digest", &["digest"], "The catch-up fold over events and ledger for a session."),
    command("feed", &["feed"], "The activity feed over questions and the graph."),
    command("needs", &["needs"], "The needs-me queue across all sessions."),
    command("pane-identity", &["pane-identity"], "Cross-check mux panes against registry rows."),
];

#[rustfmt::skip]
const AUDIT: &[Node] = &[
    command("bash-census", &["bash-census"], "Bash-call shares and top command tables over transcripts."),
    command("honesty-sweep", &["honesty-sweep"], "Declared-vs-measured sweep over declared populations."),
    command("evals-macro", &["evals-macro"], "The macro-eval failure-pattern leaderboard."),
    command("yard", &["yard"], "The yard identity fold per registry citizen."),
    command("drive-authority", &["drive-authority"], "Whether an operator holds a gate-hardening drive window."),
    command("stale-escalate", &["stale-escalate"], "Reconcile the stale-row question to the measured fleet."),
    command("friction-escalate", &["friction-escalate"], "Reconcile one watchdog-friction question to the fleet."),
    moved("incident", &["incident"], &[&["fleet-incident"]], "The durable fleet incident breaker."),
    command("test-run", &["test-run"], "The native test-suite process-group owner."),
    command("harness", &["harness"], "Instruments over the harness capability table."),
];

#[rustfmt::skip]
const SESSION: &[Node] = &[
    command("list", &["list"], "List registered agents."),
    command("logs", &["logs"], "Tail or follow an agent's log output."),
    command("attach", &["attach"], "Attach to a running agent session."),
    command("peek", &["peek"], "Observe a peer read-only."),
    command("watch", &["watch"], "Observe a held stream-json thread's turns live."),
    command("wait", &["wait"], "Block until an agent's row reaches a state."),
    command("stop", &["stop"], "Stop an agent's underlying session."),
    command("rm", &["rm"], "Remove an agent: its session first, its registry row after."),
    command("resume", &["resume"], "Resume a registered agent."),
    command("recover", &["recover"], "Restore a recorded claude session under its account."),
    command("restart", &["restart"], "Restart running fno processes onto fresh binaries."),
    command("status", &["status"], "Daemon liveness and the control-plane arms table."),
];

#[rustfmt::skip]
const REGISTRY: &[Node] = &[
    command("adopt", &["adopt"], "Register an orphaned session by its session id."),
    command("register", &["register"], "Join this session to the mesh roster."),
    command("rename", &["rename"], "Rename a registry row's label."),
    command("name", &["name"], "The bridge to the canonical agent-name owner."),
    command("retask", &["retask"], "Retask one finished worker onto the node's next command."),
    command("reap", &["reap"], "Retire finished agent rows."),
    command("reconcile", &["reconcile"], "Sync registry status with provider reality."),
    command("report", &["report"], "The inside-leg state push the per-turn hook calls."),
    command("whoami", &["whoami"], "This mesh worker's own registered name."),
    command("compaction", &["compaction"], "Compaction stamps."),
    command("transcript", &["transcript"], "Move a session bundle to another computer."),
    command("subscribe", &["subscribe"], "Stream registry transitions and pane exits as NDJSON."),
];

#[rustfmt::skip]
const CLAIM: &[Node] = &[
    command("acquire", &["claim", "acquire"], "Acquire a claim on KEY for HOLDER."),
    command("list", &["claim", "list"], "Enumerate claims under the claims directory."),
    command("reap", &["claim", "reap"], "Archive every provably-dead claim."),
    command("refresh", &["claim", "refresh"], "Extend a TTL claim's expires_at."),
    command("release", &["claim", "release"], "Release a claim this holder owns."),
    command("session-pid", &["claim", "session-pid"], "Resolve the durable session pid."),
    command("status", &["claim", "status"], "Inspect a single claim."),
    command("backlog", &["claim", "backlog"], "The shared-store backlog claims."),
    command("export", &["claim", "export"], "Export the shared claim store."),
];

#[rustfmt::skip]
const WORKSPACE: &[Node] = &[
    command("register-worker", &["workspace", "register-worker"], "Register a worker in the workers registry."),
    command("reap", &["workspace", "reap"], "Age-reap expendable state files."),
    command("worktree", &["workspace", "worktree"], "Worktree lifecycle: ensure, status, cleanup, archive."),
    command("worker", &["worker"], "Delivery worker phases."),
];

#[rustfmt::skip]
const INTERNAL: &[Node] = &[
    command("registry-json", &["registry-json"], "Registry rows as JSON, daemon-free."),
    command("discovered-json", &["discovered-json"], "The discovered-live-sessions lane as JSON."),
    command("codex-session-for-pid", &["codex-session-for-pid"], "A codex pane's session id from its rollout."),
    command("heal-token", &["heal-token"], "Adopt the session a token names, as JSON."),
    command("registry-repair", &["registry-repair"], "Roll a poisoned registry's schema_version back."),
    command("select-read", &["select-read"], "One bounded backlog read under the select timeout."),
    command("worked-nodes", &["worked-nodes"], "The worked-overlay join over a stdin rows payload."),
    command("target-family", &["target-family"], "The merge-posture family test for a message."),
    command("session-start-bytes", &["session-start-bytes"], "The session-start preamble byte total."),
    command("permission-tokens", &["permission-tokens"], "The permission-posture owner over a stdin payload."),
    command("sandbox-probe", &["sandbox-probe"], "The codex sandbox verdict over a stdin payload."),
    command("authorized-merge", &["authorized-merge"], "The one authorized merge operation over stdin JSON."),
];

#[rustfmt::skip]
const ENTRIES: &[Node] = &[
    command("spawn", &["spawn"], "Spawn a new agent; `mail ask` is the follow-up lane."),
    group("admit", "Spawn admission: gate, guard, overlays, axes, slot.", ADMIT),
    kept("mail", &["mail"], "Durable polled mailbox: send, reply, unread, ack, drain.", MAIL),
    Node {
        bare: true,
        strict: true,
        ..kept(
            "org",
            &["team"],
            "The org chart of roles and their lifecycle.",
            ORG,
        )
    },
    group("loop", "The driver loop, its stop check, and the verdicts it reads.", LOOP),
    group("fleet", "Fleet reads: processes, workers, transcripts, feeds.", FLEET),
    group("audit", "Sweeps, censuses and the incident breaker.", AUDIT),
    group("session", "One agent's session: list, logs, attach, stop, resume.", SESSION),
    group("registry", "Registry rows: register, name, reap, reconcile, report.", REGISTRY),
    kept("claim", &["claim"], "Work-claim coordination primitive.", CLAIM),
    kept("workspace", &["workspace"], "Worktree lifecycle and worker registration.", WORKSPACE),
    group("internal", "Stdin-JSON commands a hook or the daemon calls.", INTERNAL),
];

/// The whole `agents` group as one node.
pub static ROOT: Node = Node {
    strict: false,
    ..group(
        "agents",
        "sessions, mail, roles and the loop that drives them",
        ENTRIES,
    )
};

/// What the front door does with an invocation this module claims.
#[derive(Debug, PartialEq, Eq)]
pub enum Agents {
    /// Go on through the front door with this argv.
    Run(Vec<OsString>),
    /// Print this help on stdout, exit 0.
    Help(String),
    /// Print this refusal on stderr, exit 2.
    Refuse(String),
}

fn os(text: &str) -> OsString {
    OsString::from(text)
}

/// `--out` for `org chart rundown` at the place-resolved rundown page when
/// the state root is faithful; None otherwise (the ledger then writes its
/// own default, and the web bridge writes nothing).
#[cfg(not(test))]
fn rundown_out_arg() -> Option<OsString> {
    let (root, faithful) = crate::org_root::lead_state_root();
    if !faithful {
        return None;
    }
    Some(OsString::from(
        crate::state_layout::place(&root, "rundown.html")
            .display()
            .to_string(),
    ))
}

#[cfg(test)]
fn rundown_out_arg() -> Option<OsString> {
    None
}

/// `agents` + the node's legacy words + the caller's tail, plus the rundown
/// page's default `--out` when the caller gave none.
fn forward(node: &Node, rest: &[OsString], default_out: Option<OsString>) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![os("agents")];
    argv.extend(node.legacy.iter().map(|w| os(w)));
    argv.extend(rest.iter().cloned());
    if node.pages_out {
        let has_out = rest.iter().any(|a| {
            a.to_str()
                .map(|s| s == "--out" || s.starts_with("--out="))
                .unwrap_or(false)
        });
        if let (false, Some(path)) = (has_out, default_out) {
            argv.push(os("--out"));
            argv.push(path);
        }
    }
    argv
}

/// Every node under `node`, with its path of names below `agents`.
pub fn walk(node: &'static Node) -> Vec<(Vec<&'static str>, &'static Node)> {
    fn go(
        node: &'static Node,
        path: &mut Vec<&'static str>,
        out: &mut Vec<(Vec<&'static str>, &'static Node)>,
    ) {
        for child in node.children {
            path.push(child.name);
            out.push((path.clone(), child));
            go(child, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    go(node, &mut Vec::new(), &mut out);
    out
}

/// The spellings a node answered to before this table: its legacy words
/// when they differ from its path, and its listed older spellings. The
/// rundown page shares `lead ledger` with `org chart ledger`, which keeps it.
pub fn old_spellings(path: &[&str], node: &Node) -> Vec<&'static [&'static str]> {
    let mut spellings: Vec<&'static [&'static str]> = node.old.to_vec();
    if !node.legacy.is_empty() && node.legacy != path && !node.pages_out {
        spellings.push(node.legacy);
    }
    spellings
}

/// The node whose old spelling is the longest prefix of `words`.
fn old_hit(words: &[&str]) -> Option<(Vec<&'static str>, &'static Node, usize)> {
    let mut best: Option<(Vec<&'static str>, &'static Node, usize)> = None;
    for (path, node) in walk(&ROOT) {
        for spelling in old_spellings(&path, node) {
            let len = spelling.len();
            if words.len() >= len
                && words[..len] == *spelling
                && best.as_ref().map(|b| b.2 < len).unwrap_or(true)
            {
                best = Some((path.clone(), node, len));
            }
        }
    }
    best
}

fn spelled(words: &[&str]) -> String {
    if words.is_empty() {
        "fno agents".to_string()
    } else {
        format!("fno agents {}", words.join(" "))
    }
}

/// The menu of one group: every child, one line each. Nothing is hidden,
/// so this list is the whole group.
pub fn menu(path: &[&str], node: &Node) -> String {
    let width = node
        .children
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(0);
    let mut text = format!("{} - {}\n\n", spelled(path), node.about);
    for child in node.children {
        let mark = if child.children.is_empty() {
            ""
        } else {
            " (group)"
        };
        text.push_str(&format!("  {:width$}  {}{mark}\n", child.name, child.about));
    }
    text.push_str(&format!(
        "\n{} <command> --help shows one command. Nothing is hidden: this list is the whole group.\n",
        spelled(path)
    ));
    text
}

fn unknown(path: &[&str], node: &Node, word: &str) -> String {
    let names: Vec<&str> = node.children.iter().map(|c| c.name).collect();
    format!(
        "{} {word}: unknown command. Commands: {}. `{} --help` shows them.",
        spelled(path),
        names.join(", "),
        spelled(path)
    )
}

/// Resolve the words after `agents`, returning the outcome and the one
/// stderr notice an old spelling prints. None hands the argv on unchanged.
fn resolve(args: &[OsString]) -> Option<(Agents, Option<String>)> {
    let words: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let mut node: &'static Node = &ROOT;
    let mut depth = 0usize;
    loop {
        let path = &words[..depth];
        let rest = &args[depth..];
        if node.children.is_empty() {
            return Some((Agents::Run(forward(node, rest, rundown_out_arg())), None));
        }
        match words.get(depth).copied() {
            None if node.bare => return Some((Agents::Run(forward(node, rest, None)), None)),
            None | Some("-h" | "--help") => return Some((Agents::Help(menu(path, node)), None)),
            Some(flag) if flag.starts_with('-') => {
                if node.bare {
                    return Some((Agents::Run(forward(node, rest, None)), None));
                }
                if node.strict {
                    return Some((Agents::Refuse(unknown(path, node, flag)), None));
                }
                return None;
            }
            Some(word) => {
                if let Some(child) = node.children.iter().find(|c| c.name == word) {
                    node = child;
                    depth += 1;
                    continue;
                }
                if let Some((new_path, hit, len)) = old_hit(&words) {
                    let notice = format!(
                        "{} is now {}; the old spelling goes after one release.",
                        spelled(&words[..len]),
                        spelled(&new_path)
                    );
                    let argv = forward(hit, &args[len..], rundown_out_arg());
                    return Some((Agents::Run(argv), Some(notice)));
                }
                if node.strict {
                    return Some((Agents::Refuse(unknown(path, node, word)), None));
                }
                return None;
            }
        }
    }
}

/// `fno help agents [<entry>...] [--all]`: the same menus, claimed here
/// before the Python help command sees them. A command's own help forwards.
fn help(args: &[OsString]) -> Option<Agents> {
    let words: Vec<&str> = args
        .iter()
        .map(|a| a.to_str())
        .collect::<Option<Vec<&str>>>()?
        .into_iter()
        .filter(|w| !w.starts_with('-'))
        .collect();
    let mut node: &'static Node = &ROOT;
    for (depth, word) in words.iter().enumerate() {
        match node.children.iter().find(|c| c.name == *word) {
            Some(child) => node = child,
            None => return Some(Agents::Refuse(unknown(&words[..depth], node, word))),
        }
    }
    if node.children.is_empty() {
        let mut argv = forward(node, &[], None);
        argv.push(os("--help"));
        return Some(Agents::Run(argv));
    }
    Some(Agents::Help(menu(&words, node)))
}

/// The lexical claim on `fno agents ...` and `fno help agents ...`. Some
/// whenever this module decides the invocation; the notice an old spelling
/// owes is printed here, on stderr.
pub fn classify(args: &[OsString]) -> Option<Agents> {
    match (
        args.first().and_then(|a| a.to_str()),
        args.get(1).and_then(|a| a.to_str()),
    ) {
        (Some("help"), Some("agents")) => return help(&args[2..]),
        (Some("agents"), _) => {}
        _ => return None,
    }
    if let Some(command) = args.get(1).and_then(|a| a.to_str()) {
        if let Some(instead) = crate::role_migration::retired_verb(command) {
            return Some(Agents::Refuse(format!(
                "this role command is retired; use {instead}"
            )));
        }
        if command == "promote" {
            return Some(Agents::Refuse(
                "fno agents promote is now fno agents org promote <session> --scope <scope>".into(),
            ));
        }
    }
    let (outcome, notice) = resolve(&args[1..])?;
    if let Some(notice) = notice {
        eprintln!("{notice}");
    }
    Some(outcome)
}

/// The fno-agents argv prefix for each forwarded `agents <word>` the front
/// execs natively. The front execs from this table, and the Explorer tabs
/// resolve their public argv through it, so a leaf rename moves both.
const NATIVE_LEAF: &[(&str, &[&str])] = &[
    ("top", &["census", "--workers"]),
    ("org-fold", &["org-fold"]),
];

/// The leaf prefix for a natively exec'd forwarded word; empty for any other.
pub fn native_leaf(word: &str) -> &'static [&'static str] {
    NATIVE_LEAF
        .iter()
        .find(|(w, _)| *w == word)
        .map_or(&[], |(_, leaf)| leaf)
}

/// The fno-agents argv a public `fno agents ...` argv execs, when the front
/// execs it natively; None when the front would route it anywhere else.
pub fn native_leaf_argv(public: &[&str]) -> Option<Vec<OsString>> {
    let args: Vec<OsString> = public.iter().map(OsString::from).collect();
    let forwarded = match resolve(args.get(1..)?)? {
        (Agents::Run(argv), None) => argv,
        _ => return None,
    };
    let leaf = native_leaf(forwarded.get(1)?.to_str()?);
    if leaf.is_empty() {
        return None;
    }
    Some(
        leaf.iter()
            .map(OsString::from)
            .chain(forwarded[2..].iter().cloned())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osv(texts: &[&str]) -> Vec<OsString> {
        texts.iter().map(OsString::from).collect()
    }

    fn run(texts: &[&str]) -> Option<(Agents, Option<String>)> {
        resolve(&osv(texts))
    }

    /// The Explorer tabs' public argv resolves to the leaf the front execs.
    #[test]
    fn explorer_tab_argv_resolves_to_its_native_leaf() {
        assert_eq!(
            native_leaf_argv(crate::org_model::FLEET_TOP_ARGV),
            Some(osv(&["census", "--workers", "--machines", "--json"]))
        );
        assert_eq!(
            native_leaf_argv(crate::org_model::ORG_FOLD_ARGV),
            Some(osv(&["org-fold"]))
        );
        assert_eq!(native_leaf_argv(&["agents", "spawn"]), None);
    }

    #[test]
    fn agents_menu_nested_spellings_rewrite_and_old_ones_notice() {
        // A nested command forwards the argv that answers today, tail intact.
        assert_eq!(
            run(&["fleet", "census", "--json"]),
            Some((Agents::Run(osv(&["agents", "census", "--json"])), None))
        );
        assert_eq!(
            run(&["mail", "admin", "hold", "1h"]),
            Some((Agents::Run(osv(&["agents", "mail", "hold", "1h"])), None))
        );
        assert_eq!(
            run(&["claim", "release", "node:x"]),
            Some((
                Agents::Run(osv(&["agents", "claim", "release", "node:x"])),
                None
            ))
        );
        assert_eq!(
            run(&["spawn", "--json"]),
            Some((Agents::Run(osv(&["agents", "spawn", "--json"])), None))
        );
        // An old flat spelling still answers, with one notice naming the
        // nested form; a nested old spelling rewrites to its forward.
        for (old, argv, new) in [
            (
                &["loop-check", "-J"][..],
                &["agents", "loop-check", "-J"][..],
                "fno agents loop check",
            ),
            (
                &["mail", "hold"][..],
                &["agents", "mail", "hold"][..],
                "fno agents mail admin hold",
            ),
            (
                &["lead", "checkin"][..],
                &["agents", "lead", "checkin"][..],
                "fno agents org checkin",
            ),
            (
                &["team", "-J"][..],
                &["agents", "team", "-J"][..],
                "fno agents org",
            ),
            (
                &["org", "fold", "fno"][..],
                &["agents", "org-fold", "fno"][..],
                "fno agents org chart fold",
            ),
            (
                &["fleet-incident"][..],
                &["agents", "incident"][..],
                "fno agents audit incident",
            ),
        ] {
            match run(old) {
                Some((Agents::Run(got), Some(notice))) => {
                    assert_eq!(got, osv(argv), "{old:?}");
                    assert!(notice.contains(new), "{notice}");
                    assert!(notice.contains("one release"), "{notice}");
                }
                other => panic!("{old:?}: expected a noticed Run, got {other:?}"),
            }
        }
        // The org group keeps its shapes: bare and flag-led forward the
        // team argv, rundown adds the default out only when absent.
        assert_eq!(
            run(&["org"]),
            Some((Agents::Run(osv(&["agents", "team"])), None))
        );
        assert_eq!(
            run(&["org", "-J"]),
            Some((Agents::Run(osv(&["agents", "team", "-J"])), None))
        );
        // The canon-refresh row forwards to its lead-family door.
        assert_eq!(
            run(&["org", "refresh-canon"]),
            Some((Agents::Run(osv(&["agents", "lead", "refresh-canon"])), None))
        );
        let rundown = walk(&ROOT)
            .into_iter()
            .find(|(p, _)| p == &["org", "chart", "rundown"])
            .unwrap()
            .1;
        let out = Some(OsString::from("/s/pages/rundown.html"));
        assert_eq!(
            forward(rundown, &[], out.clone()),
            osv(&["agents", "lead", "ledger", "--out", "/s/pages/rundown.html"])
        );
        assert_eq!(
            forward(rundown, &osv(&["--out=/x.html"]), out),
            osv(&["agents", "lead", "ledger", "--out=/x.html"])
        );
        // An unknown command under a group this table defines refuses with one
        // line listing the group; under a kept group it goes on unchanged.
        match run(&["fleet", "nope"]) {
            Some((Agents::Refuse(line), None)) => {
                assert!(
                    line.contains("census") && line.contains("pane-identity"),
                    "{line}"
                );
                assert!(!line.contains('\n'), "{line}");
            }
            other => panic!("expected Refuse, got {other:?}"),
        }
        assert_eq!(run(&["mail", "read", "fmail-x"]), None);
        assert_eq!(run(&["no-such-command"]), None);
        // Menus are native and list every child.
        match run(&["--help"]) {
            Some((Agents::Help(text), None)) => {
                for entry in ENTRIES {
                    assert!(text.contains(entry.name), "{text}");
                }
            }
            other => panic!("expected Help, got {other:?}"),
        }
        assert!(matches!(run(&["session"]), Some((Agents::Help(_), None))));
        assert!(matches!(
            help(&osv(&["fleet", "--all"])),
            Some(Agents::Help(text)) if text.contains("watchdog")
        ));
        assert_eq!(
            help(&osv(&["fleet", "census"])),
            Some(Agents::Run(osv(&["agents", "census", "--help"])))
        );
        assert_eq!(classify(&osv(&["mux", "ls"])), None);
        assert!(matches!(
            classify(&osv(&["agents", "promote", "x"])),
            Some(Agents::Refuse(m)) if m.contains("fno agents org promote")
        ));
    }

    #[test]
    fn agents_menu_holds_twelve_at_each_level_and_places_every_python_leaf() {
        assert!(ENTRIES.len() <= 12);
        for (path, node) in walk(&ROOT) {
            if path.len() < 2 {
                assert!(
                    node.children.len() <= 12,
                    "agents {path:?} holds {}",
                    node.children.len()
                );
            }
        }
        // Every `agents` row of the collapse map resolves to exactly one
        // destination: the node whose path, forward, or old spelling is the
        // longest prefix of the row, or a deleted command's tombstone.
        let deleted = ["gate", "ping", "team-fold", "team-orphans", "term-ledger"];
        let map = include_str!("../../../scripts/ci/command-collapse-map.tsv");
        let mut unplaced: Vec<String> = Vec::new();
        for line in map.lines().skip(1) {
            let leaf = line.split('\t').next().unwrap_or("");
            let words: Vec<&str> = leaf.split(' ').collect();
            if words.first() != Some(&"agents") || words.len() < 2 {
                continue;
            }
            let row = &words[1..];
            if deleted.contains(&row[0]) {
                continue;
            }
            let mut best = 0usize;
            let mut hits = 0usize;
            for (path, node) in walk(&ROOT) {
                let mut spellings = old_spellings(&path, node);
                spellings.push(&[]);
                for spelling in spellings {
                    let candidate: Vec<&str> = if spelling.is_empty() {
                        path.clone()
                    } else {
                        spelling.to_vec()
                    };
                    let len = candidate.len();
                    if row.len() >= len && row[..len] == candidate[..] {
                        if len > best {
                            best = len;
                            hits = 1;
                        } else if len == best {
                            hits += 1;
                        }
                    }
                }
            }
            if best == 0 || hits != 1 {
                unplaced.push(format!("{leaf} ({hits} destinations)"));
            }
        }
        assert!(
            unplaced.is_empty(),
            "agents rows with no single home: {unplaced:#?}"
        );
    }
}
