//! DOOM-CX1 ProgramTest: the patch-0007 engine image loads with zero
//! `.data`/`.bss`, and one context field writes and reads back through the
//! compile-time constant DOOM_CTX under the mapping guard.
//!
//! * Honest layout (context account first and last): guard passes, the probe
//!   cell reads back both values, and the scratch triple reports the observed
//!   mapping (pinned to the M1021 constant), the SBF-side sizeof (pinned to
//!   the host-measured constant), and the readback.
//! * Moved layout (a read-only prefix account first, context second, exactly
//!   M1021's PREFIX control): the mapping moves and the guard refuses with
//!   the instruction's own code, never a generic loader or validation error.

use std::{env, error::Error, path::PathBuf};

use doom_sbf::{CTX_GUARD_REFUSED, CTX_MAGIC, CTX_MIN_BYTES};
use solana_account::Account;
use solana_banks_client::BanksClientError;
use solana_instruction::{account_meta::AccountMeta, error::InstructionError, Instruction};
use solana_message::Message;
use solana_program_test::{ProgramTest, ProgramTestBanksClientExt};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xda; 32]);
const CTX: Pubkey = Pubkey::new_from_array([0xcc; 32]);
const PREFIX: Pubkey = Pubkey::new_from_array([0xac; 32]);

/// M1021: first account data at a fixed layout.
const PINNED_ADDR: u64 = 0x4000_0006_0;
/// Host-measured sizeof(doom_ctx_t) (see CTX_MIN_BYTES).
const CTX_STRUCT_BYTES: usize = 3_440_920;

const V1: u64 = 0x0123_4567_89ab_cdef;
const V2: u64 = 0xfedc_ba98_7654_3210;

fn ctx_account() -> Account {
    Account {
        lamports: 1_000_000_000_000,
        data: vec![0; CTX_MIN_BYTES],
        owner: PROGRAM,
        executable: false,
        rent_epoch: 0,
    }
}

fn instruction(accounts: Vec<AccountMeta>, value: u64) -> Instruction {
    let mut data = CTX_MAGIC.to_vec();
    data.extend_from_slice(&value.to_le_bytes());
    Instruction {
        program_id: PROGRAM,
        accounts,
        data,
    }
}

fn scratch(data: &[u8]) -> (u64, u64, u64) {
    let u = |o: usize| u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
    (
        u(CTX_STRUCT_BYTES),
        u(CTX_STRUCT_BYTES + 8),
        u(CTX_STRUCT_BYTES + 16),
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let path =
        PathBuf::from(env::args().nth(1).ok_or("usage: ctx_probe doom_sbf.so")?).canonicalize()?;
    if path.file_name().and_then(|s| s.to_str()) != Some("doom_sbf.so") {
        return Err("expected doom_sbf.so".into());
    }
    env::set_var("BPF_OUT_DIR", path.parent().unwrap());
    env::set_var("SBF_OUT_DIR", path.parent().unwrap());

    // Honest layout: the context account travels alone (first == last).
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.add_program("doom_sbf", PROGRAM, None);
    bank.add_account(CTX, ctx_account());
    let mut context = bank.start_with_context().await;
    for value in [V1, V2] {
        let blockhash = context
            .banks_client
            .get_new_latest_blockhash(&context.last_blockhash)
            .await?;
        let tx = Transaction::new(
            &[&context.payer],
            Message::new(
                &[instruction(vec![AccountMeta::new(CTX, false)], value)],
                Some(&context.payer.pubkey()),
            ),
            blockhash,
        );
        context.banks_client.process_transaction(tx).await?;
        let data = context
            .banks_client
            .get_account(CTX)
            .await?
            .ok_or("missing ctx")?
            .data;
        let cell = u64::from_le_bytes(data[..8].try_into().unwrap());
        let (observed, size, readback) = scratch(&data);
        println!(
            "honest value=0x{value:016x} probe_cell=0x{cell:016x} observed=0x{observed:016x} sbf_size={size} readback=0x{readback:016x}"
        );
        assert_eq!(cell, value, "probe cell readback");
        assert_eq!(
            observed, PINNED_ADDR,
            "mapped address pins the M1021 constant"
        );
        assert_eq!(
            size as usize, CTX_STRUCT_BYTES,
            "SBF sizeof matches the host measurement"
        );
        assert_eq!(readback, value, "scratch readback");
    }

    // Moved layout: a read-only prefix shifts the context account to second
    // position, its mapping moves (M1021), and the guard refuses with its own
    // code.  The instruction still names the context account last, so this is
    // a guard refusal on the right account, not a validation error.
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.add_program("doom_sbf", PROGRAM, None);
    bank.add_account(
        PREFIX,
        Account {
            lamports: 100_000_000_000,
            data: vec![0; 4096],
            owner: PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    );
    bank.add_account(CTX, ctx_account());
    let mut context = bank.start_with_context().await;
    let blockhash = context
        .banks_client
        .get_new_latest_blockhash(&context.last_blockhash)
        .await?;
    let tx = Transaction::new(
        &[&context.payer],
        Message::new(
            &[instruction(
                vec![
                    AccountMeta::new_readonly(PREFIX, false),
                    AccountMeta::new(CTX, false),
                ],
                V1,
            )],
            Some(&context.payer.pubkey()),
        ),
        blockhash,
    );
    let err = context
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("moved context account must be refused");
    println!("moved refusal: {err:?}");
    assert!(
        matches!(
            err,
            BanksClientError::TransactionError(TransactionError::InstructionError(
                0,
                InstructionError::Custom(code),
            )) if code == CTX_GUARD_REFUSED
        ),
        "refusal must carry the guard's own code"
    );
    println!("ctx_probe: honest write/read x2 pinned, moved-position refusal carries Custom({CTX_GUARD_REFUSED})");
    Ok(())
}
