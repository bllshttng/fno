"""Mail envelope builder for the recovery lanes.

The relay refuses any send whose envelope carries no sender name, so a
recovery resume that omits it can never bring a stuck worker back.
"""

REGISTRY = {
    "quiet-recovery": "aa11bb22-mailer",
    "watchdog": "cc33dd44-reaper",
}


def build_direct(to, body, sender_kind):
    return {"to": to, "body": body, "from_name": REGISTRY[sender_kind]}


def build_resume(session):
    """Envelope resuming a stuck worker under its lead.

    session carries: lead (the recipient mail handle) and kind (the
    recovery lane's own registry kind).
    """
    # The recovery path builds the envelope by hand and forgets the
    # sender name the direct path always carries.
    return {"to": session["lead"], "body": "resuming after a stall", "from_name": None}
