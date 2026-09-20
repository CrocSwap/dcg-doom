//! Real DOOM-E2S/1 ProgramTest runner.
//!
//! The positive input is never synthesized: state, mirror, CI2 controller,
//! and expected native output are decoded from a retained native EX2 receipt;
//! the five WAD account images are read from the retained account-image
//! capture.  The supplied SBF image is loaded with `prefer_bpf(true)` and no
//! native processor.  A raised-limit rerun, when requested, is diagnostic and
//! never changes the acceptance label.

use std::{env, error::Error, fs, path::PathBuf};

use doom_sbf::{ex2, ex2_sbf};
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

const CONTEXT: Pubkey = Pubkey::new_from_array([0xc0; 32]);
const STATE: Pubkey = Pubkey::new_from_array([0xc1; 32]);
const MIRROR: Pubkey = Pubkey::new_from_array([0xc2; 32]);
const CONTROLLER: Pubkey = Pubkey::new_from_array([0xc3; 32]);
const BAD_CONTROLLER: Pubkey = Pubkey::new_from_array([0xc4; 32]);

fn read(path: &PathBuf) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?)
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
) -> Result<(), Box<dyn Error>> {
    fs::write(
        receipt.join("report.json"),
        format!(
            "{{\n  \"status\": {:?},\n  \"acceptance\": \"state-only-real-tic\",\n  \"first_blocker\": {:?},\n  \"normal_limit\": {},\n  \"normal_units\": {:?},\n  \"diagnostic_limit\": {},\n  \"diagnostic_units\": {:?},\n  \"malformed_controller_refusal\": {:?},\n  \"positive_fixture\": \"retained native EX2 receipt plus retained WAD account images\"\n}}\n",
            status,
            first_blocker,
            NORMAL_LIMIT,
            normal_units,
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let capture =
        PathBuf::from(args.next().ok_or(
            "usage: ex2_sbf ACCOUNT_CAPTURE_ROOT RECEIPT_DIR doom_sbf.so NATIVE_RECEIPT_DIR",
        )?)
        .canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt dir")?);
    let so = PathBuf::from(args.next().ok_or("missing doom_sbf.so")?).canonicalize()?;
    let native_receipt =
        PathBuf::from(args.next().ok_or("missing native receipt dir")?).canonicalize()?;
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    if !so.is_file() || so.file_name().and_then(|name| name.to_str()) != Some("doom_sbf.so") {
        return Err("expected a stripped deploy/doom_sbf.so rehearsal image".into());
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err(format!("refusing non-empty receipt directory {}", receipt.display()).into());
    }
    fs::create_dir_all(&receipt)?;

    let input_wire = read(&native_receipt.join("entry-0-input.ex2"))?;
    let output_wire = read(&native_receipt.join("entry-0-output.ex2"))?;
    let input = ex2::decode(&input_wire)?;
    let expected = ex2::decode(&output_wire)?;
    if input.state.len() > ex2_sbf::STATE_MAX_BYTES
        || input.mirror.len() != ex2_sbf::MIRROR_BYTES
        || input.controller.len() != ex2_sbf::CONTROLLER_BYTES
    {
        return Err("retained native receipt has an out-of-bound EX2 input".into());
    }
    let capture_mirror = read(&capture.join("mobj_pool.bin"))?;
    if capture_mirror != input.mirror {
        return Err("retained native EX2 mirror differs from the account-loader capture".into());
    }
    let mut wad_accounts = Vec::new();
    for ordinal in 0..ex2_sbf::WAD_ACCOUNT_COUNT {
        let bytes = read(&capture.join(format!("account/wad-{ordinal:03}.bin")))?;
        if bytes.len() != ex2_sbf::WAD_ACCOUNT_BYTES {
            return Err(format!("WAD account {ordinal} has {} bytes", bytes.len()).into());
        }
        wad_accounts.push(bytes);
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

    env::set_var("BPF_OUT_DIR", so.parent().unwrap());
    env::set_var("SBF_OUT_DIR", so.parent().unwrap());
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(DIAGNOSTIC_LIMIT as u64);
    bank.add_program("doom_sbf", PROGRAM, None);
    bank.add_account(CONTEXT, account(vec![0; ex2_sbf::CONTEXT_BYTES]));
    for ordinal in 0..ex2_sbf::WAD_ACCOUNT_COUNT {
        bank.add_account(
            Pubkey::new_from_array([0xd0 + ordinal as u8; 32]),
            account(wad_accounts[ordinal].clone()),
        );
    }
    bank.add_account(STATE, account(padded_state(input.state)));
    bank.add_account(MIRROR, account(input.mirror.to_vec()));
    bank.add_account(CONTROLLER, account(input.controller.to_vec()));
    bank.add_account(BAD_CONTROLLER, account(bad_controller));
    let mut context = bank.start_with_context().await;
    let mut client = context.banks_client;

    let bad = execute(
        &mut client,
        &context.payer,
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
        &context.payer,
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
            )?;
            return Err("real SBF tic output differs from retained native output".into());
        }
        let status = if normal_units.unwrap_or(u64::MAX) <= 1_190_000 {
            "measured-accepted-normal-budget"
        } else {
            "measured-over-qualification-budget"
        };
        receipt_json(&receipt, status, "none", normal_units, None, &control)?;
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
        let diagnostic = execute(
            &mut client,
            &context.payer,
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
        )?;
        None
    };
    println!(
        "SBF tic failed at normal budget: error={normal_error} units={normal_units:?} diagnostic_units={diagnostic_units:?} receipt={}",
        receipt.display()
    );
    Err("real SBF one-tic gate failed; see report.json for the first blocker".into())
}
