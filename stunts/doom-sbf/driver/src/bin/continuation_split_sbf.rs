//! Actual DOOM-E2R/4 bounded reconstruction sequence.
//!
//! Every positive phase is a separate `prefer_bpf(true)` ProgramTest
//! transaction.  The driver records the real SBF receipts and stops being a
//! qualification claim if any phase exceeds the normal-limit gate.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use doom_sbf::continuation_split_sbf::{self, Request};
use doom_sbf::resource_sbf;
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_banks_interface::BanksTransactionResultWithMetadata;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_message::Message;
use solana_program_test::ProgramTest;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::{instruction::create_account, program as system_program};
use solana_transaction::Transaction;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xd2; 32]);
const NORMAL_LIMIT: u32 = 1_400_000;
const QUALIFICATION_MAX_UNITS: u64 = 1_190_000;
const ACCOUNT_MANIFEST_SHA256: &str =
    "d23776b358184c77fba32df600bf565a56b6935d129aec6b9567ad3c1c999504";
const WAD_ACCOUNT_SHA256: [&str; 5] = [
    "122f11388481c3a6686305cf7d47e776ddaf4b41490f239e57458609befa4b9c",
    "5d62cc14b5556fb5f8a7187eb4c678abb0394fe05b26bd96b46a88ad1a1c3680",
    "813be339217ff8cc561f669b71d18629c1f5892a1a641b63b4e1a3c2dcf2f364",
    "20cc95f699d9a640c0edb4100184aeff2863aa7152492072f99db3fc131ad4a7",
    "ef64b046aaf61eeab85b3d9f10edc564f9b4b640c341d10afc6a9dd67dddd6ca",
];
const WAD_CONCAT_SHA256: &str = "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771";
const RESOURCE_IMAGE_SHA256: &str =
    "c3531934d3a2b271330653074457d8570e379a22ed32eb9225df6826a090ac50";
const DRIVER_SOURCE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/bin/continuation_split_sbf.rs"
));

const CONTEXT: Pubkey = Pubkey::new_from_array([0xe0; 32]);
const RESOURCE: Pubkey = Pubkey::new_from_array([0xe1; 32]);
const STATE: Pubkey = Pubkey::new_from_array([0xe2; 32]);
const MIRROR: Pubkey = Pubkey::new_from_array([0xe3; 32]);
const CONTROLLER: Pubkey = Pubkey::new_from_array([0xe4; 32]);
const FRAME: Pubkey = Pubkey::new_from_array([0xe5; 32]);
const BAD_RESOURCE: Pubkey = Pubkey::new_from_array([0xe6; 32]);
const BAD_CONTROLLER: Pubkey = Pubkey::new_from_array([0xe7; 32]);
const BAD_STATE: Pubkey = Pubkey::new_from_array([0xe8; 32]);
const BAD_CONTROLLER_KEY: Pubkey = Pubkey::new_from_array([0xe9; 32]);

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn json(value: &str) -> String {
    serde_json::to_string(value).unwrap()
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
fn meta(key: Pubkey, writable: bool, signer: bool) -> AccountMeta {
    AccountMeta {
        pubkey: key,
        is_signer: signer,
        is_writable: writable,
    }
}

fn wad_account(bytes: &[u8], ordinal: usize) -> Result<(), Box<dyn Error>> {
    if bytes.len() != resource_sbf::WAD_ACCOUNT_BYTES
        || sha(bytes) != WAD_ACCOUNT_SHA256[ordinal]
        || &bytes[..8] != b"DOOM-WC1"
        || u32_at(bytes, 8) != 1
        || u32_at(bytes, 12) != 32
        || u32_at(bytes, 16) as usize != resource_sbf::WAD_ACCOUNT_BYTES
        || u32_at(bytes, 20) as usize != ordinal
    {
        return Err(format!("invalid retained WAD account {ordinal}").into());
    }
    let payload = u32_at(bytes, 24) as usize;
    if payload > resource_sbf::WAD_ACCOUNT_BYTES - resource_sbf::WAD_HEADER_BYTES
        || bytes[resource_sbf::WAD_HEADER_BYTES + payload..]
            .iter()
            .any(|&b| b != 0)
    {
        return Err(format!("invalid WAD padding {ordinal}").into());
    }
    Ok(())
}

fn padded_state(state: &[u8]) -> Vec<u8> {
    let mut out = vec![0; resource_sbf::STATE_MAX_BYTES];
    out[..state.len()].copy_from_slice(state);
    out
}

fn instruction(
    authority: Pubkey,
    resource: Pubkey,
    state: Pubkey,
    controller: Pubkey,
    continuation: Pubkey,
    request: Request,
) -> Instruction {
    let mut accounts = vec![meta(CONTEXT, true, false), meta(resource, false, false)];
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        accounts.push(meta(
            Pubkey::new_from_array([0xf0 + ordinal as u8; 32]),
            false,
            false,
        ));
    }
    accounts.extend([
        meta(state, true, false),
        meta(MIRROR, true, false),
        meta(controller, false, false),
        meta(FRAME, true, false),
        meta(continuation, true, false),
        meta(authority, false, true),
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
    authority: &solana_keypair::Keypair,
    compute_limit: u32,
    resource: Pubkey,
    state: Pubkey,
    controller: Pubkey,
    continuation: Pubkey,
    request: Request,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[payer, authority],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(compute_limit),
                instruction(
                    authority.pubkey(),
                    resource,
                    state,
                    controller,
                    continuation,
                    request,
                ),
            ],
            Some(&payer.pubkey()),
            &blockhash,
        ),
        blockhash,
    );
    Ok(client.process_transaction_with_metadata(tx).await?)
}

fn program_test(
    resource: &[u8],
    bad_resource: &[u8],
    wad: &[Vec<u8>],
    state: &[u8],
    bad_state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    bad_controller: &[u8],
    bad_controller_key: &[u8],
    authority: Pubkey,
    compute_limit: u32,
) -> ProgramTest {
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(compute_limit as u64);
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
    bank.add_account(STATE, account(state.to_vec()));
    bank.add_account(BAD_STATE, account(bad_state.to_vec()));
    bank.add_account(MIRROR, account(mirror.to_vec()));
    bank.add_account(CONTROLLER, account(controller.to_vec()));
    bank.add_account(BAD_CONTROLLER, account(bad_controller.to_vec()));
    bank.add_account(BAD_CONTROLLER_KEY, account(bad_controller_key.to_vec()));
    bank.add_account(FRAME, account(vec![0; continuation_split_sbf::FRAME_BYTES]));
    bank.add_account(
        authority,
        Account {
            lamports: 100_000_000_000,
            data: Vec::new(),
            owner: system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    bank
}

async fn account_data(
    client: &solana_banks_client::BanksClient,
    key: Pubkey,
) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(client
        .get_account(key)
        .await?
        .ok_or("account disappeared")?
        .data)
}

async fn create_continuation(
    client: &mut solana_banks_client::BanksClient,
    payer: &solana_keypair::Keypair,
    continuation: &solana_keypair::Keypair,
) -> Result<(), Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[payer, continuation],
        Message::new_with_blockhash(
            &[create_account(
                &payer.pubkey(),
                &payer.pubkey(),
                &continuation.pubkey(),
                100_000_000_000,
                continuation_split_sbf::CONTINUATION_BYTES as u64,
                &PROGRAM,
            )],
            Some(&payer.pubkey()),
            &blockhash,
        ),
        blockhash,
    );
    client.process_transaction(tx).await?;
    Ok(())
}

fn failure_text(result: &BanksTransactionResultWithMetadata) -> String {
    format!("{:?}", result.result)
}
fn require_refusal(
    result: &BanksTransactionResultWithMetadata,
    needle: &str,
) -> Result<(), Box<dyn Error>> {
    let text = failure_text(result);
    if !text.contains(needle) || result.result.is_ok() {
        return Err(format!("expected refusal {needle}, got {text}").into());
    }
    Ok(())
}
fn units(result: &BanksTransactionResultWithMetadata) -> Result<u64, Box<dyn Error>> {
    Ok(result
        .metadata
        .as_ref()
        .ok_or("compute metadata missing")?
        .compute_units_consumed)
}
fn logs(result: &BanksTransactionResultWithMetadata) -> String {
    result
        .metadata
        .as_ref()
        .and_then(|m| m.log_messages.as_ref())
        .map(|l| l.join("\n") + "\n")
        .unwrap_or_default()
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let capture = PathBuf::from(args.next().ok_or("usage: continuation_split_sbf CAPTURE RECEIPT IMAGE EXPECTED_IMAGE_SHA MANIFEST EXPECTED_MANIFEST_SHA ABI DRIVER RESOURCE_IMAGE CONTROLLER")?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let image = PathBuf::from(args.next().ok_or("missing image")?).canonicalize()?;
    let expected_image = args
        .next()
        .ok_or("missing image SHA")?
        .into_string()
        .map_err(|_| "image SHA is not UTF-8")?;
    let manifest = PathBuf::from(args.next().ok_or("missing manifest")?).canonicalize()?;
    let expected_manifest = args
        .next()
        .ok_or("missing manifest SHA")?
        .into_string()
        .map_err(|_| "manifest SHA is not UTF-8")?;
    let abi = args
        .next()
        .ok_or("missing ABI")?
        .into_string()
        .map_err(|_| "ABI is not UTF-8")?;
    let expected_driver = args
        .next()
        .ok_or("missing driver SHA")?
        .into_string()
        .map_err(|_| "driver SHA is not UTF-8")?;
    let resource_path = PathBuf::from(args.next().ok_or("missing RI2 resource")?).canonicalize()?;
    let controller_path = PathBuf::from(args.next().ok_or("missing CI2")?).canonicalize()?;
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err(format!("refusing non-empty receipt {}", receipt.display()).into());
    }
    fs::create_dir_all(&receipt)?;
    let image_bytes = read(&image)?;
    let manifest_bytes = read(&manifest)?;
    if sha(&image_bytes) != expected_image
        || sha(&manifest_bytes) != expected_manifest
        || sha(DRIVER_SOURCE) != expected_driver
        || abi != sha(continuation_split_sbf::ABI_DESCRIPTOR.as_bytes())
    {
        return Err("split image/source/ABI identity mismatch".into());
    }
    if !String::from_utf8_lossy(&manifest_bytes).contains("\"sbf\": true") {
        return Err("manifest is not an SBF manifest".into());
    }
    let image_dir = image.parent().ok_or("SBF image has no parent")?;
    env::set_var("BPF_OUT_DIR", image_dir);
    env::set_var("SBF_OUT_DIR", image_dir);
    let resource = read(&resource_path)?;
    if resource.len() != resource_sbf::RI2_RESOURCE_IMAGE_BYTES
        || sha(&resource) != RESOURCE_IMAGE_SHA256
    {
        return Err("retained RI2 resource identity mismatch".into());
    }
    if sha(&read(&capture.join("account/manifest.json"))?) != ACCOUNT_MANIFEST_SHA256 {
        return Err("retained account manifest mismatch".into());
    }
    let mut wad = Vec::new();
    let mut wad_payload = Vec::new();
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let bytes = read(&capture.join(format!("account/wad-{ordinal:03}.bin")))?;
        wad_account(&bytes, ordinal)?;
        wad_payload.extend_from_slice(
            &bytes[resource_sbf::WAD_HEADER_BYTES
                ..resource_sbf::WAD_HEADER_BYTES + u32_at(&bytes, 24) as usize],
        );
        wad.push(bytes);
    }
    if wad_payload.len() != resource_sbf::WAD_IMAGE_BYTES || sha(&wad_payload) != WAD_CONCAT_SHA256
    {
        return Err("retained WAD concatenation mismatch".into());
    }
    let state_capture = read(&capture.join("dumped-doom-st1.bin"))?;
    let state = padded_state(&state_capture);
    let mirror = read(&capture.join("mobj_pool.bin"))?;
    let controller = read(&controller_path)?;
    if state_capture.len() < 64
        || &state_capture[..8] != b"DOOM-ST1"
        || u32_at(&state_capture, 16) as usize != state_capture.len()
        || mirror.len() != continuation_split_sbf::MIRROR_BYTES
        || controller.len() != continuation_split_sbf::CONTROLLER_BYTES
    {
        return Err("captured input shape mismatch".into());
    }
    let compute_limit = env::var("DOOM_CONTINUATION_SPLIT_COMPUTE_LIMIT")
        .ok()
        .map(|v| v.parse::<u32>())
        .transpose()?
        .unwrap_or(NORMAL_LIMIT);
    if compute_limit < NORMAL_LIMIT {
        return Err(
            format!("compute limit {compute_limit} below normal limit {NORMAL_LIMIT}").into(),
        );
    }
    let mut bad_controller = controller.clone();
    bad_controller[28] = 1;
    let mut bad_state = state.clone();
    bad_state[64] ^= 1;
    let request = Request {
        operation: continuation_split_sbf::PREPARE,
        state_len: state_capture.len(),
        cursor: 0,
    };
    let authority = solana_keypair::Keypair::new();
    let continuation = solana_keypair::Keypair::new();
    let bank = program_test(
        &resource,
        &resource,
        &wad,
        &state,
        &bad_state,
        &mirror,
        &controller,
        &bad_controller,
        &controller,
        authority.pubkey(),
        compute_limit,
    );
    let (mut client, payer, _) = bank.start().await;
    create_continuation(&mut client, &payer, &continuation).await?;
    let continuation_key = continuation.pubkey();
    let before_state = account_data(&client, STATE).await?;
    let before_mirror = account_data(&client, MIRROR).await?;
    let before_frame = account_data(&client, FRAME).await?;
    let before_cont = account_data(&client, continuation_key).await?;
    let out_of_order = execute(
        &mut client,
        &payer,
        &authority,
        compute_limit,
        RESOURCE,
        STATE,
        CONTROLLER,
        continuation_key,
        Request {
            operation: continuation_split_sbf::STEP,
            state_len: state_capture.len(),
            cursor: 1,
        },
    )
    .await?;
    require_refusal(&out_of_order, "Custom(241)")?;
    if account_data(&client, STATE).await? != before_state
        || account_data(&client, MIRROR).await? != before_mirror
        || account_data(&client, FRAME).await? != before_frame
        || account_data(&client, continuation_key).await? != before_cont
    {
        return Err("out-of-order refusal mutated durable accounts".into());
    }
    let prepare = execute(
        &mut client,
        &payer,
        &authority,
        compute_limit,
        RESOURCE,
        STATE,
        CONTROLLER,
        continuation_key,
        request,
    )
    .await?;
    let prepare_units = units(&prepare)?;
    if prepare.result.is_err() {
        return Err(format!("prepare failed {:?} CU={prepare_units}", prepare.result).into());
    }
    fs::write(receipt.join("prepare.logs"), logs(&prepare))?;
    let prepared = account_data(&client, continuation_key).await?;
    for (label, resource_key, state_key, controller_key) in [
        ("changed_st1_key", RESOURCE, BAD_STATE, CONTROLLER),
        ("changed_ri2_key", BAD_RESOURCE, STATE, CONTROLLER),
        ("changed_ci2_key", RESOURCE, STATE, BAD_CONTROLLER_KEY),
    ] {
        let refusal = execute(
            &mut client,
            &payer,
            &authority,
            compute_limit,
            resource_key,
            state_key,
            controller_key,
            continuation_key,
            Request {
                operation: continuation_split_sbf::STEP,
                state_len: state_capture.len(),
                cursor: 1,
            },
        )
        .await?;
        require_refusal(&refusal, "Custom(242)")?;
        if account_data(&client, continuation_key).await? != prepared
            || account_data(&client, STATE).await? != before_state
            || account_data(&client, MIRROR).await? != before_mirror
            || account_data(&client, FRAME).await? != before_frame
        {
            return Err(format!("{label} refusal mutated durable accounts").into());
        }
    }
    let mut phase_units = Vec::new();
    for cursor in 1..=6u32 {
        let step = execute(
            &mut client,
            &payer,
            &authority,
            compute_limit,
            RESOURCE,
            STATE,
            CONTROLLER,
            continuation_key,
            Request {
                operation: continuation_split_sbf::STEP,
                state_len: state_capture.len(),
                cursor,
            },
        )
        .await?;
        let cu = units(&step)?;
        fs::write(receipt.join(format!("phase-{cursor}.logs")), logs(&step))?;
        if step.result.is_err() {
            fs::write(receipt.join("phase-failure.txt"), failure_text(&step))?;
            return Err(format!("phase {cursor} failed {:?} CU={cu}", step.result).into());
        }
        phase_units.push(cu);
    }
    let output_state = account_data(&client, STATE).await?;
    let output_mirror = account_data(&client, MIRROR).await?;
    let output_frame = account_data(&client, FRAME).await?;
    if output_state != state
        || output_mirror != mirror
        || output_frame != vec![0; continuation_split_sbf::FRAME_BYTES]
    {
        return Err("split output is not oracle-equal captured input".into());
    }
    let replay = execute(
        &mut client,
        &payer,
        &authority,
        compute_limit,
        RESOURCE,
        STATE,
        CONTROLLER,
        continuation_key,
        Request {
            operation: continuation_split_sbf::STEP,
            state_len: state_capture.len(),
            cursor: 6,
        },
    )
    .await?;
    require_refusal(&replay, "Custom(241)")?;
    if account_data(&client, STATE).await? != output_state
        || account_data(&client, MIRROR).await? != output_mirror
        || account_data(&client, FRAME).await? != output_frame
    {
        return Err("replay refusal mutated outputs".into());
    }

    // Same-key WAD ordinal mutation is checked in a fresh bank, before any
    // continuation write.  This is the retained E2R/3 control carried over.
    let mut mutated_wad = wad.clone();
    mutated_wad[4][20..24].copy_from_slice(&3u32.to_le_bytes());
    let wad_continuation = solana_keypair::Keypair::new();
    let wad_bank = program_test(
        &resource,
        &resource,
        &mutated_wad,
        &state,
        &bad_state,
        &mirror,
        &controller,
        &bad_controller,
        &controller,
        authority.pubkey(),
        compute_limit,
    );
    let (mut wad_client, wad_payer, _) = wad_bank.start().await;
    create_continuation(&mut wad_client, &wad_payer, &wad_continuation).await?;
    let wad_before = account_data(&wad_client, wad_continuation.pubkey()).await?;
    let wad_result = execute(
        &mut wad_client,
        &wad_payer,
        &authority,
        compute_limit,
        RESOURCE,
        STATE,
        CONTROLLER,
        wad_continuation.pubkey(),
        request,
    )
    .await?;
    require_refusal(&wad_result, "InvalidAccountData")?;
    if account_data(&wad_client, wad_continuation.pubkey()).await? != wad_before {
        return Err("same-key WAD refusal mutated continuation".into());
    }
    fs::write(
        receipt.join("same-key-wad-mutated-account.bin"),
        &mutated_wad[4],
    )?;

    fs::write(receipt.join("output-state.st1"), &output_state)?;
    fs::write(receipt.join("output-mirror.mp1"), &output_mirror)?;
    fs::write(receipt.join("output-frame.fb1"), &output_frame)?;
    fs::write(
        receipt.join("continuation-published.bin"),
        account_data(&client, continuation_key).await?,
    )?;
    let qualification = prepare_units <= QUALIFICATION_MAX_UNITS
        && phase_units.iter().all(|&cu| cu <= QUALIFICATION_MAX_UNITS);
    fs::write(
        receipt.join("controls.json"),
        r#"{
  "out_of_order_step": {"refused": true, "outputs_preserved": true},
  "replay_step": {"refused": true, "outputs_preserved": true},
  "same_key_wad_mutation": {"mutation": "WC1 ordinal header", "refused": true, "continuation_preserved": true}
}
"#,
    )?;
    fs::write(receipt.join("identity.json"), format!("{{\n  \"profile\": \"DOOM-E2R/4-BOUNDED-SPLIT\",\n  \"image_sha256\": {},\n  \"manifest_sha256\": {},\n  \"driver_sha256\": {},\n  \"continuation_abi_id\": {},\n  \"ri2_source_abi_id\": {},\n  \"resource_sha256\": {},\n  \"wad_sha256\": {},\n  \"prepare_units\": {},\n  \"phase_units\": {:?},\n  \"normal_limit\": {},\n  \"qualification_max_units\": {},\n  \"oracle_equal\": true,\n  \"tic_render\": \"not-run; split stops after pre-tic decode/encode\"\n}}\n", json(&expected_image), json(&expected_manifest), json(&expected_driver), json(&abi), json(&sha(&resource_sbf::RI2_ABI_ID)), json(&sha(&resource)), json(WAD_CONCAT_SHA256), prepare_units, phase_units, NORMAL_LIMIT, QUALIFICATION_MAX_UNITS))?;
    fs::write(receipt.join("run.json"), format!("{{\n  \"compute_limit\": {compute_limit},\n  \"prepare_units\": {prepare_units},\n  \"phase_units\": {:?},\n  \"qualification\": {qualification},\n  \"separate_transactions\": true\n}}\n", phase_units))?;
    println!("DOOM-E2R/4 bounded split passed: prepare_cu={prepare_units} phase_cu={phase_units:?} qualification={qualification} receipt={}", receipt.display());
    Ok(())
}
