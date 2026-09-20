//! Real DOOM-E2S/1 ProgramTest runner.
//!
//! The positive input is never synthesized: state, mirror, CI2 controller,
//! and expected native output are decoded from a retained native EX2 receipt;
//! the five WAD account images are read from the retained account-image
//! capture.  The supplied SBF image is loaded with `prefer_bpf(true)` and no
//! native processor.  A raised-limit rerun, when requested, is diagnostic and
//! never changes the acceptance label.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use doom_sbf::{ex2, ex2_sbf};
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_banks_interface::BanksTransactionResultWithMetadata;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_message::Message;
use solana_program_test::{ProgramTest, ProgramTestBanksClientExt};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xda; 32]);
const NORMAL_LIMIT: u32 = 1_400_000;
const DIAGNOSTIC_LIMIT: u32 = 3_000_000;
const QUALIFICATION_MAX_UNITS: u64 = 1_190_000;

const ACCOUNT_MANIFEST_SHA256: &str =
    "d23776b358184c77fba32df600bf565a56b6935d129aec6b9567ad3c1c999504";
const WAD_ACCOUNT_SHA256: [&str; ex2_sbf::WAD_ACCOUNT_COUNT] = [
    "122f11388481c3a6686305cf7d47e776ddaf4b41490f239e57458609befa4b9c",
    "5d62cc14b5556fb5f8a7187eb4c678abb0394fe05b26bd96b46a88ad1a1c3680",
    "813be339217ff8cc561f669b71d18629c1f5892a1a641b63b4e1a3c2dcf2f364",
    "20cc95f699d9a640c0edb4100184aeff2863aa7152492072f99db3fc131ad4a7",
    "ef64b046aaf61eeab85b3d9f10edc564f9b4b640c341d10afc6a9dd67dddd6ca",
];
const WAD_CONCAT_SHA256: &str = "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771";
const INPUT_EX2_SHA256: &str = "00fd78617ee51a925b7300db46c3c0452324c3ff362f2f6f6dda6bbc96fca327";
const OUTPUT_EX2_SHA256: &str = "9cd2e3d4df76dc49e6a6d2ccd3a0a9e9a20a2419c84940bbe70bde720ce2014b";

const DRIVER_SOURCE: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bin/ex2_sbf.rs"));

const CONTEXT: Pubkey = Pubkey::new_from_array([0xc0; 32]);
const STATE: Pubkey = Pubkey::new_from_array([0xc1; 32]);
const MIRROR: Pubkey = Pubkey::new_from_array([0xc2; 32]);
const CONTROLLER: Pubkey = Pubkey::new_from_array([0xc3; 32]);
const BAD_CONTROLLER: Pubkey = Pubkey::new_from_array([0xc4; 32]);

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?)
}

fn digest_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn digest_array_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_digest(value: &str, label: &str) -> Result<[u8; 32], Box<dyn Error>> {
    if value.len() != 64 {
        return Err(format!("{label} must be exactly 64 hexadecimal characters").into());
    }
    let mut digest = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or_else(|| format!("{label} is not hexadecimal"))?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("{label} is not hexadecimal"))?;
        digest[index] = ((high << 4) | low) as u8;
    }
    Ok(digest)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn validate_captured_wad_account(bytes: &[u8], ordinal: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    if bytes.len() != ex2_sbf::WAD_ACCOUNT_BYTES {
        return Err(format!("WAD account {ordinal} has {} bytes", bytes.len()).into());
    }
    if &bytes[..8] != b"DOOM-WC1"
        || u32_at(bytes, 8) != 1
        || u32_at(bytes, 12) as usize != ex2_sbf::WAD_HEADER_BYTES
        || u32_at(bytes, 16) as usize != ex2_sbf::WAD_ACCOUNT_BYTES
        || u32_at(bytes, 20) as usize != ordinal
    {
        return Err(format!("WAD account {ordinal} header is not DOOM-WC1/1").into());
    }
    let payload_len = u32_at(bytes, 24) as usize;
    if payload_len > ex2_sbf::WAD_ACCOUNT_BYTES - ex2_sbf::WAD_HEADER_BYTES
        || bytes[ex2_sbf::WAD_HEADER_BYTES + payload_len..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(format!("WAD account {ordinal} has nonzero padding").into());
    }
    Ok(bytes[ex2_sbf::WAD_HEADER_BYTES..ex2_sbf::WAD_HEADER_BYTES + payload_len].to_vec())
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

fn instruction(controller: Pubkey, request: &ex2_sbf::Request) -> Instruction {
    let mut accounts = vec![meta(CONTEXT, true)];
    for ordinal in 0..ex2_sbf::WAD_ACCOUNT_COUNT {
        accounts.push(meta(
            Pubkey::new_from_array([0xd0 + ordinal as u8; 32]),
            false,
        ));
    }
    accounts.push(meta(STATE, true));
    accounts.push(meta(MIRROR, true));
    accounts.push(meta(controller, false));
    Instruction {
        program_id: PROGRAM,
        accounts,
        data: request.encode().to_vec(),
    }
}

fn padded_state(state: &[u8]) -> Vec<u8> {
    let mut data = vec![0u8; ex2_sbf::STATE_MAX_BYTES];
    data[..state.len()].copy_from_slice(state);
    data
}

fn receipt_json(
    receipt: &PathBuf,
    status: &str,
    first_blocker: &str,
    normal_units: Option<u64>,
    diagnostic_units: Option<u64>,
    control: &str,
    normal_error: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let normal_error = normal_error
        .map(|error| format!("{error:?}"))
        .unwrap_or_else(|| "null".to_owned());
    fs::write(
        receipt.join("report.json"),
        format!(
            "{{\n  \"status\": {:?},\n  \"acceptance\": \"state-only-real-tic\",\n  \"first_blocker\": {:?},\n  \"normal_limit\": {},\n  \"qualification_max_units\": {},\n  \"normal_units\": {:?},\n  \"normal_error\": {},\n  \"diagnostic_limit\": {},\n  \"diagnostic_units\": {:?},\n  \"malformed_controller_refusal\": {:?},\n  \"identity\": \"identity.json\",\n  \"positive_fixture\": \"retained native EX2 receipt plus retained WAD account images\"\n}}\n",
            status,
            first_blocker,
            NORMAL_LIMIT,
            QUALIFICATION_MAX_UNITS,
            normal_units,
            normal_error,
            DIAGNOSTIC_LIMIT,
            diagnostic_units,
            control,
        ),
    )?;
    Ok(())
}

async fn execute(
    client: &mut solana_banks_client::BanksClient,
    payer: &solana_keypair::Keypair,
    controller: Pubkey,
    request: &ex2_sbf::Request,
    limit: u32,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[payer],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(limit),
                instruction(controller, request),
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
    limit: u32,
    wad_accounts: &[Vec<u8>],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    bad_controller: &[u8],
) -> ProgramTest {
    env::set_var("BPF_OUT_DIR", image.parent().unwrap());
    env::set_var("SBF_OUT_DIR", image.parent().unwrap());
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(limit as u64);
    bank.add_program("doom_sbf", PROGRAM, None);
    bank.add_account(CONTEXT, account(vec![0; ex2_sbf::CONTEXT_BYTES]));
    for ordinal in 0..ex2_sbf::WAD_ACCOUNT_COUNT {
        bank.add_account(
            Pubkey::new_from_array([0xd0 + ordinal as u8; 32]),
            account(wad_accounts[ordinal].clone()),
        );
    }
    bank.add_account(STATE, account(padded_state(state)));
    bank.add_account(MIRROR, account(mirror.to_vec()));
    bank.add_account(CONTROLLER, account(controller.to_vec()));
    bank.add_account(BAD_CONTROLLER, account(bad_controller.to_vec()));
    bank
}

fn write_identity(
    receipt: &Path,
    expected_image_sha256: &str,
    actual_image_sha256: &str,
    expected_manifest_sha256: &str,
    actual_manifest_sha256: &str,
    manifest_path: &Path,
    expected_sbf_abi_id: &str,
    expected_driver_sha256: &str,
    wad_hashes: &[String],
    wad_concat_sha256: &str,
    input_sha256: &str,
    output_sha256: &str,
    capture_mirror_sha256: &str,
) -> Result<(), Box<dyn Error>> {
    let wad_rows = wad_hashes
        .iter()
        .enumerate()
        .map(|(ordinal, digest)| {
            format!(
                "    {{\"ordinal\": {ordinal}, \"sha256\": \"{digest}\"}}{}\n",
                if ordinal + 1 == wad_hashes.len() {
                    ""
                } else {
                    ","
                }
            )
        })
        .collect::<String>();
    fs::write(
        receipt.join("identity.json"),
        format!(
            "{{\n  \"image_sha256_expected\": \"{expected_image_sha256}\",\n  \"image_sha256_actual\": \"{actual_image_sha256}\",\n  \"source_manifest_sha256_expected\": \"{expected_manifest_sha256}\",\n  \"source_manifest_sha256_actual\": \"{actual_manifest_sha256}\",\n  \"source_manifest_path\": {:?},\n  \"driver_compiled_host_source_manifest_sha256\": \"{}\",\n  \"sbf_abi_id_expected\": \"{expected_sbf_abi_id}\",\n  \"sbf_abi_id_actual\": \"{}\",\n  \"sbf_abi_descriptor\": {:?},\n  \"driver_source_sha256_expected\": \"{expected_driver_sha256}\",\n  \"driver_source_sha256_actual\": \"{}\",\n  \"account_manifest_sha256\": \"{ACCOUNT_MANIFEST_SHA256}\",\n  \"wad_accounts\": [\n{wad_rows}  ],\n  \"wad_concat_sha256\": \"{wad_concat_sha256}\",\n  \"native_input_ex2_sha256\": \"{input_sha256}\",\n  \"native_output_ex2_sha256\": \"{output_sha256}\",\n  \"capture_mirror_mp1_sha256\": \"{capture_mirror_sha256}\"\n}}\n",
            manifest_path.display(),
            ex2::SOURCE_MANIFEST_SHA256,
            digest_array_hex(&ex2_sbf::ABI_ID),
            ex2::SBF_ABI_DESCRIPTOR,
            digest_hex(DRIVER_SOURCE),
        ),
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let capture =
        PathBuf::from(args.next().ok_or(
            "usage: ex2_sbf ACCOUNT_CAPTURE_ROOT RECEIPT_DIR doom_sbf.so EXPECTED_IMAGE_SHA256 EXPECTED_SOURCE_MANIFEST_SHA256 EXPECTED_SOURCE_MANIFEST_PATH EXPECTED_SBF_ABI_ID EXPECTED_DRIVER_SOURCE_SHA256 NATIVE_RECEIPT_DIR",
        )?)
        .canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt dir")?);
    let so = PathBuf::from(args.next().ok_or("missing doom_sbf.so")?).canonicalize()?;
    let expected_image_sha256 = args
        .next()
        .ok_or("missing expected image SHA-256")?
        .into_string()
        .map_err(|_| "expected image SHA-256 is not UTF-8")?;
    let expected_manifest_sha256 = args
        .next()
        .ok_or("missing expected source-manifest SHA-256")?
        .into_string()
        .map_err(|_| "expected source-manifest SHA-256 is not UTF-8")?;
    let expected_manifest_path =
        PathBuf::from(args.next().ok_or("missing expected source-manifest path")?)
            .canonicalize()?;
    let expected_sbf_abi_id = args
        .next()
        .ok_or("missing expected SBF ABI id")?
        .into_string()
        .map_err(|_| "expected SBF ABI id is not UTF-8")?;
    let expected_driver_sha256 = args
        .next()
        .ok_or("missing expected driver-source SHA-256")?
        .into_string()
        .map_err(|_| "expected driver-source SHA-256 is not UTF-8")?;
    let native_receipt =
        PathBuf::from(args.next().ok_or("missing native receipt dir")?).canonicalize()?;
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    if !so.is_file() || so.file_name().and_then(|name| name.to_str()) != Some("doom_sbf.so") {
        return Err("expected a stripped deploy/doom_sbf.so rehearsal image".into());
    }
    let expected_image = parse_digest(&expected_image_sha256, "expected image SHA-256")?;
    let expected_sbf_abi = parse_digest(&expected_sbf_abi_id, "expected SBF ABI id")?;
    let expected_driver = parse_digest(&expected_driver_sha256, "expected driver-source SHA-256")?;
    let image_bytes = read(&so)?;
    let actual_image_sha256 = digest_hex(&image_bytes);
    if actual_image_sha256 != expected_image_sha256
        || digest_array_hex(&expected_image) != actual_image_sha256
    {
        return Err(format!(
            "image SHA-256 mismatch: expected {expected_image_sha256}, actual {actual_image_sha256}"
        )
        .into());
    }
    let sbf_manifest_bytes = read(&expected_manifest_path)?;
    let actual_manifest_sha256 = digest_hex(&sbf_manifest_bytes);
    let sbf_manifest = std::str::from_utf8(&sbf_manifest_bytes)?;
    let expected_abi_descriptor = format!("\"sbf_abi_descriptor\": {:?}", ex2::SBF_ABI_DESCRIPTOR);
    if expected_manifest_sha256 != actual_manifest_sha256
        || !sbf_manifest.contains("\"sbf\": true")
        || !sbf_manifest.contains("\"target\": \"sbpf-solana-solana\"")
        || !sbf_manifest.contains(&expected_abi_descriptor)
    {
        return Err(format!(
            "SBF source manifest identity mismatch: expected {expected_manifest_sha256}, actual {actual_manifest_sha256}, path {}",
            expected_manifest_path.display()
        )
        .into());
    }
    if expected_sbf_abi != ex2_sbf::ABI_ID || expected_sbf_abi != ex2::SBF_ABI_ID {
        return Err(format!(
            "SBF ABI identity mismatch: expected {expected_sbf_abi_id}, compiled {}",
            digest_array_hex(&ex2_sbf::ABI_ID)
        )
        .into());
    }
    let actual_driver_sha256 = digest_hex(DRIVER_SOURCE);
    if actual_driver_sha256 != expected_driver_sha256
        || digest_array_hex(&expected_driver) != actual_driver_sha256
    {
        return Err(format!(
            "driver-source SHA-256 mismatch: expected {expected_driver_sha256}, actual {actual_driver_sha256}"
        )
        .into());
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err(format!("refusing non-empty receipt directory {}", receipt.display()).into());
    }
    fs::create_dir_all(&receipt)?;

    let input_wire = read(&native_receipt.join("entry-0-input.ex2"))?;
    let output_wire = read(&native_receipt.join("entry-0-output.ex2"))?;
    let input_sha256 = digest_hex(&input_wire);
    let output_sha256 = digest_hex(&output_wire);
    if input_sha256 != INPUT_EX2_SHA256 || output_sha256 != OUTPUT_EX2_SHA256 {
        return Err(format!(
            "native EX2 capture digest mismatch: input {input_sha256}, output {output_sha256}"
        )
        .into());
    }
    let input = ex2::decode(&input_wire)?;
    let expected = ex2::decode(&output_wire)?;
    if input.state.len() > ex2_sbf::STATE_MAX_BYTES
        || input.mirror.len() != ex2_sbf::MIRROR_BYTES
        || input.controller.len() != ex2_sbf::CONTROLLER_BYTES
    {
        return Err("retained native receipt has an out-of-bound EX2 input".into());
    }
    let account_manifest = read(&capture.join("account/manifest.json"))?;
    if digest_hex(&account_manifest) != ACCOUNT_MANIFEST_SHA256 {
        return Err("retained account manifest digest differs from the pinned capture".into());
    }
    let capture_mirror = read(&capture.join("mobj_pool.bin"))?;
    let capture_mirror_sha256 = digest_hex(&capture_mirror);
    if capture_mirror != input.mirror {
        return Err("retained native EX2 mirror differs from the account-loader capture".into());
    }
    let mut wad_accounts = Vec::new();
    let mut wad_payload = Vec::new();
    let mut wad_hashes = Vec::new();
    for ordinal in 0..ex2_sbf::WAD_ACCOUNT_COUNT {
        let bytes = read(&capture.join(format!("account/wad-{ordinal:03}.bin")))?;
        let actual_hash = digest_hex(&bytes);
        if actual_hash != WAD_ACCOUNT_SHA256[ordinal] {
            return Err(
                format!("retained WAD account {ordinal} digest mismatch: {actual_hash}").into(),
            );
        }
        let payload = validate_captured_wad_account(&bytes, ordinal)?;
        wad_payload.extend_from_slice(&payload);
        wad_hashes.push(actual_hash);
        wad_accounts.push(bytes);
    }
    let wad_concat_sha256 = digest_hex(&wad_payload);
    if wad_payload.len() != ex2_sbf::WAD_IMAGE_BYTES || wad_concat_sha256 != WAD_CONCAT_SHA256 {
        return Err(format!(
            "retained concatenated WAD digest mismatch: {} bytes {wad_concat_sha256}",
            wad_payload.len()
        )
        .into());
    }
    if &input.mirror[36..68] != Sha256::digest(&wad_payload).as_slice() {
        return Err("native MP1 WAD binding differs from the pinned concatenated WAD".into());
    }
    let state_len = input.state.len();
    let request = ex2_sbf::Request {
        state_len,
        wad_bytes: ex2_sbf::WAD_IMAGE_BYTES,
    };
    let bad_controller = {
        let mut bytes = input.controller.to_vec();
        bytes[28] = 1;
        bytes
    };

    write_identity(
        &receipt,
        &expected_image_sha256,
        &actual_image_sha256,
        &expected_manifest_sha256,
        &actual_manifest_sha256,
        &expected_manifest_path,
        &expected_sbf_abi_id,
        &expected_driver_sha256,
        &wad_hashes,
        &wad_concat_sha256,
        &input_sha256,
        &output_sha256,
        &capture_mirror_sha256,
    )?;

    let bank = program_test(
        &so,
        NORMAL_LIMIT,
        &wad_accounts,
        input.state,
        input.mirror,
        input.controller,
        &bad_controller,
    );
    let (mut client, payer, _) = bank.start().await;

    let bad = execute(
        &mut client,
        &payer,
        BAD_CONTROLLER,
        &request,
        NORMAL_LIMIT,
    )
    .await?;
    let control = format!("{bad:?}");
    if !control.contains("InvalidAccountData") {
        receipt_json(
            &receipt,
            "failed",
            "malformed-controller-control",
            None,
            None,
            &control,
            None,
        )?;
        return Err(format!("malformed controller was not refused: {control}").into());
    }
    let after_control_state = client
        .get_account(STATE)
        .await?
        .ok_or("state account disappeared after control")?
        .data;
    if &after_control_state[..state_len] != input.state {
        return Err("malformed-controller control changed state bytes".into());
    }

    let normal = execute(
        &mut client,
        &payer,
        CONTROLLER,
        &request,
        NORMAL_LIMIT,
    )
    .await?;
    let normal_units = normal
        .metadata
        .as_ref()
        .map(|metadata| metadata.compute_units_consumed);
    if normal.result.is_ok() {
        let state = client
            .get_account(STATE)
            .await?
            .ok_or("state account disappeared after positive run")?
            .data;
        let mirror = client
            .get_account(MIRROR)
            .await?
            .ok_or("mirror account disappeared after positive run")?
            .data;
        let output_state = &state[..expected.state.len()];
        if output_state != expected.state || mirror != expected.mirror {
            receipt_json(
                &receipt,
                "failed",
                "pointer-free-output-mismatch",
                normal_units,
                None,
                &control,
                None,
            )?;
            return Err("real SBF tic output differs from retained native output".into());
        }
        let Some(units) = normal_units else {
            receipt_json(
                &receipt,
                "failed-missing-compute-units",
                "missing-compute-units",
                None,
                None,
                &control,
                None,
            )?;
            return Err("normal transaction succeeded without consumed-CU metadata".into());
        };
        if units > QUALIFICATION_MAX_UNITS {
            receipt_json(
                &receipt,
                "failed-over-qualification-budget",
                "compute-units-over-qualification-threshold",
                Some(units),
                None,
                &control,
                None,
            )?;
            return Err(format!(
                "real SBF tic exceeded qualification threshold: {units} > {QUALIFICATION_MAX_UNITS} CU"
            )
            .into());
        }
        let status = "measured-accepted-normal-budget";
        receipt_json(&receipt, status, "none", Some(units), None, &control, None)?;
        fs::write(receipt.join("output-state.st1"), output_state)?;
        fs::write(receipt.join("output-mirror.mp1"), &mirror)?;
        println!(
            "{status}: units={normal_units:?} receipt={}",
            receipt.display()
        );
        return Ok(());
    }

    let normal_error = format!("{normal:?}");
    let diagnostic_units = if env::var_os("DOOM_SBF_RUN_DIAGNOSTIC").is_some() {
        drop(client);
        drop(payer);
        let diagnostic_bank = program_test(
            &so,
            DIAGNOSTIC_LIMIT,
            &wad_accounts,
            input.state,
            input.mirror,
            input.controller,
            &bad_controller,
        );
        let (mut diagnostic_client, diagnostic_payer, _) = diagnostic_bank.start().await;
        let diagnostic = execute(
            &mut diagnostic_client,
            &diagnostic_payer,
            CONTROLLER,
            &request,
            DIAGNOSTIC_LIMIT,
        )
        .await?;
        let units = diagnostic
            .metadata
            .as_ref()
            .map(|metadata| metadata.compute_units_consumed);
        receipt_json(
            &receipt,
            "failed-normal-budget-diagnostic-run",
            &normal_error,
            normal_units,
            units,
            &control,
            Some(&normal_error),
        )?;
        Some(units.unwrap_or(0))
    } else {
        receipt_json(
            &receipt,
            "failed-normal-budget",
            &normal_error,
            normal_units,
            None,
            &control,
            Some(&normal_error),
        )?;
        None
    };
    println!(
        "SBF tic failed at normal budget: error={normal_error} units={normal_units:?} diagnostic_units={diagnostic_units:?} receipt={}",
        receipt.display()
    );
    Err("real SBF one-tic gate failed; see report.json for the first blocker".into())
}
