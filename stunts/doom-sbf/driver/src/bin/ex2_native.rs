//! Offline native EX2 capture runner.
//!
//! The binary intentionally accepts a retained capture root and a fresh
//! receipt directory. It invokes the real producer for CI2 accounts, calls
//! the native C bridge for two-entry base/base and base/changed continuations,
//! and records refusal controls without treating a structural pass as native
//! proof.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use doom_sbf::ex2::{self, Envelope, FRAME_BYTES};

const FIRST_RECORD_OFFSET: usize = 33_614;
const FIRST_TARGET_OFFSET: usize = 33_699;
const EX2_REFUSE_STATE: i32 = 232;
const EX2_REFUSE_POOL: i32 = 233;

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
}

fn producer(
    source_root: &Path,
    capture: &Path,
    receipt: &Path,
    tic: u32,
    delta: i32,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = receipt.join(format!("controller-tic{tic}-delta{delta}.bin"));
    let python = env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let producer_script = source_root.join("stunts/doom-host/tools/doom_controller_producer.py");
    if !producer_script.is_file() {
        return Err(format!(
            "producer script is missing under source root {}",
            source_root.display()
        )
        .into());
    }
    let status = Command::new(python)
        .current_dir(source_root)
        .arg(&producer_script)
        .args([
            "--capture",
            capture.to_str().ok_or("non-UTF8 capture path")?,
        ])
        .args(["--out", output.to_str().ok_or("non-UTF8 output path")?])
        .args([
            "--tic",
            &tic.to_string(),
            "--forwardmove-delta",
            &delta.to_string(),
        ])
        .status()?;
    if !status.success() {
        return Err(format!("controller producer refused tic {tic} delta {delta}").into());
    }
    let bytes = read(&output)?;
    if bytes.len() != ex2::CONTROLLER_BYTES || &bytes[..8] != b"DOOM-CI2" {
        return Err(format!(
            "producer output is not a bounded CI2 account: {}",
            output.display()
        )
        .into());
    }
    Ok(bytes)
}

fn decoded_envelope<'a>(bytes: &'a [u8], label: &str) -> Result<Envelope<'a>, Box<dyn Error>> {
    ex2::decode(bytes)
        .map_err(|error| format!("{label} is not a valid DOOM-EX2 envelope: {error}").into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn state_hash(wad_hash: &[u8], state: &[u8]) -> Result<String, ex2::DecodeError> {
    ex2::native_state_hash(wad_hash, state)
}

fn frame_hash(wad_hash: &[u8], state: &[u8], frame: &[u8]) -> Result<String, ex2::DecodeError> {
    let tic = u32::from_le_bytes(state[20..24].try_into().unwrap())
        .checked_sub(1)
        .ok_or(ex2::DecodeError::State)?;
    ex2::native_frame_hash(wad_hash, tic, frame)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let capture = PathBuf::from(
        args.next()
            .ok_or("usage: ex2_native CAPTURE_ROOT RECEIPT_DIR")?,
    )
    .canonicalize()?;
    let receipt = PathBuf::from(
        args.next()
            .ok_or("usage: ex2_native CAPTURE_ROOT RECEIPT_DIR")?,
    );
    let source_root = args
        .next()
        .map(PathBuf::from)
        .or_else(|| env::var_os("DOOM_EX2_SOURCE_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .canonicalize()
        .map_err(|error| format!("cannot resolve native source root: {error}"))?;
    if args.next().is_some() {
        return Err(
            "usage: ex2_native CAPTURE_ROOT RECEIPT_DIR [SOURCE_ROOT] (or DOOM_EX2_SOURCE_ROOT)"
                .into(),
        );
    }
    if !capture.join("account/manifest.json").is_file() {
        return Err(format!("missing retained manifest under {}", capture.display()).into());
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err(format!("refusing non-empty receipt directory {}", receipt.display()).into());
    }
    fs::create_dir_all(&receipt)?;
    fs::write(
        receipt.join("source-manifest.json"),
        ex2::BUILD_SOURCE_MANIFEST,
    )?;
    fs::write(
        receipt.join("identity.json"),
        format!(
            "{{\n  \"image_id\": \"{}\",\n  \"abi_id\": \"{}\",\n  \"source_manifest_sha256\": \"{}\",\n  \"source_root\": {:?},\n  \"identity_rule\": \"image_id = sha256(DOOM-EX2-IMAGE-ID\\u0000 || canonical source manifest); executable bytes are not input\",\n  \"executable_hash\": \"recorded by the Python gate as sha256(raw binary bytes), outside image_id\"\n}}\n",
            hex(&ex2::IMAGE_ID),
            hex(&ex2::ABI_ID),
            ex2::SOURCE_MANIFEST_SHA256,
            source_root.display().to_string(),
        ),
    )?;
    let state0 = read(&capture.join("dumped-doom-st1.bin"))?;
    let mirror0 = read(&capture.join("mobj_pool.bin"))?;
    let mut wad_accounts: [Vec<u8>; 5] = std::array::from_fn(|_| Vec::new());
    for (i, account) in wad_accounts.iter_mut().enumerate() {
        *account = read(&capture.join(format!("account/wad-{i:03}.bin")))?;
    }
    let wad_refs = std::array::from_fn(|i| wad_accounts[i].as_slice());
    let ci0 = producer(&source_root, &capture, &receipt, 0, 0)?;
    let ci1 = producer(&source_root, &capture, &receipt, 1, 0)?;
    let ci1_changed = producer(&source_root, &capture, &receipt, 1, 1)?;
    let ci2 = producer(&source_root, &capture, &receipt, 2, 0)?;

    let input0_wire = Envelope::new(&state0, &mirror0, &ci0)?.encode()?;
    fs::write(receipt.join("entry-0-input.ex2"), &input0_wire)?;
    let input0 = decoded_envelope(&input0_wire, "entry-0-input")?;
    let out1 = ex2::native_entry(&input0, wad_refs)
        .map_err(|rc| format!("EX2 entry 0 refused with {rc}"))?;
    let out1_wire = Envelope::new(&out1.state, &out1.mirror, &ci1)?
        .with_frame(&out1.frame)?
        .encode()?;
    fs::write(receipt.join("entry-0-output.ex2"), &out1_wire)?;
    let out1_decoded = decoded_envelope(&out1_wire, "entry-0-output")?;
    let input1_base_wire =
        Envelope::new(out1_decoded.state, out1_decoded.mirror, &ci1)?.encode()?;
    let input1_changed_wire =
        Envelope::new(out1_decoded.state, out1_decoded.mirror, &ci1_changed)?.encode()?;
    fs::write(receipt.join("entry-1-base-input.ex2"), &input1_base_wire)?;
    fs::write(
        receipt.join("entry-1-changed-input.ex2"),
        &input1_changed_wire,
    )?;
    let input1_base = decoded_envelope(&input1_base_wire, "entry-1-base-input")?;
    let input1_changed = decoded_envelope(&input1_changed_wire, "entry-1-changed-input")?;
    let out2_base = ex2::native_entry(&input1_base, wad_refs)
        .map_err(|rc| format!("EX2 base/base entry refused with {rc}"))?;
    let out2_changed = ex2::native_entry(&input1_changed, wad_refs)
        .map_err(|rc| format!("EX2 base/changed entry refused with {rc}"))?;
    let out2_base_wire = Envelope::new(&out2_base.state, &out2_base.mirror, &ci2)?
        .with_frame(&out2_base.frame)?
        .encode()?;
    let out2_changed_wire = Envelope::new(&out2_changed.state, &out2_changed.mirror, &ci2)?
        .with_frame(&out2_changed.frame)?
        .encode()?;
    fs::write(receipt.join("entry-1-base.ex2"), &out2_base_wire)?;
    fs::write(receipt.join("entry-1-changed.ex2"), &out2_changed_wire)?;
    let _ = decoded_envelope(&out2_base_wire, "entry-1-base-output")?;
    let _ = decoded_envelope(&out2_changed_wire, "entry-1-changed-output")?;
    fs::write(
        receipt.join("output-binding.json"),
        "{\n  \"first_output_decoded_before_second_entry\": true,\n  \"state_hash_domain\": \"sha256(DOOM-ST1 || u32le(0) || wad_sha256 || exported_ST1_bytes)\",\n  \"frame_hash_domain\": \"sha256(DOOM-FB1-FRAME || u32le(0) || wad_sha256 || u32le(gametic) || u32le(0) || exported_frame_bytes)\"\n}\n",
    )?;

    let mut bad_image = input0.encode()?;
    bad_image[32] ^= 1;
    let bad_roles = {
        let mut b = input0.encode()?;
        b[256] ^= 1;
        b
    };
    let mut bad_truncated = input0.encode()?;
    bad_truncated.truncate(bad_truncated.len() - 1);
    let mut bad_unknown_kind = state0.clone();
    bad_unknown_kind[FIRST_RECORD_OFFSET] = 0xff;
    let mut bad_object_id = state0.clone();
    bad_object_id[FIRST_TARGET_OFFSET..FIRST_TARGET_OFFSET + 4]
        .copy_from_slice(&u32::MAX.to_le_bytes());
    let mut bad_truncated_state = state0.clone();
    let truncated_len = (bad_truncated_state.len() - 1) as u32;
    bad_truncated_state[16..20].copy_from_slice(&truncated_len.to_le_bytes());
    let malformed = [
        matches!(ex2::decode(&bad_image), Err(ex2::DecodeError::Identity)),
        matches!(ex2::decode(&bad_roles), Err(ex2::DecodeError::Roles)),
        matches!(
            ex2::decode(&bad_truncated),
            Err(ex2::DecodeError::Total | ex2::DecodeError::Bounds)
        ),
    ];
    let unknown_kind =
        ex2::native_entry(&Envelope::new(&bad_unknown_kind, &mirror0, &ci0)?, wad_refs);
    let object_id = ex2::native_entry(&Envelope::new(&bad_object_id, &mirror0, &ci0)?, wad_refs);
    let truncated = Envelope::new(&bad_truncated_state, &mirror0, &ci0)
        .map(|_| ())
        .map_err(|_| EX2_REFUSE_STATE);
    let mut bad_pool = mirror0.clone();
    bad_pool[4096 + 8..4096 + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    let pool = ex2::native_entry(&Envelope::new(&state0, &bad_pool, &ci0)?, wad_refs);
    if !malformed.iter().all(|&v| v)
        || !matches!(unknown_kind, Err(EX2_REFUSE_STATE))
        || !matches!(object_id, Err(EX2_REFUSE_STATE))
        || !matches!(truncated, Err(EX2_REFUSE_STATE))
        || !matches!(pool, Err(EX2_REFUSE_POOL))
    {
        return Err(format!("negative controls did not preserve refusal boundary: malformed={malformed:?} unknown={unknown_kind:?} object={object_id:?} truncated={truncated:?} pool={pool:?}").into());
    }

    let wad_hash = &mirror0[36..68];
    let out1_state_hash = state_hash(wad_hash, &out1.state)?;
    let out2_base_state_hash = state_hash(wad_hash, &out2_base.state)?;
    let out1_frame_hash = frame_hash(wad_hash, &out1.state, &out1.frame)?;
    let out2_base_frame_hash = frame_hash(wad_hash, &out2_base.state, &out2_base.frame)?;
    let out2_changed_state_hash = state_hash(wad_hash, &out2_changed.state)?;
    let out2_changed_frame_hash = frame_hash(wad_hash, &out2_changed.state, &out2_changed.frame)?;
    let report = format!(
        "{{\n  \"status\": \"measured\",\n  \"native_engine\": \"doom_state.c + G_Ticker + R_RenderPlayerView\",\n  \"capture_root\": {:?},\n  \"entry_count\": 3,\n  \"frame_bytes\": {},\n  \"base_base\": {{\"state_sha256\": [\"{}\", \"{}\"], \"frame_sha256\": [\"{}\", \"{}\"]}},\n  \"base_changed\": {{\"state_sha256\": [\"{}\", \"{}\"], \"frame_sha256\": [\"{}\", \"{}\"]}},\n  \"negative_controls\": {{\"unknown_kind\": true, \"object_id\": true, \"truncated_state\": true, \"pool_mismatch\": true, \"unknown_envelope_image\": true, \"unknown_envelope_role\": true, \"truncated_envelope\": true}},\n  \"resource_note\": \"native-only; no SBF/CU claim\"\n}}\n",
        capture.display(), FRAME_BYTES,
        out1_state_hash, out2_base_state_hash,
        out1_frame_hash, out2_base_frame_hash,
        out1_state_hash, out2_changed_state_hash,
        out1_frame_hash, out2_changed_frame_hash,
    );
    fs::write(receipt.join("report.json"), report)?;
    println!("EX2 native measured: {}", receipt.display());
    Ok(())
}
