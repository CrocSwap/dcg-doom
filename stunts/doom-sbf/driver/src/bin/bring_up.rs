use std::{env, error::Error, fs, path::PathBuf};

use doom_sbf::bring_up::{
    CHUNK_BYTES, COMPRESS_WINDOW_BYTES, CONFIG_BYTES, INIT_MAGIC, WAD_BYTES, WAD_HASH,
};
use solana_account::{Account, AccountSharedData};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_program_test::{processor, ProgramTest, ProgramTestBanksClientExt};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM: Pubkey = Pubkey::new_from_array([0xda; 32]);
const BUDGET: u32 = 1_190_000;

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
        return Err("usage: bring_up (--native | deploy/doom_sbf.so) STEP3_ACCOUNT_DIR; offline bounded WAD hash then DOOM-DI1 directory validation; no engine replay".into());
    }
    let native = args[1] == "--native";
    let root = PathBuf::from(&args[2]).canonicalize()?;
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
    let config = Keypair::new();
    bank.add_account(config.pubkey(), account(vec![0; CONFIG_BYTES]));
    let mut metas = vec![AccountMeta::new(config.pubkey(), true)];
    let mut tail = Vec::new();
    for index in 0..5 {
        let bytes = fs::read(root.join(format!("wad-{index:03}.bin")))?;
        assert_eq!(bytes.len(), CHUNK_BYTES);
        if index == 4 {
            tail = bytes.clone();
        }
        let key = Pubkey::new_from_array([0xe0 + index; 32]);
        bank.add_account(key, account(bytes));
        metas.push(AccountMeta::new_readonly(key, false));
    }
    let mut context = bank.start_with_context().await;
    let mut client = context.banks_client.clone();
    let mut maximum = 0;
    let mut total = 0;
    let mut steps = 0;
    let step_limit: usize = env::var("DOOM_HASH_MAX_STEPS")
        .ok()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(usize::MAX);
    for cursor in (0..WAD_BYTES)
        .step_by(COMPRESS_WINDOW_BYTES)
        .take(step_limit)
    {
        let final_step = cursor + COMPRESS_WINDOW_BYTES >= WAD_BYTES;
        let labels: &[&str] = if cursor == 0 {
            &[
                "wrong_hash",
                "wrong_version",
                "out_of_order",
                "permuted_chunks",
                "malformed",
                "honest",
            ]
        } else if final_step {
            &[
                "corrupt_tail",
                "wrong_state_version",
                "out_of_order",
                "honest",
            ]
        } else {
            &["honest"]
        };
        for &label in labels {
            let before = client.get_account(config.pubkey()).await?.unwrap();
            let mut data = INIT_MAGIC.to_vec();
            data.extend_from_slice(&1u32.to_le_bytes());
            data.extend_from_slice(&(cursor as u32).to_le_bytes());
            data.extend_from_slice(&WAD_HASH);
            let mut accounts = metas.clone();
            match label {
                "wrong_hash" => data[16] ^= 1,
                "wrong_version" => data[8] = 2,
                "out_of_order" => data[12..16]
                    .copy_from_slice(&((cursor + COMPRESS_WINDOW_BYTES) as u32).to_le_bytes()),
                "permuted_chunks" => accounts.swap(1, 2),
                "malformed" => {
                    data.pop();
                }
                "corrupt_tail" => {
                    let mut bad = tail.clone();
                    bad[32 + WAD_BYTES % doom_sbf::bring_up::PAYLOAD_BYTES - 1] ^= 1;
                    context.set_account(&metas[5].pubkey, &AccountSharedData::from(account(bad)));
                }
                "wrong_state_version" => {
                    let mut bad = before.clone();
                    bad.data[8] = 2;
                    context.set_account(&config.pubkey(), &AccountSharedData::from(bad));
                }
                _ => (),
            }
            context.last_blockhash = client
                .get_new_latest_blockhash(&context.last_blockhash)
                .await?;
            let tx = Transaction::new(
                &[&context.payer, &config],
                Message::new_with_blockhash(
                    &[
                        ComputeBudgetInstruction::set_compute_unit_limit(BUDGET),
                        Instruction {
                            program_id: PROGRAM,
                            accounts,
                            data,
                        },
                    ],
                    Some(&context.payer.pubkey()),
                    &context.last_blockhash,
                ),
                context.last_blockhash,
            );
            let packet_bytes = bincode::serialize(&tx)?.len();
            assert!(packet_bytes <= 1_232);
            let result = client.process_transaction_with_metadata(tx).await?;
            let status = format!("{:?}", result.result);
            println!("MEASURE native={native} label={label} cursor={cursor} result={status} tx_cu={} packet_bytes={packet_bytes} loaded_data_bytes={}", result.metadata.as_ref().map(|m| m.compute_units_consumed).unwrap_or(0), CONFIG_BYTES + 5 * CHUNK_BYTES);
            let details = result
                .metadata
                .as_ref()
                .ok_or(format!("missing transaction metadata: {status}"))?;
            if label == "honest" {
                assert_eq!(result.result, Ok(()), "{status} {:?}", details.log_messages);
                assert!(details.compute_units_consumed <= BUDGET as u64);
                let stored = client.get_account(config.pubkey()).await?.unwrap().data;
                assert_eq!(
                    u32::from_le_bytes(stored[12..16].try_into().unwrap()) as usize,
                    (cursor + COMPRESS_WINDOW_BYTES).min(WAD_BYTES)
                );
                assert_eq!(stored[48], u8::from(final_step));
                maximum = maximum.max(details.compute_units_consumed);
                total += details.compute_units_consumed;
                steps += 1;
            } else {
                let expected = match label {
                    "wrong_hash" | "corrupt_tail" => "Custom(1)",
                    "out_of_order" if !final_step => "Custom(2)",
                    "out_of_order" | "wrong_version" | "malformed" => "InvalidInstructionData",
                    _ => "InvalidAccountData",
                };
                assert!(status.contains(expected), "expected {expected}: {status}");
                if label == "wrong_state_version" {
                    context.set_account(&config.pubkey(), &AccountSharedData::from(before.clone()));
                }
                assert_eq!(
                    client.get_account(config.pubkey()).await?.unwrap().data,
                    before.data
                );
                if label == "corrupt_tail" {
                    context.set_account(
                        &metas[5].pubkey,
                        &AccountSharedData::from(account(tail.clone())),
                    );
                }
            }
        }
    }
    let stored = client.get_account(config.pubkey()).await?.unwrap().data;
    let complete = stored[48] == 1;
    if complete {
        assert_eq!(
            stored[64..96]
                .chunks_exact(4)
                .flat_map(|word| u32::from_le_bytes(word.try_into().unwrap()).to_be_bytes())
                .collect::<Vec<u8>>(),
            WAD_HASH,
            "final digest != committed WAD sha256"
        );
    }
    let digest: String = stored[64..96]
        .chunks_exact(4)
        .flat_map(|word| u32::from_le_bytes(word.try_into().unwrap()).to_be_bytes())
        .map(|b| format!("{b:02x}"))
        .collect();
    if complete {
        let mut cursor = 0u32;
        loop {
            let before = client.get_account(config.pubkey()).await?.unwrap();
            for label in ["before_hash", "wrong_directory_version", "ahead", "honest"] {
                let mut data = doom_sbf::bring_up::DIRECTORY_MAGIC.to_vec();
                data.extend_from_slice(&1u32.to_le_bytes());
                data.extend_from_slice(&cursor.to_le_bytes());
                match label {
                    "before_hash" => {
                        let mut bad = before.clone();
                        bad.data[48] = 0;
                        context.set_account(&config.pubkey(), &AccountSharedData::from(bad));
                    }
                    "wrong_directory_version" => data[8] = 2,
                    "ahead" => data[12..16].copy_from_slice(&(cursor + 64).to_le_bytes()),
                    _ => (),
                }
                context.last_blockhash = client
                    .get_new_latest_blockhash(&context.last_blockhash)
                    .await?;
                let tx = Transaction::new(
                    &[&context.payer, &config],
                    Message::new_with_blockhash(
                        &[
                            ComputeBudgetInstruction::set_compute_unit_limit(BUDGET),
                            Instruction {
                                program_id: PROGRAM,
                                accounts: metas.clone(),
                                data,
                            },
                        ],
                        Some(&context.payer.pubkey()),
                        &context.last_blockhash,
                    ),
                    context.last_blockhash,
                );
                let packet = bincode::serialize(&tx)?.len();
                assert!(packet <= 1_232);
                let result = client.process_transaction_with_metadata(tx).await?;
                let status = format!("{:?}", result.result);
                let units = result
                    .metadata
                    .as_ref()
                    .ok_or("missing metadata")?
                    .compute_units_consumed;
                println!("DIRECTORY native={native} label={label} cursor={cursor} result={status} tx_cu={units} packet_bytes={packet} loaded_data_bytes={}", CONFIG_BYTES + 5 * CHUNK_BYTES);
                if label == "honest" {
                    assert_eq!(result.result, Ok(()));
                    assert!(units <= BUDGET as u64);
                } else {
                    let expected = match label {
                        "before_hash" => "Custom(3)",
                        "wrong_directory_version" => "InvalidInstructionData",
                        _ => "Custom(2)",
                    };
                    assert!(status.contains(expected), "{status}");
                    let mut expected_account = before.data.clone();
                    if label == "before_hash" {
                        expected_account[48] = 0;
                    }
                    assert_eq!(
                        client.get_account(config.pubkey()).await?.unwrap().data,
                        expected_account
                    );
                    context.set_account(&config.pubkey(), &AccountSharedData::from(before.clone()));
                }
            }
            let after = client.get_account(config.pubkey()).await?.unwrap().data;
            cursor = u32::from_le_bytes(after[268..272].try_into().unwrap());
            assert_eq!(
                u32::from_le_bytes(after[272..276].try_into().unwrap()),
                1_264
            );
            assert_eq!(
                cursor,
                (u32::from_le_bytes(before.data[268..272].try_into().unwrap()) + 64).min(1_264)
            );
            if after[280] == 1 {
                break;
            }
        }
        println!("DIRECTORY_ACCEPTANCE=PASS entries={cursor}");
    }
    println!("HASH_ACCEPTANCE={} native={native} steps={steps} chunk_bytes={COMPRESS_WINDOW_BYTES} max_tx_cu={maximum} total_tx_cu={total} budget={BUDGET} digest_or_midstate={digest}", if complete { "PASS" } else { "PARTIAL" });
    println!("STEP5_ACCEPTANCE=INCOMPLETE load_level/tic/strip/golden_replay=UNIMPLEMENTED");
    Ok(())
}
