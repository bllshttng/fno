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
  *)
    case "${1:-} ${2:-}" in
      "agents session"|"agents registry"|"agents internal"|"agents fleet"|"agents audit")
        set -- agents "${@:3}" ;;
    esac ;;
esac
# The lead reads have no Python leg: the front door execs these fno-agents
# argvs (crates/fno/src/lead_reads_route.rs), so the fake does too.
case "${1:-} ${2:-} ${3:-}" in
  "agents lead history") exec fno-agents lead-history "${@:4}" ;;
  "agents lead verdict") exec fno-agents lead-history --verdict --user-command "${@:4}" ;;
  "agents lead faq") exec fno-agents lead-state --faq "${@:4}" ;;
  "agents lead shape") exec fno-agents lead-shape --user-command "${@:4}" ;;
  "agents lead manifest-path") exec fno-agents lead-state --manifest-path "${@:4}" ;;
esac
