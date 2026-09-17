//! Engine-port probe: may an SBF program image hold mutable static state?
//!
//! linuxdoom-1.10 keeps its live state in ordinary C globals.  Compiled for
//! `sbf-solana-solana` those globals land in the program image's `.data` and
//! `.bss`.  Whether the runtime lets the program write them decides whether
//! the engine can be linked as-is or whether every mutable global has to be
//! relocated into account memory first.
//!
//! This binary runs the `W4_STATIC_WRITE` instruction, which writes one C
//! global and reads it back.  It needs no WAD accounts and no goldens.
//!
//! Usage: `static_probe (--native | deploy/doom_sbf.so)`

use std::{env, error::Error, path::PathBuf};

use doom_sbf::{STATE_BYTES, W4_STATIC_WRITE};
use solana_account::Account;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_program_test::{processor, ProgramTest, ProgramTestBanksClientExt};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xda; 32]);

/// The value `doom_probe_static_write` must return on its first call:
/// the initialiser XORed with the fixed argument.
const EXPECTED_FIRST: u64 = 0x5a5a_5a5a_5a5a_5a5a ^ 0x0123_4567_89ab_cdef;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        return Err("usage: static_probe (--native | deploy/doom_sbf.so)".into());
    }
    let native = args[1] == "--native";
    let mut bank = ProgramTest::default();
    if native {
        bank.prefer_bpf(false);
        bank.add_program(
            "doom_sbf",
            PROGRAM,
            processor!(doom_sbf::process_instruction),
        );
    } else {
        let so = PathBuf::from(&args[1]).canonicalize()?;
        if so.file_name().and_then(|s| s.to_str()) != Some("doom_sbf.so") {
            return Err("ProgramTest loads doom_sbf.so; refusing a different filename".into());
        }
        env::set_var("BPF_OUT_DIR", so.parent().unwrap());
        env::set_var("SBF_OUT_DIR", so.parent().unwrap());
        bank.prefer_bpf(true);
        bank.add_program("doom_sbf", PROGRAM, None);
    }
    bank.set_compute_max_units(1_400_000);

    let state = Keypair::new();
    bank.add_account(
        state.pubkey(),
        Account {
            lamports: 100_000_000_000,
            data: vec![0; STATE_BYTES],
            owner: PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    );

    let mut context = bank.start_with_context().await;
    // A program added to ProgramTest becomes invocable only from the next
    // slot; without this the invocation fails "Program is not deployed"
    // before any program code runs.
    context.warp_to_slot(3)?;
    let mut client = context.banks_client.clone();
    let last_blockhash = client
        .get_new_latest_blockhash(&context.last_blockhash)
        .await?;
    let instruction = Instruction {
        program_id: PROGRAM,
        accounts: vec![AccountMeta::new(state.pubkey(), false)],
        data: vec![W4_STATIC_WRITE],
    };
    let payer_key = context.payer.pubkey();
    let message = Message::new(&[instruction], Some(&payer_key));
    let transaction = Transaction::new(&[&context.payer], message, last_blockhash);
    let outcome = client.process_transaction(transaction).await;

    match outcome {
        Ok(()) => {
            let data = client
                .get_account(state.pubkey())
                .await?
                .ok_or("probe state account vanished")?
                .data;
            let observed = u64::from_le_bytes(data[..8].try_into().unwrap());
            println!(
                "STATIC_WRITE=ACCEPTED processor={} observed=0x{observed:016x} expected=0x{EXPECTED_FIRST:016x} equal={}",
                if native { "native" } else { "sbf" },
                observed == EXPECTED_FIRST
            );
        }
        Err(error) => {
            println!(
                "STATIC_WRITE=REFUSED processor={} error={error:?}",
                if native { "native" } else { "sbf" }
            );
        }
    }
    Ok(())
}
