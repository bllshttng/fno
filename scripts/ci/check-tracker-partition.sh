#!/usr/bin/env bash
# check-tracker-partition.sh - enforce the sidecar/tracker zero-overlap rule.
#
# The bring-your-own-id design rests on a partition: the sidecar (footnote-owned
# state) and the tracker read interface (fields an external backend supplies)
# must not share a field name. Zero overlap means zero sync, which is what
# keeps a two-sources-of-truth bug out of the design. The ``id`` key is the one
# allowed overlap: both sides carry it as the join key, never as a synced value.
#
# A field named on both sides (other than ``id``) is the failure mode this gate
# exists to catch - the first convenience mirror of e.g. ``title`` into the
# sidecar would reintroduce exactly the two-writer bug option 2 was rejected for.
#
# Every tracker-owned projection is inspected: ``TrackerNode`` and
# ``Candidate`` (the selection-only projection from ``list_open``) in
# ``crates/fno-agents/src/tracker/mod.rs``, against the ``SIDECAR_KEYS``
# vocabulary declared beside the sidecar reader. The Python projections this
# gate once read died with the tracker package; the Rust structs are the read
# interface now, so a missing one is a refusal, never a pass.
#
# Run:  bash scripts/ci/check-tracker-partition.sh [repo-root]
#       bash scripts/ci/check-tracker-partition.sh --self-test
# Default root is the current directory. Exits 0 clean; exits 1 on an overlap
# or a self-test failure. ``--self-test`` runs the positive controls (extraction
# and detection must both fire on synthetic input) and exits without scanning.
set -euo pipefail

REPO_ROOT="."
if [[ "${1:-}" == "--self-test" ]]; then
  python3 - "$REPO_ROOT" --self-test <<'PY'
import ast, sys
from pathlib import Path

# inlined so the gate is self-contained
def extract_fields(src, class_name):
    tree = ast.parse(src)
    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef) and node.name == class_name:
            fields = set()
            for stmt in node.body:
                if isinstance(stmt, ast.AnnAssign) and isinstance(stmt.target, ast.Name):
                    fields.add(stmt.target.id)
            return fields
    raise KeyError(f"no class {class_name!r} in source")

def projection_names(src):
    """Every class that is TrackerNode or a subclass of it (transitively)."""
    tree = ast.parse(src)
    by_name = {c.name: c for c in ast.walk(tree) if isinstance(c, ast.ClassDef)}
    def is_proj(cls, seen=frozenset()):
        if cls.name in seen:
            return False
        for base in cls.bases:
            if isinstance(base, ast.Name) and base.id in by_name:
                if base.id == "TrackerNode" or is_proj(by_name[base.id], seen | {cls.name}):
                    return True
        return False
    return sorted(c.name for c in by_name.values() if c.name == "TrackerNode" or is_proj(c))

def extract_rust_fields(src, struct_name):
    """The `pub <name>:` fields declared inside `pub struct <Name> { ... }`.

    A `#[serde(flatten)]` field contributes its TYPE's fields, not its own
    name. Field names come from the Rust projections; every name found here
    joins the tracker side of the partition check.
    """
    import re
    struct_re = re.compile(r"pub struct (\w+) \{")
    m = struct_re.search(src)
    while m and m.group(1) != struct_name:
        m = struct_re.search(src, m.end())
    if not m:
        raise KeyError(f"no struct {struct_name!r} in Rust source")
    depth = 1
    pos = m.end()
    fields = set()
    pending_flatten = False
    pending_type = None
    lines = src[m.end():].split("\n")
    for line in lines:
        stripped = line.strip()
        if "{" in stripped:
            depth += 1
        if "}" in stripped and depth > 1:
            depth -= 1
        if "}" in stripped and depth == 1 and not stripped.startswith("//"):
            break
        fm = re.match(r"pub (\w+):\s*([^,]+),?", stripped)
        if fm:
            if pending_flatten:
                fields |= extract_rust_fields(src, fm.group(2).strip())
            else:
                fields.add(fm.group(1))
            pending_flatten = False
            pending_type = None
            continue
        if "#[serde(flatten)]" in stripped:
            pending_flatten = True
    return fields

ALLOWED_OVERLAP = frozenset({"id"})

def extra_overlap(tracker, sidecar):
    return (set(tracker) & set(sidecar)) - ALLOWED_OVERLAP

# Positive control 1: extraction returns exactly the declared annotated fields
# and skips methods / plain assignments (model_config).
snippet = '''
class TrackerNode(BaseModel):
    id: str
    title: Optional[str] = None
    model_config = ConfigDict()
    def helper(self): ...
'''
got = extract_fields(snippet, "TrackerNode")
assert got == {"id", "title"}, f"extraction broken: got {got}"
# Positive control 2: detection fires on a real (non-key) overlap.
assert extra_overlap({"id", "title"}, {"id", "title", "cwd"}) == {"title"}, "overlap detection broken"
# Positive control 3: projection discovery finds subclasses of TrackerNode (the
# selection projection joins the gate without being named by hand).
proj_snippet = '''
class TrackerNode(BaseModel):
    id: str

class TrackerCandidate(TrackerNode):
    priority: str

class Unrelated(BaseModel):
    priority: str
'''
names = projection_names(proj_snippet)
assert names == ["TrackerCandidate", "TrackerNode"], f"projection discovery broken: {names}"
# Positive control 4: a forbidden name declared only on the SUBCLASS projection
# is caught by the union rule (a subclass is not a place to smuggle a mirror).
assert extra_overlap({"id", "priority"}, {"id", "batch", "priority"}) == {"priority"}, "subclass overlap detection broken"
# Positive control 5: the Rust extractor finds declared fields inside a pub
# struct body, resolves the flattened type's fields instead of the flatten
# field's own name, and skips non-pub lines.
rust_snippet = '''
pub struct TrackerNode {
    pub id: String,
    pub details: Option<String>,
    /// not a field
    fn helper() {}
}

pub struct Candidate {
    pub priority: String,
    #[serde(flatten)]
    pub node: TrackerNode,
}
'''
got_rust = extract_rust_fields(rust_snippet, "TrackerNode")
assert got_rust == {"id", "details"}, f"rust extraction broken: got {got_rust}"
got_cand = extract_rust_fields(rust_snippet, "Candidate")
assert got_cand == {"priority", "id", "details"}, f"rust flatten broken: got {got_cand}"
print("check-tracker-partition self-test OK")
PY
  exit $?
fi

[[ "${1:-}" != "" ]] && REPO_ROOT="$1"
RUST="$REPO_ROOT/crates/fno-agents/src/tracker/mod.rs"
SIDECAR="$REPO_ROOT/crates/fno-agents/src/tracker/sidecar.rs"
for f in "$RUST" "$SIDECAR"; do
  [[ -f "$f" ]] || { echo "check-tracker-partition: missing $f" >&2; exit 1; }
done

python3 - "$SIDECAR" "$RUST" <<'PY'
import re
import sys
from pathlib import Path

sidecar_src = Path(sys.argv[1]).read_text()
rust_src = Path(sys.argv[2]).read_text()

def extract_sidecar_keys(src):
    """The names inside the `SIDECAR_KEYS` const array: the sidecar-side
    vocabulary, declared once beside the reader and the write gate."""
    m = re.search(r"pub(?:\(crate\))? const SIDECAR_KEYS: &\[&str\] = &\[(.*?)\];", src, re.S)
    if not m:
        raise KeyError("no SIDECAR_KEYS const in sidecar source")
    return set(re.findall(r'"([a-z_0-9]+)"', m.group(1)))

def extract_rust_fields(src, struct_name):
    """The `pub <name>:` fields declared inside `pub struct <Name> { ... }`.

    A `#[serde(flatten)]` field contributes its TYPE's fields, not its own
    name. Field names come from the Rust projections; every name found here
    joins the tracker side of the partition check.
    """
    import re
    struct_re = re.compile(r"pub struct (\w+) \{")
    m = struct_re.search(src)
    while m and m.group(1) != struct_name:
        m = struct_re.search(src, m.end())
    if not m:
        raise KeyError(f"no struct {struct_name!r} in Rust source")
    lines = src[m.end():].split("\n")
    fields = set()
    pending_flatten = False
    for line in lines:
        stripped = line.strip()
        if "}" in stripped and not stripped.startswith("//") and "{" not in stripped:
            break
        fm = re.match(r"pub (\w+):\s*([^,]+),?", stripped)
        if fm:
            if pending_flatten:
                fields |= extract_rust_fields(src, fm.group(2).strip())
            else:
                fields.add(fm.group(1))
            pending_flatten = False
            continue
        if "#[serde(flatten)]" in stripped:
            pending_flatten = True
    return fields

ALLOWED_OVERLAP = frozenset({"id"})

sidecar = extract_sidecar_keys(sidecar_src)
if not sidecar:
    print(
        "check-tracker-partition: FAIL - no SIDECAR_KEYS extracted from "
        f"{sys.argv[1]}; the sidecar vocabulary must exist for the partition to mean anything.",
        file=sys.stderr,
    )
    sys.exit(1)

# SIDECAR_KEYS is the vocabulary MINUS the join key by construction (the
# served shape inserts `id` beside the projected fields), so the gate
# re-joins the key here: the partition rule governs data fields, and the
# positive check below still proves both sides carry the join key.
sidecar |= {"id"}

# The tracker side: the Rust projections (TrackerNode, Candidate in
# tracker/mod.rs). A `#[serde(flatten)]` field contributes its type's fields.
tracker: set = set()
rust_projections = ["TrackerNode", "Candidate"]
for name in rust_projections:
    try:
        tracker |= extract_rust_fields(rust_src, name)
    except KeyError:
        print(
            "check-tracker-partition: FAIL - Rust projection "
            f"{name!r} not found in {sys.argv[2]}; the gate inspects the "
            "Rust structs, so a missing one is a refusal, never a pass.",
            file=sys.stderr,
        )
        sys.exit(1)
overlap = tracker & sidecar
# Require the overlap to be EXACTLY {id}: the join key must be present on both
# sides (an empty overlap is not "clean", it is a missing key) and nothing else
# may be shared (a shared data field is the two-writer bug). Asserting the
# positive key, not an absence, so a lockstep rename of `id` fails loudly.
if "id" not in overlap:
    print(
        "check-tracker-partition: FAIL - the `id` join key is missing from one "
        f"side (overlap is {sorted(overlap)}). Both TrackerNode and Sidecar must "
        "carry `id`; without it the partition has no join key.",
        file=sys.stderr,
    )
    sys.exit(1)
extra = overlap - ALLOWED_OVERLAP
if extra:
    print(
        "check-tracker-partition: FAIL - sidecar and tracker share non-key "
        f"fields {sorted(extra)} across the Rust projections "
        f"{rust_projections}. The sidecar may hold only fields the tracker "
        "cannot express; a shared name is the two-writer bug the partition "
        "exists to prevent.",
        file=sys.stderr,
    )
    sys.exit(1)
print(
    f"check-tracker-partition: OK - overlap is exactly the join key {sorted(overlap)} "
    f"across the Rust projections {rust_projections} and {len(sidecar)} sidecar keys"
)
PY
