"""fire_skill's own contract: one headless fire, its rc/is_error grammar,
its bounded timeout, the env seam, and the admission-gate refusals."""

import json
import subprocess


def _claude_ok_response(text: str = "done") -> subprocess.CompletedProcess:
    """Simulate a successful claude --print --output-format json response."""
    payload = json.dumps({"result": text, "is_error": False})
    return subprocess.CompletedProcess(args=[], returncode=0, stdout=payload, stderr="")


def _claude_is_error_response() -> subprocess.CompletedProcess:
    """rc=0 but is_error:true -- the load-bearing AC1-FR case."""
    payload = json.dumps({"result": "skill errored", "is_error": True})
    return subprocess.CompletedProcess(args=[], returncode=0, stdout=payload, stderr="")


class TestFireSkill:
    """AC1-FR: fire_skill honours rc=0+is_error:true as FAILURE."""

    def test_rc0_is_error_true_is_failure(self, tmp_path):
        """AC1-FR (load-bearing): rc=0 but is_error:true -> DispatchResult.ok False."""
        from fno.pr_watch._dispatch import fire_skill

        def stub_runner(cmd, **kw):
            return _claude_is_error_response()

        result = fire_skill("check", 1, tmp_path, runner=stub_runner, node_id="x-1")
        assert result.ok is False
        assert result.is_error is True
        assert result.rc == 0

    def test_admission_refusal_rc_is_not_an_error(self, tmp_path):
        """Exit 82 is the admission gate's own refusal: not ok, and not an
        error - the caller must not burn a retry on a fire that never
        started."""
        from fno.pr_watch._dispatch import fire_skill

        def stub_runner(cmd, **kw):
            proc = _claude_ok_response()
            proc.returncode = 82
            return proc

        result = fire_skill("check", 1, tmp_path, runner=stub_runner, node_id="x-1")
        assert result.ok is False
        assert result.is_error is False
        assert result.rc == 82

    def test_check_verb_fires_correct_skill(self, tmp_path):
        """AC-HP: verb='check' -> /fno:ship pr check <n> in command."""
        from fno.pr_watch._dispatch import fire_skill

        captured = {}

        def stub_runner(cmd, **kw):
            captured["cmd"] = cmd
            return _claude_ok_response()

        fire_skill("check", 7, tmp_path, runner=stub_runner, node_id="x-7")
        cmd_str = " ".join(str(c) for c in captured["cmd"])
        assert captured["cmd"][:3] != ["claude", "--print", "--output-format"]
        assert "agents" in captured["cmd"] and "spawn" in captured["cmd"]
        assert captured["cmd"][captured["cmd"].index("--substrate") + 1] == "headless"
        assert captured["cmd"][captured["cmd"].index("--harness") + 1] == "claude"
        assert captured["cmd"][captured["cmd"].index("--output-format") + 1] == "json"
        assert "check" in cmd_str
        assert "7" in cmd_str
        # `autonomous` is merged-only; check must not carry it.
        assert "autonomous" not in cmd_str

    def test_explicit_timeout_bounds_child_before_wrapper(self, tmp_path):
        """The child deadline fires before the wrapper can orphan its worker."""
        from fno.pr_watch._dispatch import fire_skill

        captured = {}

        def stub_runner(cmd, **kw):
            captured["cmd"] = cmd
            captured.update(kw)
            return _claude_ok_response()

        fire_skill("check", 1, tmp_path, runner=stub_runner, node_id="x-1", timeout_s=12.0)
        child_timeout = float(
            captured["cmd"][captured["cmd"].index("--timeout") + 1]
        )
        assert child_timeout == 12.0
        assert captured["timeout"] > child_timeout
