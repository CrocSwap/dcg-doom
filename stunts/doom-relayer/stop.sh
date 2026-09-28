#!/usr/bin/env bash
# Stop a relayer session: SIGINT to the node server first so the pipeline can
# write its receipt, then escalate through SIGTERM to SIGKILL if a process stays
# alive. This intentionally targets only the two relayer command lines.
#
# Do not use `pkill -f doom_relayer_server.py`: this script's own command line
# could match it and kill the shell running the stop request.
set -u

node_pattern='stunts/doom-relayer/server.mjs'
python_pattern='scripts/doom_relayer_server.py --run-dir'

signal_matches() {
  local signal=$1 pattern=$2 pid
  while IFS= read -r pid; do
    [ -n "$pid" ] || continue
    kill -"$signal" "$pid" 2>/dev/null || true
  done < <(pgrep -f "$pattern" || true)
}

wait_for_pattern() {
  local pattern=$1 timeout=$2
  local elapsed
  for ((elapsed = 0; elapsed < timeout; elapsed += 1)); do
    pgrep -f "$pattern" >/dev/null || return 0
    sleep 1
  done
  ! pgrep -f "$pattern" >/dev/null
}

wait_for_relayer() {
  local timeout=$1
  wait_for_pattern "$node_pattern" "$timeout" || return 1
  wait_for_pattern "$python_pattern" "$timeout"
}

# Give Node up to 40 seconds to stop cleanly and let its Python child write the
# final receipt. The child and any remaining server get 10 seconds after TERM.
signal_matches INT "$node_pattern"
wait_for_pattern "$node_pattern" 40 || true
signal_matches TERM "$python_pattern"
signal_matches TERM "$node_pattern"
wait_for_relayer 10 || true

# A process that ignored both graceful signals must not be reported as stopped.
signal_matches KILL "$node_pattern"
signal_matches KILL "$python_pattern"
wait_for_relayer 3 || true

remaining_node=$(pgrep -f "$node_pattern" || true)
remaining_python=$(pgrep -f "$python_pattern" || true)
echo "remaining node: ${remaining_node:-0}"
echo "remaining python: ${remaining_python:-0}"
if [ -n "$remaining_node" ] || [ -n "$remaining_python" ]; then
  exit 1
fi
