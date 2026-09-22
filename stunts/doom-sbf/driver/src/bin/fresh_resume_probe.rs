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
use doom_sbf::{ex2, resource, resource_sbf, sim2};
use serde_json::{json, Value};

const CORRECTED_DECODER_COMMIT: &str = "53bd00225df56df8037fc95b884c542db633a09b";
const PINNED_PATCH_0014_SHA256: &str =
    "788a1a3f830a42e4d82c704db80fda6a281ac6b09dec22fcabd8b727e7d1e08e";
const PINNED_UPSTREAM_COMMIT: &str = "a77dfb96cb91780ca334d0d4cfd86957558007e0";
const ORACLE_IDENTITY_SCHEMA: &str = "DOOM-SIM/2-NATIVE-ORACLE-IDENTITY/4";
const SOURCE_MANIFEST_SCHEMA: &str = "DOOM-EX2-NATIVE-SOURCE-MANIFEST/3";
const FINAL_DESCRIPTOR_SCHEMA: &str = "DOOM-SIM/2-NATIVE-ACCEPTANCE-DESCRIPTOR/2";
const NATIVE_PROVENANCE_PROFILE: &str =
    "DOOM-NATIVE-PROVENANCE/sweep_native+fresh_resume_probe/ctx-engine-host-release/2";
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
const ORACLE_FILE_TABLE_DOMAIN: &[u8] = b"DOOM-SIM/2-NATIVE-ORACLE-FILE-TABLE/1\0";
const INPUT_FILE_TABLE_DOMAIN: &[u8] = b"DOOM-SIM/2-NATIVE-INPUT-FILE-TABLE/1\0";
const COMMAND_DOMAIN: &str = "DOOM-SIM/2-COMMANDS/1";
const COMMAND_WIDTH: usize = 4;

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

fn nonempty_metadata(metadata: &Value) -> bool {
    let Some(metadata) = metadata.as_object() else {
        return false;
    };
    if metadata.is_empty() {
        return false;
    }
    for tool in ["rustc", "cargo", "c_compiler", "linker", "archiver"] {
        let Some(tool) = metadata.get(tool).and_then(Value::as_object) else {
            return false;
        };
        for field in ["path", "version"] {
            if tool
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return false;
            }
        }
    }
    let Some(profile) = metadata.get("profile").and_then(Value::as_object) else {
        return false;
    };
    !profile.is_empty()
        && [
            "name",
            "opt_level",
            "debug",
            "debug_assertions",
            "overflow_checks",
            "lto",
            "codegen_units",
        ]
        .iter()
        .all(|field| {
            profile
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
        })
}

fn manifest_input<'a>(manifest: &'a Value, path: &str) -> Option<&'a Value> {
    manifest["inputs"]
        .as_array()?
        .iter()
        .find(|input| input["path"].as_str() == Some(path))
}

fn file_record(path: &str, bytes: &[u8]) -> Value {
    json!({"path": path, "bytes": bytes.len(), "sha256": sha(bytes)})
}

fn collect_files(root: &Path, base: &Path, out: &mut Vec<Value>) -> Result<(), Box<dyn Error>> {
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(base)?
            .to_string_lossy()
            .replace('\\', "/");
        let kind = fs::symlink_metadata(&path)?.file_type();
        if kind.is_symlink() {
            return Err(format!("final descriptor {relative} is a symlink").into());
        }
        if kind.is_dir() {
            collect_files(&path, base, out)?;
        } else if kind.is_file() {
            out.push(file_record(&relative, &fs::read(&path)?));
        }
    }
    out.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    Ok(())
}

fn table_root(records: &[Value], domain: &[u8]) -> Result<String, Box<dyn Error>> {
    let mut encoded = domain.to_vec();
    for record in records {
        let path = record["path"].as_str().ok_or("file table path")?;
        let bytes = record["bytes"].as_u64().ok_or("file table byte length")?;
        let digest = record["sha256"].as_str().ok_or("file table sha256")?;
        encoded.extend_from_slice(path.as_bytes());
        encoded.push(0);
        encoded.extend_from_slice(bytes.to_string().as_bytes());
        encoded.push(0);
        encoded.extend_from_slice(digest.as_bytes());
        encoded.push(b'\n');
    }
    Ok(sha(&encoded))
}

fn validate_file_table(
    root: &Path,
    expected: &Value,
    expected_root: &Value,
    label: &str,
    exact: bool,
    domain: &[u8],
) -> Result<(), Box<dyn Error>> {
    let expected_records = expected
        .as_array()
        .ok_or(format!("{label} file table is missing"))?;
    let mut actual = Vec::new();
    if exact {
        collect_files(root, root, &mut actual)?;
    } else {
        for record in expected_records {
            let relative = record["path"]
                .as_str()
                .ok_or(format!("{label} file path"))?;
            let path = root.join(relative);
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("{label} required file {relative}: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "final descriptor {label} required file {relative} is a symlink"
                )
                .into());
            }
            let bytes = fs::read(&path)
                .map_err(|error| format!("{label} required file {relative}: {error}"))?;
            actual.push(file_record(relative, &bytes));
        }
    }
    if &actual != expected_records {
        let mismatch = expected_records
            .iter()
            .map(|record| record["path"].as_str().unwrap_or("<invalid>"))
            .find(|path| {
                actual
                    .iter()
                    .find(|record| record["path"].as_str() == Some(*path))
                    != expected_records
                        .iter()
                        .find(|record| record["path"].as_str() == Some(*path))
            })
            .unwrap_or("<directory-shape>");
        return Err(format!("final descriptor {label} file pin mismatch: {mismatch}").into());
    }
    let root = table_root(&actual, domain)?;
    if expected_root.as_str() != Some(root.as_str()) {
        return Err(format!("final descriptor {label} file-table root mismatch").into());
    }
    Ok(())
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

fn validate_final_descriptor(
    descriptor_path: &Path,
    descriptor_sha256: &str,
    descriptor_bytes: usize,
    role: &str,
    executable: &Path,
    oracle_path: &Path,
    capture_path: &Path,
    resource_path: &Path,
    manifest: &Value,
    manifest_bytes: &[u8],
) -> Result<Value, Box<dyn Error>> {
    let bytes = fs::read(descriptor_path)?;
    if descriptor_sha256.len() != 64
        || !descriptor_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || bytes.len() != descriptor_bytes
        || sha(&bytes) != descriptor_sha256
    {
        return Err("final descriptor bytes/hash do not match the external pin".into());
    }
    let descriptor: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("final descriptor is not JSON: {error}"))?;
    if descriptor["schema"] != FINAL_DESCRIPTOR_SCHEMA
        || descriptor["version"] != 2
        || descriptor["stage"] != "acceptance"
        || descriptor["profile"] != NATIVE_PROVENANCE_PROFILE
        || descriptor["oracle"]["directory"] != oracle_path.canonicalize()?.display().to_string()
    {
        return Err("final descriptor stage/version/profile/oracle path mismatch".into());
    }
    validate_file_table(
        oracle_path,
        &descriptor["oracle"]["file_table"],
        &descriptor["oracle"]["file_table_root"],
        "oracle",
        true,
        ORACLE_FILE_TABLE_DOMAIN,
    )?;
    if descriptor["capture"]["directory"] != capture_path.canonicalize()?.display().to_string() {
        return Err("final descriptor capture directory mismatch".into());
    }
    validate_file_table(
        capture_path,
        &descriptor["capture"]["file_table"],
        &descriptor["capture"]["file_table_root"],
        "capture",
        false,
        INPUT_FILE_TABLE_DOMAIN,
    )?;
    // `resource_path` is already canonical (its parent was canonicalized in
    // `main`), so a missing resource file reaches the labeled file-table
    // refusal below instead of a bare I/O error here.
    if descriptor["resource"]["path"] != resource_path.display().to_string() {
        return Err("final descriptor resource path mismatch".into());
    }
    let resource_parent = resource_path
        .parent()
        .ok_or("resource path has no parent")?;
    validate_file_table(
        resource_parent,
        &descriptor["resource"]["file_table"],
        &descriptor["resource"]["file_table_root"],
        "resource",
        false,
        INPUT_FILE_TABLE_DOMAIN,
    )?;
    let binary = descriptor["binaries"][role]
        .as_object()
        .ok_or_else(|| format!("acceptance descriptor has no {role} binary"))?;
    let actual_binary = fs::read(executable)?;
    if binary["path"].as_str() != Some(executable.to_str().ok_or("binary path is not UTF-8")?)
        || binary["bytes"].as_u64() != Some(actual_binary.len() as u64)
        || binary["sha256"].as_str() != Some(sha(&actual_binary).as_str())
    {
        return Err(format!("{role} binary does not match the final descriptor").into());
    }
    let compile = descriptor["compile"]
        .as_object()
        .ok_or("acceptance descriptor compile section is missing")?;
    if compile["manifest_schema"] != manifest["schema"]
        || compile["target"] != manifest["target"]
        || compile["sbf"] != manifest["sbf"]
        || compile["source_profile"] != manifest["source_profile"]
        || compile["source_commit"] != manifest["compile_source_commit"]
        || compile["source_tree"] != manifest["compile_source_tree"]
        || compile["source_dirty"] != manifest["compile_source_dirty"]
        || compile["upstream_commit"] != manifest["upstream_commit"]
        || compile["input_root"] != manifest["input_root"]
        || compile["input_count"] != manifest["input_count"]
        || compile["inputs"] != manifest["inputs"]
        || compile["build_metadata"] != manifest["build_metadata"]
        || compile["compiler_flags"] != manifest["compiler_flags"]
        || compile["source_manifest"]["bytes"] != manifest_bytes.len()
        || compile["source_manifest"]["sha256"] != sha(manifest_bytes)
        || compile["allowed_cargo_features"] != manifest["allowed_cargo_features"]
        || compile["enabled_cargo_features"] != manifest["enabled_cargo_features"]
        || compile["input_scope"] != manifest["input_scope"]
        || compile["rust_module_closure"] != manifest["rust_module_closure"]
        || compile["c_translation_units"] != manifest["c_translation_units"]
        || compile["c_translation_units_scope"] != manifest["c_translation_units_scope"]
        || compile["c_headers"] != manifest["c_headers"]
        || compile["build_scripts_and_locks"] != manifest["build_scripts_and_locks"]
    {
        return Err("embedded build manifest differs from the final descriptor".into());
    }
    if manifest["schema"] != SOURCE_MANIFEST_SCHEMA
        || manifest["binary_targets"] != json!(["sweep_native", "fresh_resume_probe"])
        || manifest["source_profile"] != NATIVE_PROVENANCE_PROFILE
        || manifest["compile_source_dirty"] != false
        || manifest["input_count"].as_u64() != manifest["inputs"].as_array().map(|v| v.len() as u64)
        || !nonempty_metadata(&manifest["build_metadata"])
        || manifest["compiler_flags"]
            .as_array()
            .is_none_or(|flags| flags.is_empty())
    {
        return Err("embedded source manifest is incomplete, debug, or feature-expanded".into());
    }
    let patch = manifest_input(manifest, PATCH_0014_PATH).ok_or("manifest omits patch 0014")?;
    let z_zone = manifest_input(manifest, Z_ZONE_PATH).ok_or("manifest omits patched z_zone.c")?;
    if compile.get("patch_0014") != Some(patch) || compile.get("z_zone") != Some(z_zone) {
        return Err(
            "manifest patch/z_zone entries differ from independently built descriptor".into(),
        );
    }
    if patch["sha256"] != json!(PINNED_PATCH_0014_SHA256) || z_zone["bytes"].as_u64() == Some(0) {
        return Err("manifest patch/z_zone pin is invalid".into());
    }
    Ok(descriptor)
}

fn source_identity(repo: &Path, manifest: &Value) -> Result<Value, Box<dyn Error>> {
    let mut files = serde_json::Map::new();
    let mut matches = true;
    for input in manifest["inputs"]
        .as_array()
        .ok_or("manifest inputs are missing")?
    {
        let path = input["path"].as_str().ok_or("manifest input path")?;
        if !path.starts_with("repo/") {
            continue;
        }
        let relative = &path[5..];
        let current_path = repo.join(relative);
        let current = match fs::read(&current_path) {
            Ok(current) => current,
            Err(_) => {
                matches = false;
                files.insert(relative.to_owned(), json!({"missing": true}));
                continue;
            }
        };
        let current_sha = sha(&current);
        let expected_sha = input["sha256"].as_str().unwrap_or("");
        let file_matches = current.len() == input["bytes"].as_u64().unwrap_or(u64::MAX) as usize
            && current_sha == expected_sha;
        matches &= file_matches;
        files.insert(
            relative.to_owned(),
            json!({
                "bytes": current.len(),
                "sha256": current_sha,
                "manifest_sha256": expected_sha,
                "matches_compile_inputs": file_matches,
                "matches_corrected_commit": file_matches,
            }),
        );
    }
    Ok(json!({
        "corrected_decoder_commit": CORRECTED_DECODER_COMMIT,
        "compile_source_commit": manifest["compile_source_commit"],
        "compile_source_tree": manifest["compile_source_tree"],
        "compile_source_dirty": manifest["compile_source_dirty"],
        "files": files,
        "matches_compile_inputs": matches,
        "matches_corrected_commit": matches,
    }))
}

fn validate_oracle_identity(
    oracle_path: &Path,
    identity: &Value,
    descriptor: &Value,
    descriptor_sha256: &str,
) -> Result<Value, Box<dyn Error>> {
    if identity["schema"] != ORACLE_IDENTITY_SCHEMA {
        return Err("oracle identity schema is missing or unsupported".into());
    }
    let manifest_path = oracle_path.join("source-manifest.json");
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|error| format!("cannot read retained oracle source manifest: {error}"))?;
    let manifest_sha256 = sha(&manifest_bytes);
    if identity["source_manifest_sha256"].as_str() != Some(manifest_sha256.as_str())
        || manifest_sha256 != descriptor["compile"]["source_manifest"]["sha256"]
    {
        return Err("oracle source manifest hash does not match its identity".into());
    }
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("oracle source manifest is not JSON: {error}"))?;
    if manifest["schema"] != SOURCE_MANIFEST_SCHEMA
        || manifest["input_root"] != descriptor["compile"]["input_root"]
        || manifest["inputs"] != descriptor["compile"]["inputs"]
        || manifest["target"] != descriptor["compile"]["target"]
        || manifest["sbf"] != descriptor["compile"]["sbf"]
        || manifest["source_profile"] != descriptor["compile"]["source_profile"]
        || manifest["compile_source_commit"] != descriptor["compile"]["source_commit"]
        || manifest["compile_source_tree"] != descriptor["compile"]["source_tree"]
        || manifest["compile_source_dirty"] != descriptor["compile"]["source_dirty"]
        || manifest["upstream_commit"] != descriptor["compile"]["upstream_commit"]
        || manifest["build_metadata"] != descriptor["compile"]["build_metadata"]
        || manifest["compiler_flags"] != descriptor["compile"]["compiler_flags"]
        || manifest["allowed_cargo_features"] != descriptor["compile"]["allowed_cargo_features"]
        || manifest["enabled_cargo_features"] != descriptor["compile"]["enabled_cargo_features"]
        || manifest["input_scope"] != descriptor["compile"]["input_scope"]
        || manifest["rust_module_closure"] != descriptor["compile"]["rust_module_closure"]
        || manifest["c_translation_units"] != descriptor["compile"]["c_translation_units"]
        || manifest["c_translation_units_scope"]
            != descriptor["compile"]["c_translation_units_scope"]
        || manifest["c_headers"] != descriptor["compile"]["c_headers"]
        || manifest["build_scripts_and_locks"] != descriptor["compile"]["build_scripts_and_locks"]
        || !nonempty_metadata(&manifest["build_metadata"])
    {
        return Err(
            "oracle source manifest is not the independently pinned complete manifest".into(),
        );
    }
    let patch = manifest_input(&manifest, PATCH_0014_PATH)
        .ok_or("oracle source manifest omits patch 0014")?;
    let patch_sha256 = required_sha(patch, "oracle patch 0014")?;
    if patch_sha256 != PINNED_PATCH_0014_SHA256
        || descriptor["compile"].get("patch_0014") != Some(patch)
    {
        return Err("oracle patch 0014 is not the pinned 53bd0022 blob".into());
    }
    let z_zone = manifest_input(&manifest, Z_ZONE_PATH)
        .ok_or("oracle source manifest omits the patched z_zone.c")?;
    let z_zone_sha256 = required_sha(z_zone, "oracle z_zone.c")?;
    if z_zone["bytes"].as_u64().unwrap_or(0) == 0
        || descriptor["compile"].get("z_zone") != Some(z_zone)
    {
        return Err("oracle z_zone.c manifest entry is empty".into());
    }
    if identity["build_descriptor"] != descriptor["build_descriptor"]
        || identity["provenance"]["build_descriptor"] != descriptor["build_descriptor"]
        || identity["provenance"]["corrected_decoder_commit"].as_str()
            != Some(CORRECTED_DECODER_COMMIT)
        || identity["provenance"]["patch_0014_sha256"].as_str() != Some(PINNED_PATCH_0014_SHA256)
        || identity["provenance"]["z_zone_sha256"].as_str() != Some(z_zone_sha256.as_str())
        || identity["provenance"]["source_manifest_sha256"].as_str()
            != Some(manifest_sha256.as_str())
        || identity["provenance"]["pinned_inputs"]["patch_0014"]["sha256"].as_str()
            != Some(PINNED_PATCH_0014_SHA256)
        || identity["provenance"]["pinned_inputs"]["z_zone"]["sha256"].as_str()
            != Some(z_zone_sha256.as_str())
        || identity["provenance"]["compile_source_commit"] != descriptor["compile"]["source_commit"]
        || identity["provenance"]["compile_source_tree"] != descriptor["compile"]["source_tree"]
        || identity["provenance"]["compile_source_dirty"] != false
        || identity["provenance"]["build_metadata"] != descriptor["compile"]["build_metadata"]
        || identity["provenance"]["compiler_flags"] != descriptor["compile"]["compiler_flags"]
        || identity["provenance"]["manifest_schema"] != SOURCE_MANIFEST_SCHEMA
        || identity["provenance"]["upstream_commit"] != descriptor["compile"]["upstream_commit"]
        || identity["provenance"]["source_profile"] != descriptor["compile"]["source_profile"]
        || identity["provenance"]["descriptor_profile"] != descriptor["profile"]
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
        || binary["bytes"].as_u64() != Some(binary_bytes.len() as u64)
        || Value::Object(binary.clone()) != descriptor["binaries"]["sweep_native"]
        || identity["provenance"]["binary"] != Value::Object(binary.clone())
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
        "source_commit": descriptor["compile"]["source_commit"],
        "source_tree": descriptor["compile"]["source_tree"],
        "source_tree_clean": true,
        "final_descriptor_sha256": descriptor_sha256,
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
        return Err("usage: fresh_resume_probe <capture> <resource> <oracle> <receipt> --final-descriptor PATH --final-descriptor-sha256 HEX --final-descriptor-bytes N [--corrected-commit COMMIT] [--source-root ROOT]".into());
    };
    let mut corrected_commit = CORRECTED_DECODER_COMMIT.to_owned();
    let mut source_root: Option<PathBuf> = None;
    let mut final_descriptor: Option<PathBuf> = None;
    let mut final_descriptor_sha256: Option<String> = None;
    let mut final_descriptor_bytes: Option<usize> = None;
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
            "--final-descriptor" => {
                final_descriptor = Some(PathBuf::from(
                    args.next().ok_or("--final-descriptor needs a value")?,
                ));
            }
            "--final-descriptor-sha256" => {
                final_descriptor_sha256 = Some(
                    args.next()
                        .ok_or("--final-descriptor-sha256 needs a value")?
                        .clone(),
                );
            }
            "--final-descriptor-bytes" => {
                final_descriptor_bytes = Some(
                    args.next()
                        .ok_or("--final-descriptor-bytes needs a value")?
                        .parse()?,
                );
            }
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    if corrected_commit != CORRECTED_DECODER_COMMIT {
        return Err("probe only accepts the corrected 53bd0022 decoder identity".into());
    }
    let final_descriptor = final_descriptor
        .ok_or("--final-descriptor is required")?
        .canonicalize()?;
    let final_descriptor_sha256 =
        final_descriptor_sha256.ok_or("--final-descriptor-sha256 is required")?;
    let final_descriptor_bytes =
        final_descriptor_bytes.ok_or("--final-descriptor-bytes is required")?;

    let capture_path = PathBuf::from(capture).canonicalize()?;
    let resource_path = {
        let given = PathBuf::from(resource_path);
        let name = given.file_name().ok_or("resource path has no file name")?.to_owned();
        let parent = given.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        parent.canonicalize()?.join(name)
    };
    let oracle_path = PathBuf::from(oracle).canonicalize()?;
    let receipt_path = PathBuf::from(receipt);
    if receipt_path.exists() && fs::read_dir(&receipt_path)?.next().is_some() {
        return Err("receipt directory is not empty".into());
    }
    fs::create_dir_all(&receipt_path)?;
    let executable = env::current_exe()?.canonicalize()?;
    let manifest_bytes = ex2::BUILD_SOURCE_MANIFEST.as_bytes();
    let build_manifest: Value = serde_json::from_str(ex2::BUILD_SOURCE_MANIFEST)
        .map_err(|error| format!("embedded Doom build manifest is not JSON: {error}"))?;
    let final_descriptor_value = validate_final_descriptor(
        &final_descriptor,
        &final_descriptor_sha256,
        final_descriptor_bytes,
        "fresh_resume_probe",
        &executable,
        &oracle_path,
        &capture_path,
        &resource_path,
        &build_manifest,
        manifest_bytes,
    )?;

    let capture = Capture::load(&capture_path, &resource_path)?;
    let states = gunzip(&oracle_path.join("oracle-st1.bin.gz"))?;
    let mirrors = gunzip(&oracle_path.join("oracle-mp1.bin.gz"))?;
    let commands = fs::read(oracle_path.join("commands.bin"))?;
    let command_descriptor = &final_descriptor_value["commands"];
    if command_descriptor["path"] != "commands.bin"
        || command_descriptor["domain"] != COMMAND_DOMAIN
        || command_descriptor["record_width"] != COMMAND_WIDTH
        || command_descriptor["eof"] != "exact"
        || command_descriptor["bytes"] != commands.len()
        || command_descriptor["sha256"] != sha(&commands)
        || command_descriptor["count"] != commands.len() / COMMAND_WIDTH
        || commands.len() % COMMAND_WIDTH != 0
    {
        return Err("final descriptor command identity/width/domain/EOF mismatch".into());
    }
    let rows: Vec<Value> = fs::read_to_string(oracle_path.join("oracle-per-tic.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    if rows.len() != FORWARD_TICS
        || commands.len() != FORWARD_TICS * COMMAND_WIDTH
        || states.len() != rows.len() * resource_sbf::STATE_MAX_BYTES
        || mirrors.len() != rows.len() * resource_sbf::MIRROR_BYTES
    {
        return Err("retained oracle does not contain exactly 1,000 full vectors".into());
    }

    let oracle_identity: Value =
        serde_json::from_slice(&fs::read(oracle_path.join("identity.json"))?)?;
    let oracle_provenance = validate_oracle_identity(
        &oracle_path,
        &oracle_identity,
        &final_descriptor_value,
        &final_descriptor_sha256,
    )?;

    let repo = source_root
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .canonicalize()?;
    let source = source_identity(&repo, &build_manifest)?;
    if source["matches_compile_inputs"] != true {
        return Err(
            "working decoder source does not match the compile-time source manifest".into(),
        );
    }
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
