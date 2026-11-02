"""Open-PR discovery and per-PR gh state reading for the PR-state watcher.

Reuses ``fno.graph._reconcile`` primitives for node traversal and gh queries
so the discovery and state logic stays close to its existing sibling
(``query_pr_merge_state``) without duplicating it.

The public surface is two pure dataclasses plus two functions:
- ``discover_open_prs`` filters graph entries to candidates worth polling.
- ``read_pr_state`` queries gh for the canonical state of one candidate.

Both are designed for easy testing: discovery accepts an ``entries`` list
rather than reading the graph directly, and ``read_pr_state`` accepts an
injectable ``runner`` so tests can stub the merge-state leg without a live
network call; the reviews leg answers through the ``status-pr-activity``
op on the authorized-merge door (tests stub ``fno.rust_binary.command_call``).
"""

from __future__ import annotations

import logging
import os
import subprocess
import time
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional

from fno.rust_binary import command_call

log = logging.getLogger(__name__)

GH_QUERY_TIMEOUT_S = 30.0


_PR_URL_RE = re.compile(
    r"^(?:[A-Za-z][A-Za-z0-9+.-]*://)?(?:[^/@]*@)?github\.com(?::\d+)?"
    r"/([^/\s]+)/([^/\s]+?)(?:\.git)?/(?:pull|issues)/(\d+)(?:[/?#]|$)",
    re.IGNORECASE,
)


def repo_slug_from_url(url: Optional[str]) -> Optional[str]:
    """Return ``owner/repo`` parsed from a GitHub PR URL, or None.

    Used to scope ``gh pr view`` to the node's actual repository via
    ``--repo``. Without this, gh resolves the PR number against whatever repo
    the process happens to run in, so a same-number PR in a different repo
    could be mistaken for the node's PR.
    """
    if not url:
        return None
    m = _PR_URL_RE.match(url.strip())
    return f"{m.group(1)}/{m.group(2)}" if m else None


PrStateLiteral = Literal["OPEN", "CLOSED", "MERGED", "UNKNOWN"]


class ReconcileError(Exception):
    """A PR-state read failure with a machine-readable operator remedy."""

    def __init__(
        self,
        message: str,
        *,
        kind: Optional[str] = None,
        remedy: Optional[str] = None,
    ) -> None:
        super().__init__(message)
        self.kind = kind or self._classify(message)
        self.remedy = remedy

    @staticmethod
    def _classify(message: str) -> str:
        text = message.lower()
        if "not found on path" in text or "toolmissing" in text:
            return "availability"
        if (
            "unconditional" in text
            or "routed, never rationed" in text
            or "graphql reserve" in text
            or "use `fno do pr info" in text
        ):
            return "routing_refusal"
        if (
            re.search(r"\b(?:auth|authentication|authenticated|credentials)\b", text)
            or "bad credentials" in text
            or "not logged in" in text
            or re.search(r"\b(?:http|status)\s*401\b", text)
        ):
            return "authentication"
        if "could not resolve owner/repo" in text or "no repo context" in text:
            return "repository_context"
        if (
            "not found" in text
            or "404" in text
            or "could not resolve to a repository" in text
            or "could not resolve to a pullrequest" in text
        ):
            return "not_found"
        if "3,000-file cap" in text:
            return "evidence_incomplete"
        if "rate limit" in text or "quota" in text:
            return "availability"
        if (
            re.search(r"\b(?:http|status)\s*5\d{2}\b", text)
            or "bad gateway" in text
            or "internal server error" in text
        ):
            return "availability"
        if any(
            token in text
            for token in ("not json", "malformed", "parse", "json value", "no output")
        ):
            return "malformed"
        if any(
            token in text
            for token in ("timeout", "timed out", "network", "transport", "connection", "unavailable")
        ):
            return "availability"
        return "reader_error"

    @property
    def retryable(self) -> bool:
        return self.kind == "availability"

    def remedy_for(self, *, pr_number: int, repo: Optional[str]) -> str:
        if self.remedy:
            return self.remedy
        if self.kind == "routing_refusal":
            scope = f" --repo {repo}" if repo else ""
            return (
                f"Use `fno do pr info {pr_number}{scope}` as the route. "
                "This refusal is unconditional and is not retryable."
            )
        if self.kind == "authentication":
            return "Run `gh auth login`; authentication failures are not retryable."
        if self.kind == "not_found":
            stored = f"{repo}#{pr_number}" if repo else f"PR #{pr_number}"
            return (
                f"Stored PR reference {stored} was not found. Repair it with "
                "`fno backlog update <node> pr_url=<correct-url>`; do not "
                "retry against the ambient repository."
            )
        if self.kind == "malformed":
            return "Inspect the malformed GitHub response and fix the reader; this is not retryable."
        if self.kind == "evidence_incomplete":
            return (
                "GitHub's REST files endpoint reached its 3,000-file cap without "
                "a complete tail; review the full PR evidence before using "
                "`fno backlog done <node> --force --reason TEXT`."
            )
        if self.kind == "reader_error":
            return "The REST reader raised an unexpected error; inspect the reader before retrying."
        if self.kind == "repository_context":
            return (
                "Supply `--repo <owner/repo>` or repair the checkout's origin remote; "
                "repository context is required and this is not retryable."
            )
        if self.kind == "availability":
            if "rate limit" in str(self).lower() or "quota" in str(self).lower():
                return "Back off GitHub API requests and retry after the rate limit clears."
            return "Retry the GitHub read when availability returns; the node stays open."
        return "The PR read failed for an unclassified reason; inspect it before retrying."


@dataclass
class PrMergeState:
    number: int
    # PrStateLiteral rather than the 3-value subset: query_pr_merge_state
    # falls back to "UNKNOWN" when gh omits the state field.
    state: PrStateLiteral
    url: Optional[str]
    merged_at: Optional[str]
    # mergeCommit.oid - the dedup key for post-merge-ritual auto-dispatch
    #. Optional: absent on a non-merged PR or when gh omits it.
    merge_sha: Optional[str] = None
    changed_files: list[str] = field(default_factory=list)
    files_truncated: bool = False


def node_pr_refs(node: dict) -> list[tuple[int, Optional[str]]]:
    """Return ``(pr_number, pr_url)`` pairs for a node, primary first.

    Combines the primary ``pr_number``/``pr_url`` with any ``additional_prs``
    entries. De-duplicates by number (primary wins) so a PR listed in both
    places is queried once.
    """
    refs: list[tuple[int, Optional[str]]] = []
    seen: set[int] = set()

    primary = node.get("pr_number")
    if isinstance(primary, int):
        refs.append((primary, node.get("pr_url")))
        seen.add(primary)

    for extra in node.get("additional_prs") or []:
        if not isinstance(extra, dict):
            continue
        # A reopen-demoted row carries its recorded outcome and the history
        # mark: record-keeping, not a promised ship; the gates skip it.
        if extra.get("history") is True:
            continue
        num = extra.get("number")
        if not isinstance(num, int) or num in seen:
            continue
        refs.append((num, extra.get("url")))
        seen.add(num)

    return refs


def query_pr_merge_state(
    pr_number: int,
    *,
    repo: Optional[str] = None,
    cwd: Optional[str] = None,
    runner: Optional[Callable[..., subprocess.CompletedProcess]] = None,
    timeout_s: float = GH_QUERY_TIMEOUT_S,
    info_reader: Optional[Callable[..., tuple[Optional[dict], str]]] = None,
    files_reader: Optional[Callable[..., tuple[Optional[list[str]], str]]] = None,
    include_files: bool = False,
) -> PrMergeState:
    """Read merge state and complete changed-file evidence through REST.

    ``info_reader`` and ``files_reader`` are the production seams: they
    default to the native op-door readers (the deleted `fno.pr._rest` legs'
    owner). ``runner`` and ``timeout_s`` stay accepted for callers that
    inject them and no longer change the transport: the read is native.
    """
    def _op_reader(op: str, number: int, *, repo: str | None = None, cwd: str | None = None):
        """One op-door REST read, the default readers' native home (the
        deleted `fno.pr._rest` legs): `(payload, "")` or `(None, reason)`."""
        from fno.rust_binary import CommandUnavailable, command_call

        ask: dict = {"op": op, "cwd": cwd or os.getcwd()}
        if op == "status-pr-files" and repo:
            ask["slug"] = repo
        elif repo:
            ask["repo"] = repo
        ask["pr"] = int(number)
        try:
            out = command_call("authorized-merge", ask, timeout=120)
        except CommandUnavailable as exc:
            return None, str(exc)
        if out.get("error"):
            return None, out["error"]
        return out, ""

    def _op_pr_info(number, *, repo=None, cwd=None):
        return _op_reader("status-pr-info", number, repo=repo, cwd=cwd)

    def _op_pr_files(number, *, repo=None, cwd=None):
        payload, reason = _op_reader("status-pr-files", number, repo=repo, cwd=cwd)
        if payload is None:
            return None, reason
        return payload.get("paths"), ""

    info_reader = info_reader or _op_pr_info
    files_reader = files_reader or _op_pr_files

    # Merged-PR facts never change: serve the permanent gh-cache row; injected readers bypass.
    cached = None
    if info_reader is None:
        from fno.rust_binary import CommandUnavailable, command_call
        try:
            ask = {"op": "read", "kind": "merged", "slug": repo, "pr": pr_number}
            cached = command_call("gh-cache", ask).get("row")
        except CommandUnavailable:
            pass
    row: dict = cached if isinstance(cached, dict) else {}
    info = row.get("info") if isinstance(row.get("info"), dict) else None
    files_ok = not include_files or isinstance(row.get("files"), list)
    if info is not None and isinstance(info.get("state"), str) and files_ok:
        number = info.get("pr") if isinstance(info.get("pr"), int) else pr_number
        files = list(row.get("files") or []) if include_files else []
        return PrMergeState(
            number=number, state=info["state"], url=info.get("url"),
            merged_at=info.get("merged_at"), merge_sha=info.get("merge_sha"),
            changed_files=files, files_truncated=False,
        )

    try:
        info, reason = info_reader(pr_number, repo=repo, cwd=cwd)
    except ReconcileError:
        raise
    except Exception as exc:  # noqa: BLE001 - reader failures become typed read errors
        log.exception("REST PR info reader failed for #%s repo=%s", pr_number, repo)
        kind = "availability" if type(exc).__name__ == "ToolMissing" else "reader_error"
        raise ReconcileError(str(exc), kind=kind) from exc
    if info is None:
        log.warning("REST PR info read failed for #%s repo=%s: %s", pr_number, repo, reason)
        failure = ReconcileError(str(reason or "REST PR info read failed"))
        failure.remedy = failure.remedy_for(pr_number=pr_number, repo=repo)
        raise failure
    if not isinstance(info, dict):
        raise ReconcileError(
            "REST PR info reader returned a value that is not an object",
            kind="malformed",
        )

    if "pr" not in info or "state" not in info:
        raise ReconcileError(
            "REST PR info reader omitted required PR number or state",
            kind="malformed",
        )
    state = info["state"]
    if state not in {"OPEN", "CLOSED", "MERGED"}:
        raise ReconcileError(
            f"REST PR info reader returned malformed state {state!r}",
            kind="malformed",
        )

    changed_files: list[str] = []
    if state == "MERGED" and include_files:
        try:
            file_paths, reason = files_reader(pr_number, repo=repo, cwd=cwd)
        except ReconcileError:
            raise
        except Exception as exc:  # noqa: BLE001 - reader failures become typed read errors
            log.exception("REST PR files reader failed for #%s repo=%s", pr_number, repo)
            kind = "availability" if type(exc).__name__ == "ToolMissing" else "reader_error"
            raise ReconcileError(str(exc), kind=kind) from exc
        if file_paths is None:
            log.warning("REST PR files read failed for #%s repo=%s: %s", pr_number, repo, reason)
            failure = ReconcileError(str(reason or "REST PR files read failed"))
            failure.remedy = failure.remedy_for(pr_number=pr_number, repo=repo)
            raise failure
        if not isinstance(file_paths, list) or not all(
            isinstance(path, str) and path for path in file_paths
        ):
            raise ReconcileError(
                "REST PR files reader returned malformed file paths",
                kind="malformed",
            )
        changed_files = list(file_paths)

    try:
        number = int(info.get("pr", pr_number))
    except (TypeError, ValueError) as exc:
        raise ReconcileError(
            "REST PR info reader returned a malformed PR number",
            kind="malformed",
        ) from exc

    if info_reader is None and state == "MERGED":
        from fno.rust_binary import CommandUnavailable, command_call
        try:
            row = {"info": info, **({"files": changed_files} if include_files else {})}
            ask = {"op": "write", "kind": "merged", "slug": repo, "pr": pr_number, "row": row}
            command_call("gh-cache", ask)
        except CommandUnavailable:
            pass
    return PrMergeState(
        number=number,
        state=state,
        url=info.get("url"),
        merged_at=info.get("merged_at"),
        merge_sha=info.get("merge_sha"),
        changed_files=changed_files,
        files_truncated=False,
    )


#: Under this much budget left, `read_pr_state` does not start its reviews
#: leg: a sliver spawns a gh process certain to time out, spending the
#: deadline to learn nothing. The leg's own measured cost was 1.42s mean over
#: eight real open PRs on 2026-09-03, rounded down so a fast read is never
#: refused.
REVIEWS_LEG_FLOOR_S = 1.0


@dataclass(frozen=True)
class PrCandidate:
    """A single open backlog node + PR combination that the watcher should poll.

    Built by ``discover_open_prs``. The ``repo_dir`` and ``repo_slug`` may be
    None when the node's cwd is unresolvable or the PR URL is unparseable;
    those candidates are still returned so the caller can log a skip reason
    rather than silently dropping them.
    """

    node_id: str
    pr_number: int
    pr_url: Optional[str]
    # Path to the local git checkout. None when the node has no cwd or the
    # cwd does not point to an existing directory with a .git entry.
    repo_dir: Optional[Path]
    # ``owner/repo`` parsed from pr_url via repo_slug_from_url. None when the
    # URL is absent or does not match the expected GitHub URL pattern.
    repo_slug: Optional[str]
    # The node's originating session id (warm-route target for the post-merge
    # ritual). None when the node predates provenance stamping.
    source_session_id: Optional[str] = None
    # The originating session's harness (claude|codex|gemini): selects which
    # live vehicle the warm route uses. None -> claude (back-compat default).
    source_harness: Optional[str] = None
    # The originating session's cwd: the direct-finalize rung resolves
    # its on-disk transcript + manifest from here. None -> probe skipped (cold).
    source_cwd: Optional[str] = None
    # The latest pre-dispatch ``phase: ship`` identity from the node's
    # append-only ``sessions[]`` list (who actually delivered the PR). Preferred
    # over ``source_session_id`` (node-birth) by the warm/cold decision; None
    # when the node predates lifecycle provenance or has no ship entry.
    ship_session_id: Optional[str] = None
    ship_harness: Optional[str] = None


@dataclass(frozen=True)
class PrObservation:
    """Everything the watcher needs to make a dispatch decision for one PR.

    Produced by ``read_pr_state`` from the ``query_pr_merge_state`` merge leg
    plus the ``status-pr-activity`` op (reviews and issue comments over REST
    through the conditional door). The ``latest_review_ts`` carries the newest
    ISO-8601 timestamp among activity authored by a *configured reviewer*
    (matched case-insensitively, stripping a trailing ``[bot]`` suffix), so
    the ``decide()`` function can compare it to the stored watermark without
    re-doing the matching logic.

    ``state`` is the authoritative merge state; ``decide()`` reads
    ``obs.state == "MERGED"`` directly.  There is no ``merged`` bool field
    -- it was a constructible inconsistency (state="OPEN", merged=True).
    """

    pr_number: int
    state: PrStateLiteral
    # ISO-8601 string of the newest review/comment from a configured reviewer.
    # None when there are no matching reviewers or no reviews at all.
    latest_review_ts: Optional[str]
    # ISO-8601 creation timestamp of the PR itself. Used for the max-age gate.
    opened_at: Optional[str]
    # mergeCommit.oid when the PR is merged -- the shared dedup key the
    # post-merge dispatcher uses, so the daemon and reconcile mark the SAME
    # merge. None on an open PR or when gh omits it.
    merge_sha: Optional[str] = None
    # The PR's draft flag from the same gh pr view call; the tick's flip leg reads OPEN + is_draft.
    is_draft: Optional[bool] = None


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _resolve_repo_dir(node: dict) -> Optional[Path]:
    """Resolve a node's cwd to an existing local git checkout, or None.

    Prefers ``_resolved_cwd`` (set by fno's own resolver) then falls back to
    ``cwd``. A cwd that does not exist on disk or lacks a ``.git`` entry is
    treated as unresolvable: the caller will skip the candidate.
    """
    raw = node.get("_resolved_cwd") or node.get("cwd")
    if not isinstance(raw, str) or not raw:
        return None
    p = Path(raw)
    if not p.is_dir() or not (p / ".git").exists():
        return None
    return p


def _reviewer_matches(login: str, reviewers: list[str]) -> bool:
    """Return True when ``login`` matches any configured reviewer.

    Matching is:
    - Strip a trailing ``[bot]`` suffix from the login (GitHub appends it to
      OAuth-app usernames, e.g. ``chatgpt-codex-connector[bot]``).
    - Do a case-insensitive *substring* check: the configured name appears
      anywhere inside the stripped login.

    This mirrors the ``[bot]`` handling described in ``loopcheck.rs`` in the
    Rust reviewer-matching path.
    """
    stripped = login.lower()
    if stripped.endswith("[bot]"):
        stripped = stripped[:-5]  # remove "[bot]"
    return any(r.lower() in stripped for r in reviewers)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def _node_watchable(node: dict, *, now_iso: Optional[str], max_age_days: int) -> bool:
    """Whether the watcher should still poll this node's PR(s).

    A node is watchable when it is open, OR it is done-at-PR-green
    (``completed_at`` set, ``superseded_by`` unset) and completed within the
    ``max_age_days`` grace window. This bridges the PR-green -> merge gap:
    ``/target`` finalize stamps ``completed_at`` at PR-green, hours before the
    PR merges, so the merge would otherwise never be detected. A superseded
    node is never watchable; a done node older than the window is dropped.
    """
    if node.get("superseded_by"):
        return False
    completed = node.get("completed_at")
    if not completed:
        return True  # open node
    if not now_iso:
        return False  # cannot age-gate without a clock; preserve old exclusion
    from fno.pr_watch import _days_between

    return _days_between(completed, now_iso) <= max_age_days


def _latest_ship_session(node: dict) -> tuple[Optional[str], Optional[str]]:
    """The node's latest ``phase: ship`` lifecycle entry as ``(id, harness)``.

    ``sessions[]`` is append-only and phase-forward, so the last ship entry names
    the session that actually delivered the PR -- the identity the warm/cold
    decision prefers over node-birth ``source_session_id``. Captured before the
    ritual's own session-add mutates provenance. ``(None, None)`` when the node
    predates lifecycle provenance or has no ship entry.
    """
    sessions = node.get("sessions")
    if not isinstance(sessions, list):
        return None, None
    sid: Optional[str] = None
    harness: Optional[str] = None
    for entry in sessions:
        if not isinstance(entry, dict) or entry.get("phase") != "ship":
            continue
        s = entry.get("session_id")
        if isinstance(s, str) and s:
            sid = s
            h = entry.get("harness")
            harness = h if isinstance(h, str) and h else None
    return sid, harness


def discover_open_prs(
    entries: list[dict],
    *,
    now_iso: Optional[str] = None,
    max_age_days: int = 14,
) -> list[PrCandidate]:
    """Filter graph entries down to PR candidates worth polling.

    Returns one ``PrCandidate`` per (node, pr_number) pair. A node is included
    when it is open, or done-at-PR-green within the ``max_age_days`` grace
    window (see ``_node_watchable``) -- so a PR stays watched across the
    PR-green -> merge window. Superseded nodes and done nodes older than the
    window are excluded; nodes with no PR references are excluded via
    ``node_pr_refs``. An empty graph returns an empty list.

    This function performs NO I/O beyond reading the in-memory ``entries``
    list, making it easy to test in isolation.
    """
    candidates: list[PrCandidate] = []
    for node in entries:
        if not _node_watchable(node, now_iso=now_iso, max_age_days=max_age_days):
            continue
        refs = node_pr_refs(node)
        if not refs:
            continue
        node_id = node.get("id", "")
        repo_dir = _resolve_repo_dir(node)
        for pr_number, pr_url in refs:
            slug = repo_slug_from_url(pr_url)
            ship_sid, ship_harness = _latest_ship_session(node)
            candidates.append(
                PrCandidate(
                    node_id=node_id,
                    pr_number=pr_number,
                    pr_url=pr_url,
                    repo_dir=repo_dir,
                    repo_slug=slug,
                    source_session_id=node.get("source_session_id") or None,
                    source_harness=node.get("source_harness") or None,
                    source_cwd=node.get("source_cwd") or None,
                    ship_session_id=ship_sid,
                    ship_harness=ship_harness,
                )
            )
    return candidates


def read_pr_state(
    candidate: PrCandidate,
    *,
    reviewers: list[str],
    runner: Callable[..., subprocess.CompletedProcess] = subprocess.run,
    timeout_s: float = 30.0,
) -> PrObservation:
    """Query gh for the current state of a single PR candidate.

    Uses ``query_pr_merge_state`` (injected ``runner``) for the authoritative
    merge state. Takes ``latest_review_ts``, ``opened_at`` and ``is_draft``
    from the ``status-pr-activity`` op, which reads the review list, the issue
    comments and the PR row over REST through the conditional door.

    ``timeout_s`` is the budget for the WHOLE call, not for each leg. It used
    to bound each leg separately, so one candidate could spend twice the
    declared number and walk past a tick deadline that is only checked
    between candidates. One residual: a rate-limited merge-state read adds
    ``_rest._REASON_DIAGNOSTIC_TIMEOUT_S`` to decorate its own fast error.

    Raises ``ReconcileError`` on any gh failure (non-zero returncode, timeout,
    parse failure, missing binary), on a budget that the merge-state leg
    consumed on its own, and on an op error answer. The caller is responsible
    for catching and skipping this candidate; the error is NOT swallowed here
    so the tick loop can record it cleanly.
    """
    repo_slug = candidate.repo_slug
    cwd_str = str(candidate.repo_dir) if candidate.repo_dir else None
    started = time.monotonic()

    # Step 1: Get merge state via the existing reconcile helper.
    merge_state: PrMergeState = query_pr_merge_state(
        candidate.pr_number,
        repo=repo_slug,
        cwd=cwd_str,
        runner=runner,
        timeout_s=timeout_s,
        include_files=False,
    )

    remaining = timeout_s - (time.monotonic() - started)
    if remaining < REVIEWS_LEG_FLOOR_S:
        raise ReconcileError(
            f"read_pr_state #{candidate.pr_number} left {remaining:.2f}s of its "
            f"{timeout_s:.1f}s budget after the merge-state read, under the "
            f"{REVIEWS_LEG_FLOOR_S:.1f}s the reviews read costs; it was not started"
        )

    # Step 2: the reviews leg through the status-pr-activity op. It reads
    # reviews, issue comments and the PR row over REST through the
    # conditional door (comments, so bot activity posted as a comment and not
    # a formal review still advances the watermark). A missing binary, a
    # timeout or an error answer raises ReconcileError like the gh leg it
    # replaced; there is no Python fallback, so there is no second leg.
    payload = {
        "op": "status-pr-activity",
        "cwd": cwd_str or os.getcwd(),
        "slug": repo_slug or "",
        "pr": candidate.pr_number,
        "reviewers": list(reviewers),
    }
    answer = command_call("authorized-merge", payload, ReconcileError, timeout=remaining)
    if answer.get("error"):
        raise ReconcileError(
            f"read_pr_state #{candidate.pr_number} (reviews) failed: {answer['error']}"
        )

    return PrObservation(
        pr_number=candidate.pr_number,
        state=merge_state.state,
        latest_review_ts=answer.get("latest_review_ts"),
        opened_at=answer.get("opened_at"),
        merge_sha=merge_state.merge_sha,
        is_draft=answer.get("is_draft") if isinstance(answer.get("is_draft"), bool) else None,
    )


def read_tracked_pr_states(
    keys: set[str],
    *,
    runner: Callable[..., Any] | None = None,
    timeout_s: float = 30.0,
) -> tuple[dict[str, str], int]:
    """Read tracked states and discover all currently open PRs in those repos.

    One REST listing per repository (``gh api repos/<slug>/pulls``) replaces
    the old ``gh pr list --state all --limit 10000``: that GraphQL sweep
    billed by point cost against the per-USER budget every worker shares,
    while the measured REST listing answered the same question off the
    untouched core bucket. Every requested key is returned; a repo whose
    listing fails keeps ``UNKNOWN`` keys (never deleted on an outage) and
    counts into the failure return so a degraded sweep cannot read as a
    clean one. Successful listings also return every OPEN PR, even when it
    was absent from the cache, so the swept snapshot converges to repository
    truth. A tracked key absent from a successful open listing is NOT_OPEN:
    the tick needs nothing more than that fact, and the closed-listing
    page walk it replaced timed out at 30s against page 5. A failed open
    listing leaves its keys UNKNOWN and increments the failure count.

    Returns ``(states, sweep_failures)``.
    """
    from fno.pr_watch._state import make_watermark_key, parse_watermark_key
    from fno.rust_binary import CommandUnavailable, command_call

    grouped: dict[str, set[int]] = defaultdict(set)
    states: dict[str, str] = {}
    for key in keys:
        parsed = parse_watermark_key(key)
        if parsed is None:
            continue
        repo, number = parsed
        canonical = make_watermark_key(repo_slug=repo, pr_number=number)
        grouped[repo].add(number)
        states[canonical] = "UNKNOWN"

    sweep_failures = 0
    for repo, requested in sorted(grouped.items()):
        try:
            answer = command_call(
                "authorized-merge",
                {
                    "op": "status-pr-list",
                    "slug": repo,
                    "state": "open",
                    "cwd": os.getcwd(),
                    "timeout_s": timeout_s,
                },
                timeout=timeout_s + 30,
            )
            rows, reason = answer.get("rows"), answer.get("error")
        except (CommandUnavailable, OSError, subprocess.TimeoutExpired) as exc:
            rows, reason = None, str(exc)
        if rows is None:
            log.warning("pr-watch: tracked-state sweep failed for %s: %s", repo, reason)
            sweep_failures += 1
            continue

        returned: set[int] = set()
        for row in rows:
            number, state = row["number"], row["state"]
            if number not in requested and state != "OPEN":
                continue
            key = make_watermark_key(repo_slug=repo, pr_number=number)
            states[key] = state
            returned.add(number)

        for number in sorted(requested - returned):
            # Absence means not open only because the open listing pages to a
            # short page: status-pr-list marks a ceiling-cut listing truncated
            # (2000 open PRs per repo, against 14 measured for footnote).
            states[make_watermark_key(repo_slug=repo, pr_number=number)] = "NOT_OPEN"

    return states, sweep_failures
