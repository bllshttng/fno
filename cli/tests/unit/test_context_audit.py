"""Read-only context census and compiler contracts."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

from fno.context_audit import (
    ContextSource,
    SUPPORTED_HARNESSES,
    active_pitfall_headings,
    build_context_report,
    compile_context,
    measure_file_source,
)
from fno.setup.managed_block import render_block


ROOT = Path(__file__).resolve().parents[3]
pytestmark = pytest.mark.xdist_group("context-audit-processes")


def _source(
    source_id: str,
    content: bytes,
    *,
    layer: str = "progressive",
    harness: str = "claude",
    ordinal: int = 0,
) -> ContextSource:
    return ContextSource(
        source_id=source_id,
        harness=harness,
        entry_state="startup",
        lifecycle="session_start",
        layer=layer,
        provenance=f"fixture/{source_id}.md",
        carrier=f"fixture/{source_id}.sh",
        reachability_condition="fixture",
        content=content,
        ordinal=ordinal,
    )


def test_census_fingerprints_sources_and_deduplicates_content() -> None:
    shared = b"Never hand-edit managed state.\n"
    sources = [
        _source("kernel", b"Footnote safety kernel.\n", layer="kernel"),
        _source("guard-a", shared, ordinal=1),
        _source("guard-b", shared, ordinal=2),
    ]
    report = compile_context(
        sources,
        packet_budget_bytes=4_096,
        node_count=1,
    )

    records = report["source_manifest"]
    content_by_id = {source.source_id: source.content for source in sources}
    for record in records:
        assert record["harness"] == "claude"
        assert record["entry_state"] == "startup"
        assert record["bytes"] == len(content_by_id[record["source_id"]])
        assert record["estimated_tokens"] == (record["bytes"] + 3) // 4
        assert len(record["content_hash"]) == 64
        assert record["provenance"].startswith("fixture/")

    assert report["duplicates"] == [
        {
            "content_hash": hashlib.sha256(shared).hexdigest(),
            "source_ids": ["guard-a", "guard-b"],
            "occurrences": 2,
        }
    ]
    assert report["kernel"]["source_ids"] == ["kernel"]
    assert [item["source_id"] for item in report["progressive_sources"]] == ["guard-a"]


def test_packet_budget_emits_explicit_omission_and_continuation() -> None:
    report = compile_context(
        [
            _source("kernel", b"1234", layer="kernel"),
            _source("first", b"5678", ordinal=1),
            _source("second", b"90", ordinal=2),
        ],
        packet_budget_bytes=8,
        node_count=1,
    )

    assert report["packet"]["bytes"] == 8
    assert report["packet"]["within_budget"] is True
    assert [item["source_id"] for item in report["progressive_sources"]] == ["first"]
    assert report["omitted_sources"] == [
        {
            "source_id": "second",
            "provenance": "fixture/second.md",
            "reason": "packet_budget",
            "bytes": 2,
        }
    ]
    assert report["continuation"] == {
        "after_source_id": "first",
        "remaining_source_ids": ["second"],
        "remaining_bytes": 2,
    }


def test_oversized_kernel_never_allows_progressive_content_without_it() -> None:
    report = compile_context(
        [
            _source("kernel", b"kernel-too-large", layer="kernel"),
            _source("small-progressive", b"x", ordinal=1),
        ],
        packet_budget_bytes=4,
        node_count=1,
    )

    assert report["packet"]["bytes"] == 0
    assert report["kernel"]["source_ids"] == []
    assert report["progressive_sources"] == []
    assert report["continuation"]["remaining_source_ids"] == [
        "kernel",
        "small-progressive",
    ]


def test_unreadable_source_is_reported_not_dropped(tmp_path: Path) -> None:
    missing = tmp_path / "missing.md"
    source = measure_file_source(
        source_id="missing",
        path=missing,
        harness="gemini",
        entry_state="startup",
        lifecycle="session_start",
        layer="progressive",
        carrier="hooks/session-start.sh",
        reachability_condition="fixture",
        ordinal=0,
        repo_root=tmp_path,
    )

    report = compile_context([source], packet_budget_bytes=100, node_count=1)
    assert report["source_manifest"][0]["status"] == "unreadable"
    assert report["source_manifest"][0]["content_hash"] is None
    assert report["omitted_sources"][0]["reason"].startswith("unreadable:")


def test_required_pitfalls_reachable_for_every_harness() -> None:
    """Pass^3 eval target: every active capped-corpus entry reaches each kernel.

    An empty corpus is a valid state (entries age out at 60 days), so the
    kernels must then carry an empty list, not a stale one.
    """
    expected = active_pitfall_headings(ROOT)

    report = build_context_report(
        ROOT,
        harnesses=SUPPORTED_HARNESSES,
        entry_states=("startup",),
        packet_budget_bytes=100_000,
        node_count=1,
    )
    by_harness = {cell["harness"]: cell for cell in report["cells"]}

    assert set(by_harness) == set(SUPPORTED_HARNESSES)
    for harness in SUPPORTED_HARNESSES:
        kernel = by_harness[harness]["compiled"]["kernel"]
        assert kernel["anchors"]["pitfalls"] == expected
        assert "kdc-delivery-sentinel-1932" in kernel["anchors"]["sentinels"]


def test_external_host_counts_only_footnote_managed_instructions(
    tmp_path: Path,
) -> None:
    managed = render_block()
    (tmp_path / "AGENTS.md").write_text(
        "# User-owned rules\n\nNever count these bytes.\n\n" + managed + "\n",
        encoding="utf-8",
    )
    rules = tmp_path / ".claude" / "rules"
    rules.mkdir(parents=True)
    (rules / "user.md").write_text("User-owned progressive rule.\n", encoding="utf-8")
    plugin = tmp_path / ".claude-plugin"
    plugin.mkdir()
    (plugin / "plugin.json").write_text(
        json.dumps({"name": "unrelated-plugin"}),
        encoding="utf-8",
    )

    report = build_context_report(
        tmp_path,
        plugin_root=ROOT,
        harnesses=("claude", "codex"),
        entry_states=("startup",),
        packet_budget_bytes=100_000,
        node_count=1,
    )

    by_harness = {cell["harness"]: cell for cell in report["cells"]}
    claude_manifest = by_harness["claude"]["compiled"]["source_manifest"]
    assert not any(
        item["source_id"] in {"project-instructions", "managed-footnote-block"}
        for item in claude_manifest
    )
    assert not any(
        item["source_id"].startswith("claude-rule:") for item in claude_manifest
    )

    codex_manifest = by_harness["codex"]["compiled"]["source_manifest"]
    managed_source = next(
        item for item in codex_manifest if item["source_id"] == "managed-footnote-block"
    )
    assert managed_source["bytes"] == len(managed.encode())
    for manifest in (claude_manifest, codex_manifest):
        assert not any(item["source_id"] == "project-instructions" for item in manifest)
        assert not any(item["source_id"].startswith("claude-rule:") for item in manifest)
        assert not any(item["source_id"] == "hook-manifest" for item in manifest)


@pytest.mark.parametrize(
    ("platform", "hook_input"),
    [
        pytest.param("claude", '{"source":"compact"}', id="claude-compact"),
        pytest.param("codex", "{}", id="codex"),
    ],
)
def test_postcompact_producer_uses_each_harness_wire_schema(
    tmp_path: Path,
    platform: str,
    hook_input: str,
) -> None:
    plugin = tmp_path / "plugin"
    guard = plugin / "scripts" / "lib" / "target-guard.sh"
    guard.parent.mkdir(parents=True)
    guard.write_text(
        "target_is_active() { return 0; }\n"
        "target_state_field() {\n"
        "  sed -n \"s/^$1: *//p\" \"$2\" | head -1 | tr -d '\\\"'\n"
        "}\n",
        encoding="utf-8",
    )
    # The hook sources its carrier from the plugin's lib dir; a fake plugin that
    # ships the guard but not the carrier would silence the hook (by design).
    shutil.copy(
        ROOT / "scripts" / "lib" / "postcompact-carrier.sh",
        plugin / "scripts" / "lib" / "postcompact-carrier.sh",
    )
    state = tmp_path / ".fno" / "target-state.md"
    state.parent.mkdir()
    state.write_text(
        "session_id: wire-session\n"
        'input: "Keep the target oriented"\n'
        "plan_path: null\n"
        "graph_node_id: x-2e3c\n",
        encoding="utf-8",
    )
    # Claude reinjects via SessionStart(source=compact); Codex via PostCompact.
    hook_env = {
        **os.environ,
        "FNO_PLATFORM": platform,
    }
    if platform == "codex":
        hook_env["PLUGIN_ROOT"] = str(plugin)
        hook_env["CLAUDE_PLUGIN_ROOT"] = str(tmp_path / "foreign-claude-plugin")
    else:
        hook_env["CLAUDE_PLUGIN_ROOT"] = str(plugin)
    result = subprocess.run(
        [str(ROOT / "hooks" / "target-postcompact-reinject.sh")],
        cwd=tmp_path,
        env=hook_env,
        input=hook_input,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    if platform == "codex":
        assert set(payload) == {"systemMessage"}
        assert "Keep the target oriented" in payload["systemMessage"]
    else:
        assert set(payload) == {"hookSpecificOutput"}
        assert payload["hookSpecificOutput"]["hookEventName"] == "SessionStart"
        assert (
            "Keep the target oriented"
            in payload["hookSpecificOutput"]["additionalContext"]
        )
