#!/usr/bin/env bash
# Usage: scripts/stage_cap.sh <ledger.jsonl> <stage-name> <cap_seconds> <cmd...>
# docs/project-rules.md rule 3: an on-chain step may run at most 90 s unless the user agreed a variance. At the cap the whole
# process tree is killed (exit 124) — no exceptions. Every attempt (pass, fail, killed) is appended to the ledger.
ledger=$1; stage=$2; cap=$3; shift 3
if command -v gtimeout >/dev/null 2>&1; then timeout_bin=gtimeout; else timeout_bin=timeout; fi
start=$(python3 -c 'import time;print(time.time())')
"$timeout_bin" --kill-after=2 --signal=TERM "$cap" "$@"
rc=$?
end=$(python3 -c 'import time;print(time.time())')
wall=$(python3 -c "print(round($end-$start,2))")
verdict=pass; [ $rc -ne 0 ] && verdict=fail; [ $rc -eq 124 -o $rc -eq 137 ] && verdict=killed_at_cap
cmd_string="$*"
printf '{"stage":"%s","cap_s":%s,"wall_s":%s,"exit":%s,"verdict":"%s","started_unix":%s,"cmd":"%s"}\n' \
  "$stage" "$cap" "$wall" "$rc" "$verdict" "$start" "$cmd_string" >> "$ledger"
echo "stage_cap: $stage $verdict wall=${wall}s cap=${cap}s exit=$rc" >&2
exit $rc
