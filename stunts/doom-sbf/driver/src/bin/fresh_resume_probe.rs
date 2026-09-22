//! Fresh decoder resume probe for the corrected R1 source.
//!
//! This is an executable gate, not a metadata check.  It opens every retained
//! ST1 checkpoint in a new `NativeSimSession`; the native session enters the
//! real `DoomStateDecode` path in `doom_resource_sim_session_open`.  It then
//! executes one retained command and compares every output byte and the
//! derived SIM/2 root with the retained oracle.

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
const TRANSITIONS: usize = 999;
const REQUIRED_RANGE: (usize, usize) = (598, 757);
const DECODER_SOURCE_FILES: &[&str] = &[
    "stunts/doom-host/patches/0009-doom-state-import-preflight.patch",
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
    if rows.len() < TRANSITIONS + 1
        || commands.len() < (TRANSITIONS + 1) * 4
        || states.len() < rows.len() * resource_sbf::STATE_MAX_BYTES
        || mirrors.len() < rows.len() * resource_sbf::MIRROR_BYTES
    {
        return Err("retained oracle does not contain the required full vectors".into());
    }

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let source = source_identity(&repo, &corrected_commit)?;
    let executable = env::current_exe()?.canonicalize()?;
    let binary = fs::read(&executable)?;
    let mut mismatches = Vec::new();
    let mut open_failures = Vec::new();
    for k in 0..TRANSITIONS {
        let state_input = if k == 0 {
            capture.state.clone()
        } else {
            let length = rows[k - 1]["st1_len"].as_u64().ok_or("oracle st1_len")? as usize;
            let start = (k - 1) * resource_sbf::STATE_MAX_BYTES;
            // DoomStateDecode authenticates the encoded ST1 length and
            // rejects the fixed account tail as input.  Reopen the retained
            // encoded bytes while comparing the complete fixed output image.
            states[start..start + length].to_vec()
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
        let step_rc = session.step(&command);
        let mut state = vec![0u8; resource_sbf::STATE_MAX_BYTES];
        let mut mirror = vec![0u8; resource_sbf::MIRROR_BYTES];
        let published = session.publish(&mut state, &mut mirror);
        let state_start = k * resource_sbf::STATE_MAX_BYTES;
        let mirror_start = k * resource_sbf::MIRROR_BYTES;
        let wanted_state = &states[state_start..state_start + resource_sbf::STATE_MAX_BYTES];
        let wanted_mirror = &mirrors[mirror_start..mirror_start + resource_sbf::MIRROR_BYTES];
        let wanted_root = hex_bytes(rows[k]["state_root"].as_str().ok_or("oracle state_root")?)?;
        let actual_root = sim2::verified_state_root(&state, &mirror)
            .map(|(_, root)| root.to_vec())
            .unwrap_or_default();
        let state_equal = state == wanted_state;
        let mirror_equal = mirror == wanted_mirror;
        let root_equal = actual_root == wanted_root;
        if step_rc != 0 || published.is_err() || !state_equal || !mirror_equal || !root_equal {
            mismatches.push(json!({
                "cursor": k,
                "step_rc": step_rc,
                "publish_rc": published.err(),
                "state_equal_every_byte": state_equal,
                "mirror_equal_every_byte": mirror_equal,
                "root_equal": root_equal,
                "first_state_difference": first_difference(&state, wanted_state),
                "first_mirror_difference": first_difference(&mirror, wanted_mirror),
                "actual_root": hex(&actual_root),
                "wanted_root": rows[k]["state_root"],
            }));
        }
    }
    let result = json!({
        "schema": "basanos/doom-fresh-resume-probe/1",
        "status": if open_failures.is_empty() && mismatches.is_empty() { "measured" } else { "failed" },
        "decoder_entry_point": "DoomStateDecode via doom_resource_sim_session_open",
        "transitions_checked": TRANSITIONS,
        "transition_cursors": [0, TRANSITIONS - 1],
        "required_midrange": [REQUIRED_RANGE.0, REQUIRED_RANGE.1],
        "midrange_checked": (REQUIRED_RANGE.0..=REQUIRED_RANGE.1).all(|cursor| cursor < TRANSITIONS),
        "open_failures": open_failures,
        "mismatches": mismatches,
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
