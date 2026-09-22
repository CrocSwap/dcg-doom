//! Where the DOOM-SIM/1 per-tic compute goes (brief item 3).
//!
//! Replays the retained SIM/1 flow (real E2R/4 reconstruction, SIM/1 INIT,
//! then N real TICKs of DEMO1 commands) on a `diagnostic-profile` legacy
//! image and splits every tic's compute by the phase markers in
//! `simulation.rs` (800-826) and `doom_sbf_sim_tick` (810-819).  Each marker
//! logs the remaining compute; the difference between consecutive markers is
//! the cost of the code between them plus one marker (~200 CU: the
//! `sol_remaining_compute_units` and `sol_log_64` syscalls).  The diagnostic
//! image is profile-only and is not a qualification image.

#[path = "continuation_split_sbf.rs"]
#[allow(dead_code)]
mod base;

use std::{collections::BTreeMap, env, error::Error, fs, path::PathBuf};

use doom_sbf::{continuation_split_sbf, resource_sbf, simulation};
use sha2::{Digest, Sha256};
use solana_banks_interface::BanksTransactionResultWithMetadata;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::Instruction;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const INPUT: Pubkey = Pubkey::new_from_array([0xeb; 32]);
const META: Pubkey = Pubkey::new_from_array([0xec; 32]);
const PHASE_MAGIC: &str = "0x444f4f4d50484153";
const SEGMENTS: &[(u64, u64, &str)] = &[
    (800, 801, "rust.validate_context_ready"),
    (801, 802, "rust.validate_input_ci3_hash"),
    (802, 803, "rust.validate_mirror_bytewise_zero_scan"),
    (803, 804, "rust.validate_state_header"),
    (804, 805, "rust.pre_state_root_hash_st1_mp1"),
    (805, 806, "rust.account_set_hash"),
    (806, 807, "rust.context_root_hash"),
    (807, 808, "rust.validate_meta_and_request"),
    (808, 809, "rust.command_copy_and_reborrow"),
    (809, 810, "ffi.enter_c"),
    (810, 811, "c.guard_arm_demo_cursor"),
    (811, 812, "c.G_Ticker"),
    (812, 813, "c.arena_mark_malloc_mirror"),
    (813, 814, "c.fill_pool_MP1_derive"),
    (814, 815, "c.DoomStateEncode_ST1"),
    (815, 816, "c.memcpy_ST1"),
    (816, 817, "c.zero_tail_bytewise_loop"),
    (817, 818, "c.memcpy_MP1"),
    (818, 819, "c.free_and_rewind"),
    (819, 820, "ffi.return_to_rust"),
    (820, 821, "rust.post_validate_state"),
    (821, 822, "rust.post_validate_mirror_bytewise_zero_scan"),
    (822, 824, "rust.post_state_root_hash_st1_mp1"),
    (824, 825, "rust.post_context_root_hash"),
    (825, 826, "rust.fill_meta"),
];

fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn state_root(state: &[u8], mirror: &[u8]) -> [u8; 32] {
    let len = base::u32_at(state, 16) as usize;
    hash_parts(&[b"DOOM-SIM/1-STATE\0", &state[..len], mirror])
}

/// Parses `(phase, remaining)` pairs from the diagnostic marker log lines.
fn markers(result: &BanksTransactionResultWithMetadata) -> Vec<(u64, u64)> {
    let logs = result
        .metadata
        .as_ref()
        .map(|m| m.log_messages.clone())
        .unwrap_or_default();
    logs.iter()
        .filter_map(|line| {
            let body = line.strip_prefix("Program log: ")?;
            let fields: Vec<&str> = body.split(", ").collect();
            if fields.len() != 5 || fields[0] != PHASE_MAGIC {
                return None;
            }
            let parse = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok();
            Some((parse(fields[1])?, parse(fields[2])?))
        })
        .collect()
}

async fn sim_tx(
    client: &mut solana_banks_client::BanksClient,
    payer: &solana_keypair::Keypair,
    authority: &solana_keypair::Keypair,
    request: simulation::Request,
) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let ix = Instruction {
        program_id: base::PROGRAM,
        accounts: vec![
            base::meta(base::CONTEXT, true, false),
            base::meta(base::STATE, true, false),
            base::meta(base::MIRROR, true, false),
            base::meta(INPUT, false, false),
            base::meta(META, true, false),
            base::meta(authority.pubkey(), false, true),
        ],
        data: request.encode().to_vec(),
    };
    let tx = Transaction::new(
        &[payer, authority],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(base::NORMAL_LIMIT),
                ix,
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
    let argv: Vec<String> = env::args().collect();
    let mut args = argv.iter().skip(1);
    let capture = PathBuf::from(args.next().ok_or("missing capture")?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let image = PathBuf::from(args.next().ok_or("missing image")?).canonicalize()?;
    let resource_path = PathBuf::from(args.next().ok_or("missing resource")?).canonicalize()?;
    let controller_path = PathBuf::from(args.next().ok_or("missing controller")?).canonicalize()?;
    let tics: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(100);
    if tics == 0 || tics > simulation::MAX_TICS {
        return Err("tics must be 1..=128 (one CI3 input)".into());
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err("receipt directory is not empty".into());
    }
    fs::create_dir_all(&receipt)?;
    let image_bytes = base::read(&image)?;
    env::set_var("BPF_OUT_DIR", image.parent().unwrap());
    env::set_var("SBF_OUT_DIR", image.parent().unwrap());
    let resource = base::read(&resource_path)?;
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
    // DEMO1 commands from the captured cursor (offset 13 at gametic 0).
    let mut payload = Vec::new();
    for account in &wad {
        let length = base::u32_at(account, 24) as usize;
        payload.extend_from_slice(&account[32..32 + length]);
    }
    let directory = base::u32_at(&payload, 8) as usize;
    let lumps = base::u32_at(&payload, 4) as usize;
    let demo = (0..lumps)
        .map(|i| directory + i * 16)
        .find(|&at| &payload[at + 8..at + 13] == b"DEMO1")
        .ok_or("DEMO1")?;
    let demo_offset = base::u32_at(&payload, demo) as usize;
    let cursor = base::u32_at(&state_capture, 24) as usize;
    let commands = payload[demo_offset + cursor..demo_offset + cursor + tics * 4].to_vec();
    let total = simulation::INPUT_HEADER_BYTES + commands.len();
    let mut input = vec![0u8; total];
    input[..8].copy_from_slice(b"DOOM-CI3");
    input[8..12].copy_from_slice(&1u32.to_le_bytes());
    input[12..16].copy_from_slice(&(simulation::INPUT_HEADER_BYTES as u32).to_le_bytes());
    input[16..20].copy_from_slice(&(total as u32).to_le_bytes());
    input[20..24].copy_from_slice(&base::u32_at(&state_capture, 20).to_le_bytes());
    input[24..28].copy_from_slice(&(tics as u32).to_le_bytes());
    input[28..32].copy_from_slice(&(commands.len() as u32).to_le_bytes());
    input[64..].copy_from_slice(&commands);
    let input_root = hash_parts(&[b"DOOM-SIM/1-INPUT\0", &input[..32], &input[64..]]);
    input[32..64].copy_from_slice(&input_root);

    let authority = solana_keypair::Keypair::new();
    let mut bank = base::program_test(
        &resource,
        &resource,
        &wad,
        &state,
        &state,
        &mirror,
        &controller,
        &controller,
        &controller,
        authority.pubkey(),
        base::NORMAL_LIMIT,
    );
    bank.add_account(INPUT, base::account(input.clone()));
    bank.add_account(META, base::account(vec![0; simulation::META_BYTES]));
    let mut context = bank.start_with_context().await;
    let continuation = solana_keypair::Keypair::new();
    base::create_continuation(&mut context.banks_client, &context.payer, &continuation).await?;
    let mut phase_units = Vec::new();
    for cursor in 0..=7u32 {
        let result = base::execute(
            &mut context.banks_client,
            &context.payer,
            &authority,
            base::NORMAL_LIMIT,
            base::RESOURCE,
            base::STATE,
            base::CONTROLLER,
            continuation.pubkey(),
            continuation_split_sbf::Request {
                operation: if cursor == 0 {
                    continuation_split_sbf::PREPARE
                } else {
                    continuation_split_sbf::STEP
                },
                state_len: state_capture.len(),
                cursor,
                fault: 0,
            },
        )
        .await?;
        if result.result.is_err() {
            return Err(format!("reconstruction cursor {cursor}: {:?}", result.result).into());
        }
        phase_units.push(base::units(&result)?);
    }
    let mut current_state = base::account_data(&context.banks_client, base::STATE).await?;
    let mut current_mirror = base::account_data(&context.banks_client, base::MIRROR).await?;
    let first_tic = base::u32_at(&current_state, 20);
    let init = sim_tx(
        &mut context.banks_client,
        &context.payer,
        &authority,
        simulation::Request {
            operation: simulation::INIT,
            expected_tic: first_tic,
            sequence: 0,
            state_root: state_root(&current_state, &current_mirror),
            input_root,
            input_index: 0,
        },
    )
    .await?;
    if init.result.is_err() {
        return Err(format!("SIM1 INIT {:?}", init.result).into());
    }
    let mut rows = Vec::new();
    let mut sums: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for index in 0..tics as u32 {
        let tick = sim_tx(
            &mut context.banks_client,
            &context.payer,
            &authority,
            simulation::Request {
                operation: simulation::TICK,
                expected_tic: first_tic + index,
                sequence: index as u64,
                state_root: state_root(&current_state, &current_mirror),
                input_root,
                input_index: index,
            },
        )
        .await?;
        let units = base::units(&tick)?;
        if tick.result.is_err() {
            return Err(format!("tick {index}: {:?}", tick.result).into());
        }
        let marks: BTreeMap<u64, u64> = markers(&tick).into_iter().collect();
        let mut row = serde_json::Map::new();
        row.insert("tic".into(), index.into());
        row.insert("total_cu".into(), units.into());
        let first = *marks.get(&800).ok_or("marker 800 missing")?;
        let last = *marks.get(&826).ok_or("marker 826 missing")?;
        let entry = base::NORMAL_LIMIT as u64 - first;
        let exit = units - (base::NORMAL_LIMIT as u64 - last);
        row.insert("entry_decode_account_validation".into(), entry.into());
        sums.entry("entry_decode_account_validation")
            .or_default()
            .push(entry);
        for (from, to, name) in SEGMENTS {
            let a = marks.get(from).ok_or(format!("marker {from} missing"))?;
            let b = marks.get(to).ok_or(format!("marker {to} missing"))?;
            let cost = a - b;
            row.insert((*name).into(), cost.into());
            sums.entry(name).or_default().push(cost);
        }
        row.insert("exit_return".into(), exit.into());
        sums.entry("exit_return").or_default().push(exit);
        row.insert("markers".into(), (marks.len() as u64).into());
        rows.push(serde_json::Value::Object(row));
        current_state = base::account_data(&context.banks_client, base::STATE).await?;
        current_mirror = base::account_data(&context.banks_client, base::MIRROR).await?;
    }
    let mut summary = serde_json::Map::new();
    for (name, values) in &sums {
        let mut sorted = values.clone();
        sorted.sort();
        summary.insert(
            (*name).into(),
            serde_json::json!({
                "min": sorted[0],
                "median": sorted[sorted.len() / 2],
                "max": sorted[sorted.len() - 1],
                "mean": values.iter().sum::<u64>() as f64 / values.len() as f64,
            }),
        );
    }
    let totals: Vec<u64> = rows
        .iter()
        .map(|r| r["total_cu"].as_u64().unwrap())
        .collect();
    fs::write(
        receipt.join("per-tic.json"),
        serde_json::to_vec_pretty(&rows)?,
    )?;
    fs::write(
        receipt.join("summary.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "profile": "DOOM-SIM/1 TICK, diagnostic-profile legacy image (markers add ~200 CU each)",
            "image_sha256": base::sha(&image_bytes),
            "image": image.display().to_string(),
            "command_line": argv,
            "tics": tics,
            "reconstruction_phase_units": phase_units,
            "total_cu": {"min": totals.iter().min(), "max": totals.iter().max(), "mean": totals.iter().sum::<u64>() as f64 / totals.len() as f64},
            "segments": summary,
            "marker_count_per_tic": rows[0]["markers"],
            "driver_sha256": base::sha(include_bytes!("sim1_profile.rs")),
        }))?,
    )?;
    println!(
        "SIM/1 profile written: {} tics, receipt {}",
        tics,
        receipt.display()
    );
    Ok(())
}
