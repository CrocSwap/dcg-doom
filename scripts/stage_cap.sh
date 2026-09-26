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
python3 - "$ledger" "$stage" "$cap" "$wall" "$rc" "$verdict" "$start" "$*" <<'PY'
import json
import sys

ledger_path, stage_name, cap_seconds, wall_seconds, exit_code, verdict, started_unix, command = sys.argv[1:]
row = {
    "stage": stage_name,
    "cap_s": int(cap_seconds),
    "wall_s": float(wall_seconds),
    "exit": int(exit_code),
    "verdict": verdict,
    "started_unix": float(started_unix),
    "cmd": command,
}
with open(ledger_path, "a", encoding="utf-8") as ledger_file:
    ledger_file.write(json.dumps(row, separators=(",", ":")) + "\n")
PY
echo "stage_cap: $stage $verdict wall=${wall}s cap=${cap}s exit=$rc" >&2
exit $rc
