#!/bin/bash
# Cheap guard for the reaper cron: start the daemon when it is gone.
if ! pgrep -f "footnoote" >/dev/null 2>&1; then
  echo "daemon not running; starting it"
  ./start-daemon --home "$HOME"
fi
