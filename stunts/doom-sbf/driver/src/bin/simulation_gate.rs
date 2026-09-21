//! ProgramTest gate for DOOM-SIM/1 persistent no-render ticking.
//!
//! This module deliberately imports the retained E2R/4 driver as a sibling so
//! the reconstruction setup and its real WAD controls remain shared.  The
//! simulation gate adds only the sealed CI3 input, SIM1 cursor, native
//! no-render oracle, and persistent tick/control checks.

#[path = "continuation_split_sbf.rs"]
mod base;

use std::{env, error::Error, fs, path::PathBuf};

use doom_sbf::{resource_sbf, simulation};
use sha2::{Digest, Sha256};
use solana_account::AccountSharedData;
use solana_banks_interface::BanksTransactionResultWithMetadata;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::Instruction;
use solana_message::Message;
use solana_program_test::ProgramTest;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

use doom_sbf::continuation_split_sbf::{self, Request};

const INPUT: Pubkey = Pubkey::new_from_array([0xeb; 32]);
const META: Pubkey = Pubkey::new_from_array([0xec; 32]);
const BAD_AUTHORITY: Pubkey = Pubkey::new_from_array([0xed; 32]);
const INPUT_HEADER: usize = 64;
const DOMAIN_INPUT: &[u8] = b"DOOM-SIM/1-INPUT\0";
const DOMAIN_STATE: &[u8] = b"DOOM-SIM/1-STATE\0";
const DOMAIN_ACCOUNTS: &[u8] = b"DOOM-SIM/1-ACCOUNTS\0";
const DOMAIN_CONTEXT: &[u8] = b"DOOM-SIM/1-CONTEXT\0";
const META_INPUT_ROOT: usize = 40;
const META_STATE_ROOT: usize = 72;

fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn input_root(input: &[u8]) -> [u8; 32] {
    hash_parts(&[DOMAIN_INPUT, &input[..32], &input[INPUT_HEADER..]])
}

fn state_root(state: &[u8], state_len: usize, mirror: &[u8]) -> [u8; 32] {
    hash_parts(&[DOMAIN_STATE, &state[..state_len], mirror])
}

fn context_root(context: &[u8]) -> [u8; 32] {
    let checkpoint = resource_sbf::CONTEXT_BYTES - 128;
    hash_parts(&[DOMAIN_CONTEXT, &context[..64], &context[checkpoint..]])
}

fn account_set_hash(authority: Pubkey) -> [u8; 32] {
    let keys = [
        base::CONTEXT,
        base::STATE,
        base::MIRROR,
        INPUT,
        META,
        authority,
    ];
    let mut bytes = Vec::with_capacity(keys.len() * 32);
    for key in keys {
        bytes.extend_from_slice(key.as_ref());
    }
    hash_parts(&[DOMAIN_ACCOUNTS, base::PROGRAM.as_ref(), &bytes])
}

fn make_input(commands: &[u8], first_tic: u32) -> Result<Vec<u8>, Box<dyn Error>> {
    if commands.len() != 400 {
        return Err("SIM1 gate requires exactly 100 captured commands".into());
    }
    let total = INPUT_HEADER + commands.len();
    let mut input = vec![0u8; total];
    input[..8].copy_from_slice(b"DOOM-CI3");
    input[8..12].copy_from_slice(&1u32.to_le_bytes());
    input[12..16].copy_from_slice(&(INPUT_HEADER as u32).to_le_bytes());
    input[16..20].copy_from_slice(&(total as u32).to_le_bytes());
    input[20..24].copy_from_slice(&first_tic.to_le_bytes());
    input[24..28].copy_from_slice(&100u32.to_le_bytes());
    input[28..32].copy_from_slice(&(commands.len() as u32).to_le_bytes());
    input[INPUT_HEADER..].copy_from_slice(commands);
    let root = input_root(&input);
    input[32..64].copy_from_slice(&root);
    Ok(input)
}

fn demo_commands(wad: &[Vec<u8>]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut payload = Vec::new();
    for account in wad {
        let length = base::u32_at(account, 24) as usize;
        payload.extend_from_slice(
            &account[resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + length],
        );
    }
    let lump_count = base::u32_at(&payload, 4) as usize;
    let directory = base::u32_at(&payload, 8) as usize;
    for index in 0..lump_count {
        let at = directory + index * 16;
        if at + 16 > payload.len() {
            return Err("WAD directory out of bounds".into());
        }
        if &payload[at + 8..at + 13] != b"DEMO1" {
            continue;
        }
        let offset = base::u32_at(&payload, at) as usize;
        let length = base::u32_at(&payload, at + 4) as usize;
        if length < 17 || offset + length > payload.len() || offset + 13 + 400 > payload.len() {
            return Err("invalid retained DEMO1 lump".into());
        }
        let commands = payload[offset + 13..offset + 13 + 400].to_vec();
        if commands.chunks_exact(4).any(|command| command[0] == 0x80) {
            return Err("DEMO1 marker inside retained command sequence".into());
        }
        return Ok(commands);
    }
    Err("DEMO1 lump not found".into())
}

fn request(
    operation: u8,
    tic: u32,
    sequence: u64,
    state: [u8; 32],
    input: [u8; 32],
    index: u32,
) -> simulation::Request {
    simulation::Request {
        operation,
        expected_tic: tic,
        sequence,
        state_root: state,
        input_root: input,
        input_index: index,
    }
}

fn sim_instruction(authority: Pubkey, request: simulation::Request, mirror: Pubkey) -> Instruction {
    Instruction {
        program_id: base::PROGRAM,
        accounts: vec![
            base::meta(base::CONTEXT, true, false),
            base::meta(base::STATE, true, false),
            base::meta(mirror, true, false),
            base::meta(INPUT, false, false),
            base::meta(META, true, false),
            base::meta(authority, false, true),
        ],
        data: request.encode().to_vec(),
    }
}

async fn execute_sim(
    client: &mut solana_banks_client::BanksClient,
    payer: &solana_keypair::Keypair,
    authority: &solana_keypair::Keypair,
    limit: u32,
    request: simulation::Request,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let transaction = Transaction::new(
        &[payer, authority],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(limit),
                sim_instruction(authority.pubkey(), request, base::MIRROR),
            ],
            Some(&payer.pubkey()),
            &blockhash,
        ),
        blockhash,
    );
    Ok(client
        .process_transaction_with_metadata(transaction)
        .await?)
}

async fn execute_duplicate(
    client: &mut solana_banks_client::BanksClient,
    payer: &solana_keypair::Keypair,
    authority: &solana_keypair::Keypair,
    limit: u32,
    request: simulation::Request,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let transaction = Transaction::new(
        &[payer, authority],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(limit),
                sim_instruction(authority.pubkey(), request, base::STATE),
            ],
            Some(&payer.pubkey()),
            &blockhash,
        ),
        blockhash,
    );
    Ok(client
        .process_transaction_with_metadata(transaction)
        .await?)
}

fn account(data: Vec<u8>) -> solana_account::Account {
    base::account(data)
}

fn simulation_bank(
    resource: &[u8],
    wad: &[Vec<u8>],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    authority: Pubkey,
    input: &[u8],
) -> ProgramTest {
    let mut bank = base::program_test(
        resource,
        resource,
        wad,
        state,
        state,
        mirror,
        controller,
        controller,
        controller,
        authority,
        base::NORMAL_LIMIT,
    );
    bank.add_account(INPUT, account(input.to_vec()));
    bank.add_account(META, account(vec![0; simulation::META_BYTES]));
    bank.add_account(
        BAD_AUTHORITY,
        solana_account::Account {
            lamports: 100_000_000_000,
            data: Vec::new(),
            owner: system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    bank
}

fn assert_refused(
    result: &BanksTransactionResultWithMetadata,
    label: &str,
) -> Result<(), Box<dyn Error>> {
    if result.result.is_ok() {
        return Err(format!("{label} unexpectedly accepted").into());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let capture = PathBuf::from(args.next().ok_or("missing capture")?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let image = PathBuf::from(args.next().ok_or("missing image")?).canonicalize()?;
    let expected_image = args
        .next()
        .ok_or("missing image SHA")?
        .into_string()
        .map_err(|_| "image SHA")?;
    let manifest = PathBuf::from(args.next().ok_or("missing manifest")?).canonicalize()?;
    let expected_manifest = args
        .next()
        .ok_or("missing manifest SHA")?
        .into_string()
        .map_err(|_| "manifest SHA")?;
    let abi = args
        .next()
        .ok_or("missing ABI")?
        .into_string()
        .map_err(|_| "ABI")?;
    let expected_driver = args
        .next()
        .ok_or("missing driver SHA")?
        .into_string()
        .map_err(|_| "driver SHA")?;
    let resource_path = PathBuf::from(args.next().ok_or("missing resource")?).canonicalize()?;
    let controller_path = PathBuf::from(args.next().ok_or("missing controller")?).canonicalize()?;
    let native_path = PathBuf::from(args.next().ok_or("missing native receipt")?).canonicalize()?;
    if args.next().is_some() || (receipt.exists() && fs::read_dir(&receipt)?.next().is_some()) {
        return Err("extra argument or non-empty receipt".into());
    }
    fs::create_dir_all(&receipt)?;
    let image_bytes = base::read(&image)?;
    let manifest_bytes = base::read(&manifest)?;
    if base::sha(&image_bytes) != expected_image
        || base::sha(&manifest_bytes) != expected_manifest
        || base::sha(include_bytes!("continuation_split_sbf.rs")) != expected_driver
        || abi != base::sha(continuation_split_sbf::ABI_DESCRIPTOR.as_bytes())
    {
        return Err("SIM1 image/source/ABI identity mismatch".into());
    }
    let image_dir = image.parent().ok_or("image parent")?;
    env::set_var("BPF_OUT_DIR", image_dir);
    env::set_var("SBF_OUT_DIR", image_dir);
    let resource = base::read(&resource_path)?;
    if resource.len() != resource_sbf::RI2_RESOURCE_IMAGE_BYTES
        || base::sha(&resource) != base::RESOURCE_IMAGE_SHA256
    {
        return Err("RI2 identity mismatch".into());
    }
    let mut wad = Vec::new();
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let bytes = base::read(&capture.join(format!("account/wad-{ordinal:03}.bin")))?;
        base::wad_account(&bytes, ordinal)?;
        wad.push(bytes);
    }
    let state_capture = base::read(&capture.join("dumped-doom-st1.bin"))?;
    let state = base::padded_state(&state_capture);
    let mirror = base::read(&capture.join("mobj_pool.bin"))?;
    let controller = base::read(&controller_path)?;
    let commands = demo_commands(&wad)?;
    if !commands
        .chunks_exact(4)
        .any(|command| command == [0, 0, 0, 0])
        || commands
            .chunks_exact(4)
            .map(|command| [command[0], command[1], command[2], command[3]])
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            < 2
    {
        return Err("retained DEMO1 lacks changed-plus-zero controls".into());
    }
    let input = make_input(&commands, base::u32_at(&state_capture, 20))?;
    let wad_refs: [&[u8]; 5] = [&wad[0], &wad[1], &wad[2], &wad[3], &wad[4]];
    let oracle = doom_sbf::resource::import_initialized_no_render_sequence(
        wad_refs,
        &resource,
        &state_capture,
        &commands,
    )
    .map_err(|rc| format!("native sequence oracle rc={rc}"))?;
    if oracle.states.len() != 100 || oracle.mirrors.len() != 100 {
        return Err("native sequence oracle length mismatch".into());
    }
    let pre_tic_state = base::read(&native_path.join("imported-state.st1"))?;
    let pre_tic_mirror = base::read(&native_path.join("imported-mirror.mp1"))?;
    let authority = solana_keypair::Keypair::new();
    let bank = simulation_bank(
        &resource,
        &wad,
        &state,
        &mirror,
        &controller,
        authority.pubkey(),
        &input,
    );
    let mut context = bank.start_with_context().await;
    let continuation = solana_keypair::Keypair::new();
    base::create_continuation(&mut context.banks_client, &context.payer, &continuation).await?;
    let prepare = base::execute(
        &mut context.banks_client,
        &context.payer,
        &authority,
        base::NORMAL_LIMIT,
        base::RESOURCE,
        base::STATE,
        base::CONTROLLER,
        continuation.pubkey(),
        Request {
            operation: continuation_split_sbf::PREPARE,
            state_len: state_capture.len(),
            cursor: 0,
            fault: 0,
        },
    )
    .await?;
    let mut phase_units = vec![base::units(&prepare)?];
    if prepare.result.is_err() || phase_units[0] > base::NORMAL_LIMIT as u64 {
        return Err("SIM1 reconstruction PREPARE failed".into());
    }
    for cursor in 1..=7u32 {
        let result = base::execute(
            &mut context.banks_client,
            &context.payer,
            &authority,
            base::NORMAL_LIMIT,
            base::RESOURCE,
            base::STATE,
            base::CONTROLLER,
            continuation.pubkey(),
            Request {
                operation: continuation_split_sbf::STEP,
                state_len: state_capture.len(),
                cursor,
                fault: 0,
            },
        )
        .await?;
        let cu = base::units(&result)?;
        if result.result.is_err() || cu > base::NORMAL_LIMIT as u64 {
            return Err(format!("SIM1 reconstruction phase {cursor} failed").into());
        }
        phase_units.push(cu);
    }
    let mut current_state = base::account_data(&context.banks_client, base::STATE).await?;
    let mut current_mirror = base::account_data(&context.banks_client, base::MIRROR).await?;
    if current_state[..pre_tic_state.len()] != pre_tic_state || current_mirror != pre_tic_mirror {
        return Err("E2R/4 output does not match retained pre-tic oracle".into());
    }
    let input_root_value = input_root(&input);
    let state_len = base::u32_at(&current_state, 16) as usize;
    let initial_tic = base::u32_at(&current_state, 20);
    let init_request = request(
        simulation::INIT,
        initial_tic,
        0,
        state_root(&current_state, state_len, &current_mirror),
        input_root_value,
        0,
    );
    let init = execute_sim(
        &mut context.banks_client,
        &context.payer,
        &authority,
        base::NORMAL_LIMIT,
        init_request,
    )
    .await?;
    if init.result.is_err() {
        return Err(format!("SIM1 INIT failed {:?}", init.result).into());
    }
    let snapshot = (
        base::account_data(&context.banks_client, base::CONTEXT).await?,
        base::account_data(&context.banks_client, base::STATE).await?,
        base::account_data(&context.banks_client, base::MIRROR).await?,
        base::account_data(&context.banks_client, META).await?,
    );
    let initial_root = state_root(&current_state, state_len, &current_mirror);
    let rollback = execute_sim(
        &mut context.banks_client,
        &context.payer,
        &authority,
        base::NORMAL_LIMIT,
        request(
            simulation::ROLLBACK_TEST,
            initial_tic,
            0,
            initial_root,
            input_root_value,
            0,
        ),
    )
    .await?;
    base::require_refusal(&rollback, "Custom(254)")?;
    let after_rollback = (
        base::account_data(&context.banks_client, base::CONTEXT).await?,
        base::account_data(&context.banks_client, base::STATE).await?,
        base::account_data(&context.banks_client, base::MIRROR).await?,
        base::account_data(&context.banks_client, META).await?,
    );
    if snapshot != after_rollback {
        return Err("rollback mutated SIM1 durable accounts".into());
    }
    let controls = [
        (
            "duplicate",
            request(
                simulation::TICK,
                initial_tic.wrapping_sub(1),
                u64::MAX,
                initial_root,
                input_root_value,
                0,
            ),
        ),
        (
            "gap",
            request(
                simulation::TICK,
                initial_tic.wrapping_add(1),
                1,
                initial_root,
                input_root_value,
                1,
            ),
        ),
        (
            "reorder",
            request(
                simulation::TICK,
                initial_tic,
                0,
                initial_root,
                input_root_value,
                1,
            ),
        ),
        (
            "wrong_root",
            request(
                simulation::TICK,
                initial_tic,
                0,
                [9; 32],
                input_root_value,
                0,
            ),
        ),
    ];
    let mut controls_json = serde_json::Map::new();
    for (label, control) in controls {
        let result = execute_sim(
            &mut context.banks_client,
            &context.payer,
            &authority,
            base::NORMAL_LIMIT,
            control,
        )
        .await?;
        assert_refused(&result, label)?;
        let after = (
            base::account_data(&context.banks_client, base::CONTEXT).await?,
            base::account_data(&context.banks_client, base::STATE).await?,
            base::account_data(&context.banks_client, base::MIRROR).await?,
            base::account_data(&context.banks_client, META).await?,
        );
        if after != snapshot {
            return Err(format!("{label} refusal mutated durable accounts").into());
        }
        controls_json.insert(label.into(), serde_json::json!({"refused":true,"outputs_preserved":true,"result":format!("{:?}", result.result)}));
    }
    let mut malformed = input.clone();
    malformed[12] = 63;
    context.set_account(&INPUT, &AccountSharedData::from(account(malformed)));
    let malformed_result = execute_sim(
        &mut context.banks_client,
        &context.payer,
        &authority,
        base::NORMAL_LIMIT,
        init_request,
    )
    .await?;
    assert_refused(&malformed_result, "malformed")?;
    context.set_account(&INPUT, &AccountSharedData::from(account(input.clone())));
    let after_malformed = (
        base::account_data(&context.banks_client, base::CONTEXT).await?,
        base::account_data(&context.banks_client, base::STATE).await?,
        base::account_data(&context.banks_client, base::MIRROR).await?,
        base::account_data(&context.banks_client, META).await?,
    );
    if after_malformed != snapshot {
        return Err("malformed refusal mutated durable accounts".into());
    }
    controls_json.insert("malformed".into(), serde_json::json!({"refused":true,"outputs_preserved":true,"result":format!("{:?}", malformed_result.result)}));
    let duplicate = execute_duplicate(
        &mut context.banks_client,
        &context.payer,
        &authority,
        base::NORMAL_LIMIT,
        init_request,
    )
    .await?;
    assert_refused(&duplicate, "duplicate_account")?;
    let after_duplicate = (
        base::account_data(&context.banks_client, base::CONTEXT).await?,
        base::account_data(&context.banks_client, base::STATE).await?,
        base::account_data(&context.banks_client, base::MIRROR).await?,
        base::account_data(&context.banks_client, META).await?,
    );
    if after_duplicate != snapshot {
        return Err("duplicate-account refusal mutated durable accounts".into());
    }
    controls_json.insert(
        "duplicate_account".into(),
        serde_json::json!({"refused":true,"outputs_preserved":true}),
    );
    let wrong_authority = solana_keypair::Keypair::new();
    context.set_account(
        &BAD_AUTHORITY,
        &AccountSharedData::from(account(Vec::new())),
    );
    let wrong = execute_sim(
        &mut context.banks_client,
        &context.payer,
        &wrong_authority,
        base::NORMAL_LIMIT,
        init_request,
    )
    .await?;
    assert_refused(&wrong, "wrong_authority")?;
    let after_wrong_authority = (
        base::account_data(&context.banks_client, base::CONTEXT).await?,
        base::account_data(&context.banks_client, base::STATE).await?,
        base::account_data(&context.banks_client, base::MIRROR).await?,
        base::account_data(&context.banks_client, META).await?,
    );
    if after_wrong_authority != snapshot {
        return Err("wrong-authority refusal mutated durable accounts".into());
    }
    controls_json.insert(
        "wrong_authority".into(),
        serde_json::json!({"refused":true,"outputs_preserved":true}),
    );
    let mut tick_units = Vec::new();
    let mut max_tick = 0u64;
    for index in 0..100u32 {
        let state_len = base::u32_at(&current_state, 16) as usize;
        let tic = base::u32_at(&current_state, 20);
        let tick = execute_sim(
            &mut context.banks_client,
            &context.payer,
            &authority,
            base::NORMAL_LIMIT,
            request(
                simulation::TICK,
                tic,
                index as u64,
                state_root(&current_state, state_len, &current_mirror),
                input_root_value,
                index,
            ),
        )
        .await?;
        let cu = base::units(&tick)?;
        max_tick = max_tick.max(cu);
        tick_units.push(cu);
        if tick.result.is_err() || cu > base::NORMAL_LIMIT as u64 {
            return Err(format!(
                "SIM1 tick {index} failed result={:?} units={cu}",
                tick.result
            )
            .into());
        }
        current_state = base::account_data(&context.banks_client, base::STATE).await?;
        current_mirror = base::account_data(&context.banks_client, base::MIRROR).await?;
        let expected = &oracle.states[index as usize];
        if current_state[..expected.len()] != expected[..]
            || current_state[expected.len()..]
                .iter()
                .any(|&byte| byte != 0)
            || current_mirror != oracle.mirrors[index as usize]
        {
            return Err(format!("native mismatch at tick {index}").into());
        }
        let meta = base::account_data(&context.banks_client, META).await?;
        let state_len = base::u32_at(&current_state, 16) as usize;
        let expected_root = state_root(&current_state, state_len, &current_mirror);
        if u64::from_le_bytes(meta[24..32].try_into().unwrap()) != index as u64 + 1
            || u32::from_le_bytes(meta[36..40].try_into().unwrap()) != index + 1
            || meta[META_INPUT_ROOT..META_INPUT_ROOT + 32] != input_root_value
            || meta[META_STATE_ROOT..META_STATE_ROOT + 32] != expected_root
        {
            return Err(format!("SIM1 sequence/root metadata mismatch at tick {index}").into());
        }
    }
    controls_json.insert(
        "rollback".into(),
        serde_json::json!({"refused":true,"outputs_preserved":true}),
    );
    fs::write(
        receipt.join("controls.json"),
        serde_json::to_vec_pretty(&controls_json)?,
    )?;
    fs::write(receipt.join("input.ci3"), &input)?;
    fs::write(receipt.join("commands.bin"), &commands)?;
    fs::write(receipt.join("output-state.st1"), &current_state)?;
    fs::write(receipt.join("output-mirror.mp1"), &current_mirror)?;
    fs::write(receipt.join("identity.json"), format!("{{\n  \"profile\": \"DOOM-SIM/1\",\n  \"image_sha256\": \"{}\",\n  \"manifest_sha256\": \"{}\",\n  \"driver_sha256\": \"{}\",\n  \"resource_sha256\": \"{}\",\n  \"wad_sha256\": \"{}\",\n  \"phase_units\": {:?},\n  \"tick_count\": 100,\n  \"max_tick_units\": {max_tick},\n  \"normal_limit\": {},\n  \"qualification_max_units\": {},\n  \"native_equal\": true,\n  \"render\": \"not called\"\n}}\n", base::sha(&image_bytes), base::sha(&manifest_bytes), base::sha(include_bytes!("continuation_split_sbf.rs")), base::sha(&resource), base::WAD_CONCAT_SHA256, phase_units, base::NORMAL_LIMIT, base::QUALIFICATION_MAX_UNITS))?;
    fs::write(receipt.join("run.json"), format!("{{\n  \"phase_units\": {:?},\n  \"tick_units\": {:?},\n  \"max_tick_units\": {max_tick},\n  \"qualification\": {}\n}}\n", phase_units, tick_units, max_tick <= base::QUALIFICATION_MAX_UNITS))?;
    println!("DOOM-SIM/1 passed: phase_cu={phase_units:?} max_tick_cu={max_tick} qualification={} receipt={}", max_tick <= base::QUALIFICATION_MAX_UNITS, receipt.display());
    Ok(())
}
