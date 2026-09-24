//! Measure DOOM-RSN/2 SNAPSHOT from retained SIM2 account readbacks in SBF ProgramTest.

use std::{env, error::Error, fs, path::PathBuf, str::FromStr};

use doom_sbf::{render_snapshot, resource_sbf, sim2};
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_banks_client::BanksClient;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{account_meta::AccountMeta, Instruction};
use solana_message::Message;
use solana_program_test::ProgramTest;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const COMPUTE_LIMIT: u32 = 30_000_000;

fn read(path: &PathBuf) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?)
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn account(program: Pubkey, data: Vec<u8>) -> Account {
    Account {
        lamports: 100_000_000_000,
        data,
        owner: program,
        executable: false,
        rent_epoch: 0,
    }
}

fn require_hash(label: &str, actual: &str, expected: &str) -> Result<(), Box<dyn Error>> {
    if actual != expected {
        return Err(format!("{label} SHA-256 mismatch: {actual} != {expected}").into());
    }
    Ok(())
}

async fn submit(
    client: &mut BanksClient,
    payer: &solana_keypair::Keypair,
    instructions: &[Instruction],
) -> Result<(u64, String), Box<dyn Error>> {
    let blockhash = client.get_latest_blockhash().await?;
    let transaction = Transaction::new(
        &[payer],
        Message::new_with_blockhash(instructions, Some(&payer.pubkey()), &blockhash),
        blockhash,
    );
    let result = client
        .process_transaction_with_metadata(transaction)
        .await?;
    if let Err(error) = &result.result {
        return Err(format!("ProgramTest transaction failed: {error:?}; {result:?}").into());
    }
    let metadata = result
        .metadata
        .ok_or("ProgramTest returned no transaction metadata")?;
    Ok((
        u64::from(metadata.compute_units_consumed),
        metadata.log_messages.join("\n"),
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let image = PathBuf::from(args.next().ok_or(
        "usage: render_snapshot_sbf IMAGE RECEIPT SIM2_READBACK_DIR EXPECTED_IMAGE_SHA EXPECTED_MANIFEST_SHA",
    )?)
    .canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt directory")?);
    let captures =
        PathBuf::from(args.next().ok_or("missing SIM2 readback directory")?).canonicalize()?;
    let expected_image_sha = args
        .next()
        .ok_or("missing image SHA-256")?
        .into_string()
        .map_err(|_| "image SHA-256 is not UTF-8")?;
    let expected_manifest_sha = args
        .next()
        .ok_or("missing source manifest SHA-256")?
        .into_string()
        .map_err(|_| "manifest SHA-256 is not UTF-8")?;
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err(format!("refusing non-empty receipt {}", receipt.display()).into());
    }
    fs::create_dir_all(&receipt)?;

    let image_bytes = read(&image)?;
    let image_sha = sha(&image_bytes);
    require_hash("SBF image", &image_sha, &expected_image_sha)?;
    let manifest = image
        .parent()
        .ok_or("SBF image has no parent")?
        .join("doom_ex2_source_manifest.json");
    let manifest_bytes = read(&manifest)?;
    let manifest_sha = sha(&manifest_bytes);
    require_hash("SBF source manifest", &manifest_sha, &expected_manifest_sha)?;

    let run: serde_json::Value = serde_json::from_slice(&read(&captures.join("run.json"))?)?;
    let provenance: serde_json::Value =
        serde_json::from_slice(&read(&captures.join("end-provenance.json"))?)?;
    let program = Pubkey::from_str(run["program"].as_str().ok_or("run.json program")?)?;
    let context = Pubkey::from_str(run["context"].as_str().ok_or("run.json context")?)?;
    let meta_key = Pubkey::from_str(run["meta"].as_str().ok_or("run.json meta")?)?;
    let meta = read(&captures.join("end-meta.bin"))?;
    let state = read(&captures.join("end-state.st1"))?;
    let mirror = read(&captures.join("end-mirror.mp1"))?;
    let state_key = Pubkey::new_from_array(meta[sim2::M_STATE..sim2::M_STATE + 32].try_into()?);
    let mirror_key = Pubkey::new_from_array(meta[sim2::M_MIRROR..sim2::M_MIRROR + 32].try_into()?);
    require_hash(
        "captured SIM2 meta",
        &sha(&meta),
        provenance["meta_sha256"]
            .as_str()
            .ok_or("provenance meta hash")?,
    )?;
    require_hash(
        "captured ST1",
        &sha(&state),
        provenance["state_sha256"]
            .as_str()
            .ok_or("provenance state hash")?,
    )?;
    require_hash(
        "captured MP1",
        &sha(&mirror),
        provenance["mirror_sha256"]
            .as_str()
            .ok_or("provenance mirror hash")?,
    )?;
    if meta.len() != sim2::META_BYTES
        || state.len() != resource_sbf::STATE_MAX_BYTES
        || mirror.len() != resource_sbf::MIRROR_BYTES
    {
        return Err(format!(
            "captured account lengths differ from SIM2: meta={} ST1={} MP1={}",
            meta.len(),
            state.len(),
            mirror.len()
        )
        .into());
    }
    if meta[sim2::M_CONTEXT..sim2::M_CONTEXT + 32] != context.as_ref()[..] {
        return Err("run.json context does not match captured SIM2 meta".into());
    }
    let (expected_meta, _) = sim2::meta_address(&program, &context);
    if expected_meta != meta_key {
        return Err("captured SIM2 meta key is not the canonical program PDA".into());
    }
    let meta_view = sim2::MetaView::parse(&meta).ok_or("captured SIM2 meta is invalid")?;
    if meta_view.cursor != provenance["cursor"].as_u64().ok_or("provenance cursor")? as u32 {
        return Err("captured SIM2 cursor differs from provenance".into());
    }

    env::set_var("BPF_OUT_DIR", image.parent().unwrap());
    env::set_var("SBF_OUT_DIR", image.parent().unwrap());
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(u64::from(COMPUTE_LIMIT));
    bank.add_program("doom_sbf", program, None);
    bank.add_account(meta_key, account(program, meta.clone()));
    bank.add_account(state_key, account(program, state.clone()));
    bank.add_account(mirror_key, account(program, mirror.clone()));
    let snapshot = render_snapshot::snapshot_address(&program, &context).0;
    // ProgramTest preallocates this test fixture. OP_INIT's 192,704-byte CPI
    // allocation exceeds the runtime's 10,240-byte inner-instruction limit.
    bank.add_account(
        snapshot,
        account(program, vec![0; render_snapshot::SNAPSHOT_BYTES]),
    );
    let mut client = bank.start_with_context().await;
    let payer = client.payer.insecure_clone();

    let request = render_snapshot::SnapshotRequest {
        cursor: meta_view.cursor,
        state_root: meta_view.state_root,
    };
    let snapshot_ix = Instruction {
        program_id: program,
        accounts: vec![
            AccountMeta::new_readonly(meta_key, false),
            AccountMeta::new_readonly(state_key, false),
            AccountMeta::new_readonly(mirror_key, false),
            AccountMeta::new(snapshot, false),
        ],
        data: request.encode().to_vec(),
    };
    let (snapshot_cu, snapshot_logs) = submit(
        &mut client.banks_client,
        &payer,
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_LIMIT),
            snapshot_ix,
        ],
    )
    .await?;
    fs::write(receipt.join("snapshot-logs.txt"), &snapshot_logs)?;

    let snapshot_account = client
        .banks_client
        .get_account(snapshot)
        .await?
        .ok_or("SNAPSHOT account missing after transaction")?;
    if snapshot_account.owner != program {
        return Err("SNAPSHOT account owner mismatch".into());
    }
    let snapshot_view = render_snapshot::validate_snapshot(&snapshot_account.data)?;
    if snapshot_view.cursor != meta_view.cursor
        || snapshot_view.state_root != meta_view.state_root
        || snapshot_view.state != &state[..sim2::state_len_checked(&state)?]
        || snapshot_view.mirror != mirror
    {
        return Err("SNAPSHOT output differs from the captured SIM2 state".into());
    }
    fs::write(receipt.join("snapshot-rsn2.bin"), &snapshot_account.data)?;
    fs::write(
        receipt.join("snapshot-report.json"),
        format!(
            "{{\n  \"status\": \"measured\",\n  \"program_id\": {},\n  \"cursor\": {},\n  \"snapshot_compute_units\": {},\n  \"snapshot_account_bytes\": {},\n  \"snapshot_account_preallocated_by_programtest\": true,\n  \"snapshot_init_status\": \"not measured; ProgramTest rejects the 192704-byte CPI allocation\",\n  \"snapshot_matches_captured_state\": true,\n  \"image_sha256\": {},\n  \"manifest_sha256\": {},\n  \"meta_sha256\": {},\n  \"state_sha256\": {},\n  \"mirror_sha256\": {}\n}}\n",
            serde_json::to_string(&program.to_string())?,
            meta_view.cursor,
            snapshot_cu,
            snapshot_account.data.len(),
            serde_json::to_string(&image_sha)?,
            serde_json::to_string(&manifest_sha)?,
            serde_json::to_string(&sha(&meta))?,
            serde_json::to_string(&sha(&state))?,
            serde_json::to_string(&sha(&mirror))?,
        ),
    )?;
    Ok(())
}
