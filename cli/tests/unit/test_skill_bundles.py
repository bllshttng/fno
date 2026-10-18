"""Unit tests for the skill-bundle generator and its helpers.

The freshness gate on the committed tree runs as its own CI step.
"""

from __future__ import annotations

import filecmp
import os
import shutil
import stat
import subprocess
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[3]
GENERATOR = REPO_ROOT / "scripts" / "generate-skill-bundles.sh"
PARSER = REPO_ROOT / "scripts" / "lib" / "parse-bundle-manifest.py"
FRONTMATTER = REPO_ROOT / "scripts" / "lib" / "bundle-frontmatter.py"
AUDIT = REPO_ROOT / "scripts" / "lint" / "no-repo-root-scripts-in-skills.sh"


def _run(
    cmd: list[str], cwd: Path | None = None, env: dict | None = None
) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, cwd=cwd, env=env, check=False)


def _shelled_python_has_yaml() -> bool:
    """Does the bare ``python3`` these scripts shell out to import yaml?

    The generator and its helpers run under the system interpreter, not the
    project venv, and PyYAML is required by design for anything beyond a
    files-only manifest: ``bundle-frontmatter.py`` always needs it, and the
    manifest parser's stdlib fallback cannot handle ``references:``/``agents:``
    blocks. CI's python3 carries it, so guarding here keeps a dev machine whose
    system interpreter lacks it from reading as a code failure.

    Every test that shells out to the parser, the generator, or the freshness
    gate needs this mark. Four did not carry it and failed on a machine whose
    python3 has no PyYAML, reading as four broken bundles. A pack manifest
    under ``plugins/`` declaring ``references:`` is enough to force the PyYAML
    path for a whole-repo run, so a test touching no bundle at all still lands
    here. A guard on some of the paths that reach one interpreter is decorative.
    """
    return _run(["python3", "-c", "import yaml"]).returncode == 0


requires_shelled_yaml = pytest.mark.skipif(
    not _shelled_python_has_yaml(),
    reason="system python3 lacks PyYAML, required for references/agents bundling (pip install pyyaml)",
)


def test_audit_passes_for_committed_state():
    """No skills/*.md file references ${REPO_ROOT}/scripts/."""
    result = _run(["bash", str(AUDIT)])
    assert result.returncode == 0, result.stdout + result.stderr


@requires_shelled_yaml
def test_generator_fails_on_missing_source(tmp_path):
    """AC4-EDGE: a manifest entry pointing at a nonexistent canonical fails."""
    fake_root = tmp_path / "fake-repo"
    fake_root.mkdir()
    bad_manifest = fake_root / "skill-bundles.yaml"
    bad_manifest.write_text(
        "bundles:\n"
        "  - skill: spec\n"
        "    files:\n"
        "      - source: scripts/does-not-exist.sh\n"
        "        dest: scripts/does-not-exist.sh\n"
    )
    (fake_root / "scripts" / "lib").mkdir(parents=True)
    shutil.copy2(GENERATOR, fake_root / "scripts" / "generate-skill-bundles.sh")
    shutil.copy2(PARSER, fake_root / "scripts" / "lib" / "parse-bundle-manifest.py")
    shutil.copy2(FRONTMATTER, fake_root / "scripts" / "lib" / "bundle-frontmatter.py")

    env = dict(os.environ)
    env["REPO_ROOT"] = str(fake_root)
    result = _run(
        ["bash", str(fake_root / "scripts" / "generate-skill-bundles.sh")],
        env=env,
    )
    assert result.returncode != 0
    assert "source not found: scripts/does-not-exist.sh" in result.stderr


# ---------------------------------------------------------------------------
# bundle-frontmatter.py helper tests
# ---------------------------------------------------------------------------


@requires_shelled_yaml
def test_rewrite_rejects_missing_required_fields(tmp_path):
    """`rewrite --as subagent` requires name, description, model, tools."""
    src = tmp_path / "input.md"
    src.write_text("---\nname: target\n---\nBody\n")
    meta = tmp_path / "meta.yaml"
    meta.write_text("name: archer\ndescription: foo\n")  # missing model, tools
    result = _run(
        [
            "python3",
            str(FRONTMATTER),
            "rewrite",
            str(src),
            "--as",
            "subagent",
            "--meta-file",
            str(meta),
        ]
    )
    assert result.returncode != 0
    assert "subagent_meta missing required fields" in result.stderr
    assert "model" in result.stderr
    assert "tools" in result.stderr


@requires_shelled_yaml
def test_strip_fails_loudly_on_unterminated_frontmatter(tmp_path):
    """A source that opens with --- but never closes is corrupt. Treating
    the whole file as body would ship YAML-looking content as prose. Fail."""
    src = tmp_path / "input.md"
    src.write_text("---\nname: target\n# never closes\nBody content\n")
    result = _run(["python3", str(FRONTMATTER), "strip", str(src)])
    assert result.returncode != 0
    assert "unterminated frontmatter" in result.stderr
    # Error mentions the source path so the contributor knows which file
    assert str(src) in result.stderr


# ---------------------------------------------------------------------------
# End-to-end: generator handles a mixed manifest with all three types
# ---------------------------------------------------------------------------


@requires_shelled_yaml
def test_generator_handles_mixed_manifest(tmp_path):
    """A synthetic manifest with file, reference, and agent entries generates
    each correctly. AC1-HP for references and agents.
    """
    fake_root = tmp_path / "fake-repo"
    fake_root.mkdir()
    # Stage scripts/
    scripts_dir = fake_root / "scripts" / "lib"
    scripts_dir.mkdir(parents=True)
    shutil.copy2(GENERATOR, fake_root / "scripts" / "generate-skill-bundles.sh")
    shutil.copy2(PARSER, fake_root / "scripts" / "lib" / "parse-bundle-manifest.py")
    shutil.copy2(FRONTMATTER, fake_root / "scripts" / "lib" / "bundle-frontmatter.py")
    # Stage canonical sources.
    canonical_file = fake_root / "scripts" / "lib" / "tool.sh"
    canonical_file.write_text("#!/bin/bash\necho hello\n")
    canonical_file.chmod(0o755)
    canonical_ref = fake_root / "shared" / "doc.md"
    canonical_ref.parent.mkdir(parents=True)
    canonical_ref.write_text("---\nname: shared\n---\n# Heading\n")
    canonical_agent = fake_root / "skills" / "target" / "SKILL.md"
    canonical_agent.parent.mkdir(parents=True)
    canonical_agent.write_text("---\nname: target\n---\n# Target body\n")
    # Manifest.
    manifest = fake_root / "skill-bundles.yaml"
    manifest.write_text(
        "bundles:\n"
        "  - skill: consumer\n"
        "    files:\n"
        "      - source: scripts/lib/tool.sh\n"
        "        dest: scripts/lib/tool.sh\n"
        "    references:\n"
        "      - source: shared/doc.md\n"
        "        dest: references/doc.md\n"
        "    agents:\n"
        "      - source: skills/target/SKILL.md\n"
        "        dest: agents/archer.md\n"
        "        rewrite_frontmatter: subagent\n"
        "        subagent_meta:\n"
        "          name: archer\n"
        "          description: test agent\n"
        "          model: opus\n"
        "          tools: [Read]\n"
    )

    env = dict(os.environ)
    env["REPO_ROOT"] = str(fake_root)
    result = _run(
        ["bash", str(fake_root / "scripts" / "generate-skill-bundles.sh")],
        env=env,
    )
    assert result.returncode == 0, result.stderr

    # File: byte-identical + executable bit preserved
    bundled_file = fake_root / "skills" / "consumer" / "scripts" / "lib" / "tool.sh"
    assert bundled_file.is_file()
    assert filecmp.cmp(canonical_file, bundled_file, shallow=False)
    assert bundled_file.stat().st_mode & stat.S_IXUSR

    # Reference: frontmatter stripped
    bundled_ref = fake_root / "skills" / "consumer" / "references" / "doc.md"
    assert bundled_ref.is_file()
    body = bundled_ref.read_text()
    assert body == "# Heading\n"

    # Agent: frontmatter rewritten with subagent_meta
    bundled_agent = fake_root / "skills" / "consumer" / "agents" / "archer.md"
    assert bundled_agent.is_file()
    agent_text = bundled_agent.read_text()
    assert agent_text.startswith("---\n")
    assert "name: archer\n" in agent_text
    assert "model: opus\n" in agent_text
    assert "name: target\n" not in agent_text  # original frontmatter gone
    assert "# Target body\n" in agent_text
