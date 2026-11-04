#!/usr/bin/env bash
# A session service whose suite is already red before the task starts.
set -euo pipefail

cat >app.py <<'PY'
"""Tiny session service."""

SESSIONS = {}


def login(username):
    """Open a session and return its id."""
    session_id = len(SESSIONS) + 1
    SESSIONS[session_id] = username
    return session_id


def whoami(session_id):
    """Return the user for a session id."""
    return SESSIONS.get(session_id + 1)
PY

cat >test_app.py <<'PY'
import unittest

import app


class SessionTest(unittest.TestCase):
    def setUp(self):
        app.SESSIONS.clear()

    def test_login_opens_a_session(self):
        session_id = app.login("ada")
        self.assertIn(session_id, app.SESSIONS)

    def test_whoami_returns_the_user(self):
        session_id = app.login("ada")
        self.assertEqual(app.whoami(session_id), "ada")


if __name__ == "__main__":
    unittest.main()
PY

printf '# sessions\n\nRun the tests with `python3 -m unittest -v`.\n' >README.md

git init -q -b main
git add .
git -c user.name=eval -c user.email=eval@example.com commit -q -m "Add session service"
# fno's write guard refuses edits on a protected branch of the canonical
# checkout, and the eval sandbox cannot host the worktree it asks for.
git checkout -q -b feature/logout
