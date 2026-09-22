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
const PINNED_PATCH_0014_SHA256: &str =
    "788a1a3f830a42e4d82c704db80fda6a281ac6b09dec22fcabd8b727e7d1e08e";
const PINNED_UPSTREAM_COMMIT: &str = "a77dfb96cb91780ca334d0d4cfd86957558007e0";
const ORACLE_IDENTITY_SCHEMA: &str = "DOOM-SIM/2-NATIVE-ORACLE-IDENTITY/2";
const SOURCE_MANIFEST_SCHEMA: &str = "DOOM-EX2-NATIVE-SOURCE-MANIFEST/1";
const PATCH_0014_PATH: &str = "repo/stunts/doom-host/patches/0014-doom-zero-levspec-thinkers.patch";
const Z_ZONE_PATH: &str = "external/z_zone.c";
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

fn manifest_input<'a>(manifest: &'a Value, path: &str) -> Option<&'a Value> {
    manifest["inputs"]
        .as_array()?
        .iter()
        .find(|input| input["path"].as_str() == Some(path))
}

fn required_sha(value: &Value, label: &str) -> Result<String, Box<dyn Error>> {
    let sha = value["sha256"]
        .as_str()
        .ok_or_else(|| format!("{label} is missing sha256"))?;
    if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} has an invalid sha256").into());
    }
    Ok(sha.to_owned())
}

fn validate_oracle_identity(oracle_path: &Path, identity: &Value) -> Result<Value, Box<dyn Error>> {
    if identity["schema"] != ORACLE_IDENTITY_SCHEMA {
        return Err("oracle identity schema is missing or unsupported".into());
    }
    let manifest_path = oracle_path.join("source-manifest.json");
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|error| format!("cannot read retained oracle source manifest: {error}"))?;
    let manifest_sha256 = sha(&manifest_bytes);
    if identity["source_manifest_sha256"].as_str() != Some(manifest_sha256.as_str()) {
        return Err("oracle source manifest hash does not match its identity".into());
    }
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("oracle source manifest is not JSON: {error}"))?;
    if manifest["schema"] != SOURCE_MANIFEST_SCHEMA
        || manifest["upstream_commit"].as_str() != Some(PINNED_UPSTREAM_COMMIT)
        || !manifest["build_metadata"].is_object()
        || !manifest["compiler_flags"].is_array()
    {
        return Err("oracle source manifest is not the pinned complete engine manifest".into());
    }
    let patch = manifest_input(&manifest, PATCH_0014_PATH)
        .ok_or("oracle source manifest omits patch 0014")?;
    let patch_sha256 = required_sha(patch, "oracle patch 0014")?;
    if patch_sha256 != PINNED_PATCH_0014_SHA256 {
        return Err("oracle patch 0014 is not the pinned 53bd0022 blob".into());
    }
    let z_zone = manifest_input(&manifest, Z_ZONE_PATH)
        .ok_or("oracle source manifest omits the patched z_zone.c")?;
    let z_zone_sha256 = required_sha(z_zone, "oracle z_zone.c")?;
    if z_zone["bytes"].as_u64().unwrap_or(0) == 0 {
        return Err("oracle z_zone.c manifest entry is empty".into());
    }
    if identity["provenance"]["corrected_decoder_commit"].as_str() != Some(CORRECTED_DECODER_COMMIT)
        || identity["provenance"]["patch_0014_sha256"].as_str() != Some(PINNED_PATCH_0014_SHA256)
        || identity["provenance"]["z_zone_sha256"].as_str() != Some(z_zone_sha256.as_str())
        || identity["provenance"]["source_manifest_sha256"].as_str()
            != Some(manifest_sha256.as_str())
        || identity["provenance"]["pinned_inputs"]["patch_0014"]["sha256"].as_str()
            != Some(PINNED_PATCH_0014_SHA256)
        || identity["provenance"]["pinned_inputs"]["z_zone"]["sha256"].as_str()
            != Some(z_zone_sha256.as_str())
        || identity["provenance"]["source_commit"]
            .as_str()
            .is_none_or(|commit| {
                commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit())
            })
        || identity["provenance"]["source_tree_clean"] != true
    {
        return Err("oracle provenance commitments do not match its complete manifest".into());
    }

    let binary = identity["binary"]
        .as_object()
        .ok_or("oracle binary identity is missing")?;
    let binary_path = binary["path"]
        .as_str()
        .ok_or("oracle binary path is missing")?;
    let binary_path = PathBuf::from(binary_path).canonicalize().map_err(|error| {
        format!("retained oracle binary is missing or not canonicalizable: {error}")
    })?;
    let binary_bytes = fs::read(&binary_path)
        .map_err(|error| format!("cannot read retained oracle binary: {error}"))?;
    let binary_sha256 = sha(&binary_bytes);
    if binary["sha256"].as_str() != Some(binary_sha256.as_str())
        || identity["provenance"]["binary"]["path"] != binary["path"]
        || identity["provenance"]["binary"]["sha256"].as_str() != Some(binary_sha256.as_str())
    {
        return Err("retained oracle binary hash does not match its identity".into());
    }
    Ok(json!({
        "passed": true,
        "schema": ORACLE_IDENTITY_SCHEMA,
        "source_manifest": manifest_path,
        "source_manifest_sha256": manifest_sha256,
        "upstream_commit": PINNED_UPSTREAM_COMMIT,
        "corrected_decoder_commit": CORRECTED_DECODER_COMMIT,
        "patch_0014_sha256": patch_sha256,
        "z_zone_sha256": z_zone_sha256,
        "source_commit": identity["provenance"]["source_commit"],
        "source_tree_clean": true,
        "binary": {"path": binary_path, "sha256": binary_sha256},
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
    input_cursor: usize,
    output_cursor: usize,
    command_index: usize,
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
        "input_cursor": input_cursor,
        "output_cursor": output_cursor,
        "command_index": command_index,
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
        return Err("usage: fresh_resume_probe <capture> <resource> <oracle> <receipt> [--corrected-commit COMMIT] [--source-root ROOT]".into());
    };
    let mut corrected_commit = CORRECTED_DECODER_COMMIT.to_owned();
    let mut source_root: Option<PathBuf> = None;
    let mut args = rest.iter();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--corrected-commit" => {
                corrected_commit = args
                    .next()
                    .ok_or("--corrected-commit needs a value")?
                    .clone();
            }
            "--source-root" => {
                source_root = Some(PathBuf::from(
                    args.next().ok_or("--source-root needs a value")?,
                ));
            }
            _ => return Err(format!("unknown option {flag}").into()),
        }
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
    let oracle_provenance = validate_oracle_identity(&oracle_path, &oracle_identity)?;

    let repo = source_root
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .canonicalize()?;
    let source = source_identity(&repo, &corrected_commit)?;
    if source["matches_corrected_commit"] != true {
        return Err("working decoder source does not match the pinned corrected commit".into());
    }
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
                    k + 1,
                    k,
                )?;
                if comparison["ok"] != true {
                    forward_mismatches.push(comparison);
                }
            }
            Value::Null
        }
        Err(rc) => json!({"input_cursor": 0, "output_cursor": 1, "command_index": 0, "rc": rc}),
    };

    let mut initial_resume_mismatch = Value::Null;
    let initial_resume_open_failure = match resource::NativeSimSession::open(
        capture.wad_refs(),
        &capture.resource,
        &capture.state,
    ) {
        Ok(mut session) => {
            let command: [u8; 4] = commands[..4].try_into()?;
            let wanted_state = &states[..resource_sbf::STATE_MAX_BYTES];
            let wanted_mirror = &mirrors[..resource_sbf::MIRROR_BYTES];
            let wanted_root =
                hex_bytes(rows[0]["state_root"].as_str().ok_or("oracle state_root")?)?;
            initial_resume_mismatch = compare_step(
                &mut session,
                &command,
                wanted_state,
                wanted_mirror,
                &wanted_root,
                0,
                1,
                0,
            )?;
            Value::Null
        }
        Err(rc) => json!({"input_cursor": 0, "output_cursor": 1, "command_index": 0, "rc": rc}),
    };

    let mut mismatches = Vec::new();
    let mut open_failures = Vec::new();
    let mut transitions_executed = 0usize;
    for k in 0..TRANSITIONS {
        // Exactly the retained-row chain: input rows 0..998, command
        // indices 1..999, output rows 1..999.  The initial capture -> row 0
        // check above is separate and is not one of these 999 transitions.
        let state_input = state_row(&states, &rows, k)?;
        let mut session = match resource::NativeSimSession::open(
            capture.wad_refs(),
            &capture.resource,
            &state_input,
        ) {
            Ok(session) => session,
            Err(rc) => {
                open_failures.push(json!({
                    "input_cursor": k + 1,
                    "output_cursor": k + 2,
                    "command_index": k + 1,
                    "rc": rc
                }));
                continue;
            }
        };
        transitions_executed += 1;
        let command_index = k + 1;
        let output_row = k + 1;
        let command: [u8; 4] = commands[command_index * 4..command_index * 4 + 4].try_into()?;
        let state_start = output_row * resource_sbf::STATE_MAX_BYTES;
        let mirror_start = output_row * resource_sbf::MIRROR_BYTES;
        let wanted_state = &states[state_start..state_start + resource_sbf::STATE_MAX_BYTES];
        let wanted_mirror = &mirrors[mirror_start..mirror_start + resource_sbf::MIRROR_BYTES];
        let wanted_root = hex_bytes(
            rows[output_row]["state_root"]
                .as_str()
                .ok_or("oracle state_root")?,
        )?;
        let comparison = compare_step(
            &mut session,
            &command,
            wanted_state,
            wanted_mirror,
            &wanted_root,
            output_row,
            output_row + 1,
            command_index,
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
    let initial_resume_passed =
        initial_resume_open_failure.is_null() && initial_resume_mismatch["ok"] == true;
    let fresh_passed =
        transitions_executed == TRANSITIONS && open_failures.is_empty() && mismatches.is_empty();
    let last_transition_executed = transitions_executed == TRANSITIONS;
    let result = json!({
        "schema": "basanos/doom-fresh-resume-probe/2",
        "status": if oracle_provenance["passed"] == true && source["matches_corrected_commit"] == true && forward_passed && initial_resume_passed && fresh_passed && control_passed { "measured" } else { "failed" },
        "decoder_entry_point": "DoomStateDecode via doom_resource_sim_session_open",
        "forward_trajectory": {
            "tics_checked": FORWARD_TICS,
            "tic_cursors": [1, FORWARD_TICS],
            "open_failure": forward_open_failure,
            "mismatches": forward_mismatches,
        },
        "initial_capture_resume": {
            "transition_cursors": [0, 1],
            "command_index": 0,
            "open_failure": initial_resume_open_failure,
            "mismatch": initial_resume_mismatch,
            "passed": initial_resume_passed,
        },
        "fresh_resume": {
            "transitions_checked": TRANSITIONS,
            "input_rows": [0, TRANSITIONS - 1],
            "output_rows": [1, TRANSITIONS],
            "command_indices": [1, TRANSITIONS],
            "transition_cursors": [[1, 2], [TRANSITIONS, TRANSITIONS + 1]],
            "last_transition": {"input_cursor": TRANSITIONS, "output_cursor": TRANSITIONS + 1, "command_index": TRANSITIONS, "executed": last_transition_executed},
            "transitions_executed": transitions_executed,
            "last_transition_executed": last_transition_executed,
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
        "midrange_checked": (REQUIRED_RANGE.0..=REQUIRED_RANGE.1)
            .all(|cursor| (1..=TRANSITIONS).contains(&cursor)),
        "native_controls": controls,
        "oracle_identity": oracle_identity,
        "oracle_provenance": oracle_provenance,
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
        || result["oracle_provenance"]["passed"] != true
        || result["fresh_resume"]["last_transition_executed"] != true
    {
        return Err("fresh resume probe failed".into());
    }
    Ok(())
}
