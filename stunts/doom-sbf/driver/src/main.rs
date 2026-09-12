//! SBF ProgramTest driver for the Doom-on-SBF hot-loop spike.
//!
//! The `prefer_bpf(true)`/`add_program(..., None)` pairing is intentional:
//! every reported CU value is from the supplied `doom_sbf.so`, not from a
//! native Rust processor.  `BPF_OUT_DIR` and `SBF_OUT_DIR` both name deploy/ so
//! the version-specific ProgramTest loader finds that same image.

use std::{env, error::Error, path::PathBuf};

use doom_sbf::{
    FRAME_BYTES, STATE_BYTES, W1_COLUMN, W2_SPAN, W3_BSP_ONLY, W3_FIXED_DIV_ONLY, W3_FIXED_MUL_DIV,
    W3_FIXED_MUL_ONLY, W3_FULL, WRITE_STRIP,
};
use solana_account::Account;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_program_test::{BanksClient, ProgramTest};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM_ID: Pubkey = Pubkey::new_from_array([0xd0; 32]);
const STATE_ID: Pubkey = Pubkey::new_from_array([0xd1; 32]);
const W1_FRAME_ID: Pubkey = Pubkey::new_from_array([0xd2; 32]);
const W2_FRAME_ID: Pubkey = Pubkey::new_from_array([0xd3; 32]);
const REQUIRED_METER: u32 = 1_400_000;

fn strip_id(strips: usize, index: usize) -> Pubkey {
    let mut bytes = [0u8; 32];
    bytes[0] = 0xe0;
    bytes[1] = strips as u8;
    bytes[2] = index as u8;
    Pubkey::new_from_array(bytes)
}

fn so_path() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(path) = env::args().nth(1) {
        return Ok(PathBuf::from(path));
    }
    Err("pass the stripped deploy/doom_sbf.so path from cargo-build-sbf".into())
}

fn state_image() -> Vec<u8> {
    let mut state = vec![0u8; STATE_BYTES];
    for (index, byte) in state.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(29).wrapping_add(7);
    }
    state
}

fn account(data: Vec<u8>) -> Account {
    Account {
        lamports: 10_000_000_000,
        data,
        owner: PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    }
}

fn work_instruction(mode: u8, frame: Pubkey) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(STATE_ID, false),
            AccountMeta::new(frame, false),
        ],
        data: vec![mode],
    }
}

fn state_instruction(mode: u8) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(STATE_ID, false)],
        data: vec![mode],
    }
}

fn strip_instruction(frame: Pubkey) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(STATE_ID, false),
            AccountMeta::new(frame, false),
        ],
        data: vec![WRITE_STRIP],
    }
}

async fn simulate(
    banks_client: &mut BanksClient,
    payer: &Keypair,
    label: &str,
    meter: u32,
    work: Instruction,
) -> Result<u64, Box<dyn Error>> {
    let instructions = [
        ComputeBudgetInstruction::set_compute_unit_limit(meter),
        work,
    ];
    // Simulated 1M+ CU instructions advance ProgramTest's bank enough that a
    // blockhash obtained before the whole suite can expire.  Fetch it per
    // transaction without changing the supplied SBF image or account setup.
    let recent_blockhash = banks_client.get_latest_blockhash().await?;
    let message =
        Message::new_with_blockhash(&instructions, Some(&payer.pubkey()), &recent_blockhash);
    let tx = Transaction::new(&[payer], message, recent_blockhash);
    let simulated = banks_client.simulate_transaction(tx).await?;
    if let Some(Err(err)) = simulated.result {
        return Err(format!(
            "{label} failed: {err:?}; logs={:?}",
            simulated.simulation_details
        )
        .into());
    }
    let details = simulated
        .simulation_details
        .ok_or("simulation returned no program-log details")?;
    let log = details
        .logs
        .iter()
        .find(|line| line.contains("consumed") && line.contains("compute units"))
        .cloned()
        .unwrap_or_else(|| "<no consumed-compute-units program log>".to_owned());
    println!("MEASURE {label} units={} log={log}", details.units_consumed);
    Ok(details.units_consumed)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let so = so_path()?;
    if !so.is_file() {
        return Err(format!("SBF .so not found: {}", so.display()).into());
    }
    let deploy = so.parent().ok_or("SBF .so has no deploy directory")?;
    env::set_var("BPF_OUT_DIR", deploy);
    env::set_var("SBF_OUT_DIR", deploy);
    let meter = env::var("DOOM_SBF_COMPUTE_LIMIT")
        .ok()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(REQUIRED_METER);
    let skip_render = env::var_os("DOOM_SBF_SKIP_RENDER").is_some();
    println!("COMPUTE_LIMIT {meter}");
    println!("RENDER_SKIPPED {skip_render}");

    let mut program_test = ProgramTest::default();
    program_test.prefer_bpf(true);
    program_test.set_compute_max_units(meter as u64);
    program_test.add_program("doom_sbf", PROGRAM_ID, None);
    program_test.add_account(STATE_ID, account(state_image()));
    program_test.add_account(W1_FRAME_ID, account(vec![0; FRAME_BYTES]));
    program_test.add_account(W2_FRAME_ID, account(vec![0; FRAME_BYTES]));
    for strips in [1usize, 4, 8, 16] {
        for index in 0..strips {
            program_test.add_account(
                strip_id(strips, index),
                account(vec![0; FRAME_BYTES / strips]),
            );
        }
    }
    let (mut banks_client, payer, _) = program_test.start().await;

    if !skip_render {
        simulate(
            &mut banks_client,
            &payer,
            "w1_column",
            meter,
            work_instruction(W1_COLUMN, W1_FRAME_ID),
        )
        .await?;
        simulate(
            &mut banks_client,
            &payer,
            "w2_span",
            meter,
            work_instruction(W2_SPAN, W2_FRAME_ID),
        )
        .await?;
    }
    let w3 = simulate(
        &mut banks_client,
        &payer,
        "w3_full",
        meter,
        state_instruction(W3_FULL),
    )
    .await?;
    let w3_bsp = simulate(
        &mut banks_client,
        &payer,
        "w3_bsp_only",
        meter,
        state_instruction(W3_BSP_ONLY),
    )
    .await?;
    let w3_mul = simulate(
        &mut banks_client,
        &payer,
        "w3_fixedmul_only",
        meter,
        state_instruction(W3_FIXED_MUL_ONLY),
    )
    .await?;
    let w3_div = simulate(
        &mut banks_client,
        &payer,
        "w3_fixeddiv_only",
        meter,
        state_instruction(W3_FIXED_DIV_ONLY),
    )
    .await?;
    let w3_pair = simulate(
        &mut banks_client,
        &payer,
        "w3_fixedmul_div",
        meter,
        state_instruction(W3_FIXED_MUL_DIV),
    )
    .await?;

    println!(
        "SUMMARY w3_full={w3} w3_bsp={w3_bsp} w3_mul={w3_mul} w3_div={w3_div} w3_pair={w3_pair}"
    );
    for strips in [1usize, 4, 8, 16] {
        let mut total = 0u64;
        let mut each = Vec::new();
        for index in 0..strips {
            let units = simulate(
                &mut banks_client,
                &payer,
                &format!("write_n{strips}_strip{index}"),
                meter,
                strip_instruction(strip_id(strips, index)),
            )
            .await?;
            total += units;
            each.push(units);
        }
        println!(
            "STRIPS n={strips} bytes_per_strip={} total_units={total} per_tx={each:?}",
            FRAME_BYTES / strips
        );
    }
    Ok(())
}
