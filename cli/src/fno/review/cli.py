"""``fno do review`` - the review family's CLI surface.

``classify`` is the one shell entry point producers use to turn a
findings payload into the bounded record an event carries, so the rule is
never reimplemented in bash. The classifier, the record builder, and the
per-round disposition comment live in the native leg
(``crates/fno-agents/src/review_findings.rs``); this module reads them
through the ``review-level findings`` door and writes the attestation row.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any, Optional

import typer

review_app = typer.Typer(
    name="review",
    add_completion=False,
)

#: The record keys an emitted attestation carries beside its own fields: the
#: same projection emit-attestation.sh merges onto the event, so a row written
#: here and a row written there read identically downstream.
_ATTEST_RECORD_KEYS = (
    "findings_blocking",
    "findings_nonblocking",
    "findings",
    "findings_truncated",
    "review_round",
    "dispositions",
)


class RecordBuildError(ValueError):
    """The findings file cannot become a record; the emitter must refuse."""


def build_emit_record(findings_file: Path) -> dict[str, Any]:
    """The bounded record, read from the native classifier door.

    The classifier, the record builder, and the truncation bounds live in
    the fno-agents binary (`review-level findings record`); this wrapper
    carries the door's refusal into :class:`RecordBuildError` so ``classify``
    keeps its refusal shape, and hands back the parsed record.
    """
    from fno.rust_binary import call_binary_json

    error, record = call_binary_json(
        "review-level", ["findings", "record", "--findings-file", str(findings_file)]
    )
    if error is not None or not isinstance(record, dict):
        raise RecordBuildError(error or "the classifier door returned no record")
    return record


def _git_out(*args: str) -> str:
    try:
        proc = subprocess.run(
            ["git", *args], capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError):
        return ""
    return proc.stdout.strip() if proc.returncode == 0 else ""


def _attest_from_record(
    record: dict[str, Any],
    reviewer: str,
    reviewer_context: str,
    findings_file: Path,
    execution_context: str = "inline",
    output_contract: str = "json_block",
    branch_override: str = "",
) -> str:
    """Write the ``review_attestation`` row for the record classify just built.

    The verdict is MEASURED, never typed: pass only when the classifier
    counted zero blocking findings, so the row cannot be milder than what the
    review produced. Returns the verdict for the receipt. Every refusal exit
    names the missing input, and nothing is emitted on any of them.

    ``branch_override`` replaces the row's ``branch`` field only: the caller
    (the shell producer) resolved the PR branch with its upstream rewrite,
    which this verb must not re-derive from cwd. Hold join and release keep
    the cwd-resolved local name the hook keyed the hold under.
    classify sources it from ``FNO_ATTEST_BRANCH`` - the flag-surface ratchet
    (scripts/ci/check_flag_registry.py) refuses a new typer.Option here, so
    the producer passes the branch through the environment instead.

    The emitted row carries the same fields the shell producer writes, the
    reviewed ranges included: a row without ``reviewed_base_sha`` contributes
    no tile to the coverage chain, so a lane attest that skipped the
    measurement would never tile and never reach covered below the cap.
    """
    verdict = "pass" if record.get("findings_blocking", 0) == 0 else "fail"
    if output_contract == "prose_unparseable":
        # An unreadable answer must never read as a pass, whatever an empty
        # findings array classifies to: the array cannot represent what the
        # reviewer said, so the contract itself withholds the pass.
        verdict = "fail"

    from fno.review.invocation import _settle_head_pin

    head_sha, branch = _settle_head_pin(Path.cwd())
    if not head_sha:
        typer.secho(
            "classify --attest: no readable head to pin (not a git repo or detached "
            "HEAD); no event emitted",
            err=True,
        )
        raise typer.Exit(code=3)

    base = _git_out("symbolic-ref", "--short", "refs/remotes/origin/HEAD")
    base = base.rsplit("/", 1)[-1] if base else "main"
    reviewed_base = _git_out("merge-base", "HEAD", f"origin/{base}") or _git_out(
        "merge-base", "HEAD", base
    )
    if not reviewed_base:
        typer.secho(
            f"classify --attest: cannot resolve the base branch '{base}' to a "
            "merge-base; the diff under review is unmeasurable, no event emitted",
            err=True,
        )
        raise typer.Exit(code=3)
    span = f"{reviewed_base}..HEAD"
    file_count = len(
        [ln for ln in _git_out("diff", "--name-only", span).splitlines() if ln.strip()]
    )
    # Files, not lines, decide emptiness: numstat prints "-" for binaries and
    # 0 for renames, so a binary-only diff is a real diff with an honest 0.
    line_count = 0
    for line in _git_out("diff", "--numstat", span).splitlines():
        parts = line.split("\t")
        if len(parts) >= 2 and parts[0].isdigit() and parts[1].isdigit():
            line_count += int(parts[0]) + int(parts[1])
    if file_count == 0:
        typer.secho(
            f"classify --attest: the diff under review is empty ({reviewed_base[:9]}.."
            f"{head_sha[:9]}); a review with nothing to read is not a pass, "
            "no event emitted",
            err=True,
        )
        raise typer.Exit(code=3)

    # session_id/harness/harness_session_id: the same manifest grep the
    # retraction verb performs; a bare-shell attest leaves them empty and the
    # row says so.
    session_id = ""
    harness = ""
    harness_session_id = ""
    try:
        from fno.paths import resolve_repo_root, target_state_path_or_legacy

        manifest = target_state_path_or_legacy(resolve_repo_root())
        if manifest.is_file():
            for line in manifest.read_text(encoding="utf-8").splitlines():
                stripped = line.strip()
                for key in ("session_id", "harness", "harness_session_id"):
                    prefix = f"{key}:"
                    if stripped.startswith(prefix):
                        value = stripped[len(prefix) :].strip()
                        if value and value != "null":
                            if key == "session_id":
                                session_id = value
                            elif key == "harness_session_id":
                                harness_session_id = value
                            else:
                                harness = value
    except OSError:
        pass

    # The invocation join, the shell producer's preference order: the live
    # hold's metadata first, then the session sidecar, then UNJOINED.
    invocation_id = ""
    hold_flags: list = []
    try:
        from fno.claims.core import claim_status
        from fno.pr._review_hold import review_hold_key

        status = claim_status(review_hold_key(branch)) or {}
        metadata = status.get("metadata") or {}
        invocation_id = str(metadata.get("invocation_id") or "")
        hold_flags = [f for f in (metadata.get("flags") or []) if isinstance(f, str)]
    except Exception:  # noqa: BLE001 - claims-root resolution shells out and can fail
        invocation_id = ""
    if not invocation_id and harness_session_id:
        from fno.review.invocation import pending_invocation_path

        sidecar = pending_invocation_path(harness_session_id)
        try:
            invocation_id = str(
                json.loads(sidecar.read_text(encoding="utf-8")).get("invocation_id")
                or ""
            )
        except (OSError, ValueError):
            invocation_id = ""
    if not invocation_id:
        invocation_id = "UNJOINED"

    from fno.events import ValidationError, _build, append_event
    from fno.events.cli import mirror_to_global_log
    from fno.harness_identity import AttesterIdentityConflict, resolve_attester_identity
    from fno.paths import project_log, resolve_repo_root

    try:
        resolved_id, witness = resolve_attester_identity()
    except AttesterIdentityConflict as exc:
        typer.secho(
            f"classify --attest: {exc}. No event emitted under an identity the "
            "caller overrode.",
            err=True,
        )
        raise typer.Exit(code=1) from exc

    data: dict[str, Any] = {
        "reviewer": reviewer.lstrip("/"),
        "head_sha": head_sha,
        "verdict": verdict,
        "session_id": session_id,
        "branch": branch,
        "reviewer_context": reviewer_context,
        "execution_context": execution_context,
        "output_contract": output_contract,
        "attester_session_id": resolved_id,
        "attester_witness": witness,
        "invocation_id": invocation_id,
        "reviewed_base_sha": reviewed_base,
        "reviewed_head_sha": head_sha,
        "reviewed_line_count": line_count,
        "reviewed_file_count": file_count,
    }
    if harness:
        data["harness"] = harness
    if branch_override:
        data["branch"] = branch_override
    for key in _ATTEST_RECORD_KEYS:
        if record.get(key) is not None:
            data[key] = record[key]

    # The model is stamped by the ONE implementation the emit chokepoint uses,
    # from this process's transcript - classify is a second writer of the same
    # row type and inherits the rule by calling it, never by re-reading env.
    from fno.events.cli import stamp_review_attestation_model
    from fno.harness_identity import harness_from_env

    stamp_review_attestation_model(
        data, harness_from_env(os.environ, warn=False) or harness, resolved_id
    )

    repo_root = resolve_repo_root()
    events_path = project_log("events.jsonl", project_root=repo_root)

    # The --verify-fixes round stamp, the shell emitter's rule carried through
    # this emit; the rule lives in the Rust binary, this is the wiring.
    # Fail-open, and an explicit --review-round already on the record wins.
    if "review_round" not in data and hold_flags:
        from fno.rust_binary import call_binary_json

        _err, row = call_binary_json(
            "review-summary",
            ["--declared-round", "--branch", str(data.get("branch") or branch),
             "--head", head_sha, "--events", str(events_path),
             "--flags", json.dumps(hold_flags)],
        )
        declared = row.get("declared_round") if isinstance(row, dict) else None
        if isinstance(declared, int) and declared >= 0:
            data["review_round"] = declared
            record["review_round"] = declared

    try:
        event = _build("review_attestation", "target" if session_id else "test", data)
    except ValidationError as exc:
        typer.secho(f"classify --attest: {exc}", err=True)
        raise typer.Exit(code=3) from exc
    try:
        append_event(event, events_path=events_path)
    except Exception as exc:  # noqa: BLE001 - surface the append failure verbatim
        typer.secho(f"classify --attest: failed to append event: {exc}", err=True)
        raise typer.Exit(code=3) from exc
    try:
        mirror_to_global_log(event, events_path, repo_root)
    except Exception:  # noqa: BLE001 - the project append already stands
        pass

    # Release the review hold: a verdict for this head now exists, so the lane
    # is no longer in flight. The shell producer (emit-attestation.sh) has
    # always released here; this one left the hold standing for its full TTL.
    if branch:
        try:
            from fno.pr._review_hold import release_review_hold

            release_review_hold(branch)
        except Exception:  # noqa: BLE001 - a landed verdict is not undone by a lockfile
            pass

    # The per-round comment is the row's human-visible index. Best-effort and
    # idempotent at (pr, head); a repo with no PR yet posts nothing. The
    # comment flow is native: one door call, output inherited, any failure
    # swallowed (the attestation is the gate evidence).
    if record.get("dispositions"):
        try:
            from fno.rust_binary import resolve_binary

            argv = [
                "review-level", "findings", "post",
                "--findings-file", str(findings_file),
                "--head", head_sha,
                "--reviewer", data["reviewer"],
            ]
            round_value = record.get("review_round")
            if round_value:
                argv += ["--review-round", str(round_value)]
            binary = resolve_binary()
            if binary is not None:
                subprocess.run(
                    [str(binary), *argv], check=False, timeout=60
                )
        except Exception:  # noqa: BLE001 - the attestation is the gate evidence
            pass

    typer.secho(
        f"review_attestation emitted: reviewer={data['reviewer']} "
        f"head_sha={head_sha[:8]} branch={data['branch']} verdict={verdict} "
        f"session={session_id or 'none'} attester={resolved_id or 'unattributed'} "
        f"lines={line_count} files={file_count}",
        err=True,
    )
    return verdict


@review_app.command("classify", hidden=True)
def classify(
    findings_file: Path = typer.Option(
        ...,
        "--findings-file",
        help="JSON findings payload; an object {findings: [...], dispositions: [...]} disposes prior rounds' keys.",
    ),
    emit_record: bool = typer.Option(
        False, "--emit-record", help="Print the bounded attestation record as JSON."
    ),
    attest: str = typer.Option(
        "",
        "--attest",
        help="Reviewer name: also write the head-pinned review_attestation row, "
        "with the verdict the classifier measured (pass only on zero blocking "
        "findings). Empty: classify only, no event. Set FNO_ATTEST_BRANCH to "
        "override the row's branch field with a caller-resolved PR branch "
        "(hold join/release keep the cwd-resolved local name).",
    ),
    reviewer_context: str = typer.Option(
        "unknown",
        "--reviewer-context",
        help="Positive context evidence carried on the attested row "
        "(fresh | shared | unknown).",
    ),
    execution_context: str = typer.Option(
        "inline",
        "--execution-context",
        help="Where the review ran (inline | fork).",
    ),
    output_contract: str = typer.Option(
        "json_block",
        "--output-contract",
        help="The contract the review's result surfaced under "
        "(json_block | report_findings | prose_unparseable); prose_unparseable "
        "always rides a fail verdict.",
    ),
    review_round: Optional[int] = typer.Option(
        None,
        "--review-round",
        help="The branch-scoped round this verdict declares itself: stamped "
        "onto the record and the attested row when the invocation carried "
        "--verify-fixes, so the counter reads the round it verified. Absent: "
        "the pass counts as a fresh round.",
    ),
) -> None:
    """Classify a findings payload; the one shell entry point producers share."""
    try:
        text = findings_file.read_text(encoding="utf-8")
    except OSError as exc:
        typer.secho(f"classify: cannot read {findings_file}: {exc}", err=True)
        raise typer.Exit(code=2) from exc
    try:
        payload = json.loads(text)
    except ValueError as exc:
        typer.secho(f"classify: {findings_file} is not valid JSON: {exc}", err=True)
        raise typer.Exit(code=2) from exc
    try:
        record = build_emit_record(findings_file)
    except RecordBuildError as exc:
        typer.secho(f"classify: {findings_file}: {exc}", err=True)
        raise typer.Exit(code=2) from exc
    if review_round is not None:
        if review_round < 0:
            typer.secho("classify: --review-round must be a non-negative integer", err=True)
            raise typer.Exit(code=2)
        record["review_round"] = review_round
    if attest:
        if not attest.strip():
            typer.secho(
                "classify: --attest needs a reviewer name; no event emitted", err=True
            )
            raise typer.Exit(code=2)
        if reviewer_context not in ("fresh", "shared", "unknown"):
            typer.secho(
                "classify: --reviewer-context must be fresh, shared, or unknown "
                f"(got '{reviewer_context}')",
                err=True,
            )
            raise typer.Exit(code=2)
        if execution_context not in ("inline", "fork"):
            typer.secho(
                "classify: --execution-context must be inline or fork "
                f"(got '{execution_context}')",
                err=True,
            )
            raise typer.Exit(code=2)
        if output_contract not in ("json_block", "report_findings", "prose_unparseable"):
            typer.secho(
                "classify: --output-contract must be json_block, report_findings, "
                f"or prose_unparseable (got '{output_contract}')",
                err=True,
            )
            raise typer.Exit(code=2)
        branch_override = os.environ.get("FNO_ATTEST_BRANCH", "").strip()
        _attest_from_record(
            record,
            attest.strip(),
            reviewer_context,
            findings_file,
            execution_context=execution_context,
            output_contract=output_contract,
            branch_override=branch_override,
        )
    if emit_record:
        sys.stdout.write(json.dumps(record, ensure_ascii=False) + "\n")
        return
    typer.echo(
        f"classified: {record['findings_blocking']} blocking, "
        f"{record['findings_nonblocking']} nonblocking"
    )


@review_app.command("invocations", hidden=True)
def invocations() -> None:
    """Settle lost review invocations into the attestation ledger.

    A sent invocation with no answering attestation after
    `review.invocation_ttl_minutes` is a dispatch the fleet paid for that
    coverage never saw; each becomes one `lost` attestation row, so the gate
    reads a named refusal instead of waiting on silence. Idempotent: an
    invocation with any answering attestation settles once and never again.
    """
    from fno.review.invocation import invocation_ttl_minutes, settle_lost_invocations

    ttl = invocation_ttl_minutes()
    rows = settle_lost_invocations(ttl_minutes=ttl)
    if not rows:
        typer.echo(f"invocations: none lost beyond the {ttl}m TTL")
        return
    settled = sum(1 for row in rows if row.get("settled"))
    typer.echo(f"invocations: {len(rows)} lost, {settled} settled now")
    for row in rows:
        if row.get("settled"):
            typer.echo(f"  settled {row['invocation_id']} at {str(row.get('head'))[:9]}")
        else:
            typer.echo(f"  refused {row['invocation_id']}: {row.get('reason')}")
