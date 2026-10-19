"""Fixture-driven tests for the machine-footprint parser."""

from __future__ import annotations

from fno.footprint import parse_footprint


def test_ac1_hp_attributes_transitive_build_descendants_and_excludes_negative_tree() -> None:
    reading = parse_footprint(
        """\
        PID PPID ELAPSED %CPU RSS COMMAND
        100 1 01:00:00 20.0 1024 fno-agents-worker --run
        101 100 01:00:00 0.0 1024 /bin/sh -c cargo test
        102 101 00:00:05 80.0 1024 cargo test -p fno
        103 101 00:00:05 60.0 1024 rustc --crate-name fno
        200 1 01:00:00 0.0 1024 /bin/sh -c cargo test elsewhere
        201 200 01:00:00 90.0 1024 cargo test -p unrelated
        202 200 01:00:00 70.0 1024 rustc --crate-name unrelated
        """,
    )

    assert reading.descendant_cpu_cores == 1.4
    assert reading.fleet_cpu_cores == 1.6
    assert reading.descendant_process_count == 3
    assert reading.direct_process_count == 1
    assert reading.process_count == 4
    assert [command for _, command in reading.top] == [
        "cargo test -p fno",
        "rustc --crate-name fno",
        "fno-agents-worker --run",
    ]


def test_ac1_edge_attributes_detached_registered_root_and_descendants() -> None:
    ps_output = """\
        PID PPID ELAPSED %CPU RSS COMMAND
        300 1 00:00:05 20.0 1024 opencode serve --detach
        301 300 00:01:00 80.0 1024 cargo test -p fno
        400 1 01:00:00 90.0 1024 cargo test -p unrelated
    """

    reading = parse_footprint(ps_output, attributed_root_pids={300})

    assert reading.process_count == 2
    assert reading.direct_process_count == 1
    assert reading.descendant_process_count == 1
    assert reading.sustained_cpu_cores == 0.2
    assert reading.transient_call_count == 0
    assert reading.descendant_cpu_cores == 0.8
    assert reading.fleet_cpu_cores == 1.0
    assert [command for _, command in reading.top] == [
        "cargo test -p fno",
        "opencode serve --detach",
    ]


def test_ac1_edge_shared_serve_is_not_a_direct_threshold_process() -> None:
    reading = parse_footprint(
        """\
        PID PPID ELAPSED %CPU RSS COMMAND
        300 1 01:00:00 20.0 1024 fno-agents-worker --run
        400 1 01:00:00 0.0 1024 opencode serve --detach
        401 400 01:00:00 10.0 1024 cargo test -p fno
        """,
        attributed_root_pids={300, 400},
        threshold_excluded_root_pids={400},
    )

    assert reading.process_count == 3
    assert reading.direct_process_count == 1
    assert reading.descendant_process_count == 1
    assert reading.fleet_cpu_cores == 0.3


def test_ac1_hp_counts_attributed_rows_and_sustained_cpu() -> None:
    reading = parse_footprint(
        """\
        PID ELAPSED %CPU RSS COMMAND
        101 01:00:00 20.0 1048576 /usr/local/bin/fno mux serve
        102 00:00:05 90.0 2048 fno --version
        103 02:00:00 80.0 4096 claude --worker
        104 00:45:00 10.0 524288 fno-agents-daemon --serve
        """,
    )

    assert reading.sustained_cpu_cores == 0.3
    assert reading.transient_call_count == 1
    assert reading.process_count == 3
    assert reading.rss_gb == (1048576 + 2048 + 524288) / (1024 * 1024)
    assert reading.top == [
        (20.0, "/usr/local/bin/fno mux serve"),
        (10.0, "fno-agents-daemon --serve"),
    ]


#: Command lines measured on one machine at 2026-09-04. A naive
#: `ps | grep -i pytest` reported four running tests here; two were real.
_REAL_CARGO_TEST = (
    "/Users/bb16/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo test "
    "--manifest-path crates/fno/Cargo.toml --lib portal_tests"
)
_LEAKED_KEEPER = (
    "/Users/bb16/.cargo/bin/fno-agents-worker --keeper --sock "
    "/private/var/folders/ch/T/fno-laneb-6nkmletp/mux/threads/wk-journey.sock "
    "--session wk-journey --cwd "
    "/private/var/folders/ch/T/pytest-of-bb16/pytest-313/test_lane_b_journey0"
)
_ZSH_WRAPPER = (
    "/bin/zsh -c -l eval 'cargo test --manifest-path crates/fno-agents/Cargo.toml "
    "--lib gc 2>&1 | tail -3'"
)


def test_ac3_hp_the_parser_counts_only_the_real_runners() -> None:
    """The positive control over one snapshot: four rows carry the word, two
    are tests. A count that read the whole command line would say four."""
    from fno.footprint import parse_footprint

    snapshot = "\n".join(
        [
            "PID PPID ELAPSED %CPU RSS COMMAND",
            f"100 1 01:00:00 10.0 1024 {_REAL_CARGO_TEST}",
            "101 1 01:00:00 10.0 1024 /usr/bin/python3 -m pytest cli/tests",
            f"102 1 01:00:00 10.0 1024 {_LEAKED_KEEPER}",
            f"103 1 01:00:00 10.0 1024 {_ZSH_WRAPPER}",
        ]
    )

    assert parse_footprint(snapshot).test_process_count == 2


def test_ac8_hp_top_consumers_name_the_process_and_the_worktree() -> None:
    """x-aeab AC8: 23 fno-py rows and 18 fno-agents-worker rows from one
    worktree aggregate to two consumers, the top named with its process
    count, the worktree named with how many of its rows run from it."""
    from fno.footprint import top_consumers

    fno_py = [
        (200.0 - float(i), f"/usr/local/bin/python3 fno-py doctor test {i}")
        for i in range(23)
    ]
    workers = [
        (90.0 - i, f"fno-agents-worker debug .fno/worktrees/x-b1ee/target/{i}")
        for i in range(18)
    ]
    sustained = fno_py + workers

    top = top_consumers(sustained)

    assert top[0]["name"] == "python3"
    assert top[0]["procs"] == 23
    # An argv that names no worktree carries None, not an empty string.
    assert top[0]["worktree"] is None
    assert top[0]["worktree_procs"] == 0
    assert top[1]["name"] == "fno-agents-worker"
    assert top[1]["procs"] == 18
    assert top[1]["worktree"] == ".fno/worktrees/x-b1ee"
    assert top[1]["worktree_procs"] == 18
    # Bounded to five consumers.
    assert len(top) <= 5


#: Measured 2026-09-07: the Claude Code background daemon's pre-warm pool
#: under both spellings it uses. Neither is an fno process, so neither is
#: fleet-attributable; they are counted only so a load refusal can name them.
_BG_SPARE = "claude bg-spare --bg-spare /tmp/cc-daemon-501/608d3bdb/spare/235506db.claim.sock"
_BG_PTY_HOST = (
    "claude bg-pty-host --bg-pty-host /tmp/cc-daemon-501/608d3bdb/spare/235506db.pty.sock 200 50"
)
_BG_PTY_HOST_APP_PATH = (
    "/Users/bb16/.local/share/claude/versions/2.1.263 --bg-spare "
    "/tmp/cc-daemon-501/608d3bdb/spare/235506db.claim.sock"
)


def test_is_claude_spare_pool_edge_a_prompt_naming_it_is_not_the_pool() -> None:
    """The trap this predicate closes: a real claude session whose PROMPT or
    argument happens to contain the word is not a pre-warm process. Matched on
    argv[0]/argv[1], never a substring scan of the whole command line."""
    from fno.footprint import is_claude_spare_pool

    assert not is_claude_spare_pool("claude chat please explain bg-spare to me")
    assert not is_claude_spare_pool(_BG_PTY_HOST_APP_PATH)  # argv[0] is not "claude"
    assert not is_claude_spare_pool("")
    assert not is_claude_spare_pool("cargo build --release")


def test_ac_hp_spare_pool_rows_are_counted_but_never_fleet_attributed() -> None:
    """A snapshot with pool rows yields a nonzero pool count, and those rows
    are not counted as fno-attributable fleet CPU (they are not fno processes
    at all, so `parse_footprint` never attributed them either)."""
    reading = parse_footprint(
        """\
        PID PPID ELAPSED %CPU RSS COMMAND
        100 1 01:00:00 20.0 1024 fno-agents-worker --run
        300 1 00:05:00 15.0 118784 {bg_spare}
        301 1 00:05:00 12.0 118784 {bg_pty_host}
        """.format(bg_spare=_BG_SPARE, bg_pty_host=_BG_PTY_HOST),
    )

    assert reading.spare_pool_process_count == 2
    assert reading.spare_pool_cpu_cores == 0.27
    assert reading.spare_pool_rss_gb > 0
    # The fleet number is fno-attributable CPU only; the pool never enters it.
    assert reading.fleet_cpu_cores == 0.2
    assert reading.process_count == 1


_GOOD_ROW = "100 1 01:00:00 20.0 1024 fno-agents-worker --run"


def test_ac0_shapes_the_same_four_shapes_under_the_state_header() -> None:
    # The shape production ps now writes (x-d6ad added STATE): the classifier
    # must probe at the shifted indices, not read the state letter as etime.
    header = "PID PPID STATE ELAPSED %CPU RSS COMMAND"
    good = "100 1 S 01:00:00 20.0 1024 fno-agents-worker --run"
    bad_rows = [
        ("200 1 S 01:00:00 20.0 1024", "field-count"),
        ("201 1 S ??:??:?? 20.0 1024 cmd", "etime"),
        ("202 1 S 01:00:00 - 1024 cmd", "cpu"),
        ("203 1 S 01:00:00 20.0 - cmd", "rss"),
        ("not-a-pid 1 S 01:00:00 20.0 1024 cmd", "pid"),
        ("204 1 R --:-- 20.0 1024 cmd", "etime"),
    ]
    for bad_row, reason in bad_rows:
        reading = parse_footprint(f"{header}\n{good}\n{bad_row}\n")
        assert reading.unparsed_lines == 1, bad_row
        assert reading.process_count == 1, bad_row
        assert reading.fleet_cpu_cores == 0.2, bad_row
        assert reading.unparsed_samples[0].reason == reason, bad_row
    # A good STATE row is a good row: the state letter never fails a cast.
    ok = parse_footprint(f"{header}\n{good}\n")
    assert ok.unparsed_lines == 0
    assert ok.process_count == 1


def test_mask_row_never_prints_a_command_field_token() -> None:
    from fno.footprint import _mask_row

    masked = _mask_row(
        "12345 1 00:01 - 4096 /usr/bin/curl -H Authorization:Bearer SUPERSECRET1234"
    )
    for secret in ("SUPERSECRET1234", "Authorization", "curl", "Bearer"):
        assert secret not in masked


def test_mask_row_strips_control_bytes() -> None:
    from fno.footprint import _mask_row

    masked = _mask_row("12345 1 00:01\x1b[31m 5.0 4096 secret-value")
    assert "\x1b" not in masked


def test_samples_cap_at_three_while_the_count_names_all() -> None:
    bad_rows = "\n".join(
        f"{300 + n} 1 01:00:00 - 1024 fno-agents-worker --run" for n in range(5)
    )
    reading = parse_footprint(
        f"PID PPID ELAPSED %CPU RSS COMMAND\n{_GOOD_ROW}\n{bad_rows}\n",
    )

    assert reading.unparsed_lines == 5
    assert len(reading.unparsed_samples) == 3
    # The uncapped pid salvage feeds the root-relevance arm even when a bad
    # root row lands beyond the sample cap.
    assert reading.unparsed_pids == {300, 301, 302, 303, 304}
    assert [s.row for s in reading.unparsed_samples] == [2, 3, 4]
