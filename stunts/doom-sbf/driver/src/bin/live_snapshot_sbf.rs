#![recursion_limit = "256"]

//! Captured-input SBF ProgramTest for E2R/5 + SIM/2 ADVANCE + SNAPSHOT-LIVE/3
//! and DOOM-RSR/1 rendering. ProgramTest creates the state by staging captured
//! inputs and running the preceding real program instructions.

use std::{env, error::Error, fs, path::PathBuf};

use doom_sbf::{
    continuation_split_sbf as e2r, render_snapshot_v3 as rsn3, resource_sbf as rs, sim2, stage,
    static_resource as rsr,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_banks_client::BanksClient;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_message::Message;
use solana_program::rent::Rent;
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::{instruction as system_instruction, program as system_program};
use solana_transaction::Transaction;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xd2; 32]);
const RESOURCE: Pubkey = Pubkey::new_from_array([0xe1; 32]);
const FRAME: Pubkey = Pubkey::new_from_array([0xe5; 32]);
const COMPUTE_LIMIT: u32 = 1_400_000;
const BUILD_LIMIT: u32 = 10_000_000;
const STAGE_CHUNK: usize = 900;
const DRIVER_SOURCE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/bin/live_snapshot_sbf.rs"
));

fn wad_key(ordinal: usize) -> Pubkey {
    Pubkey::new_from_array([0xf0 + ordinal as u8; 32])
}

fn account(owner: Pubkey, data: Vec<u8>) -> Account {
    Account {
        lamports: 100_000_000_000,
        data,
        owner,
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

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn slice_frame(frame: &[u8], strip: usize) -> Vec<u8> {
    (0..200)
        .flat_map(|row| {
            let start = row * 320 + strip * 40;
            frame[start..start + 40].iter().copied()
        })
        .collect()
}

async fn submit(
    banks: &mut BanksClient,
    payer: &solana_keypair::Keypair,
    signers: &[&solana_keypair::Keypair],
    instruction: Instruction,
    limit: u32,
) -> Result<(u64, String), Box<dyn Error>> {
    let blockhash = banks.get_latest_blockhash().await?;
    let mut instructions = vec![ComputeBudgetInstruction::set_compute_unit_limit(limit)];
    instructions.push(instruction);
    let mut keypairs = vec![payer];
    keypairs.extend_from_slice(signers);
    let tx = Transaction::new(
        &keypairs,
        Message::new_with_blockhash(&instructions, Some(&payer.pubkey()), &blockhash),
        blockhash,
    );
    let result = banks.process_transaction_with_metadata(tx).await?;
    let logs = result
        .metadata
        .as_ref()
        .map_or_else(String::new, |metadata| metadata.log_messages.join("\n"));
    if let Err(error) = result.result {
        return Err(format!("transaction failed: {error:?}\n{logs}").into());
    }
    let units = result
        .metadata
        .as_ref()
        .ok_or("ProgramTest omitted transaction metadata")?
        .compute_units_consumed;
    Ok((units, logs))
}

async fn stage_account(
    ctx: &mut ProgramTestContext,
    payer: &solana_keypair::Keypair,
    target: &solana_keypair::Keypair,
    bytes: &[u8],
) -> Result<usize, Box<dyn Error>> {
    let mut sent = 0;
    for offset in (STAGE_CHUNK..bytes.len()).step_by(STAGE_CHUNK) {
        let end = (offset + STAGE_CHUNK).min(bytes.len());
        let instruction = Instruction {
            program_id: PROGRAM,
            accounts: vec![meta(target.pubkey(), true, true)],
            data: stage::encode(offset as u32, &bytes[offset..end]),
        };
        submit(
            &mut ctx.banks_client,
            payer,
            &[target],
            instruction,
            COMPUTE_LIMIT,
        )
        .await?;
        sent += 1;
    }
    let header_end = bytes.len().min(STAGE_CHUNK);
    let instruction = Instruction {
        program_id: PROGRAM,
        accounts: vec![meta(target.pubkey(), true, true)],
        data: stage::encode(0, &bytes[..header_end]),
    };
    submit(
        &mut ctx.banks_client,
        payer,
        &[target],
        instruction,
        COMPUTE_LIMIT,
    )
    .await?;
    sent += 1;
    let actual = ctx
        .banks_client
        .get_account(target.pubkey())
        .await?
        .ok_or("staged account disappeared")?;
    if actual.data != bytes {
        return Err("staged account readback differed from the retained capture".into());
    }
    Ok(sent)
}

async fn create_account(
    ctx: &mut ProgramTestContext,
    payer: &solana_keypair::Keypair,
    key: &solana_keypair::Keypair,
    bytes: usize,
    owner: Pubkey,
    lamports: u64,
) -> Result<(), Box<dyn Error>> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await?;
    let instruction = system_instruction::create_account(
        &payer.pubkey(),
        &key.pubkey(),
        lamports,
        bytes as u64,
        &owner,
    );
    let tx = Transaction::new(
        &[payer, key],
        Message::new_with_blockhash(&[instruction], Some(&payer.pubkey()), &blockhash),
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await?;
    Ok(())
}

fn e2r_instruction(
    context: Pubkey,
    state: Pubkey,
    mirror: Pubkey,
    controller: Pubkey,
    continuation: Pubkey,
    authority: Pubkey,
    operation: u8,
    state_len: usize,
) -> Instruction {
    let mut accounts = vec![meta(context, true, operation == e2r::PREPARE)];
    accounts.push(meta(RESOURCE, false, false));
    for ordinal in 0..rs::WAD_ACCOUNT_COUNT {
        accounts.push(meta(wad_key(ordinal), false, false));
    }
    accounts.extend([
        meta(state, true, false),
        meta(mirror, true, false),
        meta(controller, false, false),
        meta(FRAME, true, false),
        meta(continuation, true, false),
        meta(authority, false, true),
    ]);
    let mut data = e2r::Request {
        operation,
        state_len,
        cursor: 0,
        fault: 0,
    }
    .encode()
    .to_vec();
    data[8..12].copy_from_slice(&5u32.to_le_bytes());
    Instruction {
        program_id: PROGRAM,
        accounts,
        data,
    }
}

fn build_instruction(static_key: Pubkey) -> Instruction {
    let mut accounts = vec![meta(static_key, true, false), meta(RESOURCE, false, false)];
    for ordinal in 0..rs::WAD_ACCOUNT_COUNT {
        accounts.push(meta(wad_key(ordinal), false, false));
    }
    Instruction {
        program_id: PROGRAM,
        accounts,
        data: rsr::Request::build().encode().to_vec(),
    }
}

fn snapshot_live_instruction(
    context: Pubkey,
    meta_key: Pubkey,
    state: Pubkey,
    mirror: Pubkey,
    static_key: Pubkey,
    snapshot: Pubkey,
    request: rsn3::SnapshotRequest,
) -> Instruction {
    Instruction {
        program_id: PROGRAM,
        accounts: vec![
            meta(context, false, false),
            meta(meta_key, false, false),
            meta(state, false, false),
            meta(mirror, false, false),
            meta(static_key, false, false),
            meta(snapshot, true, false),
        ],
        data: request.encode_live().to_vec(),
    }
}

fn render_instruction(
    static_key: Pubkey,
    snapshot: Pubkey,
    strip: Pubkey,
    request: rsr::Request,
) -> Instruction {
    Instruction {
        program_id: PROGRAM,
        accounts: vec![
            meta(static_key, true, false),
            meta(snapshot, false, false),
            meta(strip, true, false),
        ],
        data: request.encode().to_vec(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let image = PathBuf::from(args.next().ok_or(
        "usage: live_snapshot_sbf IMAGE MANIFEST CAPTURE RESOURCE CONTROLLER R3_RECEIPT RECEIPT",
    )?);
    let manifest = PathBuf::from(args.next().ok_or("missing SBF source manifest")?);
    let capture = PathBuf::from(args.next().ok_or("missing retained capture directory")?);
    let resource_path = PathBuf::from(args.next().ok_or("missing RI2 image")?);
    let controller_path = PathBuf::from(args.next().ok_or("missing CI2 controller")?);
    let r3_root = PathBuf::from(args.next().ok_or("missing R3 oracle receipt")?);
    let receipt = PathBuf::from(args.next().ok_or("missing receipt directory")?);
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err("receipt directory is not empty".into());
    }
    fs::create_dir_all(&receipt)?;
    let retained = receipt.join("inputs");
    fs::create_dir_all(&retained)?;
    let image_bytes = fs::read(&image)?;
    let manifest_bytes = fs::read(&manifest)?;
    let manifest_json: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    let profile = manifest_json["profile"].as_str().unwrap_or("");
    let render_abi = manifest_json["render_snapshot_v3_abi_descriptor"]
        .as_str()
        .unwrap_or("");
    if !matches!(profile, "sim-transport" | "render-clip-measurement")
        || manifest_json["sbf"] != true
        || !render_abi.contains("SNAPSHOT-LIVE/3")
    {
        return Err("image manifest is not the DOOM-SIM/2 RSN/3 SBF profile".into());
    }

    let mut wad = Vec::with_capacity(rs::WAD_ACCOUNT_COUNT);
    for ordinal in 0..rs::WAD_ACCOUNT_COUNT {
        let bytes = fs::read(capture.join(format!("account/wad-{ordinal:03}.bin")))?;
        if bytes.len() != rs::WAD_ACCOUNT_BYTES
            || &bytes[..8] != b"DOOM-WC1"
            || u32::from_le_bytes(bytes[20..24].try_into()?) as usize != ordinal
        {
            return Err(format!("invalid captured WAD account {ordinal}").into());
        }
        wad.push(bytes);
    }
    let resource_image = fs::read(&resource_path)?;
    if resource_image.len() != rs::RI2_RESOURCE_ACCOUNT_BYTES {
        return Err("retained RI2 image has the wrong account size".into());
    }
    let state_capture = fs::read(capture.join("dumped-doom-st1.bin"))?;
    let mirror_capture = fs::read(capture.join("mobj_pool.bin"))?;
    let controller = fs::read(&controller_path)?;
    if controller.len() != rs::CONTROLLER_BYTES || &controller[..8] != b"DOOM-CI2" {
        return Err("retained CI2 controller has the wrong format".into());
    }
    let commands = {
        let mut payload = Vec::new();
        for account in &wad {
            let n = u32::from_le_bytes(account[24..28].try_into()?) as usize;
            payload.extend_from_slice(&account[rs::WAD_HEADER_BYTES..rs::WAD_HEADER_BYTES + n]);
        }
        let lump_count = u32::from_le_bytes(payload[4..8].try_into()?) as usize;
        let directory = u32::from_le_bytes(payload[8..12].try_into()?) as usize;
        let cursor = u32::from_le_bytes(state_capture[24..28].try_into()?) as usize;
        let mut found = None;
        for index in 0..lump_count {
            let at = directory + index * 16;
            if &payload[at + 8..at + 13] == b"DEMO1" {
                let offset = u32::from_le_bytes(payload[at..at + 4].try_into()?) as usize;
                found = Some(payload[offset + cursor..offset + cursor + 4].to_vec());
                break;
            }
        }
        found.ok_or("captured WAD has no DEMO1 first command")?
    };
    if controller[24..28] != commands {
        return Err("captured controller command differs from first DEMO1 command".into());
    }
    let r3_snapshot_bytes = fs::read(r3_root.join("snapshot-rsn3.bin"))?;
    let r3_snapshot = rsn3::validate_snapshot(&r3_snapshot_bytes)?;
    let r3_native_state = fs::read(r3_root.join("inputs/native-imported-state.st1"))?;
    let r3_native_mirror = fs::read(r3_root.join("inputs/native-imported-mirror.mp1"))?;
    let r3_frame = fs::read(r3_root.join("inputs/native-imported-frame.fb1"))?;
    if r3_native_state.len() != state_capture.len()
        || r3_native_mirror.len() != rs::MIRROR_BYTES
        || r3_frame.len() != rs::RI2_FRAME_BYTES
    {
        return Err("retained R3 native state, mirror, or frame has the wrong size".into());
    }
    let expected_root = {
        let mut padded = vec![0; rs::STATE_MAX_BYTES];
        padded[..r3_native_state.len()].copy_from_slice(&r3_native_state);
        sim2::verified_state_root(&padded, &r3_native_mirror)?.1
    };
    if r3_snapshot.cursor != 1
        || r3_snapshot.state_root != expected_root
        || r3_snapshot.gametic != u32::from_le_bytes(r3_native_state[20..24].try_into()?)
    {
        return Err("retained R3 snapshot does not describe the one-tic native oracle".into());
    }
    for (source, name) in [
        (&image, "doom_sbf.so"),
        (&manifest, "doom_ex2_source_manifest.json"),
        (&resource_path, "resource-image.ri2"),
        (&controller_path, "controller.bin"),
    ] {
        fs::copy(source, retained.join(name))?;
    }
    fs::copy(
        capture.join("dumped-doom-st1.bin"),
        retained.join("captured-doom-st1.bin"),
    )?;
    fs::copy(
        capture.join("mobj_pool.bin"),
        retained.join("captured-mp1.bin"),
    )?;
    for ordinal in 0..rs::WAD_ACCOUNT_COUNT {
        fs::copy(
            capture.join(format!("account/wad-{ordinal:03}.bin")),
            retained.join(format!("wad-{ordinal:03}.bin")),
        )?;
    }
    fs::copy(
        r3_root.join("snapshot-rsn3.bin"),
        retained.join("r3-snapshot-rsn3.bin"),
    )?;
    fs::copy(
        r3_root.join("inputs/native-imported-frame.fb1"),
        retained.join("r3-native-oracle-frame.fb1"),
    )?;

    let context = solana_keypair::Keypair::new();
    let state = solana_keypair::Keypair::new();
    let mirror = solana_keypair::Keypair::new();
    let controller_key = solana_keypair::Keypair::new();
    let authority = solana_keypair::Keypair::new();
    let continuation = solana_keypair::Keypair::new();
    let buffer = solana_keypair::Keypair::new();
    let (meta_key, bump) = sim2::meta_address(&PROGRAM, &context.pubkey());
    let static_key = rsr::static_address(&PROGRAM).0;
    let snapshot_key = rsn3::snapshot_address(&PROGRAM, &context.pubkey()).0;
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(30_000_000);
    bank.add_program("doom_sbf", PROGRAM, None);
    bank.add_account(
        context.pubkey(),
        account(PROGRAM, vec![0; rs::CONTEXT_BYTES]),
    );
    bank.add_account(
        state.pubkey(),
        account(PROGRAM, vec![0; rs::STATE_MAX_BYTES]),
    );
    bank.add_account(mirror.pubkey(), account(PROGRAM, vec![0; rs::MIRROR_BYTES]));
    bank.add_account(
        controller_key.pubkey(),
        account(PROGRAM, vec![0; rs::CONTROLLER_BYTES]),
    );
    bank.add_account(FRAME, account(PROGRAM, vec![0; rs::RI2_FRAME_BYTES]));
    bank.add_account(RESOURCE, account(PROGRAM, resource_image.clone()));
    for (ordinal, bytes) in wad.iter().enumerate() {
        bank.add_account(wad_key(ordinal), account(PROGRAM, bytes.clone()));
    }
    bank.add_account(
        authority.pubkey(),
        Account {
            lamports: 100_000_000_000,
            data: Vec::new(),
            owner: system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    bank.add_account(static_key, account(PROGRAM, vec![0; rsr::ACCOUNT_BYTES]));
    bank.add_account(
        snapshot_key,
        account(PROGRAM, vec![0; rsn3::SNAPSHOT_BYTES]),
    );
    for strip_id in 0..8u8 {
        let key = rsn3::strip_address(&PROGRAM, &snapshot_key, strip_id).0;
        bank.add_account(key, account(PROGRAM, vec![0; rsn3::STRIP_ACCOUNT_BYTES]));
    }

    env::set_var("BPF_OUT_DIR", image.parent().ok_or("SBF image parent")?);
    env::set_var("SBF_OUT_DIR", image.parent().ok_or("SBF image parent")?);
    let mut ctx = bank.start_with_context().await;
    let payer = ctx.payer.insecure_clone();
    let staged_state = {
        let mut bytes = vec![0; rs::STATE_MAX_BYTES];
        bytes[..state_capture.len()].copy_from_slice(&state_capture);
        bytes
    };
    let stage_state = stage_account(&mut ctx, &payer, &state, &staged_state).await?;
    let stage_mirror = stage_account(&mut ctx, &payer, &mirror, &mirror_capture).await?;
    let stage_controller = stage_account(&mut ctx, &payer, &controller_key, &controller).await?;
    let continuation_len = e2r::CONTINUATION_BYTES;
    create_account(
        &mut ctx,
        &payer,
        &continuation,
        continuation_len,
        PROGRAM,
        Rent::default().minimum_balance(continuation_len),
    )
    .await?;
    let state_len = state_capture.len();
    let mut e2r_units = Vec::new();
    for cursor in 0..=7u32 {
        let operation = if cursor == 0 { e2r::PREPARE } else { e2r::STEP };
        let instruction = e2r_instruction(
            context.pubkey(),
            state.pubkey(),
            mirror.pubkey(),
            controller_key.pubkey(),
            continuation.pubkey(),
            authority.pubkey(),
            operation,
            state_len,
        );
        let signers: Vec<&solana_keypair::Keypair> = if cursor == 0 {
            vec![&context, &authority]
        } else {
            vec![&authority]
        };
        let (units, logs) = submit(
            &mut ctx.banks_client,
            &payer,
            &signers,
            instruction,
            COMPUTE_LIMIT,
        )
        .await?;
        fs::write(receipt.join(format!("e2r-phase-{cursor}-logs.txt")), logs)?;
        e2r_units.push(units);
    }

    let capacity = sim2::DEFAULT_BUFFER_CAPACITY as u32;
    let buffer_total = sim2::buffer_bytes(capacity as usize).ok_or("SIM2 buffer length")?;
    let mut buffer_len =
        sim2::initial_buffer_bytes(capacity as usize).ok_or("SIM2 initial buffer length")?;
    create_account(
        &mut ctx,
        &payer,
        &buffer,
        buffer_len,
        PROGRAM,
        Rent::default().minimum_balance(buffer_total),
    )
    .await?;
    while buffer_len < buffer_total {
        let instruction = Instruction {
            program_id: PROGRAM,
            accounts: vec![meta(buffer.pubkey(), true, true)],
            data: sim2::encode_grow_buffer(capacity, buffer_len as u32),
        };
        let (_, logs) = submit(
            &mut ctx.banks_client,
            &payer,
            &[&buffer],
            instruction,
            COMPUTE_LIMIT,
        )
        .await?;
        fs::write(
            receipt.join(format!("buffer-grow-{buffer_len}-logs.txt")),
            logs,
        )?;
        buffer_len = sim2::next_buffer_len(buffer_len, capacity as usize)
            .ok_or("SIM2 buffer growth did not advance")?;
    }
    let mut e2r_keys = [Pubkey::default(); sim2::INIT_KEY_COUNT];
    e2r_keys[0] = RESOURCE;
    for ordinal in 0..rs::WAD_ACCOUNT_COUNT {
        e2r_keys[1 + ordinal] = wad_key(ordinal);
    }
    e2r_keys[6] = controller_key.pubkey();
    e2r_keys[7] = FRAME;
    e2r_keys[8] = continuation.pubkey();
    let frame_zero = vec![0; rs::RI2_FRAME_BYTES];
    let init = sim2::InitRequest {
        bump,
        capacity,
        e2r_keys,
        resource_hash: solana_program::hash::hash(&resource_image).to_bytes(),
        controller_hash: solana_program::hash::hash(&controller).to_bytes(),
        frame_hash: solana_program::hash::hash(&frame_zero).to_bytes(),
    };
    let init_instruction = Instruction {
        program_id: PROGRAM,
        accounts: vec![
            meta(context.pubkey(), false, false),
            meta(state.pubkey(), false, false),
            meta(mirror.pubkey(), false, false),
            meta(meta_key, true, false),
            meta(buffer.pubkey(), true, false),
            meta(authority.pubkey(), true, true),
            meta(system_program::id(), false, false),
        ],
        data: init.encode().to_vec(),
    };
    let (init_units, init_logs) = submit(
        &mut ctx.banks_client,
        &payer,
        &[&authority],
        init_instruction,
        COMPUTE_LIMIT,
    )
    .await?;
    fs::write(receipt.join("sim2-init-logs.txt"), init_logs)?;
    let deposit = Instruction {
        program_id: PROGRAM,
        accounts: vec![
            meta(buffer.pubkey(), true, false),
            meta(authority.pubkey(), false, true),
        ],
        data: sim2::encode_deposit(0, &commands),
    };
    let (deposit_units, deposit_logs) = submit(
        &mut ctx.banks_client,
        &payer,
        &[&authority],
        deposit,
        COMPUTE_LIMIT,
    )
    .await?;
    fs::write(receipt.join("sim2-deposit-logs.txt"), deposit_logs)?;
    let advance = Instruction {
        program_id: PROGRAM,
        accounts: vec![
            meta(context.pubkey(), true, false),
            meta(state.pubkey(), true, false),
            meta(mirror.pubkey(), true, false),
            meta(meta_key, true, false),
            meta(buffer.pubkey(), false, false),
            meta(authority.pubkey(), false, true),
        ],
        data: sim2::encode_advance(1),
    };
    let (advance_units, advance_logs) = submit(
        &mut ctx.banks_client,
        &payer,
        &[&authority],
        advance,
        COMPUTE_LIMIT,
    )
    .await?;
    fs::write(receipt.join("advance-logs.txt"), advance_logs)?;
    let meta_account = ctx
        .banks_client
        .get_account(meta_key)
        .await?
        .ok_or("SIM2 meta missing after ADVANCE")?;
    let meta_view = sim2::MetaView::parse(&meta_account.data).ok_or("invalid SIM2 meta")?;
    let final_state = ctx
        .banks_client
        .get_account(state.pubkey())
        .await?
        .ok_or("ST1 missing after ADVANCE")?;
    let final_mirror = ctx
        .banks_client
        .get_account(mirror.pubkey())
        .await?
        .ok_or("MP1 missing after ADVANCE")?;
    if meta_view.cursor != 1
        || meta_view.state_root != expected_root
        || final_state.data[..r3_native_state.len()] != r3_native_state
        || final_state.data[r3_native_state.len()..]
            .iter()
            .any(|byte| *byte != 0)
        || final_mirror.data != r3_native_mirror
    {
        return Err("ADVANCE state/root differs from the captured native one-tic oracle".into());
    }
    fs::write(receipt.join("advance-state.st1"), &final_state.data)?;
    fs::write(receipt.join("advance-mirror.mp1"), &final_mirror.data)?;

    let (build_units, build_logs) = submit(
        &mut ctx.banks_client,
        &payer,
        &[],
        build_instruction(static_key),
        BUILD_LIMIT,
    )
    .await?;
    fs::write(receipt.join("static-build-logs.txt"), build_logs)?;
    let static_account = ctx
        .banks_client
        .get_account(static_key)
        .await?
        .ok_or("static resource account missing after BUILD")?;
    if static_account.data[rsr::HEADER_OFFSET..rsr::HEADER_OFFSET + 8] != *rsr::MAGIC {
        return Err("static BUILD did not write a valid RSR header".into());
    }
    fs::write(
        receipt.join("static-resource-account.bin"),
        &static_account.data,
    )?;

    let live_request = rsn3::SnapshotRequest {
        cursor: meta_view.cursor,
        state_root: meta_view.state_root,
    };
    let (snapshot_units, snapshot_logs) = submit(
        &mut ctx.banks_client,
        &payer,
        &[],
        snapshot_live_instruction(
            context.pubkey(),
            meta_key,
            state.pubkey(),
            mirror.pubkey(),
            static_key,
            snapshot_key,
            live_request,
        ),
        COMPUTE_LIMIT,
    )
    .await?;
    fs::write(receipt.join("snapshot-live-logs.txt"), snapshot_logs)?;
    let snapshot_account = ctx
        .banks_client
        .get_account(snapshot_key)
        .await?
        .ok_or("live snapshot PDA missing")?;
    let snapshot_view = rsn3::validate_snapshot(&snapshot_account.data)?;
    if snapshot_view.cursor != r3_snapshot.cursor
        || snapshot_view.state_root != r3_snapshot.state_root
        || snapshot_view.gametic != r3_snapshot.gametic
        || snapshot_view.wad_hash != r3_snapshot.wad_hash
        || snapshot_view.resource_hash != r3_snapshot.resource_hash
        || snapshot_view.payload != r3_snapshot.payload
    {
        return Err("SNAPSHOT-LIVE projection differs from the retained R3 snapshot".into());
    }
    fs::write(
        receipt.join("snapshot-live-rsn3.bin"),
        &snapshot_account.data,
    )?;

    let mut reconstructed = vec![0; rs::RI2_FRAME_BYTES];
    let mut render_units = Vec::new();
    let mut strip_results = Vec::new();
    for strip_id in 0..8u8 {
        let strip_key = rsn3::strip_address(&PROGRAM, &snapshot_key, strip_id).0;
        let request =
            rsr::Request::render(strip_id, snapshot_view.cursor, snapshot_view.snapshot_hash);
        let (units, logs) = submit(
            &mut ctx.banks_client,
            &payer,
            &[],
            render_instruction(static_key, snapshot_key, strip_key, request),
            COMPUTE_LIMIT,
        )
        .await?;
        fs::write(
            receipt.join(format!("render-strip-{strip_id}-logs.txt")),
            logs,
        )?;
        let strip_account = ctx
            .banks_client
            .get_account(strip_key)
            .await?
            .ok_or("render strip account missing")?;
        let strip_view = rsn3::validate_strip(&strip_account.data)?;
        let expected = slice_frame(&r3_frame, strip_id as usize);
        if strip_view.pixels != expected {
            return Err(format!("strip {strip_id} differs from native oracle").into());
        }
        for row in 0..200 {
            let from = row * 40;
            let to = row * 320 + strip_id as usize * 40;
            reconstructed[to..to + 40].copy_from_slice(&strip_view.pixels[from..from + 40]);
        }
        fs::write(
            receipt.join(format!("render-strip-{strip_id}.px8")),
            strip_view.pixels,
        )?;
        fs::write(
            receipt.join(format!("render-strip-{strip_id}.bin")),
            &strip_account.data,
        )?;
        render_units.push(units);
        strip_results.push(json!({"strip_id":strip_id,"compute_units":units,"native_byte_exact":true,"strip_sha256":sha(strip_view.pixels)}));
    }
    if reconstructed != r3_frame {
        return Err("assembled RENDER frame differs from the captured native oracle".into());
    }
    fs::write(receipt.join("reconstructed-frame.fb1"), &reconstructed)?;
    fs::write(receipt.join("native-oracle-frame.fb1"), &r3_frame)?;
    fs::write(receipt.join("captured-r3-frame.fb1"), &r3_frame)?;

    let report = json!({
        "status":"measured-byte-exact",
        "profile":"DOOM-SIM/2 + SNAPSHOT-LIVE/3 + RSR/1 under SBF ProgramTest",
        "local_programtest_only":true,
        "network_calls":0,
        "image_sha256":sha(&image_bytes),
        "source_manifest_sha256":sha(&manifest_bytes),
        "driver_source_sha256":sha(DRIVER_SOURCE),
        "captured_state_sha256":sha(&state_capture),
        "captured_mirror_sha256":sha(&mirror_capture),
        "resource_image_sha256":sha(&resource_image),
        "controller_sha256":sha(&controller),
        "one_tic_command_hex":hex(&commands),
        "advance_compute_units":advance_units,
        "advance_matches_native_state_mirror_and_root":true,
        "static_build_compute_units":build_units,
        "static_build_account_bytes":rsr::ACCOUNT_BYTES,
        "static_build_account_preallocated":true,
        "static_build_rent_measured":false,
        "snapshot_live_compute_units":snapshot_units,
        "snapshot_target_compute_units":COMPUTE_LIMIT,
        "snapshot_payload_bytes":snapshot_view.payload.len(),
        "snapshot_matches_legacy_r3_payload_byte_exact":true,
        "e2r5_phase_compute_units":e2r_units,
        "sim2_init_compute_units":init_units,
        "deposit_compute_units":deposit_units,
        "stage_transactions":{"state":stage_state,"mirror":stage_mirror,"controller":stage_controller},
        "build_transaction_count":1,
        "snapshot_transaction_count":1,
        "render_transaction_count":8,
        "render_compute_units":render_units,
        "all_eight_strips_match_native":true,
        "reassembled_frame_matches_native_byte_exact":true,
        "native_frame_sha256":sha(&r3_frame),
        "r3_retained_frame_sha256":sha(&r3_frame),
        "snapshot_hash":hex(&snapshot_view.snapshot_hash),
        "state_root":hex(&snapshot_view.state_root),
        "snapshot_rent_measured":false,
        "strip_rent_measured":false,
        "programtest_accounts_preallocated":true,
        "strips":strip_results
    });
    fs::write(
        receipt.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::write(
        receipt.join("identity.json"),
        serde_json::to_vec_pretty(&json!({
            "image_path":image.display().to_string(),
            "image_sha256":sha(&image_bytes),
            "manifest_sha256":sha(&manifest_bytes),
            "driver_sha256":sha(DRIVER_SOURCE),
            "resource_sha256":sha(&resource_image),
            "controller_sha256":sha(&controller),
            "r3_snapshot_hash":hex(&r3_snapshot.snapshot_hash),
            "r3_frame_sha256":sha(&r3_frame)
        }))?,
    )?;
    println!(
        "SNAPSHOT-LIVE/3 measured {snapshot_units} CU; BUILD={build_units} CU; render max={} CU; 8/8 strips byte exact; receipt={}",
        render_units.iter().max().copied().unwrap_or(0),
        receipt.display()
    );
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
