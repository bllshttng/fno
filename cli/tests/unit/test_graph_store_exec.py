"""The exec store transport: `_client_for` routes the spawn-needed branch to
`_ExecClient`, no resident keeper is minted, and the envelope semantics match
the socket path (ok -> result; error kind -> typed exception; a lost write is
`WriteUnconfirmed`)."""
import os

import pytest

from fno.graph import store as store_mod


@pytest.fixture()
def stub_worker(tmp_path, monkeypatch):
    """A stub `fno-agents-worker` script; yields (script_path, argv_log_path)."""
    script = tmp_path / "fno-agents-worker"
    argv_log = tmp_path / "argv.log"
    script.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$@\" >> " + str(argv_log) + "\n"
        "cat > /dev/null\n"
    )
    os.chmod(script, 0o755)
    monkeypatch.setattr(store_mod, "_worker_binary", lambda: script)
    return script, argv_log


def test_exec_refusal_maps_to_the_typed_error(tmp_path, stub_worker):
    script, _ = stub_worker
    script.write_text(
        "#!/bin/sh\n"
        "cat > /dev/null\n"
        "printf '%s' '{\"ok\":false,\"error\":{\"kind\":\"corrupt\",\"message\":\"bad\"}}'\n"
    )
    graph = tmp_path / "graph.json"
    client = store_mod._ExecClient(graph)
    with pytest.raises(store_mod.GraphCorruptError):
        client.request("read", {})


def test_exec_lost_write_is_unconfirmed(tmp_path, stub_worker):
    script, _ = stub_worker
    # exit 1 with NO stdout: the request may have been served; the client
    # refuses to claim either way.
    script.write_text("#!/bin/sh\ncat > /dev/null\nexit 1\n")
    graph = tmp_path / "graph.json"
    client = store_mod._ExecClient(graph)
    with pytest.raises(store_mod.WriteUnconfirmed):
        client.request(
            "commit", {"version": "sha256:x", "entries": [], "plan_rungs": {}, "attempt": 1}
        )


def test_exec_read_failure_without_reply_names_binary_lag(tmp_path, stub_worker):
    script, _ = stub_worker
    script.write_text("#!/bin/sh\ncat > /dev/null\nexit 1\n")
    graph = tmp_path / "graph.json"
    client = store_mod._ExecClient(graph)
    with pytest.raises(store_mod.StoreUnavailable) as excinfo:
        client.request("read", {})
    assert "spawn_failed" in str(excinfo.value)
    assert "fno-agents-worker current" in str(excinfo.value)
