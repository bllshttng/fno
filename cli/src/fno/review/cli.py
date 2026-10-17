"""``fno do review`` - the review family's CLI surface.

``invocations`` is the one leaf left in this package: it settles lost review
invocations into the attestation ledger. The classifier, the record builder,
and the per-round disposition comment answer from the native leg
(``crates/fno-agents/src/review_findings.rs``) behind the ``review-level
findings`` door, and the attestation row has one writer: the shell emitter
(skills/review/scripts/emit-attestation.sh) through the ``fno doctor event
emit`` chokepoint.
"""
from __future__ import annotations

import typer

review_app = typer.Typer(
    name="review",
    add_completion=False,
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
