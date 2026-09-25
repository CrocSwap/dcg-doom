//! Actual DOOM-E2R/2 SBF ProgramTest gate.
//!
//! Every positive byte is read from the retained RI2/2 native receipt and the
//! retained account-image capture.  ProgramTest is configured to load the
//! built SBF image; no native processor or handcrafted state fixture is used.

use std::{env, error::Error, fs, path::{Path, PathBuf}};

use doom_sbf::{render_snapshot, resource_sbf, sim2};
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

const PROGRAM: Pubkey = Pubkey::new_from_array([0xd2; 32]);
const NORMAL_LIMIT: u32 = 1_400_000;
const DIAGNOSTIC_LIMIT: u32 = 30_000_000;
const QUALIFICATION_MAX_UNITS: u64 = 1_190_000;
const RESOURCE_IMAGE_SHA256: &str =
    "a8a716edc79b58c433fdbedefce6b842536b5ec00c2a1140768a6be1e3ab250a";
const ACCOUNT_MANIFEST_SHA256: &str =
    "d23776b358184c77fba32df600bf565a56b6935d129aec6b9567ad3c1c999504";
const WAD_CONCAT_SHA256: &str =
    "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771";
const WAD_ACCOUNT_SHA256: [&str; 5] = [
    "122f11388481c3a6686305cf7d47e776ddaf4b41490f239e57458609befa4b9c",
    "5d62cc14b5556fb5f8a7187eb4c678abb0394fe05b26bd96b46a88ad1a1c3680",
    "813be339217ff8cc561f669b71d18629c1f5892a1a641b63b4e1a3c2dcf2f364",
    "20cc95f699d9a640c0edb4100184aeff2863aa7152492072f99db3fc131ad4a7",
    "ef64b046aaf61eeab85b3d9f10edc564f9b4b640c341d10afc6a9dd67dddd6ca",
];
const DRIVER_SOURCE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bin/resource_ri2_sbf.rs"));

const CONTEXT: Pubkey = Pubkey::new_from_array([0xd0; 32]);
const RESOURCE: Pubkey = Pubkey::new_from_array([0xd1; 32]);
const BAD_RESOURCE: Pubkey = Pubkey::new_from_array([0xd3; 32]);
const STATE: Pubkey = Pubkey::new_from_array([0xd4; 32]);
const MIRROR: Pubkey = Pubkey::new_from_array([0xd5; 32]);
const CONTROLLER: Pubkey = Pubkey::new_from_array([0xd6; 32]);
const BAD_CONTROLLER: Pubkey = Pubkey::new_from_array([0xd7; 32]);
const FRAME: Pubkey = Pubkey::new_from_array([0xd8; 32]);

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn json(value: &str) -> String { serde_json::to_string(value).unwrap() }

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn account(data: Vec<u8>) -> Account {
    Account { lamports: 100_000_000_000, data, owner: PROGRAM, executable: false, rent_epoch: 0 }
}

fn meta(key: Pubkey, writable: bool) -> AccountMeta {
    if writable { AccountMeta::new(key, false) } else { AccountMeta::new_readonly(key, false) }
}

fn wad_account(bytes: &[u8], ordinal: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    if bytes.len() != resource_sbf::WAD_ACCOUNT_BYTES || &bytes[..8] != b"DOOM-WC1"
        || u32_at(bytes, 8) != 1 || u32_at(bytes, 12) as usize != resource_sbf::WAD_HEADER_BYTES
        || u32_at(bytes, 16) as usize != resource_sbf::WAD_ACCOUNT_BYTES
        || u32_at(bytes, 20) as usize != ordinal {
        return Err(format!("invalid retained WAD account {ordinal}").into());
    }
    let n = u32_at(bytes, 24) as usize;
    if n > resource_sbf::WAD_ACCOUNT_BYTES - resource_sbf::WAD_HEADER_BYTES
        || bytes[resource_sbf::WAD_HEADER_BYTES + n..].iter().any(|b| *b != 0) {
        return Err(format!("invalid WAD padding {ordinal}").into());
    }
    Ok(bytes[resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + n].to_vec())
}

fn padded_state(state: &[u8]) -> Vec<u8> {
    let mut out = vec![0; resource_sbf::STATE_MAX_BYTES];
    out[..state.len()].copy_from_slice(state);
    out
}

fn instruction(resource: Pubkey, controller: Pubkey, request_data: &[u8]) -> Instruction {
    let mut accounts = vec![meta(CONTEXT, true), meta(resource, false)];
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        accounts.push(meta(Pubkey::new_from_array([0xe0 + ordinal as u8; 32]), false));
    }
    accounts.extend([
        meta(STATE, true), meta(MIRROR, true), meta(controller, false), meta(FRAME, true),
    ]);
    Instruction { program_id: PROGRAM, accounts, data: request_data.to_vec() }
}

fn render_instruction(snapshot: Pubkey, strip_id: u8, request_data: &[u8]) -> Instruction {
    let scratch = render_snapshot::scratch_address(&PROGRAM, &snapshot, strip_id).0;
    let mut accounts = vec![meta(scratch, true), meta(RESOURCE, false)];
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        accounts.push(meta(Pubkey::new_from_array([0xe0 + ordinal as u8; 32]), false));
    }
    accounts.extend([
        meta(snapshot, false),
        meta(render_snapshot::strip_address(&PROGRAM, &snapshot, strip_id).0, true),
    ]);
    Instruction { program_id: PROGRAM, accounts, data: request_data.to_vec() }
}

async fn execute(
    client: &mut solana_banks_client::BanksClient,
    payer: &solana_keypair::Keypair,
    resource: Pubkey,
    controller: Pubkey,
    request_data: &[u8],
    limit: u32,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[payer],
        Message::new_with_blockhash(
            &[ComputeBudgetInstruction::set_compute_unit_limit(limit), instruction(resource, controller, request_data)],
            Some(&payer.pubkey()), &blockhash,
        ),
        blockhash,
    );
    Ok(client.process_transaction_with_metadata(tx).await?)
}

async fn execute_render(
    client: &mut solana_banks_client::BanksClient,
    payer: &solana_keypair::Keypair,
    snapshot: Pubkey,
    strip_id: u8,
    request_data: &[u8],
    limit: u32,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[payer],
        Message::new_with_blockhash(
            &[ComputeBudgetInstruction::set_compute_unit_limit(limit),
                render_instruction(snapshot, strip_id, request_data)],
            Some(&payer.pubkey()), &blockhash,
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
    bad_controller: &[u8],
    render_image: Option<(&[u8], u8)>,
    limit: u32,
) -> ProgramTest {
    env::set_var("BPF_OUT_DIR", image.parent().unwrap());
    env::set_var("SBF_OUT_DIR", image.parent().unwrap());
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(limit as u64);
    bank.add_program("doom_sbf", PROGRAM, None);
    bank.add_account(CONTEXT, account(vec![0; resource_sbf::CONTEXT_BYTES]));
    bank.add_account(RESOURCE, account(resource.to_vec()));
    bank.add_account(BAD_RESOURCE, account(bad_resource.to_vec()));
    for (ordinal, bytes) in wad.iter().enumerate() {
        bank.add_account(Pubkey::new_from_array([0xe0 + ordinal as u8; 32]), account(bytes.clone()));
    }
    bank.add_account(STATE, account(padded_state(state)));
    bank.add_account(MIRROR, account(mirror.to_vec()));
    bank.add_account(CONTROLLER, account(controller.to_vec()));
    bank.add_account(BAD_CONTROLLER, account(bad_controller.to_vec()));
    bank.add_account(FRAME, account(vec![0; resource_sbf::RI2_FRAME_BYTES]));
    if let Some((image, strip_id)) = render_image {
        let snapshot = render_snapshot::snapshot_address(&PROGRAM, &CONTEXT).0;
        bank.add_account(snapshot, account(image.to_vec()));
        let scratch = render_snapshot::scratch_address(&PROGRAM, &snapshot, strip_id).0;
        bank.add_account(scratch, account(vec![0; resource_sbf::CONTEXT_BYTES]));
        let strip = render_snapshot::strip_address(&PROGRAM, &snapshot, strip_id).0;
        bank.add_account(strip, account(vec![0; render_snapshot::STRIP_ACCOUNT_BYTES]));
    }
    bank
}

fn account_data(
    client: &solana_banks_client::BanksClient,
    key: Pubkey,
) -> impl std::future::Future<Output = Result<Vec<u8>, Box<dyn Error>>> + '_ {
    async move { Ok(client.get_account(key).await?.ok_or("account disappeared")?.data) }
}

fn write_report(
    receipt: &Path,
    status: &str,
    blocker: &str,
    normal_limit: u32,
    compute_limit: u32,
    units: Option<u64>,
    normal_limit_succeeded: Option<bool>,
    controller_control: &str,
    resource_control: &str,
    render_control: &str,
    error: Option<&str>,
    state_equal: Option<bool>,
    mirror_equal: Option<bool>,
    frame_equal: Option<bool>,
    render_mode: Option<u8>,
    strip_id: Option<u8>,
    frame_oracle_scope: &str,
    image_sha: &str,
    manifest_sha: &str,
    abi: &str,
) -> Result<(), Box<dyn Error>> {
    fs::write(receipt.join("report.json"), format!(
        "{{\n  \"status\": {},\n  \"profile\": {},\n  \"first_blocker\": {},\n  \"normal_limit\": {},\n  \"compute_limit\": {},\n  \"qualification_max_units\": {},\n  \"compute_units_consumed\": {},\n  \"normal_limit_succeeded\": {},\n  \"malformed_controller_refusal\": {},\n  \"malformed_resource_refusal\": {},\n  \"malformed_render_refusal\": {},\n  \"normal_error\": {},\n  \"image_sha256\": {},\n  \"source_manifest_sha256\": {},\n  \"resource_sbf_abi_id\": {},\n  \"resource_image_bytes\": {},\n  \"state_equal_native_oracle\": {},\n  \"mirror_equal_native_oracle\": {},\n  \"frame_equal_native_oracle\": {},\n  \"frame_oracle_scope\": {},\n  \"render_mode\": {},\n  \"strip_id\": {},\n  \"consumer_setup_calls\": 0,\n  \"public_send\": false\n}}\n",
        json(status), json(if render_mode.is_some() { "DOOM-E2R/3-SBF-CLIPPED-DIAGNOSTIC" } else { "DOOM-E2R/2-SBF-ACTUAL" }), json(blocker), normal_limit, compute_limit, QUALIFICATION_MAX_UNITS,
        units.map_or("null".to_owned(), |x| x.to_string()),
        normal_limit_succeeded.map_or("null".to_owned(), |x| x.to_string()), json(controller_control),
        json(resource_control), json(render_control), error.map_or("null".to_owned(), json), json(image_sha),
        json(manifest_sha), json(abi), resource_sbf::RI2_RESOURCE_IMAGE_BYTES,
        state_equal.map_or("null".to_owned(), |x| x.to_string()),
        mirror_equal.map_or("null".to_owned(), |x| x.to_string()),
        frame_equal.map_or("null".to_owned(), |x| x.to_string()),
        json(frame_oracle_scope),
        render_mode.map_or("null".to_owned(), |x| x.to_string()),
        strip_id.map_or("null".to_owned(), |x| x.to_string()),
    ))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let capture = PathBuf::from(args.next().ok_or("usage: resource_ri2_sbf CAPTURE RECEIPT IMAGE EXPECTED_IMAGE_SHA MANIFEST EXPECTED_MANIFEST_SHA ABI DRIVER RESOURCE_IMAGE NATIVE_RECEIPT CONTROLLER [--diagnostic | --clipped MODE STRIP]")?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let image = PathBuf::from(args.next().ok_or("missing image")?).canonicalize()?;
    let expected_image_sha = args.next().ok_or("missing expected image SHA")?.into_string().map_err(|_| "image SHA is not UTF-8")?;
    let manifest = PathBuf::from(args.next().ok_or("missing source manifest")?).canonicalize()?;
    let expected_manifest_sha = args.next().ok_or("missing manifest SHA")?.into_string().map_err(|_| "manifest SHA is not UTF-8")?;
    let abi = args.next().ok_or("missing RI2 SBF ABI ID")?.into_string().map_err(|_| "ABI is not UTF-8")?;
    let driver_sha = args.next().ok_or("missing driver SHA")?.into_string().map_err(|_| "driver SHA is not UTF-8")?;
    let resource_path = PathBuf::from(args.next().ok_or("missing RI2 resource image")?).canonicalize()?;
    let native = PathBuf::from(args.next().ok_or("missing native receipt")?).canonicalize()?;
    let controller_path = PathBuf::from(args.next().ok_or("missing controller")?).canonicalize()?;
    let (diagnostic, render_clip) = match args.next() {
        None => (false, None),
        Some(flag) if flag == "--diagnostic" => {
            if args.next().is_some() { return Err("unexpected argument".into()); }
            (true, None)
        }
        Some(flag) if flag == "--clipped" => {
            let mode: u8 = args.next().ok_or("missing clipped render mode")?.into_string()
                .map_err(|_| "render mode is not UTF-8")?.parse()?;
            let strip: u8 = args.next().ok_or("missing strip id")?.into_string()
                .map_err(|_| "strip id is not UTF-8")?.parse()?;
            if !(1..=2).contains(&mode) || strip >= 8 || args.next().is_some() {
                return Err("clipped mode must be 1 or 2 and strip must be 0..7".into());
            }
            (true, Some((mode, strip)))
        }
        Some(_) => return Err("unexpected argument".into()),
    };
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err(format!("refusing non-empty receipt {}", receipt.display()).into());
    }
    fs::create_dir_all(&receipt)?;
    let limit = if diagnostic { DIAGNOSTIC_LIMIT } else { NORMAL_LIMIT };
    fs::write(receipt.join("run-mode.json"), format!(
        "{{\n  \"schema\": \"doom-e2r3-sbf-run-mode/1\",\n  \"diagnostic_only\": {},\n  \"compute_limit\": {},\n  \"qualification_limit\": {},\n  \"render_mode\": {},\n  \"strip_id\": {}\n}}\n", diagnostic, limit, NORMAL_LIMIT,
        render_clip.map_or("null".to_owned(), |x| x.0.to_string()),
        render_clip.map_or("null".to_owned(), |x| x.1.to_string())))?;

    if image.file_name().and_then(|s| s.to_str()) != Some("doom_sbf.so") { return Err("expected doom_sbf.so".into()); }
    let image_bytes = read(&image)?;
    let image_actual_sha = sha(&image_bytes);
    if image_actual_sha != expected_image_sha { return Err(format!("image SHA mismatch: {image_actual_sha}").into()); }
    let manifest_bytes = read(&manifest)?;
    let manifest_text = std::str::from_utf8(&manifest_bytes)?;
    let manifest_actual_sha = sha(&manifest_bytes);
    let expected_abi = digest_hex_to_array(&abi)?;
    let expected_descriptor = if render_clip.is_some() {
        if expected_abi != resource_sbf::RI3_ABI_ID { return Err("RI3 ABI identity mismatch".into()); }
        resource_sbf::RESOURCE_RI3_SBF_ABI_DESCRIPTOR
    } else {
        if expected_abi != resource_sbf::RI2_ABI_ID { return Err("RI2 ABI identity mismatch".into()); }
        resource_sbf::RESOURCE_RI2_SBF_ABI_DESCRIPTOR
    };
    if manifest_actual_sha != expected_manifest_sha || !manifest_text.contains("\"sbf\": true")
        || !manifest_text.contains("\"target\": \"sbpf-solana-solana\"")
        || !manifest_text.contains(&format!("\"resource_ri{}_sbf_abi_descriptor\": \"{}\"", if render_clip.is_some() { 3 } else { 2 }, expected_descriptor)) {
        return Err("source manifest identity mismatch".into());
    }
    if sha(DRIVER_SOURCE) != driver_sha { return Err("driver source SHA mismatch".into()); }
    fs::write(receipt.join("identity.json"), format!(
        "{{\n  \"image_sha256\": \"{}\",\n  \"source_manifest_sha256\": \"{}\",\n  \"resource_sbf_abi_id\": \"{}\",\n  \"driver_sha256\": \"{}\"\n}}\n", image_actual_sha, manifest_actual_sha, abi, driver_sha))?;

    let resource_image = read(&resource_path)?;
    if resource_image.len() != resource_sbf::RI2_RESOURCE_IMAGE_BYTES || sha(&resource_image) != RESOURCE_IMAGE_SHA256 {
        return Err("retained RI2 image identity mismatch".into());
    }
    let state = read(&capture.join("dumped-doom-st1.bin"))?;
    let mirror = read(&capture.join("mobj_pool.bin"))?;
    let controller = read(&controller_path)?;
    let manifest_capture = read(&capture.join("account/manifest.json"))?;
    if sha(&manifest_capture) != ACCOUNT_MANIFEST_SHA256 { return Err("retained account manifest mismatch".into()); }
    if state.len() < 64 || &state[..8] != b"DOOM-ST1" || u32_at(&state, 16) as usize != state.len() {
        return Err("retained ST1 capture is invalid".into());
    }
    if mirror.len() != resource_sbf::MIRROR_BYTES || &mirror[..8] != b"DOOM-MP1" { return Err("retained MP1 capture is invalid".into()); }
    if controller.len() != resource_sbf::CONTROLLER_BYTES || &controller[..8] != b"DOOM-CI2" || u32_at(&controller, 20) != u32_at(&state, 20) { return Err("retained CI2 capture is invalid".into()); }
    let expected_state = read(&native.join("imported-state.st1"))?;
    let expected_mirror = read(&native.join("imported-mirror.mp1"))?;
    let expected_frame = read(&native.join("imported-frame.fb1"))?;
    let native_report = String::from_utf8(read(&native.join("report.json"))?)?;
    if !native_report.contains("\"status\": \"measured\"") || expected_frame.len() != resource_sbf::RI2_FRAME_BYTES { return Err("native RI2 receipt is not measured".into()); }
    if expected_state.len() != state.len() || expected_mirror.len() != resource_sbf::MIRROR_BYTES { return Err("native RI2 output shape mismatch".into()); }
    let render_snapshot_image = if let Some((1, strip_id)) = render_clip {
        let padded = padded_state(&expected_state);
        let (_, state_root) = sim2::verified_state_root(&padded, &expected_mirror)?;
        let mut image = vec![0u8; render_snapshot::SNAPSHOT_BYTES];
        render_snapshot::write_snapshot(
            &mut image, u32_at(&expected_state, 20), &state_root, &padded, &expected_mirror,
        )?;
        Some((image, strip_id))
    } else { None };
    let mut wad_accounts = Vec::new();
    let mut wad_payload = Vec::new();
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let bytes = read(&capture.join(format!("account/wad-{ordinal:03}.bin")))?;
        if sha(&bytes) != WAD_ACCOUNT_SHA256[ordinal] { return Err(format!("WAD {ordinal} identity mismatch").into()); }
        wad_payload.extend_from_slice(&wad_account(&bytes, ordinal)?);
        wad_accounts.push(bytes);
    }
    if wad_payload.len() != resource_sbf::WAD_IMAGE_BYTES || sha(&wad_payload) != WAD_CONCAT_SHA256 { return Err("WAD concat identity mismatch".into()); }
    let request = resource_sbf::Ri2Request { resource_bytes: resource_image.len(), state_len: state.len(), wad_bytes: wad_payload.len(), frame_bytes: resource_sbf::RI2_FRAME_BYTES };
    let request_data = if let Some((mode, strip)) = render_clip {
        resource_sbf::Ri3Request {
            resource_bytes: resource_image.len(), state_len: state.len(), wad_bytes: wad_payload.len(),
            frame_bytes: resource_sbf::RI2_FRAME_BYTES, render_mode: mode, strip_id: strip,
        }.encode().to_vec()
    } else { request.encode().to_vec() };
    let mut bad_controller = controller.clone();
    bad_controller[28] = 1;
    let mut bad_resource = resource_image.clone();
    bad_resource[256..260].copy_from_slice(&u32::MAX.to_le_bytes());
    let bank = program_test(
        &image, &resource_image, &bad_resource, &wad_accounts, &state, &mirror, &controller,
        &bad_controller,
        render_snapshot_image.as_ref().map(|(image, strip)| (image.as_slice(), *strip)),
        limit,
    );
    let (mut client, payer, _) = bank.start().await;

    let malformed_controller = execute(&mut client, &payer, RESOURCE, BAD_CONTROLLER, &request.encode(), NORMAL_LIMIT).await?;
    let controller_text = format!("{malformed_controller:?}");
    if !controller_text.contains("InvalidAccountData") { return Err(format!("malformed CI2 control failed: {controller_text}").into()); }
    let before_state = account_data(&client, STATE).await?;
    let before_mirror = account_data(&client, MIRROR).await?;
    let before_frame = account_data(&client, FRAME).await?;
    if before_state != padded_state(&state) || before_mirror != mirror || before_frame.iter().any(|b| *b != 0) { return Err("malformed CI2 mutated outputs".into()); }

    let malformed_resource = execute(&mut client, &payer, BAD_RESOURCE, CONTROLLER, &request.encode(), NORMAL_LIMIT).await?;
    let resource_text = format!("{malformed_resource:?}");
    if !resource_text.contains("Custom(240)") { return Err(format!("malformed RI2 control was not Custom(240): {resource_text}").into()); }
    if account_data(&client, STATE).await? != before_state || account_data(&client, MIRROR).await? != before_mirror || account_data(&client, FRAME).await? != before_frame { return Err("malformed RI2 mutated outputs".into()); }
    let render_text = if let Some((_, _)) = render_clip {
        let mut malformed_render = request_data.clone();
        malformed_render[13] = 3;
        let refusal = execute(&mut client, &payer, RESOURCE, CONTROLLER, &malformed_render, NORMAL_LIMIT).await?;
        let text = format!("{refusal:?}");
        if !text.contains("InvalidInstructionData") { return Err(format!("malformed RI3 render mode was not refused: {text}").into()); }
        if account_data(&client, STATE).await? != before_state || account_data(&client, MIRROR).await? != before_mirror || account_data(&client, FRAME).await? != before_frame { return Err("malformed RI3 mutated outputs".into()); }
        fs::write(receipt.join("malformed-render-refusal.txt"), &text)?;
        text
    } else { "not-applicable".to_owned() };

    let (normal, normal_limit_succeeded) = if render_clip.is_some() {
        let first = execute(&mut client, &payer, RESOURCE, CONTROLLER, &request_data, NORMAL_LIMIT).await?;
        if first.result.is_ok() {
            (first, Some(true))
        } else {
            fs::write(receipt.join("normal-limit-attempt.txt"), format!("{first:?}"))?;
            (execute(&mut client, &payer, RESOURCE, CONTROLLER, &request_data, DIAGNOSTIC_LIMIT).await?, Some(false))
        }
    } else {
        let transaction = execute(&mut client, &payer, RESOURCE, CONTROLLER, &request_data, limit).await?;
        let succeeded = if diagnostic { None } else { Some(transaction.result.is_ok()) };
        (transaction, succeeded)
    };
    let logs = normal.metadata.as_ref().map(|m| m.log_messages.join("\n")).unwrap_or_default();
    fs::write(receipt.join("tic-logs.txt"), logs)?;
    let units = normal.metadata.as_ref().map(|m| m.compute_units_consumed);
    let normal_limit_succeeded = if render_clip.is_some() {
        let fits = units.map(|value| value <= u64::from(NORMAL_LIMIT));
        if fits == Some(false) {
            fs::write(receipt.join("normal-limit-attempt.txt"), format!(
                "Measured CU exceeds the {NORMAL_LIMIT} CU threshold; ProgramTest bank maximum is {limit} CU, so this is a CU-fit comparison, not a limit-enforced success.\n"
            ))?;
        }
        fits
    } else {
        normal_limit_succeeded
    };
    if normal.result.is_err() {
        let text = format!("{normal:?}");
        write_report(&receipt, if diagnostic { "failed-diagnostic-runtime" } else { "failed-normal-budget" }, "actual-sbf-resource-tic", NORMAL_LIMIT, if normal_limit_succeeded == Some(false) { DIAGNOSTIC_LIMIT } else { limit }, units, normal_limit_succeeded, &controller_text, &resource_text, &render_text, Some(&text), None, None, None, render_clip.map(|x| x.0), render_clip.map(|x| x.1), if render_clip.is_some() { "40-column strip" } else { "full framebuffer" }, &image_actual_sha, &manifest_actual_sha, &abi)?;
        return Err(format!("actual RI2 SBF tic failed: {text}").into());
    }
    let output_state = account_data(&client, STATE).await?;
    let output_mirror = account_data(&client, MIRROR).await?;
    let output_frame = account_data(&client, FRAME).await?;
    let state_equal = &output_state[..expected_state.len()] == expected_state && output_state[expected_state.len()..].iter().all(|b| *b == 0);
    let mirror_equal = output_mirror == expected_mirror;
    let frame_equal = if let Some((_, strip)) = render_clip {
        (0..200).all(|row| {
            let start = row * 320 + usize::from(strip) * 40;
            output_frame[start..start + 40] == expected_frame[start..start + 40]
        })
    } else { output_frame == expected_frame };
    fs::write(receipt.join("output-state.st1"), &output_state[..expected_state.len()])?;
    fs::write(receipt.join("output-mirror.mp1"), &output_mirror)?;
    fs::write(receipt.join("output-frame.fb1"), &output_frame)?;
    if let Some((_, strip)) = render_clip {
        let mut pixels = Vec::with_capacity(40 * 200);
        for row in 0..200 {
            let start = row * 320 + usize::from(strip) * 40;
            pixels.extend_from_slice(&output_frame[start..start + 40]);
        }
        fs::write(receipt.join("output-strip.px8"), pixels)?;
    }
    if !state_equal || !mirror_equal || !frame_equal {
        write_report(&receipt, "failed-output-mismatch", "native-oracle-mismatch", NORMAL_LIMIT, if normal_limit_succeeded == Some(false) { DIAGNOSTIC_LIMIT } else { limit }, units, normal_limit_succeeded, &controller_text, &resource_text, &render_text, None, Some(state_equal), Some(mirror_equal), Some(frame_equal), render_clip.map(|x| x.0), render_clip.map(|x| x.1), if render_clip.is_some() { "40-column strip" } else { "full framebuffer" }, &image_actual_sha, &manifest_actual_sha, &abi)?;
        return Err(format!("actual RI2 SBF mismatch state={state_equal} mirror={mirror_equal} frame={frame_equal}").into());
    }
    if let Some((snapshot_image, strip_id)) = &render_snapshot_image {
        let snapshot_key = render_snapshot::snapshot_address(&PROGRAM, &CONTEXT).0;
        let snapshot = render_snapshot::validate_snapshot(snapshot_image)?;
        let request = render_snapshot::RenderRequest {
            strip_id: *strip_id, cursor: snapshot.cursor, snapshot_hash: snapshot.snapshot_hash,
        }.encode();
        let render = execute_render(
            &mut client, &payer, snapshot_key, *strip_id, &request, DIAGNOSTIC_LIMIT,
        ).await?;
        let render_units = render.metadata.as_ref().map(|m| m.compute_units_consumed);
        let render_logs = render.metadata.as_ref().map(|m| m.log_messages.join("\n")).unwrap_or_default();
        fs::write(receipt.join("render-only-strip-logs.txt"), &render_logs)?;
        let strip_key = render_snapshot::strip_address(&PROGRAM, &snapshot_key, *strip_id).0;
        let strip_data = account_data(&client, strip_key).await?;
        if strip_data.len() != render_snapshot::STRIP_ACCOUNT_BYTES {
            return Err("standalone render strip account size mismatch".into());
        }
        let expected_pixels = (0..200).flat_map(|row| {
            let start = row * 320 + usize::from(*strip_id) * 40;
            expected_frame[start..start + 40].iter().copied()
        }).collect::<Vec<_>>();
        let strip_view = if render.result.is_ok() {
            Some(render_snapshot::validate_strip(&strip_data)?)
        } else { None };
        let pixels_equal = strip_view.is_some_and(|view| {
            view.strip_id == u32::from(*strip_id)
                && view.cursor == snapshot.cursor
                && view.gametic == snapshot.gametic
                && view.snapshot_hash == snapshot.snapshot_hash
                && view.state_root == snapshot.state_root
                && view.pixels == expected_pixels
        });
        fs::write(receipt.join("render-only-strip.px8"), &strip_data[render_snapshot::STRIP_HEADER_BYTES..])?;
        fs::write(receipt.join("render-only-strip-report.json"), format!(
            "{{\n  \"status\": {},\n  \"compute_units_consumed\": {},\n  \"comparison_limit\": {},\n  \"strip_id\": {},\n  \"frame_tic\": {},\n  \"snapshot_hash\": {},\n  \"state_root\": {},\n  \"pixel_equal_native\": {},\n  \"transaction_ok\": {}\n}}\n",
            json(if pixels_equal { "measured-equal" } else { "failed" }),
            render_units.map_or("null".to_owned(), |value| value.to_string()),
            NORMAL_LIMIT, strip_id, snapshot.gametic, json(&hex(&snapshot.snapshot_hash)),
            json(&hex(&snapshot.state_root)), pixels_equal, render.result.is_ok(),
        ))?;
        if !pixels_equal { return Err(format!("standalone snapshot render mismatch: {render:?}").into()); }
    }
    let Some(units) = units else { return Err("actual SBF run did not report compute units".into()); };
    let status = if render_clip.is_some() && normal_limit_succeeded == Some(true) { "measured-within-normal-limit" } else if diagnostic { "measured-diagnostic-only-equality" } else if units > QUALIFICATION_MAX_UNITS { "failed-over-qualification-budget" } else { "measured-accepted-normal-budget" };
    let blocker = if diagnostic && normal_limit_succeeded != Some(true) { "diagnostic-not-normal-limit" } else if units > QUALIFICATION_MAX_UNITS { "compute-units-over-qualification-threshold" } else { "none" };
    write_report(&receipt, status, blocker, NORMAL_LIMIT, if normal_limit_succeeded == Some(false) { DIAGNOSTIC_LIMIT } else { limit }, Some(units), normal_limit_succeeded, &controller_text, &resource_text, &render_text, None, Some(true), Some(true), Some(frame_equal), render_clip.map(|x| x.0), render_clip.map(|x| x.1), if render_clip.is_some() { "40-column strip" } else { "full framebuffer" }, &image_actual_sha, &manifest_actual_sha, &abi)?;
    println!("RI2 SBF {status}: units={units} receipt={}", receipt.display());
    if !diagnostic && units > QUALIFICATION_MAX_UNITS { return Err(format!("RI2 SBF over qualification threshold: {units}").into()); }
    Ok(())
}

fn digest_hex_to_array(value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    if value.len() != 64 { return Err("digest must be 64 hex characters".into()); }
    let mut out = [0u8; 32];
    for (i, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        out[i] = (pair[0] as char).to_digit(16).ok_or("bad hex")? as u8 * 16
            + (pair[1] as char).to_digit(16).ok_or("bad hex")? as u8;
    }
    Ok(out)
}
