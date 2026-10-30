# Source this inside a test's fake `fno` that runs the Python CLI, before the
# exec. The real `fno` front door rewrites a nested `fno agents` spelling to
# the flat argv the Python CLI registers; a Python-backed fake has no front
# door, so it does the same rewrite here. Sourcing shares the caller's
# positional parameters, so `set --` rewrites the fake's own "$@". The org
# chart arm covers the lead-backed rows (shape, manifest-path) the hooks call.
case "${1:-} ${2:-} ${3:-}" in
  "agents admit guard") set -- agents spawn-guard "${@:4}" ;;
  "agents admit capabilities") set -- agents capabilities "${@:4}" ;;
  "agents org chart") set -- agents lead "${@:4}" ;;
  "agents org done"|"agents org escalate"|"agents org drain") set -- agents lead "${@:3}" ;;
  *)
    case "${1:-} ${2:-}" in
      "agents session"|"agents registry"|"agents internal"|"agents fleet"|"agents audit")
        set -- agents "${@:3}" ;;
    esac ;;
esac
# The lead commands have no Python leg: the front door execs these fno-agents
# argvs (crates/fno/src/lead_route.rs), so the fake does too.
case "${1:-} ${2:-} ${3:-}" in
  "agents lead history") exec fno-agents lead-history "${@:4}" ;;
  "agents lead verdict") exec fno-agents lead-history --verdict --user-command "${@:4}" ;;
  "agents lead faq") exec fno-agents lead-state --faq "${@:4}" ;;
  "agents lead shape") exec fno-agents lead-shape --user-command "${@:4}" ;;
  "agents lead manifest-path") exec fno-agents lead-state --manifest-path "${@:4}" ;;
  "agents lead init") exec fno-agents lead-state --init "${@:4}" ;;
  "agents lead checkin") exec fno-agents lead-checkin --user-command "${@:4}" ;;
  "agents lead cancel") exec fno-agents lead-state --cancel "${@:4}" ;;
  "agents lead term") exec fno-agents lead-state --term "${@:4}" ;;
  "agents lead done") exec fno-agents lead-state --done "${@:4}" ;;
  "agents lead escalate") exec fno-agents lead-state --escalate "${@:4}" ;;
  "agents lead drain") exec fno-agents lead-state --drain "${@:4}" ;;
  "agents lead ledger") exec fno-agents lead-rundown --ledger "${@:4}" ;;
esac
# Every other agents word has no Python leg either: the front door's route
# (crates/fno/src/agents_client_route.rs) execs the client, so the fake does too.
case "${1:-}" in
  agents)
    case "${2:-}" in
      claim) shift; set -- claim "${@:2}" ;;
      capabilities|target-family|register|name|whoami|registry-repair|heal-token|pane-identity|watch|truth|peek|team|role|liveness)
        exec fno-agents status "--$2" "${@:3}" ;;
      watchdog|sweep|stale-escalate|friction-escalate|distress-verdicts|gate-status|codex-session-for-pid|spawn-guard|discovered-json)
        exec fno-agents census "${@:2}" ;;
      top) exec fno-agents census --workers "${@:3}" ;;
      orphans) exec fno-agents census --orphans "${@:3}" ;;
      incident) exec fno-agents fleet-incident "${@:3}" ;;
      *) exec fno-agents "${@:2}" ;;
    esac ;;
esac
