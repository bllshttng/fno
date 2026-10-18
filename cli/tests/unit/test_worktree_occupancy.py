"""Rules R0-R5 over a fabricated process table; no live processes."""

import sys
from pathlib import Path


# The classifier is sweep-side tooling in scripts/lib (see its header), not a
# package module; it imports the fno modules it composes from the cli venv.
SCRIPT = Path(__file__).parents[3] / "scripts" / "lib" / "worktree_occupancy.py"
sys.path.insert(0, str(SCRIPT.parent))

from worktree_occupancy import INERT, RETIRE, TERMINATE, classify  # noqa: E402


HOME = "/Users/tester"


def row(pid, ppid, argv, **extra):
    d = {
        "pid": pid,
        "ppid": ppid,
        "name": argv[0].rsplit("/", 1)[-1] if argv else "",
        "cmdline": argv,
    }
    d.update(extra)
    return d


def run(pids, procs, keeper_verdicts=None, job_of_pid=None, job_state=None, home=HOME):
    return classify(
        pids,
        procs=procs,
        keeper_verdicts=keeper_verdicts,
        job_of_pid=job_of_pid,
        job_state=job_state,
        home=home,
        now=1_000_000.0,
    )


def by_pid(hits):
    return {h.pid: h for h in hits}


KEEPER = row(
    10,
    1,
    [
        "/usr/local/bin/fno-agents-worker",
        "--pane",
        "--sock",
        "/tmp/fno-mux-test-x/panes/test-1.sock",
        "--",
        "/bin/cat",
    ],
)
CAT = row(11, 10, ["/bin/cat"])
REAP_VERDICT = ("reap", "socket absent, no registry row claims it, age 90000s > grace 600s")


class TestKeeper:
    def test_reap_keeper_and_cat_child_release_the_tree(self):
        """AC1-HP: REAP keeper plus its /bin/cat child are both inert terminate."""
        hits = by_pid(run([10, 11], {10: KEEPER, 11: CAT}, keeper_verdicts={10: REAP_VERDICT}))
        assert hits[10].verdict == INERT and hits[10].action == TERMINATE
        assert hits[11].verdict == INERT and hits[11].action == TERMINATE
        assert hits[11].reason.startswith("child of 10: ")
        assert hits[10].reason == REAP_VERDICT[1]

    def test_broken_lane_keeps_the_keeper(self):
        """AC7-EDGE: a keeper with no verdict (lane broken) holds."""
        hits = by_pid(run([10], {10: KEEPER}, keeper_verdicts={}))
        assert (hits[10].verdict, hits[10].action) == ("holds", "keep")


SPARE_ARGV = ["claude", "bg-spare", "--bg-spare", "/tmp/cc-daemon-501/d/spare/s.claim.sock"]


def job_state_factory(states):
    def read(job_id):
        return states[job_id]

    return read


class TestClaudeSession:
    def test_working_blocked_and_fresh_done_hold_with_job_id_and_state(self):
        """AC3-HP: the positive control - a live-argv bg session holds."""
        for state, age in (("working", 5.0), ("blocked", 60.0), ("done", 600.0)):
            procs = {30: row(30, 1, SPARE_ARGV, cwd="/Users/tester/wt/x-live")}
            hits = by_pid(
                run(
                    [30],
                    procs,
                    job_of_pid={30: "abc123"},
                    job_state=job_state_factory({"abc123": (state, age)}),
                )
            )
            assert (hits[30].verdict, hits[30].action) == ("holds", "keep"), state
            assert "abc123" in hits[30].reason and state in hits[30].reason

    def test_terminal_silent_job_releases_the_tree(self):
        """AC4-HP: terminal state + transcript silent past 7200s -> inert retire."""
        for state in ("done", "stopped", "failed"):
            hits = by_pid(
                run(
                    [30],
                    {30: row(30, 1, SPARE_ARGV)},
                    job_of_pid={30: "abc123"},
                    job_state=job_state_factory({"abc123": (state, 7201.0)}),
                )
            )
            assert hits[30].verdict == INERT and hits[30].action == RETIRE, state
            assert hits[30].job_id == "abc123"

    def test_join_failures_hold_even_when_a_cwd_would_say_free(self):
        """AC5-EDGE: unreadable map, absent pid, unreadable state.json all hold.

        The fabricated rows carry cwd values pointing INSIDE a worktree - the
        registry cwd and state.json cwd are spawn directories and the
        classifier never reads them, so they cannot flip a row to free."""
        procs = {30: row(30, 1, SPARE_ARGV, cwd="/Users/tester/wt/x-live")}
        unreadable = job_state_factory({})

        cases = [
            run([30], procs, job_of_pid=None, job_state=unreadable),
            run([30], procs, job_of_pid={}, job_state=unreadable),
            run([30], procs, job_of_pid={99: "abc123"}, job_state=unreadable),
            run(
                [30], procs, job_of_pid={30: "nojob"}, job_state=job_state_factory({"nojob": None})
            ),
        ]
        for hits in cases:
            h = by_pid(hits)[30]
            assert (h.verdict, h.action) == ("holds", "keep")


class TestUnclassified:
    def test_unmatched_programs_hold_with_their_name(self):
        """AC6-EDGE: vim, a bare cargo, an unplaceable zsh."""
        procs = {
            40: row(40, 1, ["vim", "notes.txt"]),
            41: row(41, 1, ["cargo", "build"]),
            42: row(
                42,
                42,
                ["/bin/zsh", "-c", f"source {HOME}/.claude/shell-snapshots/snapshot-zsh.sh; true"],
            ),
        }
        hits = by_pid(run([40, 41, 42], procs))
        assert hits[40].reason == "unclassified: vim"
        assert hits[41].reason == "unclassified: cargo"
        assert hits[42].reason == "unclassified: zsh"
        for h in hits.values():
            assert (h.verdict, h.action) == ("holds", "keep")

    def test_pid_absent_from_the_table_holds(self):
        """AC6-EDGE: no ps row is a holder, never a free answer."""
        hits = run([50], {})
        h = hits[0]
        assert (h.verdict, h.action) == ("holds", "keep") and h.reason == "no ps row"
