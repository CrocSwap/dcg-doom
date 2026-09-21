//! Real DOOM-E2R/1 resource-account ProgramTest gate.
//!
//! All positive bytes come from retained captures: the RI1 image is the
//! measured fresh-process native output, WAD/state/MP1 are the retained
//! account-image capture, CI2 is the producer output copied by the native RI1
//! receipt, and the expected ST1 is the native RI1 import/tic output.  No
//! positive account image is constructed in this driver.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use doom_sbf::{resource, resource_sbf};
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_banks_interface::BanksTransactionResultWithMetadata;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_message::Message;
use solana_program_test::ProgramTest;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xda; 32]);
const NORMAL_LIMIT: u32 = 1_400_000;
/// Diagnostic-only ceiling; never used by the qualification path. It is
/// deliberately high enough to expose the post-initialization boundaries;
/// `run-mode.json` still labels the run ineligible for qualification.
const DIAGNOSTIC_LIMIT: u32 = 20_000_000;
const QUALIFICATION_MAX_UNITS: u64 = 1_190_000;
const ACCOUNT_MANIFEST_SHA256: &str =
    "d23776b358184c77fba32df600bf565a56b6935d129aec6b9567ad3c1c999504";
const RESOURCE_IMAGE_SHA256: &str =
    "bff8f5e26465612e6d06b18dbfac98ac1d8f4f997e633be45c4334c8ef3c3e6e";
const WAD_ACCOUNT_SHA256: [&str; 5] = [
    "122f11388481c3a6686305cf7d47e776ddaf4b41490f239e57458609befa4b9c",
    "5d62cc14b5556fb5f8a7187eb4c678abb0394fe05b26bd96b46a88ad1a1c3680",
    "813be339217ff8cc561f669b71d18629c1f5892a1a641b63b4e1a3c2dcf2f364",
    "20cc95f699d9a640c0edb4100184aeff2863aa7152492072f99db3fc131ad4a7",
    "ef64b046aaf61eeab85b3d9f10edc564f9b4b640c341d10afc6a9dd67dddd6ca",
];
const WAD_CONCAT_SHA256: &str = "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771";
const DRIVER_SOURCE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/bin/resource_sbf.rs"
));

const CONTEXT: Pubkey = Pubkey::new_from_array([0xe0; 32]);
const RESOURCE: Pubkey = Pubkey::new_from_array([0xe1; 32]);
const STATE: Pubkey = Pubkey::new_from_array([0xe2; 32]);
const MIRROR: Pubkey = Pubkey::new_from_array([0xe3; 32]);
const CONTROLLER: Pubkey = Pubkey::new_from_array([0xe4; 32]);
const BAD_CONTROLLER: Pubkey = Pubkey::new_from_array([0xe5; 32]);
const BAD_RESOURCE: Pubkey = Pubkey::new_from_array([0xe6; 32]);

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn json(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn account(data: Vec<u8>) -> Account {
    Account {
        lamports: 100_000_000_000,
        data,
        owner: PROGRAM,
        executable: false,
        rent_epoch: 0,
    }
}
fn meta(key: Pubkey, writable: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(key, false)
    } else {
        AccountMeta::new_readonly(key, false)
    }
}

fn wad_account(bytes: &[u8], ordinal: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    if bytes.len() != resource_sbf::WAD_ACCOUNT_BYTES
        || &bytes[..8] != b"DOOM-WC1"
        || u32_at(bytes, 8) != 1
        || u32_at(bytes, 12) as usize != resource_sbf::WAD_HEADER_BYTES
        || u32_at(bytes, 16) as usize != resource_sbf::WAD_ACCOUNT_BYTES
        || u32_at(bytes, 20) as usize != ordinal
    {
        return Err(format!("invalid captured WAD account {ordinal}").into());
    }
    let n = u32_at(bytes, 24) as usize;
    if n > resource_sbf::WAD_ACCOUNT_BYTES - resource_sbf::WAD_HEADER_BYTES
        || bytes[32 + n..].iter().any(|b| *b != 0)
    {
        return Err(format!("invalid WAD padding {ordinal}").into());
    }
    Ok(bytes[32..32 + n].to_vec())
}

fn padded_state(state: &[u8]) -> Vec<u8> {
    let mut out = vec![0; resource_sbf::STATE_MAX_BYTES];
    out[..state.len()].copy_from_slice(state);
    out
}

fn instruction(
    resource: Pubkey,
    controller: Pubkey,
    request: &resource_sbf::Request,
) -> Instruction {
    let mut accounts = vec![meta(CONTEXT, true), meta(resource, false)];
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        accounts.push(meta(
            Pubkey::new_from_array([0xf0 + ordinal as u8; 32]),
            false,
        ));
    }
    accounts.extend([
        meta(STATE, true),
        meta(MIRROR, true),
        meta(controller, false),
    ]);
    Instruction {
        program_id: PROGRAM,
        accounts,
        data: request.encode().to_vec(),
    }
}

async fn execute(
    client: &mut solana_banks_client::BanksClient,
    payer: &solana_keypair::Keypair,
    resource: Pubkey,
    controller: Pubkey,
    request: &resource_sbf::Request,
    compute_limit: u32,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[payer],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(compute_limit),
                instruction(resource, controller, request),
            ],
            Some(&payer.pubkey()),
            &blockhash,
        ),
        blockhash,
    );
    Ok(client.process_transaction_with_metadata(tx).await?)
}

fn program_test(
    image: &Path,
    resource: &[u8],
    bad_resource: &[u8],
    wad: &[Vec<u8>],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    bad: &[u8],
    compute_max_units: u32,
) -> ProgramTest {
    env::set_var("BPF_OUT_DIR", image.parent().unwrap());
    env::set_var("SBF_OUT_DIR", image.parent().unwrap());
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(compute_max_units as u64);
    bank.add_program("doom_sbf", PROGRAM, None);
    bank.add_account(CONTEXT, account(vec![0; resource_sbf::CONTEXT_BYTES]));
    bank.add_account(RESOURCE, account(resource.to_vec()));
    bank.add_account(BAD_RESOURCE, account(bad_resource.to_vec()));
    for (ordinal, bytes) in wad.iter().enumerate() {
        bank.add_account(
            Pubkey::new_from_array([0xf0 + ordinal as u8; 32]),
            account(bytes.clone()),
        );
    }
    bank.add_account(STATE, account(padded_state(state)));
    bank.add_account(MIRROR, account(mirror.to_vec()));
    bank.add_account(CONTROLLER, account(controller.to_vec()));
    bank.add_account(BAD_CONTROLLER, account(bad.to_vec()));
    bank
}

fn write_report(
    receipt: &Path,
    status: &str,
    blocker: &str,
    units: Option<u64>,
    controller_control: &str,
    resource_control: &str,
    error: Option<&str>,
    native_frame_sha256: &str,
    output_state_sha256: Option<&str>,
    output_mirror_sha256: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    fs::write(receipt.join("report.json"), format!("{{\n  \"status\": {},\n  \"profile\": \"DOOM-E2R/1\",\n  \"first_blocker\": {},\n  \"normal_limit\": {},\n  \"qualification_max_units\": {},\n  \"normal_units\": {},\n  \"malformed_controller_refusal\": {},\n  \"malformed_resource_refusal\": {},\n  \"normal_error\": {},\n  \"resource_image_bytes\": {},\n  \"resource_image_sha256\": \"{}\",\n  \"native_import_frame_sha256\": \"{}\",\n  \"sbf_frame_output\": \"not-exported-by-E2R/1\",\n  \"output_state_sha256\": {},\n  \"output_mirror_sha256\": {}\n}}\n", json(status), json(blocker), NORMAL_LIMIT, QUALIFICATION_MAX_UNITS, units.map_or("null".into(), |x| x.to_string()), json(controller_control), json(resource_control), error.map_or("null".into(), json), resource_sbf::RESOURCE_IMAGE_BYTES, RESOURCE_IMAGE_SHA256, native_frame_sha256, output_state_sha256.map_or("null".into(), json), output_mirror_sha256.map_or("null".into(), json)))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let capture = PathBuf::from(args.next().ok_or("usage: resource_sbf CAPTURE RECEIPT IMAGE EXPECTED_IMAGE_SHA256 MANIFEST EXPECTED_MANIFEST_SHA256 ABI DRIVER RESOURCE_IMAGE NATIVE_RECEIPT CONTROLLER")?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let image = PathBuf::from(args.next().ok_or("missing image")?).canonicalize()?;
    let expected_image_sha = args
        .next()
        .ok_or("missing expected image SHA-256")?
        .into_string()
        .map_err(|_| "image SHA not UTF-8")?;
    let manifest = PathBuf::from(args.next().ok_or("missing manifest")?).canonicalize()?;
    let expected_manifest_sha = args
        .next()
        .ok_or("missing expected manifest SHA-256")?
        .into_string()
        .map_err(|_| "manifest SHA not UTF-8")?;
    let abi = args
        .next()
        .ok_or("missing ABI")?
        .into_string()
        .map_err(|_| "ABI not UTF-8")?;
    let driver_sha = args
        .next()
        .ok_or("missing driver SHA")?
        .into_string()
        .map_err(|_| "driver SHA not UTF-8")?;
    let resource_path =
        PathBuf::from(args.next().ok_or("missing resource image")?).canonicalize()?;
    let native = PathBuf::from(args.next().ok_or("missing native RI1 receipt")?).canonicalize()?;
    let controller_path = PathBuf::from(args.next().ok_or("missing controller")?).canonicalize()?;
    let diagnostic = match args.next() {
        None => false,
        Some(flag) if flag == "--diagnostic" => true,
        Some(_) => return Err("optional final argument must be --diagnostic".into()),
    };
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err(format!("refusing non-empty receipt {}", receipt.display()).into());
    }
    fs::create_dir_all(&receipt)?;
    fs::write(
        receipt.join("run-mode.json"),
        format!(
            "{{\n  \"schema\": \"doom-e2r-run-mode/1\",\n  \"diagnostic_only\": {},\n  \"positive_compute_limit\": {},\n  \"qualification_compute_limit\": {}\n}}\n",
            diagnostic,
            if diagnostic { DIAGNOSTIC_LIMIT } else { NORMAL_LIMIT },
            NORMAL_LIMIT
        ),
    )?;
    if !image.is_file() || image.file_name().and_then(|x| x.to_str()) != Some("doom_sbf.so") {
        return Err("expected stripped deploy/doom_sbf.so".into());
    }
    let image_bytes = read(&image)?;
    let image_sha = digest(&image_bytes);
    if image_sha != expected_image_sha {
        return Err(format!(
            "image identity mismatch: expected {expected_image_sha}, actual {image_sha}"
        )
        .into());
    }
    let manifest_bytes = read(&manifest)?;
    let manifest_sha = digest(&manifest_bytes);
    if !std::str::from_utf8(&manifest_bytes)?.contains("\"sbf\": true") {
        return Err("manifest is not SBF".into());
    }
    let expected_abi = digest_hex_to_array(&abi)?;
    if expected_abi != resource_sbf::ABI_ID {
        return Err(format!("resource ABI mismatch: {abi}").into());
    }
    if manifest_sha != expected_manifest_sha
        || !std::str::from_utf8(&manifest_bytes)?.contains("\"target\": \"sbpf-solana-solana\"")
        || !std::str::from_utf8(&manifest_bytes)?.contains(&format!(
            "\"resource_sbf_abi_descriptor\": \"{}\"",
            resource_sbf::RESOURCE_SBF_ABI_DESCRIPTOR
        ))
    {
        return Err(format!("SBF resource source-manifest identity mismatch: expected {expected_manifest_sha}, actual {manifest_sha}").into());
    }
    if digest(DRIVER_SOURCE) != driver_sha {
        return Err("resource driver source digest mismatch".into());
    }
    fs::write(receipt.join("identity.json"), format!("{{\n  \"image_sha256\": \"{}\",\n  \"source_manifest_sha256\": \"{}\",\n  \"resource_abi_id\": \"{}\",\n  \"driver_sha256\": \"{}\"\n}}\n", image_sha, manifest_sha, abi, driver_sha))?;

    let resource_image = read(&resource_path)?;
    if resource_image.len() != resource_sbf::RESOURCE_IMAGE_BYTES
        || digest(&resource_image) != RESOURCE_IMAGE_SHA256
    {
        return Err("resource image identity mismatch".into());
    }
    let state = read(&capture.join("dumped-doom-st1.bin"))?;
    let mirror = read(&capture.join("mobj_pool.bin"))?;
    let controller = read(&controller_path)?;
    if !capture.join("account/manifest.json").is_file()
        || digest(&read(&capture.join("account/manifest.json"))?) != ACCOUNT_MANIFEST_SHA256
    {
        return Err("account manifest identity mismatch".into());
    }
    let mut wad_accounts = Vec::new();
    let mut wad_payload = Vec::new();
    for ordinal in 0..5 {
        let bytes = read(&capture.join(format!("account/wad-{ordinal:03}.bin")))?;
        if digest(&bytes) != WAD_ACCOUNT_SHA256[ordinal] {
            return Err(format!("WAD account {ordinal} identity mismatch").into());
        }
        wad_payload.extend_from_slice(&wad_account(&bytes, ordinal)?);
        wad_accounts.push(bytes);
    }
    if wad_payload.len() != resource_sbf::WAD_IMAGE_BYTES
        || digest(&wad_payload) != WAD_CONCAT_SHA256
    {
        return Err("WAD concatenation identity mismatch".into());
    }
    if controller.len() != resource_sbf::CONTROLLER_BYTES
        || &controller[..8] != b"DOOM-CI2"
        || u32_at(&controller, 20) != u32_at(&state, 20)
    {
        return Err("controller capture is not CI2".into());
    }
    if !String::from_utf8(read(&native.join("report.json"))?)
        .map_err(|_| "native RI1 report is not UTF-8")?
        .contains("\"status\": \"measured\"")
    {
        return Err("native RI1 receipt is not measured".into());
    }
    let expected_state = read(&native.join("import-state.st1"))?;
    let expected_mirror = read(&native.join("import-mirror.mp1"))?;
    let native_frame = read(&native.join("import-frame.fb1"))?;
    if native_frame.len() != resource::FRAME_BYTES {
        return Err("native RI1 receipt frame is not a full FB1 frame".into());
    }
    let native_frame_sha256 = digest(&native_frame);
    if expected_state.len() != state.len() || expected_mirror.len() != resource_sbf::MIRROR_BYTES {
        return Err("native RI1 output shape is not the retained ST1/MP1 shape".into());
    }
    let request = resource_sbf::Request {
        resource_bytes: resource_image.len(),
        state_len: state.len(),
        wad_bytes: wad_payload.len(),
    };
    let mut bad = controller.clone();
    bad[28] = 1;
    let mut bad_resource_image = resource_image.clone();
    bad_resource_image[256..260].copy_from_slice(&u32::MAX.to_le_bytes());
    let bank = program_test(
        &image,
        &resource_image,
        &bad_resource_image,
        &wad_accounts,
        &state,
        &mirror,
        &controller,
        &bad,
        if diagnostic { DIAGNOSTIC_LIMIT } else { NORMAL_LIMIT },
    );
    let (mut client, payer, _) = bank.start().await;
    let control = execute(
        &mut client,
        &payer,
        RESOURCE,
        BAD_CONTROLLER,
        &request,
        NORMAL_LIMIT,
    )
    .await?;
    let control_text = format!("{control:?}");
    if !control_text.contains("InvalidAccountData") {
        write_report(
            &receipt,
            "failed",
            "malformed-controller-control",
            None,
            &control_text,
            "not-run",
            None,
            &native_frame_sha256,
            None,
            None,
        )?;
        return Err("malformed controller was not refused".into());
    }
    let before = client
        .get_account(STATE)
        .await?
        .ok_or("state disappeared")?
        .data;
    let before_mirror = client
        .get_account(MIRROR)
        .await?
        .ok_or("mirror disappeared")?
        .data;
    if &before[..state.len()] != state || before_mirror != mirror {
        return Err("malformed controller mutated state or mirror".into());
    }
    let malformed_resource = execute(
        &mut client,
        &payer,
        BAD_RESOURCE,
        CONTROLLER,
        &request,
        NORMAL_LIMIT,
    )
    .await?;
    let malformed_resource_text = format!("{malformed_resource:?}");
    if !malformed_resource_text.contains("Custom(235)") {
        write_report(
            &receipt,
            "failed",
            "malformed-resource-control",
            None,
            &control_text,
            &malformed_resource_text,
            None,
            &native_frame_sha256,
            None,
            None,
        )?;
        return Err(format!(
            "malformed resource was not refused with E2R image code 235: {malformed_resource_text}"
        )
        .into());
    }
    let after_resource_state = client
        .get_account(STATE)
        .await?
        .ok_or("state disappeared after resource control")?
        .data;
    let after_resource_mirror = client
        .get_account(MIRROR)
        .await?
        .ok_or("mirror disappeared after resource control")?
        .data;
    if &after_resource_state[..state.len()] != state || after_resource_mirror != mirror {
        return Err("malformed resource mutated state or mirror".into());
    }
    let normal = execute(
        &mut client,
        &payer,
        RESOURCE,
        CONTROLLER,
        &request,
        if diagnostic { DIAGNOSTIC_LIMIT } else { NORMAL_LIMIT },
    )
    .await?;
    let units = normal.metadata.as_ref().map(|m| m.compute_units_consumed);
    if normal.result.is_err() {
        let e = format!("{normal:?}");
        write_report(
            &receipt,
            "failed-normal-budget",
            "real-resource-tic",
            units,
            &control_text,
            &malformed_resource_text,
            Some(&e),
            &native_frame_sha256,
            None,
            None,
        )?;
        return Err(format!("E2R real tic failed: {e}").into());
    }
    let output_state = client
        .get_account(STATE)
        .await?
        .ok_or("state disappeared after tic")?
        .data;
    let output_mirror = client
        .get_account(MIRROR)
        .await?
        .ok_or("mirror disappeared after tic")?
        .data;
    let state_equal = &output_state[..expected_state.len()] == expected_state;
    let mirror_equal = output_mirror == expected_mirror;
    if !state_equal || !mirror_equal {
        write_report(
            &receipt,
            "failed-output-mismatch",
            "st1-or-mp1-mismatch",
            units,
            &control_text,
            &malformed_resource_text,
            None,
            &native_frame_sha256,
            Some(&digest(&output_state)),
            Some(&digest(&output_mirror)),
        )?;
        return Err(
            format!("E2R output mismatch state={state_equal} mirror={mirror_equal}").into(),
        );
    }
    let Some(units) = units else {
        write_report(
            &receipt,
            "failed-missing-compute-units",
            "missing-compute-units",
            None,
            &control_text,
            &malformed_resource_text,
            None,
            &native_frame_sha256,
            None,
            None,
        )?;
        return Err("missing consumed CU".into());
    };
    fs::write(
        receipt.join("output-state.st1"),
        &output_state[..expected_state.len()],
    )?;
    fs::write(receipt.join("output-mirror.mp1"), &output_mirror)?;
    if units > QUALIFICATION_MAX_UNITS {
        write_report(
            &receipt,
            "failed-over-qualification-budget",
            "compute-units-over-qualification-threshold",
            Some(units),
            &control_text,
            &malformed_resource_text,
            None,
            &native_frame_sha256,
            Some(&digest(&output_state)),
            Some(&digest(&output_mirror)),
        )?;
        return Err(format!("E2R exceeded qualification target: {units} CU").into());
    }
    write_report(
        &receipt,
        "measured-accepted-normal-budget",
        "none",
        Some(units),
        &control_text,
        &malformed_resource_text,
        None,
        &native_frame_sha256,
        Some(&digest(&output_state)),
        Some(&digest(&output_mirror)),
    )?;
    println!(
        "E2R measured accepted: units={units} receipt={}",
        receipt.display()
    );
    Ok(())
}

fn digest_hex_to_array(value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    if value.len() != 64 {
        return Err("digest must be 64 hex characters".into());
    }
    let mut out = [0u8; 32];
    for (i, p) in value.as_bytes().chunks_exact(2).enumerate() {
        out[i] = ((p[0] as char).to_digit(16).ok_or("bad hex")? as u8) << 4
            | (p[1] as char).to_digit(16).ok_or("bad hex")? as u8;
    }
    Ok(out)
}
