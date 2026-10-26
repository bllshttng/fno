# register goldens

Captured exit code, stdout and stderr of the Python `fno agents register`
command before its front was flipped to the refuse shape, one case per triple
(`<case>.exit`, `<case>.out`, `<case>.err`). `register_parity.rs` runs the
Rust command under the same fixture env and asserts these bytes.

Regenerate with `capture.py` — but only from a checkout whose Python leg
still carries the command (a tree where `cmd_register` is already the refuse
shape produces 127-refusal bytes, not the oracle). The pending-window case
was captured that way from the canonical tree after the flip; its script
lives nowhere on purpose: copy `capture.py`, point `REPO` at the canonical
checkout, and run one case.

Fixture contract (both sides must agree):

- per-root `FNO_AGENTS_HOME` / `FNO_HOME` / `FNO_STATE_DIR`, empty
  harness-store dirs, so no machine state leaks in;
- the `AMBIENT_IDENTITY_ENV` scrub plus `FNO_AGENT_SELF` /
  `FNO_AGENT_ROW_PENDING` / `FNO_BG` removal, so each case names exactly the
  identity it wants seen;
- seeded registries at `schema_version` 43 with `cwd` and `log_path` set (a
  lower version refuses the write from a source checkout);
- the `.path-migration-done` sentinel pre-created, so no golden carries the
  one-time Python bootstrap line;
- the real installed `fno-agents` answers the doors the Python leg shells
  (harness-roster, registry-commit, mail-hold); no shim.

The mixed-markers (ambiguous identity) case is deliberately absent: its
disposition is runtime-proven (process-tree attestation), so the golden
depends on the ancestry of whoever captures. The exit-4 message is covered
by the owned-identity ladder's own Rust tests.
