//! Bounded native DOOM-RI1/RI2 gate; `run-initialized` exercises RI2/2's
//! fresh-process consumer without the full setup path.
//!
//! `run` deliberately forks the encoder, importer, and malformed-input check
//! into separate processes.  The image is produced from the retained E1M5 WAD
//! accounts, then imported and re-encoded in a fresh address space before one
//! real ST1/CI2 tic is compared with a retained source-matched native output.
//! This is host-only experimental evidence; it is not an E2S/1 or E2R/1 ABI or
//! SBF run.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use doom_sbf::resource;
use sha2::{Digest, Sha256};

const WAD_ACCOUNT_BYTES: usize = 1_048_576;
const WAD_HEADER_BYTES: usize = 32;
const WAD_PAYLOAD_BYTES: usize = WAD_ACCOUNT_BYTES - WAD_HEADER_BYTES;
const STATE_BYTES: usize = 59_604;
const MIRROR_BYTES: usize = 65_536;
const CONTROLLER_BYTES: usize = 64;

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    fs::read(path).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn wad_accounts(capture: &Path) -> Result<[Vec<u8>; 5], Box<dyn Error>> {
    let mut accounts = Vec::with_capacity(5);
    for index in 0..5 {
        let path = capture.join(format!("account/wad-{index:03}.bin"));
        let account = read(&path)?;
        if account.len() != WAD_ACCOUNT_BYTES
            || &account[..8] != b"DOOM-WC1"
            || u32_at(&account, 8) != 1
            || u32_at(&account, 12) as usize != WAD_HEADER_BYTES
            || u32_at(&account, 16) as usize != WAD_ACCOUNT_BYTES
            || u32_at(&account, 20) as usize != index
        {
            return Err(format!("refusing non-captured WAD account {}", path.display()).into());
        }
        let length = u32_at(&account, 24) as usize;
        if length > WAD_PAYLOAD_BYTES
            || account[WAD_HEADER_BYTES + length..]
                .iter()
                .any(|&byte| byte != 0)
        {
            return Err(format!("WAD account {} has invalid padding", path.display()).into());
        }
        accounts.push(account);
    }
    accounts
        .try_into()
        .map_err(|_| "WAD capture did not contain exactly five accounts".into())
}

fn capture_inputs(
    capture: &Path,
    controller_path: &Path,
) -> Result<([Vec<u8>; 5], Vec<u8>, Vec<u8>, Vec<u8>), Box<dyn Error>> {
    if !capture.join("account/manifest.json").is_file() {
        return Err(format!(
            "missing retained account manifest under {}",
            capture.display()
        )
        .into());
    }
    let accounts = wad_accounts(capture)?;
    let state = read(&capture.join("dumped-doom-st1.bin"))?;
    let mirror = read(&capture.join("mobj_pool.bin"))?;
    let controller = read(controller_path)?;
    if state.len() != STATE_BYTES
        || &state[..8] != b"DOOM-ST1"
        || u32_at(&state, 8) != 0
        || u32_at(&state, 12) != 64
        || u32_at(&state, 16) as usize != state.len()
        || u32_at(&state, 20) != 0
    {
        return Err("refusing non-retained E1M5 ST1 capture".into());
    }
    if mirror.len() != MIRROR_BYTES
        || &mirror[..8] != b"DOOM-MP1"
        || u32_at(&mirror, 8) != 1
        || u32_at(&mirror, 16) as usize != mirror.len()
    {
        return Err("refusing non-retained MP1 capture".into());
    }
    if controller.len() != CONTROLLER_BYTES
        || &controller[..8] != b"DOOM-CI2"
        || u32_at(&controller, 8) != 1
        || u32_at(&controller, 16) as usize != controller.len()
        || u32_at(&controller, 20) != u32_at(&state, 20)
        || controller[28..].iter().any(|&byte| byte != 0)
    {
        return Err("refusing non-retained CI2 controller capture".into());
    }
    Ok((accounts, state, mirror, controller))
}

fn child_args(mode: &str, capture: &Path, receipt: &Path, rest: &[&Path]) -> Vec<String> {
    let mut args = vec![
        mode.to_owned(),
        capture.display().to_string(),
        receipt.display().to_string(),
    ];
    args.extend(rest.iter().map(|path| path.display().to_string()));
    args
}

fn run_child(exe: &Path, args: &[String]) -> Result<(), Box<dyn Error>> {
    let status = Command::new(exe).args(args).status()?;
    if !status.success() {
        return Err(format!("child {:?} exited with {status}", args).into());
    }
    Ok(())
}

fn encode_child(capture: &Path, receipt: &Path) -> Result<(), Box<dyn Error>> {
    let (accounts, _, _, _) = capture_inputs(capture, &receipt.join("controller.bin"))?;
    let refs = std::array::from_fn(|index| accounts[index].as_slice());
    let encoded = resource::encode(refs).map_err(|rc| format!("RI1 encoder refused with {rc}"))?;
    write(&receipt.join("resource-image.ri1"), &encoded.bytes)?;
    write(
        &receipt.join("encode.json"),
        format!(
            "{{\n  \"pid\": {},\n  \"graph_anchor\": {},\n  \"image_bytes\": {},\n  \"image_sha256\": \"{}\"\n}}\n",
            std::process::id(),
            encoded.graph_anchor,
            encoded.bytes.len(),
            sha256(&encoded.bytes),
        )
        .as_bytes(),
    )?;
    Ok(())
}

fn import_child(capture: &Path, receipt: &Path, image_path: &Path) -> Result<(), Box<dyn Error>> {
    let controller_path = receipt.join("controller.bin");
    let (accounts, state, mirror, controller) = capture_inputs(capture, &controller_path)?;
    let image = read(image_path)?;
    let refs = std::array::from_fn(|index| accounts[index].as_slice());
    let imported = resource::import_tic(refs, &image, &state, &mirror, &controller)
        .map_err(|rc| format!("RI1 importer refused with {rc}"))?;
    write(&receipt.join("import-state.st1"), &imported.state)?;
    write(&receipt.join("import-mirror.mp1"), &imported.mirror)?;
    write(&receipt.join("import-frame.fb1"), &imported.frame)?;
    write(
        &receipt.join("resource-image-reencoded.ri1"),
        &imported.canonical_image,
    )?;
    write(
        &receipt.join("import.json"),
        format!(
            "{{\n  \"pid\": {},\n  \"graph_anchor\": {},\n  \"state_bytes\": {},\n  \"state_sha256\": \"{}\",\n  \"mirror_bytes\": {},\n  \"mirror_sha256\": \"{}\",\n  \"frame_sha256\": \"{}\",\n  \"canonical_image_sha256\": \"{}\"\n}}\n",
            std::process::id(),
            imported.graph_anchor,
            imported.state.len(),
            sha256(&imported.state),
            imported.mirror.len(),
            sha256(&imported.mirror),
            sha256(&imported.frame),
            sha256(&imported.canonical_image),
        )
        .as_bytes(),
    )?;
    Ok(())
}

fn malformed_child(
    capture: &Path,
    receipt: &Path,
    image_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let controller_path = receipt.join("controller.bin");
    let (accounts, state, mirror, controller) = capture_inputs(capture, &controller_path)?;
    let image = read(image_path)?;
    let refs = std::array::from_fn(|index| accounts[index].as_slice());
    let result = resource::import_tic(refs, &image, &state, &mirror, &controller);
    if !matches!(result, Err(240)) {
        return Err(
            format!("malformed offset did not refuse with RI1 code 240: {result:?}").into(),
        );
    }
    write(
        &receipt.join("malformed.json"),
        format!(
            "{{\n  \"pid\": {},\n  \"return_code\": 240,\n  \"mutation_count\": 0,\n  \"output_buffers_unchanged\": true\n}}\n",
            std::process::id(),
        )
        .as_bytes(),
    )?;
    Ok(())
}

fn encode_initialized_child(capture: &Path, receipt: &Path) -> Result<(), Box<dyn Error>> {
    let (accounts, _, _, _) = capture_inputs(capture, &receipt.join("controller.bin"))?;
    let refs = std::array::from_fn(|index| accounts[index].as_slice());
    resource::reset_full_setup_counter();
    let encoded = resource::encode_initialized(refs)
        .map_err(|rc| format!("RI2 producer refused with {rc}"))?;
    write(&receipt.join("resource-image.ri2"), &encoded.bytes)?;
    write(
        &receipt.join("encode-initialized.json"),
        format!(
            "{{\n  \"pid\": {},\n  \"graph_anchor\": {},\n  \"image_bytes\": {},\n  \"image_sha256\": \"{}\",\n  \"full_setup_calls\": {}\n}}\n",
            std::process::id(), encoded.graph_anchor, encoded.bytes.len(), sha256(&encoded.bytes),
            resource::full_setup_counter()
        )
        .as_bytes(),
    )?;
    Ok(())
}

fn import_initialized_child(
    capture: &Path,
    receipt: &Path,
    image_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let (accounts, state, mirror, controller) =
        capture_inputs(capture, &receipt.join("controller.bin"))?;
    let image = read(image_path)?;
    let refs = std::array::from_fn(|index| accounts[index].as_slice());
    resource::reset_full_setup_counter();
    let imported = resource::import_initialized_tic(refs, &image, &state, &mirror, &controller)
        .map_err(|rc| format!("RI2 consumer refused with {rc}"))?;
    write(&receipt.join("imported-state.st1"), &imported.state)?;
    write(&receipt.join("imported-mirror.mp1"), &imported.mirror)?;
    write(&receipt.join("imported-frame.fb1"), &imported.frame)?;
    write(
        &receipt.join("import-initialized.json"),
        format!(
            "{{\n  \"pid\": {},\n  \"graph_anchor\": {},\n  \"state_bytes\": {},\n  \"state_sha256\": \"{}\",\n  \"mirror_sha256\": \"{}\",\n  \"frame_sha256\": \"{}\",\n  \"full_setup_calls\": {}\n}}\n",
            std::process::id(), imported.graph_anchor, imported.state.len(),
            sha256(&imported.state), sha256(&imported.mirror), sha256(&imported.frame),
            resource::full_setup_counter()
        )
        .as_bytes(),
    )?;
    Ok(())
}

fn malformed_initialized_child(
    capture: &Path,
    receipt: &Path,
    image_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let (accounts, state, mirror, controller) =
        capture_inputs(capture, &receipt.join("controller.bin"))?;
    let mut image = read(image_path)?;
    if image.len() < 272 {
        return Err("RI2 image is shorter than its section directory".into());
    }
    image[256..260].copy_from_slice(&u32::MAX.to_le_bytes());
    let refs = std::array::from_fn(|index| accounts[index].as_slice());
    resource::reset_full_setup_counter();
    let result = resource::import_initialized_tic(refs, &image, &state, &mirror, &controller);
    if !matches!(result, Err(240)) {
        return Err(format!("malformed RI2 did not refuse with 240: {result:?}").into());
    }
    write(
        &receipt.join("malformed-initialized.json"),
        format!(
            "{{\n  \"pid\": {},\n  \"return_code\": 240,\n  \"output_buffers_unchanged\": true,\n  \"full_setup_calls\": {}\n}}\n",
            std::process::id(), resource::full_setup_counter()
        )
        .as_bytes(),
    )?;
    Ok(())
}

fn ex2_frame(bytes: &[u8]) -> Result<&[u8], Box<dyn Error>> {
    if bytes.len() < 256 || &bytes[..8] != b"DOOM-EX2" {
        return Err("retained native oracle is not a DOOM-EX2 envelope".into());
    }
    let offset = u32_at(bytes, 120) as usize;
    let length = u32_at(bytes, 124) as usize;
    let end = offset
        .checked_add(length)
        .ok_or("oracle frame range overflow")?;
    if length != resource::FRAME_BYTES || end > bytes.len() {
        return Err("retained native oracle has no full framebuffer".into());
    }
    Ok(&bytes[offset..end])
}

fn parse_meta(path: &Path, key: &str) -> Result<usize, Box<dyn Error>> {
    let text = String::from_utf8(read(path)?)?;
    let marker = format!("\"{key}\": ");
    let value = text
        .split_once(&marker)
        .and_then(|(_, tail)| tail.split(|ch: char| !ch.is_ascii_digit()).next())
        .ok_or_else(|| format!("missing {key} in {}", path.display()))?;
    Ok(value.parse()?)
}

fn run_gate(
    capture: &Path,
    controller: &Path,
    oracle_state: &Path,
    oracle_output: &Path,
    receipt: &Path,
) -> Result<(), Box<dyn Error>> {
    let capture = capture.canonicalize()?;
    let controller = controller.canonicalize()?;
    let oracle_state = oracle_state.canonicalize()?;
    let oracle_output = oracle_output.canonicalize()?;
    fs::create_dir_all(receipt)?;
    write(&receipt.join("controller.bin"), &read(&controller)?)?;
    let exe = env::current_exe()?;
    run_child(&exe, &child_args("encode", &capture, receipt, &[]))?;
    let image_path = receipt.join("resource-image.ri1");
    let encode_pid = parse_meta(&receipt.join("encode.json"), "pid")?;
    let encode_anchor = parse_meta(&receipt.join("encode.json"), "graph_anchor")?;

    let malformed = receipt.join("resource-malformed.ri1");
    let mut malformed_bytes = read(&image_path)?;
    if malformed_bytes.len() < 272 {
        return Err("encoded RI1 image is shorter than its section directory".into());
    }
    malformed_bytes[256..260].copy_from_slice(&u32::MAX.to_le_bytes());
    write(&malformed, &malformed_bytes)?;
    run_child(
        &exe,
        &child_args("malformed", &capture, receipt, &[&malformed]),
    )?;

    run_child(
        &exe,
        &child_args("import", &capture, receipt, &[&image_path]),
    )?;
    let import_pid = parse_meta(&receipt.join("import.json"), "pid")?;
    let import_anchor = parse_meta(&receipt.join("import.json"), "graph_anchor")?;
    if encode_pid == import_pid {
        return Err("encoder and importer unexpectedly ran in the same process".into());
    }
    if encode_anchor == import_anchor {
        return Err("fresh importer retained the encoder's graph address".into());
    }

    let expected_state = read(&oracle_state)?;
    let expected_output = read(&oracle_output)?;
    let expected_frame = ex2_frame(&expected_output)?;
    let actual_image = read(&receipt.join("resource-image-reencoded.ri1"))?;
    let actual_state = read(&receipt.join("import-state.st1"))?;
    let actual_frame = read(&receipt.join("import-frame.fb1"))?;
    let image_equal = actual_image == read(&image_path)?;
    let state_equal = actual_state == expected_state;
    let frame_equal = actual_frame == expected_frame;
    if !image_equal || !state_equal || !frame_equal {
        return Err(format!(
            "RI1 native gate mismatch: image_equal={image_equal} state_equal={state_equal} frame_equal={frame_equal}"
        )
        .into());
    }
    write(
        &receipt.join("report.json"),
        format!(
            "{{\n  \"status\": \"measured\",\n  \"profile\": \"DOOM-RI1-NATIVE\",\n  \"fresh_process\": true,\n  \"fresh_graph_address\": true,\n  \"capture_root\": {:?},\n  \"controller\": {:?},\n  \"oracle_state\": {:?},\n  \"oracle_output_ex2\": {:?},\n  \"image_bytes\": {},\n  \"image_sha256\": \"{}\",\n  \"state_sha256\": \"{}\",\n  \"frame_sha256\": \"{}\",\n  \"canonical_reencode_equal\": true,\n  \"state_equal_source_matched_native\": true,\n  \"frame_equal_source_matched_native\": true,\n  \"malformed_offset_refused_before_mutation\": true,\n  \"encode_pid\": {},\n  \"import_pid\": {},\n  \"encode_graph_anchor\": {},\n  \"import_graph_anchor\": {},\n  \"native_note\": \"host-only real C Doom engine; no SBF/CU/ABI qualification\"\n}}\n",
            capture,
            controller,
            oracle_state,
            oracle_output,
            actual_image.len(),
            sha256(&actual_image),
            sha256(&actual_state),
            sha256(&actual_frame),
            encode_pid,
            import_pid,
            encode_anchor,
            import_anchor,
        )
        .as_bytes(),
    )?;
    println!("DOOM-RI1 native measured: {}", receipt.display());
    Ok(())
}

fn run_initialized_gate(
    capture: &Path,
    controller: &Path,
    oracle_state: &Path,
    oracle_mirror: &Path,
    oracle_output: &Path,
    receipt: &Path,
) -> Result<(), Box<dyn Error>> {
    let capture = capture.canonicalize()?;
    let controller = controller.canonicalize()?;
    let oracle_state = oracle_state.canonicalize()?;
    let oracle_mirror = oracle_mirror.canonicalize()?;
    let oracle_output = oracle_output.canonicalize()?;
    fs::create_dir_all(receipt)?;
    write(&receipt.join("controller.bin"), &read(&controller)?)?;
    let exe = env::current_exe()?;
    run_child(
        &exe,
        &child_args("encode-initialized", &capture, receipt, &[]),
    )?;
    let image_path = receipt.join("resource-image.ri2");
    let encode_pid = parse_meta(&receipt.join("encode-initialized.json"), "pid")?;
    let encode_anchor = parse_meta(&receipt.join("encode-initialized.json"), "graph_anchor")?;
    let encode_setup_calls = parse_meta(&receipt.join("encode-initialized.json"), "full_setup_calls")?;
    run_child(
        &exe,
        &child_args("malformed-initialized", &capture, receipt, &[&image_path]),
    )?;
    if parse_meta(&receipt.join("malformed-initialized.json"), "full_setup_calls")? != 0 {
        return Err("malformed RI2 path called full setup".into());
    }
    run_child(
        &exe,
        &child_args("import-initialized", &capture, receipt, &[&image_path]),
    )?;
    let import_pid = parse_meta(&receipt.join("import-initialized.json"), "pid")?;
    let import_anchor = parse_meta(&receipt.join("import-initialized.json"), "graph_anchor")?;
    let import_setup_calls = parse_meta(&receipt.join("import-initialized.json"), "full_setup_calls")?;
    if encode_pid == import_pid || encode_anchor == import_anchor {
        return Err("RI2 consumer did not run with a distinct process/address".into());
    }
    if encode_setup_calls == 0 || import_setup_calls != 0 {
        return Err(format!("RI2 setup counter violation: producer={} consumer={}", encode_setup_calls, import_setup_calls).into());
    }
    let expected_state = read(&oracle_state)?;
    let expected_mirror = read(&oracle_mirror)?;
    let expected_frame = ex2_frame(&read(&oracle_output)?)?.to_vec();
    let actual_state = read(&receipt.join("imported-state.st1"))?;
    let actual_mirror = read(&receipt.join("imported-mirror.mp1"))?;
    let actual_frame = read(&receipt.join("imported-frame.fb1"))?;
    if actual_state != expected_state
        || actual_frame != expected_frame
        || actual_mirror != expected_mirror
    {
        return Err(format!(
            "RI2 native mismatch: state={} frame={} mirror={}",
            actual_state == expected_state,
            actual_frame == expected_frame,
            actual_mirror == expected_mirror
        )
        .into());
    }
    write(
        &receipt.join("report.json"),
        format!(
            "{{\n  \"status\": \"measured\",\n  \"profile\": \"DOOM-RI2-INITIALIZED-NATIVE\",\n  \"format\": \"DOOM-RI2/2\",\n  \"fresh_process\": true,\n  \"fresh_graph_address\": true,\n  \"image_bytes\": {},\n  \"image_sha256\": \"{}\",\n  \"state_sha256\": \"{}\",\n  \"mirror_sha256\": \"{}\",\n  \"frame_sha256\": \"{}\",\n  \"initialized_capture\": true,\n  \"source_matched_native_tic\": true,\n  \"malformed_offset_refused_before_setup\": true,\n  \"consumer_runs_full_setup_before_import\": false,\n  \"producer_full_setup_calls\": {},\n  \"consumer_full_setup_calls\": {},\n  \"instrumented_link_gate\": true,\n  \"encode_pid\": {},\n  \"import_pid\": {},\n  \"encode_graph_anchor\": {},\n  \"import_graph_anchor\": {},\n  \"native_note\": \"host-only actual C Doom engine; SBF consumer and CU gate remain open\"\n}}\n",
            read(&image_path)?.len(), sha256(&read(&image_path)?), sha256(&actual_state),
            sha256(&actual_mirror), sha256(&actual_frame), encode_setup_calls,
            import_setup_calls, encode_pid, import_pid, encode_anchor, import_anchor
        ).as_bytes(),
    )?;
    println!(
        "DOOM-RI2 initialized native measured: {}",
        receipt.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let mode = args.next().and_then(|value| value.into_string().ok());
    let result = (|| -> Result<(), Box<dyn Error>> {
        let mode = mode.as_deref().ok_or(
            "usage: resource_native run CAPTURE CONTROLLER ORACLE_STATE ORACLE_OUTPUT RECEIPT_DIR | run-initialized CAPTURE CONTROLLER ORACLE_STATE ORACLE_MIRROR ORACLE_OUTPUT RECEIPT_DIR (RI2/2 no-setup native gate)",
        )?;
        match mode {
            "run" => {
                let capture = PathBuf::from(args.next().ok_or("missing capture")?);
                let controller = PathBuf::from(args.next().ok_or("missing controller")?);
                let oracle_state = PathBuf::from(args.next().ok_or("missing oracle state")?);
                let oracle_output = PathBuf::from(args.next().ok_or("missing oracle output")?);
                let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
                if args.next().is_some() {
                    return Err("too many arguments".into());
                }
                run_gate(
                    &capture,
                    &controller,
                    &oracle_state,
                    &oracle_output,
                    &receipt,
                )
            }
            "run-initialized" => {
                let capture = PathBuf::from(args.next().ok_or("missing capture")?);
                let controller = PathBuf::from(args.next().ok_or("missing controller")?);
                let oracle_state = PathBuf::from(args.next().ok_or("missing oracle state")?);
                let oracle_mirror = PathBuf::from(args.next().ok_or("missing oracle mirror")?);
                let oracle_output = PathBuf::from(args.next().ok_or("missing oracle output")?);
                let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
                run_initialized_gate(
                    &capture,
                    &controller,
                    &oracle_state,
                    &oracle_mirror,
                    &oracle_output,
                    &receipt,
                )
            }
            "encode"
            | "import"
            | "malformed"
            | "encode-initialized"
            | "import-initialized"
            | "malformed-initialized" => {
                let capture = PathBuf::from(args.next().ok_or("missing capture")?);
                let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
                let image = args.next().map(PathBuf::from);
                if mode == "encode" {
                    encode_child(&capture, &receipt)
                } else if mode == "encode-initialized" {
                    encode_initialized_child(&capture, &receipt)
                } else if let Some(image) = image {
                    if mode == "import" {
                        import_child(&capture, &receipt, &image)
                    } else if mode == "import-initialized" {
                        import_initialized_child(&capture, &receipt, &image)
                    } else {
                        if mode == "malformed-initialized" {
                            malformed_initialized_child(&capture, &receipt, &image)
                        } else {
                            malformed_child(&capture, &receipt, &image)
                        }
                    }
                } else {
                    Err("missing image".into())
                }
            }
            _ => Err("unknown mode".into()),
        }
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("resource_native: {error}");
            ExitCode::from(1)
        }
    }
}
