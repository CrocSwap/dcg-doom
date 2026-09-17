use std::{env, error::Error, path::PathBuf};

use solana_account::Account;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_message::Message;
use solana_program_test::{ProgramTest, ProgramTestBanksClientExt};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xda; 32]);
const STATE: Pubkey = Pubkey::new_from_array([0xab; 32]);
const PREFIX: Pubkey = Pubkey::new_from_array([0xac; 32]);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(env::args().nth(1).ok_or("usage: address_probe doom_sbf.so")?)
        .canonicalize()?;
    if path.file_name().and_then(|s| s.to_str()) != Some("doom_sbf.so") {
        return Err("expected doom_sbf.so".into());
    }
    env::set_var("BPF_OUT_DIR", path.parent().unwrap());
    env::set_var("SBF_OUT_DIR", path.parent().unwrap());
    let mut addresses = Vec::new();
    for instance in 0..2 {
        let mut bank = ProgramTest::default();
        bank.prefer_bpf(true);
        bank.add_program("doom_sbf", PROGRAM, None);
        for (key, size) in [(STATE, 3 * 1024 * 1024), (PREFIX, 4096)] {
            bank.add_account(key, Account {
                lamports: 100_000_000_000,
                data: vec![0; size],
                owner: PROGRAM,
                executable: false,
                rent_epoch: 0,
            });
        }
        let mut context = bank.start_with_context().await;
        context.warp_to_slot(3)?;
        let mut last = 0;
        for sequence in 0..3u8 {
            let blockhash = context.banks_client
                .get_new_latest_blockhash(&context.last_blockhash).await?;
            let mut accounts = Vec::new();
            if sequence == 2 {
                accounts.push(AccountMeta::new_readonly(PREFIX, false));
            }
            accounts.push(AccountMeta::new(STATE, false));
            let mut data = b"DOOM-AP1".to_vec();
            data.push(sequence);
            let instruction = Instruction { program_id: PROGRAM, accounts, data };
            let message = Message::new(&[instruction], Some(&context.payer.pubkey()));
            let tx = Transaction::new(&[&context.payer], message, blockhash);
            context.banks_client.process_transaction(tx).await?;
            let data = context.banks_client.get_account(STATE).await?
                .ok_or("missing state")?.data;
            let address = u64::from_le_bytes(data[..8].try_into().unwrap());
            let previous = u64::from_le_bytes(data[8..16].try_into().unwrap());
            assert_eq!(previous, last);
            assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), u64::from(sequence));
            assert_ne!(address, 0);
            println!("instance={instance} transaction={sequence} prefix={} address=0x{address:016x} persisted_previous=0x{previous:016x}", sequence == 2);
            addresses.push(address);
            last = address;
        }
    }
    println!("same_layout_transactions_stable={} instances_stable={} account_layout_change_stable={}",
        addresses[0] == addresses[1] && addresses[3] == addresses[4],
        addresses[..3] == addresses[3..],
        addresses[1] == addresses[2] && addresses[4] == addresses[5]);
    Ok(())
}
