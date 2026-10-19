"""The spawn phase inference: label the work from the verb table, never a
guess (x-70e1, x-007c)."""

from __future__ import annotations

from fno.agents.spawn_phase import infer_phase


def test_table_maps_every_shipped_work_verb_in_all_three_spellings():
    """AC1: every shipped work verb labels its phase in the bare (/verb),
    plugin (/fno:verb) and codex ($fno:verb) spellings."""
    cases = [
        ("/blueprint x-1", "blueprint"), ("/fno:blueprint x-1", "blueprint"),
        ("$fno:blueprint x-1", "blueprint"),
        ("/think q", "think"), ("$fno:think q", "think"),
        ("/fix", "execute"), ("/fno:fix t", "execute"), ("$fno:tdd x", "execute"),
        ("/execute p.md", "execute"), ("/fno:execute waves p.md", "execute"), ("/do x", "execute"),
        ("/fno:ship pr create", "ship"), ("/fno:ship pr check 12", "ship"), ("/ship pr", "ship"),
        ("/target", "execute"), ("/fno:target", "execute"), ("$fno:target", "execute"),
        ("/review", "review"), ("/fno:review", "review"), ("/code-review", "review"),
    ]
    for message, expected in cases:
        assert infer_phase(message) == expected, message


def test_prefixless_and_unmapped_payloads_stay_unlabeled():
    """AC2: a first token with no leading / or $, an unmapped verb, or an
    empty message answers ""."""
    assert infer_phase("review this") == ""
    assert infer_phase("blueprint x-1") == ""
    assert infer_phase("/fno:triage deep") == ""
    assert infer_phase("") == ""
    assert infer_phase(None) == ""
