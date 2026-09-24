#!/bin/zsh
# Usage: scripts/stage_cap.sh <ledger.jsonl> <stage-name> <cap_seconds> <cmd...>
# Project rule (AGENTS.md "On-chain step time cap"): an on-chain step may run at most 90 s unless the user granted a variance. At the cap the whole
# process tree is killed (exit 124) — no exceptions. Every attempt (pass, fail, killed) is appended to the ledger.
ledger=$1; stage=$2; cap=$3; shift 3
start=$(python3 -c 'import time;print(time.time())')
/opt/homebrew/bin/gtimeout --kill-after=2 --signal=TERM "$cap" "$@"
rc=$?
end=$(python3 -c 'import time;print(time.time())')
wall=$(python3 -c "print(round($end-$start,2))")
verdict=pass; [ $rc -ne 0 ] && verdict=fail; [ $rc -eq 124 -o $rc -eq 137 ] && verdict=killed_at_cap
printf '{"stage":"%s","cap_s":%s,"wall_s":%s,"exit":%s,"verdict":"%s","started_unix":%s,"cmd":"%s"}\n' \
  "$stage" "$cap" "$wall" "$rc" "$verdict" "$start" "${(j: :)@//\"/\\\"}" >> "$ledger"
echo "stage_cap: $stage $verdict wall=${wall}s cap=${cap}s exit=$rc" >&2
exit $rc
