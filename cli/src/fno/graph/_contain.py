"""The containment release, shared by `fno backlog contain`.

The fold + refusal guards live in the native command (workflows.rs contain_write,
decompose.rs contain_into); only the release still has a wheel consumer
(strand.py).
"""

def release_contained(entries: list[dict], node: dict) -> None:
    """Un-contain ``node`` (must carry ``contained_in``), dropping the PR refs
    inherited from its owner so the owner's merge cannot close it."""
    from fno.graph._reconcile import node_pr_refs, repo_slug_from_url as slug  # local: cycle
    owner_id = node.pop("contained_in")
    owner = next((e for e in entries if isinstance(e, dict) and e.get("id") == owner_id), None)
    keys = {(n, (slug(u) or "").lower()) for n, u in node_pr_refs(owner)} if owner else set()
    own = (node.get("pr_number"), (slug(node.get("pr_url")) or "").lower())
    if isinstance(node.get("pr_number"), int) and own in keys:
        node["pr_number"] = node["pr_url"] = node["merge_status"] = None
    if keys:
        node["additional_prs"] = [
            ref for ref in (node.get("additional_prs") or []) if isinstance(ref, dict)
            and (ref.get("number"), (slug(ref.get("url")) or "").lower()) not in keys
        ]
    node["released_from"] = owner_id
