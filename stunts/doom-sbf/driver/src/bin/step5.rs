//! Local ProgramTest matrix for the Doom step-5 lifecycle.
//!
//! This binary is intentionally separate from the prior hot-loop driver. It
//! uses the supplied SBF image, constructs a synthetic no-WAD-in-git fixture,
//! and exercises the honest, stale, malformed, live-frame, and exhausted-pool
//! paths. It never contacts RPC or creates a chain account.

use std::{env, error::Error, path::PathBuf};

use doom_sbf::{
    host_init_data, host_loaded_state, host_state_hash, host_tic_after, host_tic_state,
    CONFIG_BYTES, INIT_BYTES, INSTRUCTION_MAGIC, LEVEL_STATE_BYTES, LOAD_LEVEL_BYTES,
    MOBJ_POOL_BYTES, OP_LOAD_LEVEL, OP_TIC, PROTOCOL_VERSION, TIC_BYTES, WAD_ACCOUNT_BYTES,
    WAD_HEADER_BYTES, WAD_PAYLOAD_BYTES,
};
use solana_account::Account;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction as SolanaInstruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_program::system_program;
use solana_program_test::{BanksClient, ProgramTest};
use solana_pubkey::Pubkey as SolanaPubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM_ID: SolanaPubkey = SolanaPubkey::new_from_array([0xd0; 32]);
const CONFIG_ID: SolanaPubkey = SolanaPubkey::new_from_array([0xd1; 32]);
const LEVEL_ID: SolanaPubkey = SolanaPubkey::new_from_array([0xd2; 32]);
const POOL_ID: SolanaPubkey = SolanaPubkey::new_from_array([0xd3; 32]);
const REQUIRED_METER: u32 = 1_400_000;

fn wad_id(index: usize) -> SolanaPubkey {
    let mut bytes = [0u8; 32];
    bytes[0] = 0xd4;
    bytes[1] = index as u8;
    SolanaPubkey::new_from_array(bytes)
}

fn synthetic_wad() -> Vec<u8> {
    let names: [&[u8; 8]; 12] = [
        b"PLAYPAL\0",
        b"E1M1\0\0\0\0",
        b"THINGS\0\0",
        b"LINEDEFS",
        b"SIDEDEFS",
        b"VERTEXES",
        b"SEGS\0\0\0\0",
        b"SSECTORS",
        b"NODES\0\0\0",
        b"SECTORS\0",
        b"REJECT\0\0",
        b"BLOCKMAP",
    ];
    let mut payload = vec![0u8; 768];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(17).wrapping_add(3);
    }
    let directory = 12 + payload.len();
    let mut wad = Vec::with_capacity(directory + names.len() * 16);
    wad.extend_from_slice(b"IWAD");
    wad.extend_from_slice(&(names.len() as u32).to_le_bytes());
    wad.extend_from_slice(&(directory as u32).to_le_bytes());
    wad.extend_from_slice(&payload);
    for (index, name) in names.iter().enumerate() {
        let (position, length) = if index == 0 { (12, 768) } else { (12, 0) };
        wad.extend_from_slice(&(position as u32).to_le_bytes());
        wad.extend_from_slice(&(length as u32).to_le_bytes());
        wad.extend_from_slice(*name);
    }
    wad
}

fn wad_chunks(wad: &[u8]) -> Vec<Vec<u8>> {
    (0..5)
        .map(|index| {
            let start = index * WAD_PAYLOAD_BYTES;
            let end = (start + WAD_PAYLOAD_BYTES).min(wad.len());
            let payload = &wad[start..end];
            let mut account = vec![0u8; WAD_ACCOUNT_BYTES];
            account[..8].copy_from_slice(b"DOOM-WC1");
            account[8..12].copy_from_slice(&1u32.to_le_bytes());
            account[12..16].copy_from_slice(&(WAD_HEADER_BYTES as u32).to_le_bytes());
            account[16..20].copy_from_slice(&(WAD_ACCOUNT_BYTES as u32).to_le_bytes());
            account[20..24].copy_from_slice(&(index as u32).to_le_bytes());
            account[24..28].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            account[WAD_HEADER_BYTES..WAD_HEADER_BYTES + payload.len()].copy_from_slice(payload);
            account
        })
        .collect()
}

fn owned_account(data: Vec<u8>, owner: SolanaPubkey) -> Account {
    Account {
        lamports: 10_000_000_000,
        data,
        owner,
        executable: false,
        rent_epoch: 0,
    }
}

fn init_instruction(payer: SolanaPubkey, data: Vec<u8>) -> SolanaInstruction {
    let mut accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(CONFIG_ID, false),
    ];
    accounts.extend((0..5).map(|index| AccountMeta::new_readonly(wad_id(index), false)));
    accounts.push(AccountMeta::new_readonly(system_program::id(), false));
    SolanaInstruction {
        program_id: PROGRAM_ID,
        accounts,
        data,
    }
}

fn load_instruction(data: Vec<u8>) -> SolanaInstruction {
    let mut accounts = vec![AccountMeta::new_readonly(CONFIG_ID, false)];
    accounts.extend((0..5).map(|index| AccountMeta::new_readonly(wad_id(index), false)));
    accounts.push(AccountMeta::new(LEVEL_ID, false));
    accounts.push(AccountMeta::new(POOL_ID, false));
    SolanaInstruction {
        program_id: PROGRAM_ID,
        accounts,
        data,
    }
}

fn tic_instruction(data: Vec<u8>) -> SolanaInstruction {
    SolanaInstruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(CONFIG_ID, false),
            AccountMeta::new(LEVEL_ID, false),
            AccountMeta::new(POOL_ID, false),
        ],
        data,
    }
}

fn load_data(state_hash: [u8; 32]) -> Vec<u8> {
    let mut data = Vec::with_capacity(LOAD_LEVEL_BYTES);
    data.extend_from_slice(INSTRUCTION_MAGIC);
    data.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    data.extend_from_slice(&OP_LOAD_LEVEL.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&state_hash);
    data
}

fn tic_data(number: u32, before: [u8; 32], after: [u8; 32], command: [u8; 4]) -> Vec<u8> {
    let mut data = Vec::with_capacity(TIC_BYTES);
    data.extend_from_slice(INSTRUCTION_MAGIC);
    data.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    data.extend_from_slice(&OP_TIC.to_le_bytes());
    data.extend_from_slice(&number.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&before);
    data.extend_from_slice(&after);
    data.extend_from_slice(&command);
    data
}

async fn process(
    banks: &mut BanksClient,
    payer: &Keypair,
    meter: u32,
    instruction: SolanaInstruction,
) -> Result<(), Box<dyn Error>> {
    let recent_blockhash = banks.get_latest_blockhash().await?;
    let instructions = [
        ComputeBudgetInstruction::set_compute_unit_limit(meter),
        instruction,
    ];
    let message =
        Message::new_with_blockhash(&instructions, Some(&payer.pubkey()), &recent_blockhash);
    let transaction = Transaction::new(&[payer], message, recent_blockhash);
    banks.process_transaction(transaction).await?;
    Ok(())
}

async fn expect_refusal(
    banks: &mut BanksClient,
    payer: &Keypair,
    meter: u32,
    instruction: SolanaInstruction,
) -> Result<(), Box<dyn Error>> {
    if process(banks, payer, meter, instruction).await.is_ok() {
        return Err("control unexpectedly succeeded".into());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let so = env::args()
        .nth(1)
        .ok_or("pass the stripped doom_sbf.so path")?;
    let so = PathBuf::from(so);
    if !so.is_file() {
        return Err(format!("SBF .so not found: {}", so.display()).into());
    }
    let deploy = so.parent().ok_or("SBF path has no parent")?;
    env::set_var("BPF_OUT_DIR", deploy);
    env::set_var("SBF_OUT_DIR", deploy);
    let meter = env::var("DOOM_SBF_COMPUTE_LIMIT")
        .ok()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(REQUIRED_METER);

    let wad = synthetic_wad();
    let chunks = wad_chunks(&wad);
    let init_data = host_init_data(&chunks)?;
    assert_eq!(init_data.len(), INIT_BYTES);
    let (mut expected_level, mut expected_pool, s0) = host_loaded_state(&chunks)?;
    let root: [u8; 32] = init_data[48..80].try_into()?;

    let mut program_test = ProgramTest::default();
    program_test.prefer_bpf(true);
    program_test.set_compute_max_units(meter as u64);
    program_test.add_program("doom_sbf", PROGRAM_ID, None);
    program_test.add_account(CONFIG_ID, owned_account(vec![0; CONFIG_BYTES], PROGRAM_ID));
    program_test.add_account(
        LEVEL_ID,
        owned_account(vec![0; LEVEL_STATE_BYTES], PROGRAM_ID),
    );
    program_test.add_account(POOL_ID, owned_account(vec![0; MOBJ_POOL_BYTES], PROGRAM_ID));
    for (index, chunk) in chunks.iter().enumerate() {
        program_test.add_account(
            wad_id(index),
            owned_account(chunk.clone(), system_program::id()),
        );
    }
    let (mut banks, payer, _) = program_test.start().await;

    process(
        &mut banks,
        &payer,
        meter,
        init_instruction(payer.pubkey(), init_data.clone()),
    )
    .await?;
    expect_refusal(
        &mut banks,
        &payer,
        meter,
        init_instruction(payer.pubkey(), init_data),
    )
    .await?;
    expect_refusal(
        &mut banks,
        &payer,
        meter,
        load_instruction(load_data(s0).into_iter().take(55).collect()),
    )
    .await?;

    process(&mut banks, &payer, meter, load_instruction(load_data(s0))).await?;
    let loaded = banks
        .get_account(LEVEL_ID)
        .await?
        .ok_or("missing level after load")?;
    if loaded.data != expected_level {
        return Err("SBF level bytes differ from host load reference".into());
    }

    let before = host_state_hash(&root, &expected_level, &expected_pool);
    let command = [3u8, 0xfe, 7, 0];
    let after = host_tic_after(&root, &expected_level, &expected_pool, 1, &command)?;
    expect_refusal(
        &mut banks,
        &payer,
        meter,
        tic_instruction(tic_data(1, [0; 32], after, command)),
    )
    .await?;
    expect_refusal(
        &mut banks,
        &payer,
        meter,
        tic_instruction(tic_data(1, before, [0; 32], command)),
    )
    .await?;
    process(
        &mut banks,
        &payer,
        meter,
        tic_instruction(tic_data(1, before, after, command)),
    )
    .await?;
    (expected_level, expected_pool, _) =
        host_tic_state(&root, &expected_level, &expected_pool, 1, &command)?;

    for number in 2..=383u32 {
        let before = host_state_hash(&root, &expected_level, &expected_pool);
        let command = [0, 0, 0, doom_sbf::BUTTON_ALLOCATE_PROBE];
        let after = host_tic_after(&root, &expected_level, &expected_pool, number, &command)?;
        process(
            &mut banks,
            &payer,
            meter,
            tic_instruction(tic_data(number, before, after, command)),
        )
        .await?;
        (expected_level, expected_pool, _) =
            host_tic_state(&root, &expected_level, &expected_pool, number, &command)?;
    }
    let before = host_state_hash(&root, &expected_level, &expected_pool);
    expect_refusal(
        &mut banks,
        &payer,
        meter,
        tic_instruction(tic_data(
            384,
            before,
            [0; 32],
            [0, 0, 0, doom_sbf::BUTTON_ALLOCATE_PROBE],
        )),
    )
    .await?;
    let final_level = banks
        .get_account(LEVEL_ID)
        .await?
        .ok_or("missing final level")?;
    let final_pool = banks
        .get_account(POOL_ID)
        .await?
        .ok_or("missing final pool")?;
    if final_level.data != expected_level || final_pool.data != expected_pool {
        return Err("SBF state differs from host reference after pool matrix".into());
    }
    println!("STEP5_CONTROLS honest=1 stale=2 malformed=1 live=1 exhausted=1");
    println!(
        "STEP5_ACCOUNTS config={} level={} pool={} wad_each={}",
        CONFIG_BYTES, LEVEL_STATE_BYTES, MOBJ_POOL_BYTES, WAD_ACCOUNT_BYTES
    );
    Ok(())
}
