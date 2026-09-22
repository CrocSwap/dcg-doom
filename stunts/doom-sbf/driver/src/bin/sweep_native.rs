//! DOOM-SIM/2 gate (a): the transport cores on the host engine.
//!
//! Runs the program's own `sim2::deposit` / `sim2::sweep` / `sim2::init_layout`
//! / `sim2::snapshot_check` against the resident host session engine (same C
//! source as the SBF image, host build, this process), with deposits landed in
//! a deliberately scrambled order, two held-back late arrivals, and every
//! refusal control at the initial state, mid-run and at the end.  After every
//! sweep that ran, ST1/MP1, the state root and the input chain must equal the
//! sequential no-render oracle (every tic published) at that cursor.
//!
//! Account-level checks (signer flags, owners, PDA, the E2R/5 binding) are
//! not reachable here; gate (b) runs them on SBF.

#[path = "continuation_split_sbf.rs"]
#[allow(dead_code)]
mod base;
#[path = "../sweep_common.rs"]
#[allow(dead_code)]
mod common;

use std::{cell::Cell, env, error::Error, fs, io::Write, path::PathBuf, rc::Rc};

use common::{hex, Capture, CellBudget, NativeEngine, Rng};
use doom_sbf::{ex2, resource, resource_sbf, sim2};
use serde_json::Value;
use solana_program::program_error::ProgramError;
use solana_pubkey::Pubkey;

const CONTEXT: Pubkey = Pubkey::new_from_array([0x21; 32]);
const STATE: Pubkey = Pubkey::new_from_array([0x22; 32]);
const MIRROR: Pubkey = Pubkey::new_from_array([0x23; 32]);
const BUFFER: Pubkey = Pubkey::new_from_array([0x24; 32]);
const META: Pubkey = Pubkey::new_from_array([0x25; 32]);
const AUTHORITY: Pubkey = Pubkey::new_from_array([0x26; 32]);
const ATTACKER: Pubkey = Pubkey::new_from_array([0x27; 32]);
/// Modeled compute for the native sweep budget: a flat per-tic spend and a
/// starting budget that admits up to 12 tics under the program's reserves.
/// Only the stop logic is exercised here; real CU is measured on SBF in
/// gate (b).
const MODEL_TIC: u64 = 60_000;
const MODEL_LIMIT: u64 = sim2::TIC_RESERVE_CU + sim2::PUBLISH_RESERVE_CU + 11 * MODEL_TIC;
const HOLD: [u32; 2] = [37, 555];
const CORRECTED_DECODER_COMMIT: &str = "53bd00225df56df8037fc95b884c542db633a09b";
const PINNED_PATCH_0014_SHA256: &str =
    "788a1a3f830a42e4d82c704db80fda6a281ac6b09dec22fcabd8b727e7d1e08e";
const PINNED_UPSTREAM_COMMIT: &str = "a77dfb96cb91780ca334d0d4cfd86957558007e0";
const PATCH_0014_PATH: &str = "repo/stunts/doom-host/patches/0014-doom-zero-levspec-thinkers.patch";
const Z_ZONE_PATH: &str = "external/z_zone.c";
const SOURCE_MANIFEST_SCHEMA: &str = "DOOM-EX2-NATIVE-SOURCE-MANIFEST/2";
const ACCEPTANCE_DESCRIPTOR_SCHEMA: &str = "DOOM-SIM/2-NATIVE-ACCEPTANCE-DESCRIPTOR/1";
const NATIVE_PROVENANCE_PROFILE: &str =
    "DOOM-NATIVE-PROVENANCE/sweep_native+fresh_resume_probe/ctx-engine-host-release/1";

type Snapshot = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);

struct Harness {
    meta: Vec<u8>,
    buffer: Vec<u8>,
    state: Vec<u8>,
    mirror: Vec<u8>,
    engine: NativeEngine,
    budget: Rc<Cell<u64>>,
    checkpoint: Vec<u8>,
    events: fs::File,
    sweeps: Vec<sim2::SweepReport>,
}

impl Harness {
    fn snapshot(&self) -> Snapshot {
        (
            self.meta.clone(),
            self.buffer.clone(),
            self.state.clone(),
            self.mirror.clone(),
        )
    }

    fn deposit(
        &mut self,
        signer: &Pubkey,
        first: u32,
        commands: &[u8],
    ) -> Result<(), ProgramError> {
        let result = sim2::deposit(&mut self.buffer, signer, first, commands);
        writeln!(
            self.events,
            "{}",
            serde_json::json!({"event":"deposit","first":first,"count":commands.len()/4,"signer_is_authority":*signer==AUTHORITY,"result":format!("{result:?}")})
        )
        .unwrap();
        result
    }

    fn sweep_with(
        &mut self,
        signer: &Pubkey,
        max_tics: u32,
        budget: u64,
        checkpoint: Option<&[u8]>,
    ) -> Result<sim2::SweepReport, ProgramError> {
        self.budget.set(budget);
        let checkpoint = checkpoint
            .map(<[u8]>::to_vec)
            .unwrap_or_else(|| self.checkpoint.clone());
        let result = sim2::sweep(
            &mut self.meta,
            &self.buffer,
            &mut self.state,
            &mut self.mirror,
            &sim2::SweepKeys {
                context: &CONTEXT,
                state: &STATE,
                mirror: &MIRROR,
                buffer: &BUFFER,
                meta: &META,
                signer,
            },
            &checkpoint,
            max_tics,
            &mut self.engine,
            &CellBudget(self.budget.clone()),
        );
        let line = match &result {
            Ok(r) => {
                serde_json::json!({"event":"sweep","before":r.cursor_before,"after":r.cursor_after,"ran":r.ran,"stop":format!("{:?}",r.stop),"halt":r.halt,"max_tics":max_tics,"budget":budget})
            }
            Err(e) => {
                serde_json::json!({"event":"sweep","error":format!("{e:?}"),"max_tics":max_tics,"budget":budget})
            }
        };
        writeln!(self.events, "{line}").unwrap();
        if let Ok(report) = &result {
            self.sweeps.push(*report);
        }
        result
    }

    fn sweep(&mut self) -> Result<sim2::SweepReport, ProgramError> {
        self.sweep_with(&AUTHORITY, 0, MODEL_LIMIT, None)
    }

    fn cursor(&self) -> u32 {
        sim2::MetaView::parse(&self.meta).unwrap().cursor
    }
}

fn expect_code(
    result: Result<impl std::fmt::Debug, ProgramError>,
    code: u32,
    label: &str,
) -> Result<(), Box<dyn Error>> {
    match result {
        Err(ProgramError::Custom(got)) if got == code => Ok(()),
        other => Err(format!("{label}: expected Custom({code}), got {other:?}").into()),
    }
}

fn refusal(
    harness: &mut Harness,
    controls: &mut serde_json::Map<String, serde_json::Value>,
    label: &str,
    code: u32,
    run: impl FnOnce(&mut Harness) -> Result<(), ProgramError>,
) -> Result<(), Box<dyn Error>> {
    let before = harness.snapshot();
    let cursor = harness.cursor();
    let result = run(harness);
    expect_code(result, code, label)?;
    if harness.snapshot() != before {
        return Err(format!("{label}: refusal mutated durable bytes").into());
    }
    controls.insert(
        label.into(),
        serde_json::json!({"refused":true,"code":code,"cursor":cursor,"bytes_preserved":true}),
    );
    Ok(())
}

fn check_against_oracle(harness: &Harness, oracle: &common::Oracle) -> Result<(), Box<dyn Error>> {
    let view = sim2::MetaView::parse(&harness.meta).ok_or("meta parse")?;
    if view.cursor == 0 {
        return Ok(());
    }
    let tic = &oracle.tics[view.cursor as usize - 1];
    if harness.state != tic.state
        || harness.mirror != tic.mirror
        || view.state_root != tic.state_root
        || view.input_chain != tic.chain
        || view.state_len as usize != tic.state_len
    {
        return Err(format!(
            "divergence from the sequential oracle at cursor {}",
            view.cursor
        )
        .into());
    }
    Ok(())
}

fn manifest_input<'a>(manifest: &'a Value, path: &str) -> Option<&'a Value> {
    manifest["inputs"]
        .as_array()?
        .iter()
        .find(|input| input["path"].as_str() == Some(path))
}

fn manifest_sha(value: &Value, label: &str) -> Result<String, Box<dyn Error>> {
    let sha = value["sha256"]
        .as_str()
        .ok_or_else(|| format!("{label} is missing sha256"))?;
    if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} has an invalid sha256").into());
    }
    Ok(sha.to_owned())
}

fn nonempty_metadata(metadata: &Value) -> bool {
    let Some(metadata) = metadata.as_object() else {
        return false;
    };
    if metadata.is_empty() {
        return false;
    }
    for tool in ["rustc", "cargo", "c_compiler", "archiver"] {
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

fn descriptor_string(descriptor: &Value, path: &str) -> Result<String, Box<dyn Error>> {
    descriptor[path]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("acceptance descriptor field {path} is missing").into())
}

fn validate_acceptance_descriptor(
    descriptor_path: &std::path::Path,
    descriptor_sha256: &str,
    descriptor_bytes: usize,
    role: &str,
    executable: &std::path::Path,
    oracle_path: &std::path::Path,
    manifest: &Value,
    manifest_bytes: &[u8],
) -> Result<Value, Box<dyn Error>> {
    if descriptor_sha256.len() != 64
        || !descriptor_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("acceptance descriptor SHA-256 is not hexadecimal".into());
    }
    let bytes = fs::read(descriptor_path)?;
    if bytes.len() != descriptor_bytes || base::sha(&bytes) != descriptor_sha256 {
        return Err("acceptance descriptor bytes/hash do not match the external pin".into());
    }
    let descriptor: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("acceptance descriptor is not JSON: {error}"))?;
    if descriptor["schema"] != ACCEPTANCE_DESCRIPTOR_SCHEMA
        || descriptor["profile"] != NATIVE_PROVENANCE_PROFILE
        || descriptor_string(&descriptor, "oracle_directory")?
            != oracle_path.canonicalize()?.display().to_string()
    {
        return Err("acceptance descriptor schema/profile/oracle path mismatch".into());
    }
    let binary = descriptor["binaries"][role]
        .as_object()
        .ok_or_else(|| format!("acceptance descriptor has no {role} binary"))?;
    let actual_binary = fs::read(executable)?;
    if binary["path"].as_str() != Some(executable.to_str().ok_or("binary path is not UTF-8")?)
        || binary["bytes"].as_u64() != Some(actual_binary.len() as u64)
        || binary["sha256"].as_str() != Some(base::sha(&actual_binary).as_str())
    {
        return Err(
            format!("{role} binary does not match the external acceptance descriptor").into(),
        );
    }
    let compile = descriptor["compile"]
        .as_object()
        .ok_or("acceptance descriptor compile section is missing")?;
    if compile["target"] != manifest["target"]
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
        || compile["source_manifest_sha256"] != base::sha(manifest_bytes)
    {
        return Err(
            "embedded build manifest differs from the external acceptance descriptor".into(),
        );
    }
    if manifest["schema"] != SOURCE_MANIFEST_SCHEMA
        || manifest["binary_targets"] != serde_json::json!(["sweep_native", "fresh_resume_probe"])
        || manifest["source_profile"] != NATIVE_PROVENANCE_PROFILE
        || manifest["compile_source_dirty"] != false
        || manifest["input_count"].as_u64() != manifest["inputs"].as_array().map(|v| v.len() as u64)
        || !nonempty_metadata(&manifest["build_metadata"])
        || manifest["compiler_flags"]
            .as_array()
            .is_none_or(|flags| flags.is_empty())
    {
        return Err(
            "embedded source manifest is incomplete or has empty toolchain metadata".into(),
        );
    }
    let patch = manifest_input(manifest, PATCH_0014_PATH).ok_or("manifest omits patch 0014")?;
    let z_zone = manifest_input(manifest, Z_ZONE_PATH).ok_or("manifest omits patched z_zone.c")?;
    if compile.get("patch_0014") != Some(patch) || compile.get("z_zone") != Some(z_zone) {
        return Err(
            "manifest patch/z_zone entries differ from independently built descriptor".into(),
        );
    }
    if patch["sha256"] != PINNED_PATCH_0014_SHA256 || z_zone["bytes"].as_u64() == Some(0) {
        return Err("manifest patch/z_zone pin is invalid".into());
    }
    Ok(descriptor)
}

fn build_provenance(
    executable: &std::path::Path,
    descriptor_path: &std::path::Path,
    descriptor_sha256: &str,
    descriptor_bytes: usize,
    receipt: &std::path::Path,
) -> Result<Value, Box<dyn Error>> {
    let manifest_bytes = ex2::BUILD_SOURCE_MANIFEST.as_bytes();
    let manifest: Value = serde_json::from_str(ex2::BUILD_SOURCE_MANIFEST)
        .map_err(|error| format!("embedded Doom build manifest is not JSON: {error}"))?;
    let descriptor = validate_acceptance_descriptor(
        descriptor_path,
        descriptor_sha256,
        descriptor_bytes,
        "sweep_native",
        executable,
        receipt,
        &manifest,
        manifest_bytes,
    )?;
    let patch_sha256 = manifest_sha(
        manifest_input(&manifest, PATCH_0014_PATH).unwrap(),
        "patch 0014",
    )?;
    let z_zone_sha256 = manifest_sha(manifest_input(&manifest, Z_ZONE_PATH).unwrap(), "z_zone.c")?;
    let binary = fs::read(executable)?;
    Ok(serde_json::json!({
        "schema": "DOOM-SIM/2-NATIVE-ORACLE-IDENTITY/3",
        "corrected_decoder_commit": CORRECTED_DECODER_COMMIT,
        "source_manifest_sha256": base::sha(manifest_bytes),
        "patch_0014_sha256": patch_sha256,
        "z_zone_sha256": z_zone_sha256,
        "pinned_inputs": {
            "patch_0014": {"path": PATCH_0014_PATH, "sha256": patch_sha256},
            "z_zone": {"path": Z_ZONE_PATH, "sha256": z_zone_sha256},
        },
        "binary": {"path": executable, "sha256": base::sha(&binary), "bytes": binary.len()},
        "manifest_schema": manifest["schema"],
        "upstream_commit": PINNED_UPSTREAM_COMMIT,
        "compile_source_commit": manifest["compile_source_commit"],
        "compile_source_tree": manifest["compile_source_tree"],
        "compile_source_dirty": manifest["compile_source_dirty"],
        "source_profile": manifest["source_profile"],
        "build_metadata": manifest["build_metadata"],
        "compiler_flags": manifest["compiler_flags"],
        "acceptance_descriptor": {
            "path": descriptor_path,
            "sha256": descriptor_sha256,
            "bytes": descriptor_bytes,
        },
        "descriptor_profile": descriptor["profile"],
    }))
}

fn main() -> Result<(), Box<dyn Error>> {
    let argv: Vec<String> = env::args().collect();
    let mut args = argv.iter().skip(1);
    let capture = PathBuf::from(args.next().ok_or("missing capture")?).canonicalize()?;
    let resource_path = PathBuf::from(args.next().ok_or("missing RI2 resource")?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let mut tics = 1000usize;
    let mut seed = 0x5eed_d00d_2026_0922u64;
    let mut checkpoint_path: Option<PathBuf> = None;
    let mut sim1_receipt: Option<PathBuf> = None;
    let mut acceptance_descriptor: Option<PathBuf> = None;
    let mut acceptance_descriptor_sha256: Option<String> = None;
    let mut acceptance_descriptor_bytes: Option<usize> = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("flag without value")?;
        match flag.as_str() {
            "--tics" => tics = value.parse()?,
            "--seed" => seed = u64::from_str_radix(value.trim_start_matches("0x"), 16)?,
            "--checkpoint" => checkpoint_path = Some(PathBuf::from(value).canonicalize()?),
            "--sim1-receipt" => sim1_receipt = Some(PathBuf::from(value).canonicalize()?),
            "--acceptance-descriptor" => {
                acceptance_descriptor = Some(PathBuf::from(value).canonicalize()?)
            }
            "--acceptance-descriptor-sha256" => acceptance_descriptor_sha256 = Some(value),
            "--acceptance-descriptor-bytes" => acceptance_descriptor_bytes = Some(value.parse()?),
            _ => return Err(format!("unknown flag {flag}").into()),
        }
    }
    let acceptance_descriptor =
        acceptance_descriptor.ok_or("--acceptance-descriptor is required")?;
    let acceptance_descriptor_sha256 =
        acceptance_descriptor_sha256.ok_or("--acceptance-descriptor-sha256 is required")?;
    let acceptance_descriptor_bytes =
        acceptance_descriptor_bytes.ok_or("--acceptance-descriptor-bytes is required")?;
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err("receipt directory is not empty".into());
    }
    fs::create_dir_all(&receipt)?;
    let executable = env::current_exe()?.canonicalize()?;
    let provenance = build_provenance(
        &executable,
        &acceptance_descriptor,
        &acceptance_descriptor_sha256,
        acceptance_descriptor_bytes,
        &receipt,
    )?;
    fs::write(
        receipt.join("source-manifest.json"),
        ex2::BUILD_SOURCE_MANIFEST,
    )?;
    let capture_data = Capture::load(&capture, &resource_path)?;
    let commands = capture_data.demo_commands(tics)?;
    let first_tic = base::u32_at(&capture_data.state, 20);

    // Sequential oracle, retained in full.
    let oracle = common::sequential_oracle(&capture_data, &commands, &CONTEXT)?;
    common::retain_oracle(&receipt, &oracle)?;
    let halts = oracle
        .tics
        .iter()
        .filter(|t| t.probe.gamestate != 0 || t.probe.gameaction != 0 || t.probe.playerstate != 0)
        .count();

    // Tie the new session oracle to the M1201 oracle function (same C
    // source, same process) and, if given, to the retained SBF SIM/1 output.
    let legacy_count = tics.min(100);
    let legacy = resource::import_initialized_no_render_sequence(
        capture_data.wad_refs(),
        &capture_data.resource,
        &capture_data.state,
        &commands[..legacy_count * 4],
    )
    .map_err(|rc| format!("M1201 sequence oracle rc={rc}"))?;
    for index in 0..legacy_count {
        let tic = &oracle.tics[index];
        if legacy.states[index][..] != tic.state[..tic.state_len]
            || tic.state[tic.state_len..].iter().any(|&b| b != 0)
            || legacy.mirrors[index] != tic.mirror
        {
            return Err(format!(
                "session oracle differs from the M1201 sequence oracle at tic {index}"
            )
            .into());
        }
    }
    let sim1_equal = match &sim1_receipt {
        Some(dir) if legacy_count == 100 => {
            let st1 = base::read(&dir.join("output-state.st1"))?;
            let mp1 = base::read(&dir.join("output-mirror.mp1"))?;
            if st1 != oracle.tics[99].state || mp1 != oracle.tics[99].mirror {
                return Err(
                    "session oracle tic 99 differs from the retained SBF SIM/1 output".into(),
                );
            }
            serde_json::json!({"path": dir.display().to_string(), "equal_at_tic_99": true})
        }
        _ => serde_json::Value::Null,
    };

    // Transport run.
    let (checkpoint, checkpoint_source) = match &checkpoint_path {
        Some(path) => (base::read(path)?, format!("captured:{}", path.display())),
        None => {
            // crafted_fixture: a published-shape CT5 checkpoint.  The sweep
            // only checks its published header fields and its hash; the real
            // E2R/5 publication binding is exercised on SBF in gate (b).
            let mut ckpt = vec![0u8; 128];
            ckpt[..8].copy_from_slice(b"DOOM-CT5");
            ckpt[8..12].copy_from_slice(&5u32.to_le_bytes());
            ckpt[12..16].copy_from_slice(&8u32.to_le_bytes());
            ckpt[16..20].copy_from_slice(&8u32.to_le_bytes());
            ckpt[88..92].copy_from_slice(
                &(doom_sbf::continuation_split_sbf::CONTEXT_CHECKPOINT_OFFSET as u32).to_le_bytes(),
            );
            (ckpt, "crafted_fixture:published-shape".to_owned())
        }
    };
    let budget = Rc::new(Cell::new(0));
    let session = resource::NativeSimSession::open(
        capture_data.wad_refs(),
        &capture_data.resource,
        &capture_data.state,
    )
    .map_err(|rc| format!("sweep session open rc={rc}"))?;
    let mut harness = Harness {
        meta: vec![0; sim2::META_BYTES],
        buffer: vec![0; sim2::BUFFER_BYTES],
        state: capture_data.state_account(),
        mirror: capture_data.mirror.clone(),
        engine: NativeEngine {
            session,
            budget: budget.clone(),
            tic_cost: MODEL_TIC,
            steps: 0,
            publishes: 0,
        },
        budget,
        checkpoint: checkpoint.clone(),
        events: fs::File::create(receipt.join("events.jsonl"))?,
        sweeps: Vec::new(),
    };
    sim2::init_layout(
        &mut harness.meta,
        &mut harness.buffer,
        &sim2::InitParams {
            context: CONTEXT,
            state: STATE,
            mirror: MIRROR,
            buffer: BUFFER,
            meta: META,
            authority: AUTHORITY,
            bump: 255,
            first_tic,
            state_len: capture_data.state.len() as u32,
            state_root: oracle.initial_root,
            checkpoint_hash: sim2::checkpoint_hash(&CONTEXT, &checkpoint),
        },
    )
    .map_err(|e| format!("init_layout {e:?}"))?;
    let command =
        |seq: u32| -> Vec<u8> { commands[seq as usize * 4..seq as usize * 4 + 4].to_vec() };
    let mut controls = serde_json::Map::new();

    // --- controls at the initial state -------------------------------------
    let before = harness.snapshot();
    let noop = harness
        .sweep()
        .map_err(|e| format!("initial no-op {e:?}"))?;
    if noop.stop != sim2::Stop::NoneStaged || harness.snapshot() != before {
        return Err("initial sweep-with-nothing was not a no-op".into());
    }
    controls.insert(
        "sweep_with_nothing_initial".into(),
        serde_json::json!({"ok":true,"stop":"NoneStaged","bytes_preserved":true}),
    );
    refusal(
        &mut harness,
        &mut controls,
        "wrong_authority_sweep_initial",
        sim2::REFUSE_AUTHORITY,
        |h| h.sweep_with(&ATTACKER, 0, MODEL_LIMIT, None).map(|_| ()),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "wrong_authority_deposit_initial",
        sim2::REFUSE_AUTHORITY,
        |h| h.deposit(&ATTACKER, 0, &command(0)),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "malformed_deposit_empty",
        sim2::REFUSE_MALFORMED,
        |h| h.deposit(&AUTHORITY, 0, &[]),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "malformed_deposit_oversize",
        sim2::REFUSE_MALFORMED,
        |h| h.deposit(&AUTHORITY, 0, &vec![0; 4 * (sim2::MAX_DEPOSIT_SLOTS + 1)]),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "slot_beyond_capacity",
        sim2::REFUSE_SLOT_RANGE,
        |h| h.deposit(&AUTHORITY, sim2::BUFFER_CAPACITY as u32, &command(0)),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "run_crossing_capacity",
        sim2::REFUSE_SLOT_RANGE,
        |h| h.deposit(&AUTHORITY, sim2::BUFFER_CAPACITY as u32 - 1, &[0; 8]),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "special_button_savegame",
        sim2::REFUSE_COMMAND,
        |h| h.deposit(&AUTHORITY, 0, &[0, 0, 0, 0x82]),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "special_button_pause",
        sim2::REFUSE_COMMAND,
        |h| h.deposit(&AUTHORITY, 0, &[0, 0, 0, 0x81]),
    )?;

    // --- scrambled deposits interleaved with sweeps --------------------------
    let mut rng = Rng(seed);
    let runs = common::scrambled_runs(&mut rng, tics as u32, 4, &HOLD);
    let mut budget_checked = false;
    let mut until_sweep = 1 + rng.below(12);
    let mut gap_noops = 0usize;
    for (first, len) in &runs {
        harness
            .deposit(
                &AUTHORITY,
                *first,
                &commands[*first as usize * 4..(*first + *len) as usize * 4],
            )
            .map_err(|e| format!("deposit {first}+{len}: {e:?}"))?;
        until_sweep -= 1;
        if until_sweep > 0 {
            continue;
        }
        until_sweep = 1 + rng.below(12);
        let cursor = harness.cursor();
        let staged =
            harness.buffer[sim2::BUFFER_HEADER_BYTES + cursor as usize * sim2::SLOT_BYTES + 4] != 0;
        if staged && !budget_checked {
            // Budget stops: nothing affordable, then exactly one tic.
            let before = harness.snapshot();
            let none = harness
                .sweep_with(&AUTHORITY, 0, 0, None)
                .map_err(|e| format!("{e:?}"))?;
            if none.stop != sim2::Stop::Budget || none.ran != 0 || harness.snapshot() != before {
                return Err("zero-budget sweep was not a no-op".into());
            }
            let one = harness
                .sweep_with(
                    &AUTHORITY,
                    0,
                    sim2::TIC_RESERVE_CU + sim2::PUBLISH_RESERVE_CU,
                    None,
                )
                .map_err(|e| format!("{e:?}"))?;
            if one.stop != sim2::Stop::Budget || one.ran != 1 {
                return Err(
                    format!("one-tic budget sweep ran {} ({:?})", one.ran, one.stop).into(),
                );
            }
            check_against_oracle(&harness, &oracle)?;
            controls.insert("budget_stop".into(), serde_json::json!({"zero_budget":{"stop":"Budget","ran":0,"bytes_preserved":true},"one_tic_budget":{"stop":"Budget","ran":1,"oracle_equal":true}}));
            budget_checked = true;
        }
        let report = harness.sweep().map_err(|e| format!("sweep {e:?}"))?;
        if report.ran == 0 && report.stop == sim2::Stop::NoneStaged {
            let later_staged = (report.cursor_before as usize + 1..tics).any(|seq| {
                harness.buffer[sim2::BUFFER_HEADER_BYTES + seq * sim2::SLOT_BYTES + 4] != 0
            });
            if later_staged {
                gap_noops += 1;
            }
        }
        check_against_oracle(&harness, &oracle)?;
    }
    // Everything except the held seqs is staged: drain to the first hold.
    loop {
        let report = harness.sweep().map_err(|e| format!("drain {e:?}"))?;
        check_against_oracle(&harness, &oracle)?;
        if report.ran == 0 {
            break;
        }
    }
    if harness.cursor() != HOLD[0] {
        return Err(format!(
            "expected to stall at {}, cursor {}",
            HOLD[0],
            harness.cursor()
        )
        .into());
    }

    // --- mid-run controls, stalled at the first held slot --------------------
    let before = harness.snapshot();
    let gap = harness.sweep().map_err(|e| format!("{e:?}"))?;
    if gap.stop != sim2::Stop::NoneStaged || harness.snapshot() != before {
        return Err("sweep-during-gap was not a no-op".into());
    }
    controls.insert("sweep_during_gap".into(), serde_json::json!({"ok":true,"cursor":HOLD[0],"later_slot_staged":true,"stop":"NoneStaged","bytes_preserved":true,"gap_noops_during_scramble":gap_noops}));
    refusal(
        &mut harness,
        &mut controls,
        "duplicate_slot_consumed_identical",
        sim2::REFUSE_DUPLICATE_SLOT,
        |h| h.deposit(&AUTHORITY, 5, &command(5)),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "duplicate_slot_consumed_different",
        sim2::REFUSE_DUPLICATE_SLOT,
        |h| h.deposit(&AUTHORITY, 5, &[1, 2, 3, 4]),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "duplicate_slot_staged",
        sim2::REFUSE_DUPLICATE_SLOT,
        |h| h.deposit(&AUTHORITY, HOLD[0] + 1, &command(HOLD[0] + 1)),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "run_overlapping_staged",
        sim2::REFUSE_DUPLICATE_SLOT,
        |h| {
            let run = [command(HOLD[0]), command(HOLD[0] + 1)].concat();
            h.deposit(&AUTHORITY, HOLD[0], &run)
        },
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "wrong_authority_fills_gap",
        sim2::REFUSE_AUTHORITY,
        |h| h.deposit(&ATTACKER, HOLD[0], &command(HOLD[0])),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "wrong_authority_sweep_midrun",
        sim2::REFUSE_AUTHORITY,
        |h| h.sweep_with(&ATTACKER, 0, MODEL_LIMIT, None).map(|_| ()),
    )?;
    let mut other_checkpoint = checkpoint.clone();
    other_checkpoint[20] ^= 1;
    refusal(
        &mut harness,
        &mut controls,
        "changed_checkpoint",
        sim2::REFUSE_CONTEXT,
        |h| {
            h.deposit(&AUTHORITY, HOLD[0], &command(HOLD[0]))?;
            let r = h.sweep_with(&AUTHORITY, 0, MODEL_LIMIT, Some(&other_checkpoint));
            // undo the staged deposit so the snapshot comparison is exact
            let at = sim2::BUFFER_HEADER_BYTES + HOLD[0] as usize * sim2::SLOT_BYTES;
            h.buffer[at..at + sim2::SLOT_BYTES].fill(0);
            r.map(|_| ())
        },
    )?;
    // fault injection: an out-of-band ST1 byte change (unreachable on chain,
    // where only ADVANCE writes ST1) must refuse before the engine runs.
    refusal(
        &mut harness,
        &mut controls,
        "tampered_state_fault_injection",
        sim2::REFUSE_STATE_ROOT,
        |h| {
            h.deposit(&AUTHORITY, HOLD[0], &command(HOLD[0]))?;
            h.state[100] ^= 1;
            let r = h.sweep().map(|_| ());
            h.state[100] ^= 1;
            let at = sim2::BUFFER_HEADER_BYTES + HOLD[0] as usize * sim2::SLOT_BYTES;
            h.buffer[at..at + sim2::SLOT_BYTES].fill(0);
            r
        },
    )?;
    let view = sim2::MetaView::parse(&harness.meta).unwrap();
    sim2::snapshot_check(
        &harness.meta,
        &harness.state,
        &harness.mirror,
        &STATE,
        &MIRROR,
        view.cursor,
        &view.state_root,
    )
    .map_err(|e| format!("snapshot at committed cursor refused: {e:?}"))?;
    refusal(
        &mut harness,
        &mut controls,
        "snapshot_stale_cursor",
        sim2::REFUSE_SNAPSHOT,
        |h| {
            let v = sim2::MetaView::parse(&h.meta).unwrap();
            sim2::snapshot_check(
                &h.meta,
                &h.state,
                &h.mirror,
                &STATE,
                &MIRROR,
                v.cursor - 1,
                &v.state_root,
            )
        },
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "snapshot_wrong_root",
        sim2::REFUSE_SNAPSHOT,
        |h| {
            let v = sim2::MetaView::parse(&h.meta).unwrap();
            sim2::snapshot_check(
                &h.meta,
                &h.state,
                &h.mirror,
                &STATE,
                &MIRROR,
                v.cursor,
                &oracle.tics[0].state_root,
            )
        },
    )?;
    controls.insert(
        "snapshot_committed_cursor".into(),
        serde_json::json!({"ok":true,"cursor":view.cursor}),
    );

    // --- late arrivals --------------------------------------------------------
    let mut late = Vec::new();
    for (index, &held) in HOLD.iter().enumerate() {
        if harness.cursor() != held {
            return Err(format!("late arrival {held}: cursor {}", harness.cursor()).into());
        }
        harness
            .deposit(&AUTHORITY, held, &command(held))
            .map_err(|e| format!("late {held}: {e:?}"))?;
        let first = harness.sweep().map_err(|e| format!("{e:?}"))?;
        if first.cursor_before != held || first.ran == 0 {
            return Err(format!("late arrival {held} did not unblock the cursor").into());
        }
        check_against_oracle(&harness, &oracle)?;
        let target = HOLD.get(index + 1).copied().unwrap_or(tics as u32);
        if index == 0 {
            let limited = harness
                .sweep_with(&AUTHORITY, 3, MODEL_LIMIT, None)
                .map_err(|e| format!("{e:?}"))?;
            if limited.stop != sim2::Stop::MaxTics || limited.ran != 3 {
                return Err("max_tics sweep did not stop at 3".into());
            }
            check_against_oracle(&harness, &oracle)?;
            controls.insert(
                "max_tics_stop".into(),
                serde_json::json!({"max_tics":3,"ran":3,"stop":"MaxTics","oracle_equal":true}),
            );
        }
        while harness.cursor() < target {
            let report = harness.sweep().map_err(|e| format!("{e:?}"))?;
            check_against_oracle(&harness, &oracle)?;
            if report.ran == 0 {
                break;
            }
        }
        late.push(
            serde_json::json!({"seq":held,"stalled_cursor":held,"unblocked_to":first.cursor_after}),
        );
    }
    controls.insert("late_arrival".into(), serde_json::Value::Array(late));
    if harness.cursor() != tics as u32 {
        return Err(format!("final cursor {} != {tics}", harness.cursor()).into());
    }
    let before = harness.snapshot();
    let end = harness.sweep().map_err(|e| format!("{e:?}"))?;
    if end.stop != sim2::Stop::NoneStaged || harness.snapshot() != before {
        return Err("final sweep-with-nothing was not a no-op".into());
    }
    controls.insert(
        "sweep_with_nothing_final".into(),
        serde_json::json!({"ok":true,"stop":"NoneStaged","bytes_preserved":true}),
    );
    refusal(
        &mut harness,
        &mut controls,
        "duplicate_slot_final",
        sim2::REFUSE_DUPLICATE_SLOT,
        |h| h.deposit(&AUTHORITY, tics as u32 - 1, &command(tics as u32 - 1)),
    )?;
    refusal(
        &mut harness,
        &mut controls,
        "wrong_authority_sweep_final",
        sim2::REFUSE_AUTHORITY,
        |h| h.sweep_with(&ATTACKER, 0, MODEL_LIMIT, None).map(|_| ()),
    )?;

    // --- final equality -------------------------------------------------------
    check_against_oracle(&harness, &oracle)?;
    let last = oracle.tics.last().unwrap();
    let ran_sweeps = harness.sweeps.iter().filter(|r| r.ran > 0).count() as u64;
    if harness.engine.steps != tics as u64 || harness.engine.publishes != ran_sweeps {
        return Err(format!(
            "engine ran {} steps / {} publishes; expected {tics} / {ran_sweeps}",
            harness.engine.steps, harness.engine.publishes
        )
        .into());
    }
    let mut stops = std::collections::BTreeMap::<String, usize>::new();
    let mut per_sweep = Vec::new();
    for r in &harness.sweeps {
        *stops.entry(format!("{:?}", r.stop)).or_default() += 1;
        if r.ran > 0 {
            per_sweep.push(r.ran);
        }
    }
    fs::write(receipt.join("final-state.st1"), &harness.state)?;
    fs::write(receipt.join("final-mirror.mp1"), &harness.mirror)?;
    fs::write(receipt.join("final-meta.bin"), &harness.meta)?;
    fs::write(receipt.join("commands.bin"), &commands)?;
    let binary_identity = provenance["binary"].clone();
    let source_manifest_sha256 = provenance["source_manifest_sha256"].clone();
    common::write_json(
        &receipt.join("controls.json"),
        &serde_json::Value::Object(controls),
    )?;
    common::write_json(
        &receipt.join("summary.json"),
        &serde_json::json!({
            "gate": "DOOM-SIM/2 (a) host transport cores vs sequential no-render oracle",
            "tics": tics,
            "first_tic": first_tic,
            "deposit_runs": runs.len(),
            "held_late_arrivals": HOLD,
            "sweeps": harness.sweeps.len(),
            "sweeps_that_ran": ran_sweeps,
            "stops": stops,
            "tics_per_running_sweep_model": per_sweep,
            "engine_steps": harness.engine.steps,
            "engine_publishes": harness.engine.publishes,
            "final_cursor": harness.cursor(),
            "final_state_root": hex(&last.state_root),
            "final_input_chain": hex(&last.chain),
            "final_st1_account_sha256": base::sha(&harness.state),
            "final_mp1_sha256": base::sha(&harness.mirror),
            "oracle_equal_every_sweep": true,
            "oracle_tics_with_nonlevel_or_pending_action": halts,
            "m1201_sequence_oracle_equal_tics": legacy_count,
            "sim1_sbf_receipt": sim1_equal,
            "model_budget": {"limit": MODEL_LIMIT, "tic": MODEL_TIC, "tic_reserve": sim2::TIC_RESERVE_CU, "publish_reserve": sim2::PUBLISH_RESERVE_CU},
        }),
    )?;
    common::write_json(
        &receipt.join("identity.json"),
        &serde_json::json!({
            "schema": "DOOM-SIM/2-NATIVE-ORACLE-IDENTITY/3",
            "profile": "DOOM-SIM/2",
            "implementation": "host build of doom-sbf sim2 cores + host C session engine (same C source as SBF, host compiler, this process)",
            "command_line": argv,
            "capture": capture.display().to_string(),
            "resource": resource_path.display().to_string(),
            "resource_sha256": base::sha(&capture_data.resource),
            "wad_payload_sha256_recomputed": capture_data.wad_payload_sha256,
            "wad_account_sha256": capture_data.wad.iter().map(|w| base::sha(w)).collect::<Vec<_>>(),
            "st1_capture_sha256": base::sha(&capture_data.state),
            "mp1_capture_sha256": base::sha(&capture_data.mirror),
            "commands_sha256": base::sha(&commands),
            "checkpoint": checkpoint_source,
            "checkpoint_sha256": base::sha(&checkpoint),
            "seed": format!("{seed:#x}"),
            "sim2_abi": sim2::ABI_DESCRIPTOR,
            "provenance": provenance,
            "acceptance_descriptor": provenance["acceptance_descriptor"],
            "source_manifest": "source-manifest.json",
            "source_manifest_sha256": source_manifest_sha256,
            "binary": binary_identity,
            "sources": common::driver_identity(&[
                ("driver/src/bin/sweep_native.rs", include_bytes!("sweep_native.rs")),
                ("driver/src/sweep_common.rs", include_bytes!("../sweep_common.rs")),
                ("src/sim2.rs", include_bytes!("../../../src/sim2.rs")),
                ("src/doom_sim2_guard.h", include_bytes!("../../../src/doom_sim2_guard.h")),
                ("src/doom_resource_ri2.c", include_bytes!("../../../src/doom_resource_ri2.c")),
                ("src/doom_sbf_resource_ri2.c", include_bytes!("../../../src/doom_sbf_resource_ri2.c")),
            ]),
            "state_bytes": resource_sbf::STATE_MAX_BYTES,
        }),
    )?;
    println!(
        "DOOM-SIM/2 gate (a) passed: tics={tics} sweeps={} ran={ran_sweeps} publishes={} final_root={} receipt={}",
        harness.sweeps.len(),
        harness.engine.publishes,
        hex(&last.state_root),
        receipt.display()
    );
    Ok(())
}
