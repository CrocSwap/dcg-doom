use std::{env, error::Error, fs, path::PathBuf};

use doom_sbf::bring_up::{CHUNK_BYTES, CONFIG_BYTES, INIT_MAGIC, WAD_HASH};
use solana_account::Account;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_program_test::ProgramTest;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xda; 32]);

fn account(data: Vec<u8>) -> Account {
    Account {
        lamports: 100_000_000_000,
        data,
        owner: PROGRAM,
        executable: false,
        rent_epoch: 0,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        return Err("usage: bring_up deploy/doom_sbf.so STEP3_ACCOUNT_DIR (offline init-prefix diagnostic; not a replay)".into());
    }
    let so = PathBuf::from(&args[1]).canonicalize()?;
    if so.file_name().and_then(|s| s.to_str()) != Some("doom_sbf.so") {
        return Err("ProgramTest loads doom_sbf.so; refusing a different filename".into());
    }
    let root = PathBuf::from(&args[2]).canonicalize()?;
    env::set_var("BPF_OUT_DIR", so.parent().unwrap());
    env::set_var("SBF_OUT_DIR", so.parent().unwrap());
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(1_400_000);
    bank.add_program("doom_sbf", PROGRAM, None);
    let config = Keypair::new();
    bank.add_account(config.pubkey(), account(vec![0; CONFIG_BYTES]));
    let mut metas = vec![AccountMeta::new(config.pubkey(), true)];
    for index in 0..5 {
        let bytes = fs::read(root.join(format!("wad-{index:03}.bin")))?;
        assert_eq!(bytes.len(), CHUNK_BYTES);
        let key = Pubkey::new_from_array([0xe0 + index; 32]);
        bank.add_account(key, account(bytes));
        metas.push(AccountMeta::new_readonly(key, false));
    }
    let (client, payer, _) = bank.start().await;
    for label in ["honest", "stale_hash", "malformed", "permuted_chunks"] {
        let mut data = INIT_MAGIC.to_vec();
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&WAD_HASH);
        let mut accounts = metas.clone();
        match label {
            "stale_hash" => data[12] ^= 1,
            "malformed" => {
                data.pop();
            }
            "permuted_chunks" => accounts.swap(1, 2),
            _ => (),
        }
        let work = Instruction {
            program_id: PROGRAM,
            accounts,
            data,
        };
        let data_bytes = work.data.len();
        let instructions = [
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            work,
        ];
        let hash = client.get_latest_blockhash().await?;
        let message = Message::new_with_blockhash(&instructions, Some(&payer.pubkey()), &hash);
        let tx = Transaction::new(&[&payer, &config], message, hash);
        let packet_bytes = bincode::serialize(&tx)?.len();
        assert!(packet_bytes <= 1_232);
        let result = client.simulate_transaction(tx).await?;
        let details = result.simulation_details.ok_or("missing logs")?;
        println!("MEASURE {label} result={:?} tx_cu={} instruction_bytes={data_bytes} packet_bytes={packet_bytes} loaded_data_bytes={}", result.result, details.units_consumed, CONFIG_BYTES + 5 * CHUNK_BYTES);
        for log in &details.logs {
            println!("{log}");
        }
        let status = format!("{:?}", result.result);
        match label {
            "honest" => {
                assert!(details
                    .logs
                    .iter()
                    .any(|x| x.contains("DOOM_INIT_BEFORE_WAD_SHA256")));
                assert!(!details
                    .logs
                    .iter()
                    .any(|x| x.contains("DOOM_INIT_AFTER_WAD_SHA256")));
                assert!(status.contains("ComputationalBudgetExceeded"));
            }
            "stale_hash" => assert!(status.contains("Custom(1)")),
            "malformed" => assert!(status.contains("InvalidInstructionData")),
            "permuted_chunks" => assert!(status.contains("InvalidAccountData")),
            _ => unreachable!(),
        }
        assert_eq!(
            client.get_account(config.pubkey()).await?.unwrap().data,
            vec![0; CONFIG_BYTES]
        );
    }
    println!("STEP5_ACCEPTANCE=FAIL load_level/tic/exhausted_pool/golden_replay=UNIMPLEMENTED");
    Ok(())
}
