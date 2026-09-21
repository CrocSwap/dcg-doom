//! Actual DOOM-E2R/3 durable continuation seam.
//!
//! The positive sequence is two separate SBF ProgramTest transactions.  The
//! first instruction serializes the retained captured ST1/MP1/CI2 and a zero
//! framebuffer into a program-owned continuation account.  The second
//! instruction verifies the durable pointer-free record and publishes those
//! exact bytes.  This is intentionally a pre-tic seam: it does not claim a
//! renderer continuation or playable Doom.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use doom_sbf::continuation_sbf::{self, Request};
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
use solana_system_interface::instruction::create_account;
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
    "/src/bin/continuation_sbf.rs"
));

const CONTEXT: Pubkey = Pubkey::new_from_array([0xe0; 32]);
const RESOURCE: Pubkey = Pubkey::new_from_array([0xe1; 32]);
const STATE: Pubkey = Pubkey::new_from_array([0xe2; 32]);
const MIRROR: Pubkey = Pubkey::new_from_array([0xe3; 32]);
const CONTROLLER: Pubkey = Pubkey::new_from_array([0xe4; 32]);
const FRAME: Pubkey = Pubkey::new_from_array([0xe5; 32]);
const BAD_CONTROLLER: Pubkey = Pubkey::new_from_array([0xe7; 32]);
const BAD_STATE: Pubkey = Pubkey::new_from_array([0xe8; 32]);

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?)
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
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
        || u32_at(bytes, 12) as usize != resource_sbf::WAD_HEADER_BYTES
        || u32_at(bytes, 16) as usize != resource_sbf::WAD_ACCOUNT_BYTES
        || u32_at(bytes, 20) as usize != ordinal
    {
        return Err(format!("invalid retained WAD account {ordinal}").into());
    }
    let payload = u32_at(bytes, 24) as usize;
    if payload > resource_sbf::WAD_ACCOUNT_BYTES - resource_sbf::WAD_HEADER_BYTES
        || bytes[resource_sbf::WAD_HEADER_BYTES + payload..]
            .iter()
            .any(|&byte| byte != 0)
    {
        return Err(format!("invalid WAD padding {ordinal}").into());
    }
    Ok(())
}

fn padded_state(state: &[u8]) -> Vec<u8> {
    let mut padded = vec![0; resource_sbf::STATE_MAX_BYTES];
    padded[..state.len()].copy_from_slice(state);
    padded
}

fn instruction(
    payer: Pubkey,
    state: Pubkey,
    controller: Pubkey,
    continuation: Pubkey,
    request: Request,
) -> Instruction {
    let mut accounts = vec![meta(CONTEXT, true, false), meta(RESOURCE, false, false)];
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
        meta(payer, false, true),
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
    state: Pubkey,
    controller: Pubkey,
    continuation: Pubkey,
    request: Request,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[payer],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(NORMAL_LIMIT),
                instruction(payer.pubkey(), state, controller, continuation, request),
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
    wad: &[Vec<u8>],
    state: &[u8],
    bad_state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    bad_controller: &[u8],
) -> ProgramTest {
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(NORMAL_LIMIT as u64);
    bank.add_program("doom_sbf", PROGRAM, None);
    bank.add_account(CONTEXT, account(vec![0; resource_sbf::CONTEXT_BYTES]));
    bank.add_account(RESOURCE, account(resource.to_vec()));
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
    bank.add_account(FRAME, account(vec![0; continuation_sbf::FRAME_BYTES]));
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
                &continuation.pubkey(),
                100_000_000_000,
                continuation_sbf::CONTINUATION_BYTES as u64,
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let capture = PathBuf::from(args.next().ok_or(
        "usage: continuation_sbf CAPTURE RECEIPT IMAGE EXPECTED_IMAGE_SHA MANIFEST EXPECTED_MANIFEST_SHA ABI DRIVER RESOURCE_IMAGE CONTROLLER",
    )?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let image = PathBuf::from(args.next().ok_or("missing image")?).canonicalize()?;
    let expected_image = args
        .next()
        .ok_or("missing expected image SHA")?
        .into_string()
        .map_err(|_| "image SHA is not UTF-8")?;
    let manifest = PathBuf::from(args.next().ok_or("missing source manifest")?).canonicalize()?;
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
    let expected_abi = sha(continuation_sbf::ABI_DESCRIPTOR.as_bytes());
    if sha(&image_bytes) != expected_image
        || sha(&manifest_bytes) != expected_manifest
        || sha(DRIVER_SOURCE) != expected_driver
        || abi != expected_abi
    {
        return Err("continuation image/source/ABI identity mismatch".into());
    }
    if !String::from_utf8_lossy(&manifest_bytes).contains("\"sbf\": true") {
        return Err("continuation source manifest is not an SBF manifest".into());
    }
    let resource = read(&resource_path)?;
    if resource.len() != resource_sbf::RI2_RESOURCE_IMAGE_BYTES
        || sha(&resource) != RESOURCE_IMAGE_SHA256
    {
        return Err("retained RI2 resource identity mismatch".into());
    }
    let capture_manifest = read(&capture.join("account/manifest.json"))?;
    if sha(&capture_manifest) != ACCOUNT_MANIFEST_SHA256 {
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
        || mirror.len() != continuation_sbf::MIRROR_BYTES
        || controller.len() != continuation_sbf::CONTROLLER_BYTES
    {
        return Err("retained ST1/MP1/CI2 input shape mismatch".into());
    }
    let mut bad_controller = controller.clone();
    bad_controller[28] = 1;
    let mut bad_state = state.clone();
    bad_state[64] ^= 1;
    let request = Request {
        operation: continuation_sbf::PREPARE,
        state_len: state_capture.len(),
    };
    let continuation = solana_keypair::Keypair::new();
    let continuation_key = continuation.pubkey();
    let bank = program_test(
        &resource,
        &wad,
        &state,
        &bad_state,
        &mirror,
        &controller,
        &bad_controller,
    );
    let (mut client, payer, _) = bank.start().await;
    create_continuation(&mut client, &payer, &continuation).await?;

    // Out-of-order resume on the zero continuation must refuse and leave all
    // output accounts unchanged.
    let resume_request = Request {
        operation: continuation_sbf::RESUME,
        state_len: state_capture.len(),
    };
    let before_state = account_data(&client, STATE).await?;
    let before_mirror = account_data(&client, MIRROR).await?;
    let before_frame = account_data(&client, FRAME).await?;
    require_refusal(
        &execute(
            &mut client,
            &payer,
            STATE,
            CONTROLLER,
            continuation_key,
            resume_request,
        )
        .await?,
        "Custom(241)",
    )?;
    if account_data(&client, STATE).await? != before_state
        || account_data(&client, MIRROR).await? != before_mirror
        || account_data(&client, FRAME).await? != before_frame
    {
        return Err("out-of-order resume mutated outputs".into());
    }

    let prepare = execute(
        &mut client,
        &payer,
        STATE,
        CONTROLLER,
        continuation_key,
        request,
    )
    .await?;
    let prepare_units = prepare
        .metadata
        .as_ref()
        .map(|meta| meta.compute_units_consumed)
        .ok_or("prepare CU metadata missing")?;
    if prepare.result.is_err() || prepare_units > QUALIFICATION_MAX_UNITS {
        return Err(format!(
            "prepare failed or over target: {:?} CU={prepare_units}",
            prepare.result
        )
        .into());
    }
    let prepared = account_data(&client, continuation_key).await?;
    if &prepared[..8] != continuation_sbf::MAGIC
        || u32_at(&prepared, 8) != continuation_sbf::VERSION
        || u32_at(&prepared, 24) != 1
    {
        return Err("prepared continuation header is not durable E2R/3".into());
    }
    fs::write(receipt.join("continuation-prepared.bin"), &prepared)?;

    // A changed input key is a real alternate program-owned account, not a
    // driver-side rewrite of the continuation.  The stored account-set root
    // must refuse it before any output publication.
    require_refusal(
        &execute(
            &mut client,
            &payer,
            BAD_STATE,
            CONTROLLER,
            continuation_key,
            resume_request,
        )
        .await?,
        "Custom(242)",
    )?;
    if account_data(&client, STATE).await? != before_state
        || account_data(&client, MIRROR).await? != before_mirror
        || account_data(&client, FRAME).await? != before_frame
    {
        return Err("changed-input refusal mutated outputs".into());
    }
    // A malformed CI2 is rejected in the actual preparation instruction and
    // cannot create a continuation.
    let malformed = execute(
        &mut client,
        &payer,
        STATE,
        BAD_CONTROLLER,
        continuation_key,
        request,
    )
    .await?;
    require_refusal(&malformed, "InvalidAccountData")?;

    let resume = execute(
        &mut client,
        &payer,
        STATE,
        CONTROLLER,
        continuation_key,
        resume_request,
    )
    .await?;
    let resume_units = resume
        .metadata
        .as_ref()
        .map(|meta| meta.compute_units_consumed)
        .ok_or("resume CU metadata missing")?;
    if resume.result.is_err() || resume_units > QUALIFICATION_MAX_UNITS {
        return Err(format!(
            "resume failed or over target: {:?} CU={resume_units}",
            resume.result
        )
        .into());
    }
    let output_state = account_data(&client, STATE).await?;
    let output_mirror = account_data(&client, MIRROR).await?;
    let output_frame = account_data(&client, FRAME).await?;
    if output_state != state
        || output_mirror != mirror
        || output_frame != vec![0; continuation_sbf::FRAME_BYTES]
    {
        return Err("resume output is not byte-identical to actual captured input".into());
    }
    let replay = execute(
        &mut client,
        &payer,
        STATE,
        CONTROLLER,
        continuation_key,
        resume_request,
    )
    .await?;
    require_refusal(&replay, "Custom(241)")?;
    if account_data(&client, STATE).await? != output_state
        || account_data(&client, MIRROR).await? != output_mirror
        || account_data(&client, FRAME).await? != output_frame
    {
        return Err("replay refusal mutated outputs".into());
    }
    let final_continuation = account_data(&client, continuation_key).await?;
    fs::write(
        receipt.join("continuation-published.bin"),
        &final_continuation,
    )?;
    fs::write(receipt.join("output-state.st1"), &output_state)?;
    fs::write(receipt.join("output-mirror.mp1"), &output_mirror)?;
    fs::write(receipt.join("output-frame.fb1"), &output_frame)?;
    fs::write(receipt.join("identity.json"), format!(
        "{{\n  \"profile\": \"DOOM-E2R/3-DURABLE-SEAM\",\n  \"image_sha256\": {},\n  \"manifest_sha256\": {},\n  \"driver_sha256\": {},\n  \"continuation_abi_id\": {},\n  \"ri2_source_abi_id\": {},\n  \"resource_sha256\": {},\n  \"wad_sha256\": {},\n  \"state_input_sha256\": {},\n  \"mirror_input_sha256\": {},\n  \"controller_input_sha256\": {},\n  \"frame_input_sha256\": {},\n  \"prepare_units\": {},\n  \"resume_units\": {},\n  \"normal_limit\": {},\n  \"qualification_max_units\": {},\n  \"separate_transactions\": true,\n  \"native_tick_render_equality\": \"not-run; this seam intentionally stops before G_Ticker/R_RenderPlayerView\"\n}}\n",
        json(&expected_image), json(&expected_manifest), json(&expected_driver), json(&abi), json(&sha(&resource_sbf::RI2_ABI_ID)), json(&sha(&resource)), json(WAD_CONCAT_SHA256), json(&sha(&state)), json(&sha(&mirror)), json(&sha(&controller)), json(&sha(&vec![0; continuation_sbf::FRAME_BYTES])), prepare_units, resume_units, NORMAL_LIMIT, QUALIFICATION_MAX_UNITS
    ))?;
    println!("DOOM-E2R/3 durable seam passed: prepare_cu={prepare_units} resume_cu={resume_units} receipt={}", receipt.display());
    Ok(())
}
