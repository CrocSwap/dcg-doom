//! Fresh decoder resume probe for the corrected R1 source.
//!
//! This is an executable gate, not a metadata check.  It first replays the
//! complete retained 1,000-tic trajectory in one resident `NativeSimSession`,
//! then opens every retained ST1 checkpoint in a new session; both paths enter
//! the real `DoomStateDecode` path in `doom_resource_sim_session_open`.  Each
//! output byte and the derived SIM/2 root is compared with the retained oracle.

#[path = "continuation_split_sbf.rs"]
#[allow(dead_code)]
mod base;
#[path = "../sweep_common.rs"]
#[allow(dead_code)]
mod common;

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use common::{hex, Capture};
use doom_sbf::{resource, resource_sbf, sim2};
use serde_json::{json, Value};

const CORRECTED_DECODER_COMMIT: &str = "53bd00225df56df8037fc95b884c542db633a09b";
const FORWARD_TICS: usize = 1000;
const TRANSITIONS: usize = 999;
const REQUIRED_RANGE: (usize, usize) = (598, 757);
// The retained cursor-598 state is the first state containing the platform
// that exposed the provenance mismatch.  These offsets are diagnostics only;
// the native decoder remains the authority for parsing and validation.
const DIAGNOSTIC_STATE_ROW: usize = 597;
const DIAGNOSTIC_PLAT_RECORD_OFFSET: usize = 59128;
const DIAGNOSTIC_PLAT_COUNT_OFFSET: usize = 59149;
const DS_PLAT: u8 = 5;
const DECODER_SOURCE_FILES: &[&str] = &[
    "stunts/doom-host/patches/0009-doom-state-import-preflight.patch",
    "stunts/doom-host/patches/0014-doom-zero-levspec-thinkers.patch",
    "stunts/doom-sbf/src/doom_resource_ri2.c",
    "stunts/doom-sbf/src/doom_sbf_resource_ri2.c",
    "stunts/doom-sbf/src/doom_sim2_guard.h",
    "stunts/doom-sbf/src/resource.rs",
    "stunts/doom-sbf/src/resource_sbf.rs",
    "stunts/doom-sbf/src/sim2.rs",
    "stunts/doom-sbf/driver/src/sweep_common.rs",
    "stunts/doom-sbf/driver/src/bin/continuation_split_sbf.rs",
];

fn sha(bytes: &[u8]) -> String {
    base::sha(bytes)
}

fn gunzip(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = Command::new("gzip").arg("-dc").arg(path).output()?;
    if !output.status.success() {
        return Err(format!("gzip failed for {}", path.display()).into());
    }
    Ok(output.stdout)
}

fn hex_bytes(value: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    if value.len() != 64 {
        return Err("root is not 32 bytes".into());
    }
    Ok((0..value.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&value[at..at + 2], 16))
        .collect::<Result<Vec<_>, _>>()?)
}

fn current_head(repo: &Path) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args([
            "-C",
            repo.to_str().ok_or("repo path is not UTF-8")?,
            "rev-parse",
            "HEAD",
        ])
        .output()?;
    if !output.status.success() {
        return Err("git rev-parse HEAD failed".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn source_identity(repo: &Path, expected_commit: &str) -> Result<Value, Box<dyn Error>> {
    let head = current_head(repo)?;
    let mut files = serde_json::Map::new();
    for relative in DECODER_SOURCE_FILES {
        let current_path = repo.join(relative);
        let current = fs::read(&current_path)?;
        let expected = Command::new("git")
            .args([
                "-C",
                repo.to_str().ok_or("repo path is not UTF-8")?,
                "show",
                &format!("{expected_commit}:{relative}"),
            ])
            .output()?;
        if !expected.status.success() {
            return Err(format!("missing source blob {expected_commit}:{relative}").into());
        }
        let expected = expected.stdout;
        files.insert(
            (*relative).to_owned(),
            json!({
                "sha256": sha(&current),
                "corrected_commit_sha256": sha(&expected),
                "matches_corrected_commit": current == expected,
            }),
        );
    }
    let matches = files
        .values()
        .all(|file| file["matches_corrected_commit"] == true);
    Ok(json!({
        "corrected_decoder_commit": expected_commit,
        "working_tree_head": head,
        "files": files,
        "matches_corrected_commit": matches,
    }))
}

fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter()
        .zip(right)
        .position(|(a, b)| a != b)
        .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len())))
}

fn state_row(states: &[u8], rows: &[Value], row: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let length = rows
        .get(row)
        .and_then(|value| value["st1_len"].as_u64())
        .ok_or("oracle st1_len")? as usize;
    let start = row
        .checked_mul(resource_sbf::STATE_MAX_BYTES)
        .ok_or("oracle state offset overflow")?;
    let end = start
        .checked_add(length)
        .ok_or("oracle state length overflow")?;
    if length > resource_sbf::STATE_MAX_BYTES || end > states.len() {
        return Err(format!("oracle state row {row} is out of bounds").into());
    }
    Ok(states[start..end].to_vec())
}

fn compare_step(
    session: &mut resource::NativeSimSession,
    command: &[u8; 4],
    wanted_state: &[u8],
    wanted_mirror: &[u8],
    wanted_root: &[u8],
    cursor: usize,
) -> Result<Value, Box<dyn Error>> {
    let step_rc = session.step(command);
    let mut state = vec![0u8; resource_sbf::STATE_MAX_BYTES];
    let mut mirror = vec![0u8; resource_sbf::MIRROR_BYTES];
    let published = session.publish(&mut state, &mut mirror);
    let actual_root = sim2::verified_state_root(&state, &mirror)
        .map(|(_, root)| root.to_vec())
        .unwrap_or_default();
    let state_equal = state == wanted_state;
    let mirror_equal = mirror == wanted_mirror;
    let root_equal = actual_root == wanted_root;
    let ok = step_rc == 0 && published.is_ok() && state_equal && mirror_equal && root_equal;
    Ok(json!({
        "cursor": cursor,
        "ok": ok,
        "step_rc": step_rc,
        "publish_rc": published.err(),
        "state_equal_every_byte": state_equal,
        "mirror_equal_every_byte": mirror_equal,
        "root_equal": root_equal,
        "first_state_difference": first_difference(&state, wanted_state),
        "first_mirror_difference": first_difference(&mirror, wanted_mirror),
        "actual_state_sha256": sha(&state),
        "wanted_state_sha256": sha(wanted_state),
        "actual_mirror_sha256": sha(&mirror),
        "wanted_mirror_sha256": sha(wanted_mirror),
        "actual_root": hex(&actual_root),
        "wanted_root": hex(wanted_root),
    }))
}

fn open_control(capture: &Capture, state: &[u8], label: &str, expect_open: bool) -> Value {
    let result =
        match resource::NativeSimSession::open(capture.wad_refs(), &capture.resource, state) {
            Ok(session) => {
                drop(session);
                json!({"opened": true, "rc": Value::Null})
            }
            Err(rc) => json!({"opened": false, "rc": rc}),
        };
    let opened = result["opened"] == true;
    json!({
        "label": label,
        "expected_open": expect_open,
        "opened": opened,
        "rc": result["rc"],
        "passed": opened == expect_open,
    })
}

fn malformed_controls(
    capture: &Capture,
    states: &[u8],
    rows: &[Value],
) -> Result<Value, Box<dyn Error>> {
    let mut state = state_row(states, rows, DIAGNOSTIC_STATE_ROW)?;
    if state.len() <= DIAGNOSTIC_PLAT_COUNT_OFFSET
        || state[DIAGNOSTIC_PLAT_RECORD_OFFSET] != DS_PLAT
    {
        return Err("diagnostic cursor-598 state no longer has the expected DS_PLAT layout".into());
    }
    let expected_count =
        state[DIAGNOSTIC_PLAT_COUNT_OFFSET..DIAGNOSTIC_PLAT_COUNT_OFFSET + 4].to_vec();

    let mut truncated = state.clone();
    truncated.pop();
    let truncation = open_control(capture, &truncated, "st1-truncate-one-byte", false);

    let mut unknown_kind = state.clone();
    unknown_kind[DIAGNOSTIC_PLAT_RECORD_OFFSET] = 0xff;
    let malformed_kind = open_control(capture, &unknown_kind, "st1-unknown-special-kind", false);

    state[DIAGNOSTIC_PLAT_COUNT_OFFSET] ^= 0x01;
    let field_mutation = open_control(capture, &state, "st1-ds-plat-count-mutation", true);
    Ok(json!({
        "source": "retained oracle row 597 (cursor 598), native NativeSimSession::open",
        "record": "DS_PLAT",
        "record_offset": DIAGNOSTIC_PLAT_RECORD_OFFSET,
        "count_offset": DIAGNOSTIC_PLAT_COUNT_OFFSET,
        "count_bytes_before": hex(&expected_count),
        "controls": [truncation, malformed_kind, field_mutation],
    }))
}

fn main() -> Result<(), Box<dyn Error>> {
    let argv: Vec<String> = env::args().collect();
    let [_, capture, resource_path, oracle, receipt, rest @ ..] = &argv[..] else {
        return Err("usage: fresh_resume_probe <capture> <resource> <oracle> <receipt> [--corrected-commit COMMIT]".into());
    };
    let mut corrected_commit = CORRECTED_DECODER_COMMIT.to_owned();
    let mut args = rest.iter();
    while let Some(flag) = args.next() {
        if flag != "--corrected-commit" {
            return Err(format!("unknown option {flag}").into());
        }
        corrected_commit = args
            .next()
            .ok_or("--corrected-commit needs a value")?
            .clone();
    }
    if corrected_commit != CORRECTED_DECODER_COMMIT {
        return Err("probe only accepts the corrected 53bd0022 decoder identity".into());
    }

    let capture_path = PathBuf::from(capture).canonicalize()?;
    let resource_path = PathBuf::from(resource_path).canonicalize()?;
    let oracle_path = PathBuf::from(oracle).canonicalize()?;
    let receipt_path = PathBuf::from(receipt);
    if receipt_path.exists() && fs::read_dir(&receipt_path)?.next().is_some() {
        return Err("receipt directory is not empty".into());
    }
    fs::create_dir_all(&receipt_path)?;

    let capture = Capture::load(&capture_path, &resource_path)?;
    let states = gunzip(&oracle_path.join("oracle-st1.bin.gz"))?;
    let mirrors = gunzip(&oracle_path.join("oracle-mp1.bin.gz"))?;
    let commands = fs::read(oracle_path.join("commands.bin"))?;
    let rows: Vec<Value> = fs::read_to_string(oracle_path.join("oracle-per-tic.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    if rows.len() != FORWARD_TICS
        || commands.len() != FORWARD_TICS * 4
        || states.len() != rows.len() * resource_sbf::STATE_MAX_BYTES
        || mirrors.len() != rows.len() * resource_sbf::MIRROR_BYTES
    {
        return Err("retained oracle does not contain exactly 1,000 full vectors".into());
    }

    let oracle_identity: Value =
        serde_json::from_slice(&fs::read(oracle_path.join("identity.json"))?)?;
    let oracle_patch_0014 = oracle_identity["sources"]
        .get("stunts/doom-host/patches/0014-doom-zero-levspec-thinkers.patch")
        .cloned();
    let oracle_binary_identity = oracle_identity["command_line"]
        .as_array()
        .and_then(|command_line| command_line.first())
        .and_then(Value::as_str)
        .and_then(|path| {
            let path = PathBuf::from(path).canonicalize().ok()?;
            let bytes = fs::read(&path).ok()?;
            Some(json!({"path": path, "sha256": sha(&bytes)}))
        })
        .unwrap_or_else(|| json!({"path": Value::Null, "sha256": Value::Null}));

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let source = source_identity(&repo, &corrected_commit)?;
    let executable = env::current_exe()?.canonicalize()?;
    let binary = fs::read(&executable)?;
    let mut forward_mismatches = Vec::new();
    let forward_open =
        resource::NativeSimSession::open(capture.wad_refs(), &capture.resource, &capture.state);
    let forward_open_failure = match forward_open {
        Ok(mut session) => {
            for k in 0..FORWARD_TICS {
                let command: [u8; 4] = commands[k * 4..k * 4 + 4].try_into()?;
                let wanted_state = &states
                    [k * resource_sbf::STATE_MAX_BYTES..(k + 1) * resource_sbf::STATE_MAX_BYTES];
                let wanted_mirror =
                    &mirrors[k * resource_sbf::MIRROR_BYTES..(k + 1) * resource_sbf::MIRROR_BYTES];
                let wanted_root =
                    hex_bytes(rows[k]["state_root"].as_str().ok_or("oracle state_root")?)?;
                let comparison = compare_step(
                    &mut session,
                    &command,
                    wanted_state,
                    wanted_mirror,
                    &wanted_root,
                    k,
                )?;
                if comparison["ok"] != true {
                    forward_mismatches.push(comparison);
                }
            }
            Value::Null
        }
        Err(rc) => json!({"cursor": 0, "rc": rc}),
    };

    let mut mismatches = Vec::new();
    let mut open_failures = Vec::new();
    for k in 0..TRANSITIONS {
        let state_input = if k == 0 {
            capture.state.clone()
        } else {
            // DoomStateDecode authenticates the encoded ST1 length and
            // rejects the fixed account tail as input.  Reopen the retained
            // encoded bytes while comparing the complete fixed output image.
            state_row(&states, &rows, k - 1)?
        };
        let mut session = match resource::NativeSimSession::open(
            capture.wad_refs(),
            &capture.resource,
            &state_input,
        ) {
            Ok(session) => session,
            Err(rc) => {
                open_failures.push(json!({"cursor": k, "rc": rc}));
                continue;
            }
        };
        let command: [u8; 4] = commands[k * 4..k * 4 + 4].try_into()?;
        let state_start = k * resource_sbf::STATE_MAX_BYTES;
        let mirror_start = k * resource_sbf::MIRROR_BYTES;
        let wanted_state = &states[state_start..state_start + resource_sbf::STATE_MAX_BYTES];
        let wanted_mirror = &mirrors[mirror_start..mirror_start + resource_sbf::MIRROR_BYTES];
        let wanted_root = hex_bytes(rows[k]["state_root"].as_str().ok_or("oracle state_root")?)?;
        let comparison = compare_step(
            &mut session,
            &command,
            wanted_state,
            wanted_mirror,
            &wanted_root,
            k,
        )?;
        if comparison["ok"] != true {
            mismatches.push(comparison);
        }
    }
    let controls = malformed_controls(&capture, &states, &rows)?;
    let control_passed = controls["controls"]
        .as_array()
        .is_some_and(|controls| controls.iter().all(|control| control["passed"] == true));
    let forward_passed = forward_open_failure.is_null() && forward_mismatches.is_empty();
    let fresh_passed = open_failures.is_empty() && mismatches.is_empty();
    let result = json!({
        "schema": "basanos/doom-fresh-resume-probe/2",
        "status": if forward_passed && fresh_passed && control_passed { "measured" } else { "failed" },
        "decoder_entry_point": "DoomStateDecode via doom_resource_sim_session_open",
        "forward_trajectory": {
            "tics_checked": FORWARD_TICS,
            "tic_cursors": [0, FORWARD_TICS - 1],
            "open_failure": forward_open_failure,
            "mismatches": forward_mismatches,
        },
        "fresh_resume": {
            "transitions_checked": TRANSITIONS,
            "transition_cursors": [0, TRANSITIONS - 1],
            "open_failures": open_failures,
            "mismatches": mismatches,
        },
        "known_transition_diagnosis": {
            "cursor": 597,
            "state_offset": DIAGNOSTIC_PLAT_COUNT_OFFSET,
            "st1_field": "DS_PLAT.count",
            "record_offset": DIAGNOSTIC_PLAT_RECORD_OFFSET,
            "source": "retained cursor-598 ST1 layout; diagnostic label only",
        },
        "required_midrange": [REQUIRED_RANGE.0, REQUIRED_RANGE.1],
        "midrange_checked": (REQUIRED_RANGE.0..=REQUIRED_RANGE.1).all(|cursor| cursor < TRANSITIONS),
        "native_controls": controls,
        "oracle_identity": {
            "recorded": oracle_identity,
            "patch_0014_recorded": oracle_patch_0014.is_some(),
            "patch_0014": oracle_patch_0014,
            "binary": oracle_binary_identity,
        },
        "source_identity": source,
        "binary_identity": {"path": executable, "sha256": sha(&binary)},
        "inputs": {
            "capture": capture_path,
            "resource": resource_path,
            "oracle": oracle_path,
            "oracle_state_bytes_sha256": sha(&states),
            "oracle_mirror_bytes_sha256": sha(&mirrors),
            "commands_sha256": sha(&commands),
        },
        "historical_scratch_receipt": "out/runs/doom-staged-sweep-review-20260922/probe-resume-preflightfix/resume.json",
    });
    fs::write(
        &receipt_path.join("resume.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if result["status"] != "measured"
        || result["source_identity"]["matches_corrected_commit"] != true
    {
        return Err("fresh resume probe failed".into());
    }
    Ok(())
}
