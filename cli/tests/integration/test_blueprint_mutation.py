"""Integration tests for /blueprint single-doc mutation behavior.

Tests the mutate_doc.py script against real fixture files.

TDD discipline: these tests were written BEFORE implementation.
Each test maps to an acceptance criterion from the lean-blueprint plan.
"""
from __future__ import annotations

import hashlib
import re
import shutil
import subprocess
import sys
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).parents[3]  # madison/
FIXTURES_DIR = REPO_ROOT / "cli" / "tests" / "fixtures" / "plans"
MUTATE_SCRIPT = REPO_ROOT / "skills" / "blueprint" / "scripts" / "mutate_doc.py"
GREENFIELD_FIXTURE = FIXTURES_DIR / "blueprint_input_greenfield.md"
BROWNFIELD_FIXTURE = FIXTURES_DIR / "blueprint_input_brownfield.md"


def _run_mutate(path: Path, *extra_args: str) -> subprocess.CompletedProcess:
    """Run mutate_doc.py against a given plan file path."""
    cmd = [sys.executable, str(MUTATE_SCRIPT), str(path), *extra_args]
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
    )


def _copy_fixture(fixture: Path, tmp_path: Path) -> Path:
    """Copy a fixture file to a temp directory for mutation."""
    dest = tmp_path / fixture.name
    shutil.copy2(fixture, dest)
    return dest


def _load_frontmatter(path: Path) -> dict:
    """Extract YAML frontmatter from a plan file."""
    text = path.read_text(encoding="utf-8")
    if not text.startswith("---"):
        return {}
    rest = text[4:]  # skip "---\n"
    close = rest.find("\n---")
    if close == -1:
        return {}
    fm_yaml = rest[:close]
    result = yaml.safe_load(fm_yaml)
    return result if isinstance(result, dict) else {}


def _section_hash(path: Path, section_name: str) -> str | None:
    """Return MD5 hash of a section body, or None if section absent."""
    text = path.read_text(encoding="utf-8")
    # Strip frontmatter
    if text.startswith("---"):
        close = text.find("\n---", 4)
        if close != -1:
            text = text[close + 4:]
            if text.startswith("\n"):
                text = text[1:]
    lines = text.splitlines()
    in_section = False
    section_lines = []
    for line in lines:
        if line.startswith("## ") and line[3:].rstrip() == section_name:
            in_section = True
            continue
        if in_section:
            if line.startswith("## "):
                break
            section_lines.append(line)
    if not in_section:
        return None
    body = "\n".join(section_lines)
    return hashlib.md5(body.encode()).hexdigest()


def _has_section(path: Path, section_name: str) -> bool:
    """Return True if the document has a ## section_name heading."""
    text = path.read_text(encoding="utf-8")
    return f"\n## {section_name}\n" in text or text.startswith(f"## {section_name}\n")


# ---------------------------------------------------------------------------
# AC1-HP: greenfield happy path
# ---------------------------------------------------------------------------


class TestAC1HPGreenfield:


    def test_kill_criteria_in_frontmatter(self, tmp_path):
        """AC1-HP: the default kill_criteria list is written to frontmatter in the
        canonical {name, predicate, reason} shape the engine + validate-plan.sh
        both read (the legacy flat one-key maps were invisible to both)."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        fm = _load_frontmatter(doc)
        kc = fm.get("kill_criteria")
        assert kc is not None, "kill_criteria missing from frontmatter"
        assert isinstance(kc, list), f"kill_criteria should be a list, got {type(kc)}"
        assert len(kc) == 2, f"Expected 2 default kill_criteria entries, got {len(kc)}"
        for entry in kc:
            assert {"name", "predicate", "reason"} <= entry.keys(), (
                f"entry missing required fields: {entry}"
            )

    def test_greenfield_no_file_ownership_map(self, tmp_path):
        """AC1-HP: greenfield mode does NOT add File Ownership Map."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        assert not _has_section(doc, "File Ownership Map"), \
            "File Ownership Map should NOT be present in greenfield mode"


    def test_think_sections_unchanged(self, tmp_path):
        """AC1-HP: /think-owned sections are not modified."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        # Hash /think sections before mutation
        think_sections = ["Overview", "Architecture", "User Stories", "Failure Modes", "Acceptance Criteria"]
        before_hashes = {s: _section_hash(doc, s) for s in think_sections}
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        after_hashes = {s: _section_hash(doc, s) for s in think_sections}
        for section in think_sections:
            assert before_hashes[section] == after_hashes[section], \
                f"Section '{section}' was modified by /blueprint (should be read-only)"


class TestExecutableDraftLifecycle:
    def test_draft_stays_design_until_semantic_finalize(self, tmp_path):
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)

        draft = _run_mutate(doc, "--mode", "greenfield", "--draft")

        assert draft.returncode == 0, draft.stderr
        assert _load_frontmatter(doc).get("status") == "design"

        rejected = _run_mutate(doc, "--finalize")

        assert rejected.returncode == 2
        assert "tasks.1.1.surface" in rejected.stderr
        assert _load_frontmatter(doc).get("status") == "design"

        text = doc.read_text(encoding="utf-8")
        text = text.replace("surface: []", "surface: [src/generated.py]")
        text = text.replace(
            "verify: '# fill in verify command'",
            "verify: pytest tests/test_generated.py",
        )
        # AC1 resolves to the fixture's compiled AC1-HP criterion (compiled-v1
        # strict reference resolution, enforced at finalize).
        text = text.replace("acceptance: []", "acceptance: [AC1-HP]")
        doc.write_text(text, encoding="utf-8")

        finalized = _run_mutate(doc, "--finalize")

        assert finalized.returncode == 0, finalized.stderr
        assert _load_frontmatter(doc).get("status") == "ready"


    def test_finalize_refuses_unresolved_acceptance_reference(self, tmp_path):
        """compiled-v1 finalization refuses a task ref that resolves to nothing."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        assert _run_mutate(doc, "--mode", "greenfield", "--draft").returncode == 0
        text = doc.read_text(encoding="utf-8")
        text = text.replace("surface: []", "surface: [src/generated.py]")
        text = text.replace(
            "verify: '# fill in verify command'",
            "verify: pytest tests/test_generated.py",
        )
        # AC9 does not exist in the fixture's Acceptance Criteria section.
        text = text.replace("acceptance: []", "acceptance: [AC9]")
        doc.write_text(text, encoding="utf-8")

        result = _run_mutate(doc, "--finalize")

        assert result.returncode == 2
        assert "AC9" in result.stderr
        # Refused: status stayed design and no contract was stamped.
        assert _load_frontmatter(doc).get("status") == "design"
        assert _load_frontmatter(doc).get("acceptance_contract") is None

    def test_finalize_revalidates_compiled_v1_ready_doc(self, tmp_path):
        """A stamped ready doc is re-validated: a --rewrite or hand-edit that
        left it violating compiled-v1 is refused, not silently re-stamped."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        assert _run_mutate(doc, "--mode", "greenfield", "--draft").returncode == 0
        text = doc.read_text(encoding="utf-8")
        text = text.replace("surface: []", "surface: [src/generated.py]")
        text = text.replace(
            "verify: '# fill in verify command'",
            "verify: pytest tests/test_generated.py",
        )
        text = text.replace("acceptance: []", "acceptance: [AC1-HP]")
        doc.write_text(text, encoding="utf-8")
        # Promote to ready + compiled-v1.
        assert _run_mutate(doc, "--finalize").returncode == 0
        assert _load_frontmatter(doc).get("acceptance_contract") == "compiled-v1"

        # Corrupt the already-stamped plan: reference a missing criterion.
        corrupted = doc.read_text(encoding="utf-8").replace(
            "acceptance: [AC1-HP]", "acceptance: [AC9]"
        )
        doc.write_text(corrupted, encoding="utf-8")

        refused = _run_mutate(doc, "--finalize")

        assert refused.returncode == 2
        assert "AC9" in refused.stderr


# ---------------------------------------------------------------------------
# Projectable frontmatter block-lists
# The reader _stamp.parse_frontmatter only consumes INDENTED block-list
# children; mutate_doc must emit indented lists so blueprint-mutated plans stay
# parseable and projectable.
# ---------------------------------------------------------------------------


class TestProjectableFrontmatter:
    def _mutate(self, tmp_path):
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        return doc


    def test_project_graph_nodes_succeeds(self, tmp_path, capsys, monkeypatch):
        """AC2: a freshly blueprint-mutated plan projects (rewritten 1) with no
        'plan projection skipped' warning on stderr."""
        from fno.plan._project import project_graph_nodes
        from tests.fixtures.graph_seed import seed_graph

        doc = self._mutate(tmp_path)
        g = tmp_path / "graph.json"
        # The writer's client reads the store, so the seed must materialize
        # the db, not just the frozen json export.
        seed_graph(
            g,
            {
                "entries": [
                    {
                        "id": "x-test",
                        "slug": "test",
                        "plan_path": str(doc),
                        "priority": "p1",
                        "type": "feature",
                        "blocked_by": [],
                        "project": "fno",
                        "status": "ready",
                    }
                ]
            },
        )
        import fno.graph.store as gs

        monkeypatch.setattr(gs, "GRAPH_JSON", g)
        rewritten = project_graph_nodes([{"id": "x-test"}], ["x-test"])
        captured = capsys.readouterr()
        assert "plan projection skipped" not in captured.err
        assert rewritten == 1


# ---------------------------------------------------------------------------
# AC1-HP: brownfield happy path
# ---------------------------------------------------------------------------


class TestAC1HPBrownfield:
    def test_brownfield_adds_file_ownership_map(self, tmp_path):
        """AC1-HP brownfield: File Ownership Map section is added."""
        doc = _copy_fixture(BROWNFIELD_FIXTURE, tmp_path)
        result = _run_mutate(doc, "--mode", "brownfield")
        assert result.returncode == 0, f"Expected exit 0\nstderr: {result.stderr}"
        assert _has_section(doc, "File Ownership Map"), \
            "## File Ownership Map should be present in brownfield mode"


# ---------------------------------------------------------------------------
# AC1-ERR: status=ready without --rewrite
# ---------------------------------------------------------------------------


class TestAC1ERR:
    def test_existing_draft_without_rewrite_exits_1(self, tmp_path):
        """AC1-ERR: an existing execution draft requires explicit rewrite."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        # First run creates a design-status execution draft.
        first = _run_mutate(doc, "--mode", "greenfield")
        assert first.returncode == 0, f"Setup failed: {first.stderr}"
        # Second run without --rewrite
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 1, \
            f"Expected exit 1 for draft re-run without --rewrite, got {result.returncode}"


    def test_in_progress_with_execution_strategy_exits_1_even_with_rewrite(self, tmp_path):
        """AC1-ERR: a doc that really is past blueprint exits 1 even with --rewrite."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        # Blueprint it first, so the doc genuinely carries an execution draft.
        first = _run_mutate(doc, "--mode", "greenfield")
        assert first.returncode == 0, f"Setup failed: {first.stderr}"
        text = doc.read_text(encoding="utf-8")
        text = re.sub(r"(?m)^status:.*$", "status: in_progress", text, count=1)
        doc.write_text(text, encoding="utf-8")
        result = _run_mutate(doc, "--mode", "greenfield", "--rewrite")
        assert result.returncode == 1, \
            f"Expected exit 1 for a blueprinted in_progress doc, got {result.returncode}"

    def test_in_progress_without_execution_strategy_is_still_blueprintable(self, tmp_path):
        """`in_progress` alone is NOT proof that blueprint ran.

        `fno do target init` acquiring a node claim projects `in_progress` onto the
        plan doc along a forward-only axis, so the documented /target design-rung
        branch (init, then blueprint) would refuse itself: holding the claim needed
        to work the node made the node unblueprintable, and resetting the doc did
        not persist. The artifact is the evidence, which the sibling test above
        pins; the bare status token is not.
        """
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        text = doc.read_text(encoding="utf-8")
        text = text.replace("status: design", "status: in_progress")
        doc.write_text(text, encoding="utf-8")
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, \
            f"Expected exit 0 for a claim-stamped doc with no draft, got {result.returncode}\nstderr: {result.stderr}"


# ---------------------------------------------------------------------------
# AC1-ERR-rewrite: status=ready + --rewrite regenerates sections
# ---------------------------------------------------------------------------


class TestAC1ERRRewrite:

    def test_rewrite_does_not_duplicate_sections(self, tmp_path):
        """AC1-ERR-rewrite: --rewrite replaces sections, not duplicates."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        _run_mutate(doc, "--mode", "greenfield")  # first run
        _run_mutate(doc, "--mode", "greenfield", "--rewrite")  # second run
        text = doc.read_text(encoding="utf-8")
        # Count occurrences of ## Execution Strategy heading
        count = text.count("\n## Execution Strategy\n")
        # May also appear at start of body
        if text.startswith("## Execution Strategy\n"):
            count += 1
        assert count == 1, f"Expected exactly 1 ## Execution Strategy, found {count}"


# ---------------------------------------------------------------------------
# AC1-FR: atomic write - doc unchanged on failure
# ---------------------------------------------------------------------------


class TestAC1FR:
    def test_no_emit_leaves_doc_unchanged(self, tmp_path):
        """AC1-FR: --no-emit (dry-run) does not modify the document."""
        doc = _copy_fixture(GREENFIELD_FIXTURE, tmp_path)
        before_text = doc.read_text(encoding="utf-8")
        result = _run_mutate(doc, "--mode", "greenfield", "--no-emit")
        after_text = doc.read_text(encoding="utf-8")
        assert before_text == after_text, "Doc was modified despite --no-emit"
        assert result.returncode == 0, f"Expected exit 0 for --no-emit, got {result.returncode}"


# ---------------------------------------------------------------------------
# AC3-HP: non-existent path -> exit 1 with fno do plan path guidance
# ---------------------------------------------------------------------------


class TestAC3HP:

    def test_nonexistent_path_stderr_mentions_plan_path(self, tmp_path):
        """AC3-HP: missing doc points to fno do plan path, not /think."""
        nonexistent = tmp_path / "does_not_exist.md"
        result = _run_mutate(nonexistent)
        assert "plan path" in result.stderr.lower(), \
            f"Expected 'fno do plan path' guidance in stderr: {result.stderr}"
        assert "think" not in result.stderr.lower(), \
            f"blueprint owns the plan; no /think redirect: {result.stderr}"


# ---------------------------------------------------------------------------
# AC3-EDGE: feature-description string (no / in name) -> redirect
# ---------------------------------------------------------------------------


class TestAC3EDGE:

    def test_feature_description_mentions_plan_path(self):
        """AC3-EDGE: raw description points to fno do plan path, not /think."""
        cmd = [sys.executable, str(MUTATE_SCRIPT), "build a new feature"]
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            cwd=str(REPO_ROOT),
        )
        stderr = result.stderr.lower()
        assert "plan path" in stderr, f"Expected 'fno do plan path' guidance in stderr: {result.stderr}"
        assert "think" not in stderr, f"blueprint owns the plan; no /think redirect: {result.stderr}"


# ---------------------------------------------------------------------------
# Auto-detect greenfield/brownfield (--mode auto)
# ---------------------------------------------------------------------------


class TestAutoDetect:

    def test_auto_brownfield_on_existing_paths(self, tmp_path):
        """Auto-detect: existing Architecture paths -> brownfield (has File Ownership Map)."""
        doc = _copy_fixture(BROWNFIELD_FIXTURE, tmp_path)
        result = _run_mutate(doc, "--mode", "auto")
        assert result.returncode == 0, result.stderr
        # Brownfield fixture references cli/src/fno/plan/*.py which exist
        assert _has_section(doc, "File Ownership Map"), \
            "Should have File Ownership Map in auto-brownfield mode"


# ---------------------------------------------------------------------------
# Missing status in frontmatter -> exit 3
# ---------------------------------------------------------------------------


class TestMissingStatus:
    def test_missing_status_exits_3(self, tmp_path):
        """exit 3 for missing/invalid frontmatter status."""
        doc = tmp_path / "no_status.md"
        doc.write_text(
            "---\ncreated: 2026-05-18\n---\n\n# Test\n\n## Failure Modes\n\n**Boundaries**\n- boundary\n",
            encoding="utf-8",
        )
        result = _run_mutate(doc)
        assert result.returncode == 3, \
            f"Expected exit 3 for missing status, got {result.returncode}\nstderr: {result.stderr}"


# ---------------------------------------------------------------------------
# User Stories parser: multiple recognized formats
# ---------------------------------------------------------------------------


def _doc_with_us_section(us_section: str) -> str:
    """Build a minimal valid design doc whose User Stories body is `us_section`."""
    return (
        "---\nstatus: design\n---\n\n# Test\n\n"
        "## Overview\n\nSome overview text.\n\n"
        "## User Stories\n\n"
        f"{us_section.rstrip()}\n\n"
        "## Failure Modes\n\n**Boundaries**\n- some boundary\n"
    )


def _read_strategy_yaml(doc_text: str) -> dict:
    """Extract the YAML block from the ## Execution Strategy section."""
    marker = "## Execution Strategy"
    idx = doc_text.find(marker)
    assert idx != -1, "Execution Strategy section missing"
    after = doc_text[idx + len(marker):]
    fence_start = after.find("```yaml")
    assert fence_start != -1, "yaml fence missing"
    fence_end = after.find("```", fence_start + len("```yaml"))
    yaml_body = after[fence_start + len("```yaml"):fence_end]
    parsed = yaml.safe_load(yaml_body)
    assert isinstance(parsed, dict), f"expected dict, got {type(parsed)}"
    return parsed


class TestUserStoryParserFormats:
    """Parser must handle the three User Story shapes /think emits.

    Bug: pre-fix parser only matched `**USN:** desc` inline-bold. Heading-style
    stories from `/think` got silently dropped, and the parser emitted a single
    default task with a WARNING — clobbering Execution Strategy with placeholder
    content.
    """


    def test_nonempty_unparseable_stories_fails_loud(self, tmp_path):
        """A non-empty ## User Stories body that yields zero stories must FAIL
        LOUD (non-zero exit, naming the accepted formats) instead of silently
        degrading to one default task. The guard-corollary rule: a parser that
        recognizes N of M legal shapes must refuse on the rest, never no-op into
        a plausible-looking single-task plan.
        """
        doc = tmp_path / "prose_only.md"
        doc.write_text(
            _doc_with_us_section("Just some prose with no story markers at all.\n"),
            encoding="utf-8",
        )
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode != 0, \
            "Non-empty unparseable User Stories must fail loud, not exit 0"
        assert "no ## User Stories entries found" not in result.stderr, \
            "Fail-loud must replace the old silent-degrade warning"
        # Message must name the expected formats so the author can fix the doc.
        assert "US1" in result.stderr or "format" in result.stderr.lower(), \
            f"Error must name an accepted format; got: {result.stderr}"
        # Failure happens before the write, so the doc is left unmutated.
        assert "## Execution Strategy" not in doc.read_text(encoding="utf-8"), \
            "Fail-loud must not write a degraded Execution Strategy"

    def test_truly_empty_section_keeps_default_escape(self, tmp_path):
        """A genuinely EMPTY section (whitespace only) is a different case from
        unparseable prose: the section may be absent or intentionally blank, and
        think.md's empty-stories reviewer check catches it at design time. The
        default-task escape hatch is preserved so blueprint does not hard-fail on
        a blank section a concurrent edit may have left half-written.
        """
        doc = tmp_path / "blank.md"
        doc.write_text(
            _doc_with_us_section("   \n\n  \n"),
            encoding="utf-8",
        )
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        assert "no ## User Stories entries found" in result.stderr, \
            "Genuinely empty section should keep the default-task escape"
        strategy = _read_strategy_yaml(doc.read_text(encoding="utf-8"))
        assert len(strategy["tasks"]) == 1, \
            "Genuinely empty should fall through to single default task"


    def test_empty_heading_does_not_steal_next_bold_story(self, tmp_path):
        """Gemini HIGH: `### US1` (no title) followed by `**US2:** desc` must
        not let the heading-empty fallback consume US2's line as US1's title.
        """
        doc = tmp_path / "empty_heading_steals.md"
        doc.write_text(
            _doc_with_us_section(
                "### US1\n\n"
                "**US2:** real description for US2.\n"
            ),
            encoding="utf-8",
        )
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        strategy = _read_strategy_yaml(doc.read_text(encoding="utf-8"))
        # US1 has no descriptive content -> skipped; US2 must own its task.
        notes = [t["notes"] for t in strategy["tasks"]]
        assert "Implement US2." in notes, \
            f"Empty heading stole next bold story's content; notes={notes}"
        # US1 must NOT appear -- if it did, it would mean the empty-heading
        # fallback stole US2's content as US1's title.
        assert "Implement US1." not in notes, \
            f"Empty heading was added as a task by stealing next story: notes={notes}"
        assert len(strategy["tasks"]) == 1, \
            f"Expected exactly 1 task (US2 only), got {len(strategy['tasks'])}: {strategy['tasks']}"

    def test_multiline_bold_description_recovered_from_next_paragraph(self, tmp_path):
        """Codex P2 / Gemini MEDIUM (converged): `**US1:**` with description on
        the next paragraph must still be parsed as a real story, not dropped.
        """
        doc = tmp_path / "multiline_bold.md"
        doc.write_text(
            _doc_with_us_section(
                "**US1:**\n\n"
                "Description for US1 lives on the next paragraph.\n\n"
                "**US2:**\n\n"
                "Description for US2 also on next paragraph.\n"
            ),
            encoding="utf-8",
        )
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        assert "no ## User Stories entries found" not in result.stderr, \
            f"Parser dropped multi-line bold stories: {result.stderr}"
        strategy = _read_strategy_yaml(doc.read_text(encoding="utf-8"))
        assert len(strategy["tasks"]) == 2, \
            f"Expected 2 multi-line bold tasks, got {len(strategy['tasks'])}: {strategy['tasks']}"
        titles = [t["title"] for t in strategy["tasks"]]
        assert any("US1" in t for t in titles) or "Description for US1" in titles[0], \
            f"US1's next-paragraph description not used as title: {titles}"

    def test_multiline_bold_does_not_steal_next_anchor(self, tmp_path):
        """Multi-line bold recovery must NOT swallow the next story's marker.

        `**US1:**` with no description, then `**US2:** desc` -- the recovery
        loop must break at the next anchor and leave US1 unmatched (it has no
        real content), not consume US2's line.
        """
        doc = tmp_path / "multiline_bold_no_steal.md"
        doc.write_text(
            _doc_with_us_section(
                "**US1:**\n\n"
                "**US2:** real desc for US2.\n"
            ),
            encoding="utf-8",
        )
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        strategy = _read_strategy_yaml(doc.read_text(encoding="utf-8"))
        notes = [t["notes"] for t in strategy["tasks"]]
        assert "Implement US2." in notes, \
            f"Multi-line recovery dropped US2; notes={notes}"
        assert "Implement US1." not in notes, \
            f"Empty US1 was added by stealing US2's line; notes={notes}"

    def test_bold_reference_without_delimiter_is_not_an_anchor(self, tmp_path):
        """Codex P2 (third pass): bold prose like `**US1 baseline constraints**`
        must NOT be accepted as a US1 story anchor, because first-occurrence-wins
        dedup would then drop the real `**US1:** ...` story later in the doc.
        """
        doc = tmp_path / "bold_prose_reference.md"
        doc.write_text(
            _doc_with_us_section(
                "Some prose mentioning **US1 baseline constraints** as a reference.\n\n"
                "**US1:** the real story description.\n"
            ),
            encoding="utf-8",
        )
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        strategy = _read_strategy_yaml(doc.read_text(encoding="utf-8"))
        assert len(strategy["tasks"]) == 1, \
            f"Expected 1 task (the real US1 story), got {len(strategy['tasks'])}: {strategy['tasks']}"
        title = strategy["tasks"][0]["title"]
        assert "real story" in title.lower(), \
            f"Bold prose reference hijacked US1 anchor; title={title!r}"


    def test_document_order_wins_on_duplicate_ids(self, tmp_path):
        """Gemini MEDIUM: when the same ID appears in two formats, the one that
        appears FIRST in the document wins (matches docstring contract).
        """
        # US1 first as inline bold, then again as h3 heading later.
        doc = tmp_path / "dup_bold_first.md"
        doc.write_text(
            _doc_with_us_section(
                "**US1:** bold first wins.\n\n"
                "### US1: heading later (should be ignored).\n\n"
                "Body for the heading.\n"
            ),
            encoding="utf-8",
        )
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, result.stderr
        strategy = _read_strategy_yaml(doc.read_text(encoding="utf-8"))
        assert len(strategy["tasks"]) == 1, \
            f"Duplicate ID should collapse to 1 task, got {len(strategy['tasks'])}"
        title = strategy["tasks"][0]["title"]
        assert "bold first wins" in title, \
            f"Expected the FIRST occurrence (bold) to win, got title={title!r}"


# ---------------------------------------------------------------------------
# Plain list-item shapes (dash / asterisk / numbered): the natural forms a
# hand-written or /think-emitted spec uses when it does not wrap ids in bold.
# Pre-fix these parsed to zero and silently degraded to one default task.
# ---------------------------------------------------------------------------


def test_story_count_equals_task_count_across_every_shape(tmp_path):
    """The contract this defect is named for: for EVERY supported shape, the
    number of stories in the section equals the number of tasks in Execution
    Strategy. A shape that silently drops stories breaks this equality, which is
    exactly how the degrade-to-one-default failure hides.
    """
    shapes = {
        "bold_colon": "**US1:** a.\n**US2:** b.\n",
        "bold_title": "**US1: Title one.** a.\n**US2: Title two.** b.\n",
        "heading_colon": "### US1: Alpha\n### US2: Beta\n",
        "heading_emdash": "### US1 — Alpha\n### US2 — Beta\n",
        "table": "| US1 | a |\n| US2 | b |\n",
        "dash_bold": "- **US1: Title.** a.\n- **US2: Title.** b.\n",
        "numbered_bold": "1. **US1:** a.\n2. **US2:** b.\n",
        "dash_plain": "- US1: a.\n- US2: b.\n",
        "numbered_plain": "1. US1: a.\n2. US2: b.\n",
    }
    for name, body in shapes.items():
        doc = tmp_path / f"{name}.md"
        doc.write_text(_doc_with_us_section(body), encoding="utf-8")
        result = _run_mutate(doc, "--mode", "greenfield")
        assert result.returncode == 0, \
            f"{name}: blueprint failed: {result.stderr}"
        strategy = _read_strategy_yaml(doc.read_text(encoding="utf-8"))
        assert len(strategy["tasks"]) == 2, \
            f"{name}: expected 2 tasks (story count), got {len(strategy['tasks'])}"
