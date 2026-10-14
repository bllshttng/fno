manifests:
- <TMP>/home/.fno/spaces/<TMPD_REPO>/worktrees/<NODE>/target-state.md
  ---
  fno_id: 00000000-0000-0000-0000-000000000000
  session_id: 00000000-0000-0000-0000-000000000000
  created_at: <TS>
  initial_head: <SHA>
  input: "<NODE>"
  plan_path: ""
  deliverables: 1
  cross_project: false
  harness: unknown
  harness_mode: standard
  harness_session_id: null
  harness_model:
  harness_effort:
  provider: claude
  provider_mode: standard
  provider_upgrade_reason: ""
  owner_pid: <PID>
  owner_started_at: <TS>
  owner_cwd: "<TMP>/home/.fno/worktrees/repo/<NODE>"
  claude_session_id: null
  codex_thread_id: null
  scratchpad_path: <TMP>/home/.fno/spaces/<TMPD_REPO>/worktrees/<NODE>/scratchpad
  target_size:
  dispatch_model:
  dispatch_provider:
  no_external: false
  no_docs: false
  no_ship: false
  batched: false
  no_verify: true
  no_goals: false
  no_browser: false
  no_clean: true
  no_how_to: false
  no_memory: false
  no_deferrals_capture: false
  has_ui: false
  attended: true
  advisory: false
  auto_merge_enabled: false
  auto_merge_approved: false
  auto_merge_source: default-off
  mission_id: null
  mission_wave: null
  mission_slug: null
  mission_from_msg_id: null
  ---
  Immutable session manifest. Initialized at <TS>.
  plan_path may be first-filled if empty; all other fields are read-only.
  target_claim_key: "node:<NODE>"
  target_claim_holder: "target-session:00000000-0000-0000-0000-000000000000"
  target_claim_ttl: "2h"
  graph_node_id: <NODE>
claims:
  exit=0
  {
  "key": "node:<NODE>",
  "state": "live",
  "basis": "live",
  "holder": "target-session:00000000-0000-0000-0000-000000000000",
  "schema_version": 1,
  "pid": <PID>,
  "pid_unavailable": false,
  "host": "<HOST>",
  "machine_id": "6219ED9C-AC92-542C-AE74-E95C55C08C1D",
  "acquired_at": <TS_MS>,
  "expires_at": <TS_MS>,
  "expired": false,
  "reason": "target dispatch",
  "harness": "unknown",
  "metadata": {
  "worktree": "<TMP>/home/.fno/worktrees/repo/<NODE>"
  }
  }
worktrees:
- <TMP>/home/.fno/worktrees/repo/<NODE>
