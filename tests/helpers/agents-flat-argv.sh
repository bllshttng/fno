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
