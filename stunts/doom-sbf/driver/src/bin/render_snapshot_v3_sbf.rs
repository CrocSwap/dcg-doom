//! Local SBF ProgramTest gate for DOOM-RSN/3.
//!
//! The renderer input and pixel oracle are retained native RI2 outputs from
//! the captured DEMO1 tic.  ProgramTest uses a local SIM2 meta fixture whose
//! root is recomputed over those exact ST1/MP1 bytes; this does not claim that
//! the fixture was a chain account readback.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use doom_sbf::{render_snapshot_v3 as rsn3, resource_sbf, sim2};
use serde_json::{json, Value};
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
const DRIVER_SOURCE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/bin/render_snapshot_v3_sbf.rs"
));
const RESOURCE: Pubkey = Pubkey::new_from_array([0xd1; 32]);

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn retain_file(source: &Path, directory: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    fs::copy(source, directory.join(name))
        .map(|_| ())
        .map_err(|e| format!("retain {}: {e}", source.display()).into())
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn account(owner: Pubkey, data: Vec<u8>) -> Account {
    Account {
        lamports: 100_000_000_000,
        data,
        owner,
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

fn wad_account(path: &Path, ordinal: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let bytes = read(path)?;
    if bytes.len() != resource_sbf::WAD_ACCOUNT_BYTES
        || &bytes[..8] != b"DOOM-WC1"
        || u32_at(&bytes, 8) != 1
        || u32_at(&bytes, 12) as usize != resource_sbf::WAD_HEADER_BYTES
        || u32_at(&bytes, 16) as usize != resource_sbf::WAD_ACCOUNT_BYTES
        || u32_at(&bytes, 20) as usize != ordinal
    {
        return Err(format!("invalid retained WAD account {ordinal}: {}", path.display()).into());
    }
    let payload_len = u32_at(&bytes, 24) as usize;
    if payload_len > resource_sbf::WAD_ACCOUNT_BYTES - resource_sbf::WAD_HEADER_BYTES
        || bytes[resource_sbf::WAD_HEADER_BYTES + payload_len..]
            .iter()
            .any(|b| *b != 0)
    {
        return Err(format!("invalid retained WAD account padding {ordinal}").into());
    }
    Ok(bytes)
}

async fn submit(
    banks: &mut BanksClient,
    payer: &solana_keypair::Keypair,
    instruction: Instruction,
) -> Result<(u64, String), Box<dyn Error>> {
    let blockhash = banks.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[payer],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_LIMIT),
                instruction,
            ],
            Some(&payer.pubkey()),
            &blockhash,
        ),
        blockhash,
    );
    let result = banks.process_transaction_with_metadata(tx).await?;
    let logs = result
        .metadata
        .as_ref()
        .map_or_else(String::new, |m| m.log_messages.join("\n"));
    if let Err(error) = result.result {
        return Err(format!("SBF transaction failed: {error:?}\n{logs}").into());
    }
    let units = result
        .metadata
        .as_ref()
        .ok_or("ProgramTest omitted transaction metadata")?
        .compute_units_consumed;
    Ok((units, logs))
}

fn snapshot_instruction(
    program: Pubkey,
    meta_key: Pubkey,
    state_key: Pubkey,
    mirror_key: Pubkey,
    scratch: Pubkey,
    snapshot: Pubkey,
    request: &[u8],
) -> Instruction {
    let mut accounts = vec![
        meta(meta_key, false),
        meta(state_key, false),
        meta(mirror_key, false),
        meta(RESOURCE, false),
    ];
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        accounts.push(meta(
            Pubkey::new_from_array([0xe0 + ordinal as u8; 32]),
            false,
        ));
    }
    accounts.extend([meta(scratch, true), meta(snapshot, true)]);
    Instruction {
        program_id: program,
        accounts,
        data: request.to_vec(),
    }
}

fn render_instruction(
    program: Pubkey,
    snapshot: Pubkey,
    strip_id: u8,
    request: &[u8],
) -> Instruction {
    let mut accounts = vec![
        meta(rsn3::scratch_address(&program, &snapshot, strip_id).0, true),
        meta(RESOURCE, false),
    ];
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        accounts.push(meta(
            Pubkey::new_from_array([0xe0 + ordinal as u8; 32]),
            false,
        ));
    }
    accounts.extend([
        meta(snapshot, false),
        meta(rsn3::strip_address(&program, &snapshot, strip_id).0, true),
    ]);
    Instruction {
        program_id: program,
        accounts,
        data: request.to_vec(),
    }
}

fn slice_frame(frame: &[u8], strip: usize) -> Vec<u8> {
    (0..200)
        .flat_map(|row| {
            let start = row * 320 + strip * 40;
            frame[start..start + 40].iter().copied()
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let image = PathBuf::from(
        args.next()
            .ok_or("usage: render_snapshot_v3_sbf IMAGE SOURCE_MANIFEST R2_RUN_ROOT RECEIPT_DIR")?,
    );
    let manifest = PathBuf::from(args.next().ok_or("missing source manifest")?);
    let run_root = PathBuf::from(args.next().ok_or("missing R2 run root")?);
    let receipt = PathBuf::from(args.next().ok_or("missing receipt directory")?);
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    fs::create_dir_all(&receipt)?;
    let source_bytes = fs::read(&image)?;
    let manifest_bytes = fs::read(&manifest)?;
    let manifest_text = std::str::from_utf8(&manifest_bytes)?;
    if !manifest_text.contains("\"sbf\": true")
        || !manifest_text.contains("\"render_snapshot_v3_abi_descriptor\"")
    {
        return Err("SBF manifest does not identify DOOM-RSN/3".into());
    }
    let image_sha = sha(&source_bytes);
    let manifest_sha = sha(&manifest_bytes);
    let driver_sha = sha(DRIVER_SOURCE);
    let retained = receipt.join("inputs");
    fs::create_dir_all(&retained)?;
    retain_file(&image, &retained, "doom_sbf.so")?;
    retain_file(&manifest, &retained, "doom_ex2_source_manifest.json")?;

    let profile = run_root.join("profile-inputs");
    let capture = profile.join("capture");
    let native = profile.join("native");
    let live = run_root.join("snapshot-inputs/live-2000");
    let run: Value = serde_json::from_slice(&read(&live.join("run.json"))?)?;
    let program = run["program"]
        .as_str()
        .ok_or("run.json program")?
        .parse::<Pubkey>()?;
    let context = run["context"]
        .as_str()
        .ok_or("run.json context")?
        .parse::<Pubkey>()?;
    let meta_key = run["meta"]
        .as_str()
        .ok_or("run.json meta")?
        .parse::<Pubkey>()?;
    let captured_meta = read(&live.join("end-meta.bin"))?;
    let captured_state = read(&live.join("end-state.st1"))?;
    let captured_mirror = read(&live.join("end-mirror.mp1"))?;
    let captured_provenance: Value =
        serde_json::from_slice(&read(&live.join("end-provenance.json"))?)?;
    let captured_meta_sha = sha(&captured_meta);
    let captured_state_sha = sha(&captured_state);
    let captured_mirror_sha = sha(&captured_mirror);
    if captured_provenance["meta_sha256"].as_str() != Some(captured_meta_sha.as_str())
        || captured_provenance["state_sha256"].as_str() != Some(captured_state_sha.as_str())
        || captured_provenance["mirror_sha256"].as_str() != Some(captured_mirror_sha.as_str())
    {
        return Err("captured SIM2 account provenance hash mismatch".into());
    }
    if captured_meta.len() != sim2::META_BYTES {
        return Err("captured SIM2 meta has wrong size".into());
    }
    let state_key =
        Pubkey::new_from_array(captured_meta[sim2::M_STATE..sim2::M_STATE + 32].try_into()?);
    let mirror_key =
        Pubkey::new_from_array(captured_meta[sim2::M_MIRROR..sim2::M_MIRROR + 32].try_into()?);
    if sim2::meta_address(&program, &context).0 != meta_key
        || captured_meta[sim2::M_CONTEXT..sim2::M_CONTEXT + 32] != context.as_ref()[..]
    {
        return Err("captured SIM2 meta PDA/context mismatch".into());
    }

    let native_state = read(&native.join("imported-state.st1"))?;
    let native_mirror = read(&native.join("imported-mirror.mp1"))?;
    let native_frame = read(&native.join("imported-frame.fb1"))?;
    if native_frame.len() != resource_sbf::RI2_FRAME_BYTES
        || native_state.len() < 64
        || &native_state[..8] != b"DOOM-ST1"
        || native_mirror.len() != resource_sbf::MIRROR_BYTES
        || &native_mirror[..8] != b"DOOM-MP1"
    {
        return Err("retained native DEMO1 oracle shape mismatch".into());
    }
    let mut state = vec![0u8; resource_sbf::STATE_MAX_BYTES];
    state[..native_state.len()].copy_from_slice(&native_state);
    let (state_len, state_root) = sim2::verified_state_root(&state, &native_mirror)?;
    let cursor = u32_at(&native_state, 20);
    let mut meta_image = captured_meta;
    put_u32(&mut meta_image, sim2::M_CURSOR, cursor);
    put_u32(&mut meta_image, sim2::M_STATE_LEN, state_len as u32);
    meta_image[sim2::M_STATE_ROOT..sim2::M_STATE_ROOT + 32].copy_from_slice(&state_root);

    let resource_image = read(&profile.join("resource-image.ri2"))?;
    if resource_image.len() != resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES {
        return Err("RI2 image size mismatch".into());
    }
    let mut wads = Vec::with_capacity(resource_sbf::WAD_ACCOUNT_COUNT);
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let wad_path = capture.join(format!("account/wad-{ordinal:03}.bin"));
        retain_file(&wad_path, &retained, &format!("wad-{ordinal:03}.bin"))?;
        wads.push(wad_account(&wad_path, ordinal)?);
    }
    retain_file(
        &profile.join("resource-image.ri2"),
        &retained,
        "resource-image.ri2",
    )?;
    retain_file(
        &capture.join("account/manifest.json"),
        &retained,
        "wad-account-manifest.json",
    )?;
    retain_file(
        &native.join("imported-state.st1"),
        &retained,
        "native-imported-state.st1",
    )?;
    retain_file(
        &native.join("imported-mirror.mp1"),
        &retained,
        "native-imported-mirror.mp1",
    )?;
    retain_file(
        &native.join("imported-frame.fb1"),
        &retained,
        "native-imported-frame.fb1",
    )?;
    retain_file(&native.join("report.json"), &retained, "native-report.json")?;
    retain_file(&live.join("run.json"), &retained, "sim2-run.json")?;
    retain_file(
        &live.join("end-meta.bin"),
        &retained,
        "sim2-captured-meta.bin",
    )?;
    retain_file(
        &live.join("end-state.st1"),
        &retained,
        "sim2-captured-state.st1",
    )?;
    retain_file(
        &live.join("end-mirror.mp1"),
        &retained,
        "sim2-captured-mirror.mp1",
    )?;
    retain_file(
        &live.join("end-provenance.json"),
        &retained,
        "sim2-captured-provenance.json",
    )?;
    let wad_manifest: Value =
        serde_json::from_slice(&read(&capture.join("account/manifest.json"))?)?;
    let native_report: Value = serde_json::from_slice(&read(&native.join("report.json"))?)?;
    if native_report["status"] != "measured" || native_report["source_matched_native_tic"] != true {
        return Err("native pixel oracle is not a measured source-matched output".into());
    }

    env::set_var("BPF_OUT_DIR", image.parent().ok_or("image has no parent")?);
    env::set_var("SBF_OUT_DIR", image.parent().ok_or("image has no parent")?);
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(u64::from(COMPUTE_LIMIT));
    bank.add_program("doom_sbf", program, None);
    bank.add_account(meta_key, account(program, meta_image.clone()));
    bank.add_account(state_key, account(program, state));
    bank.add_account(mirror_key, account(program, native_mirror.clone()));
    bank.add_account(RESOURCE, account(program, resource_image.clone()));
    for (ordinal, data) in wads.iter().enumerate() {
        bank.add_account(
            Pubkey::new_from_array([0xe0 + ordinal as u8; 32]),
            account(program, data.clone()),
        );
    }
    let snapshot = rsn3::snapshot_address(&program, &context).0;
    bank.add_account(snapshot, account(program, vec![0; rsn3::SNAPSHOT_BYTES]));
    let snapshot_scratch = rsn3::scratch_address(&program, &snapshot, 0).0;
    bank.add_account(
        snapshot_scratch,
        account(program, vec![0; resource_sbf::CONTEXT_BYTES]),
    );
    for strip_id in 0..8u8 {
        let scratch = rsn3::scratch_address(&program, &snapshot, strip_id).0;
        if strip_id != 0 {
            bank.add_account(
                scratch,
                account(program, vec![0; resource_sbf::CONTEXT_BYTES]),
            );
        }
        let strip = rsn3::strip_address(&program, &snapshot, strip_id).0;
        bank.add_account(strip, account(program, vec![0; rsn3::STRIP_ACCOUNT_BYTES]));
    }
    let mut client = bank.start_with_context().await;
    let payer = client.payer.insecure_clone();

    let snapshot_request = rsn3::SnapshotRequest { cursor, state_root };
    let (snapshot_units, snapshot_logs) = submit(
        &mut client.banks_client,
        &payer,
        snapshot_instruction(
            program,
            meta_key,
            state_key,
            mirror_key,
            snapshot_scratch,
            snapshot,
            &snapshot_request.encode(),
        ),
    )
    .await?;
    fs::write(receipt.join("snapshot-logs.txt"), snapshot_logs)?;
    let snapshot_account = client
        .banks_client
        .get_account(snapshot)
        .await?
        .ok_or("snapshot account missing")?;
    if snapshot_account.owner != program {
        return Err("snapshot owner mismatch".into());
    }
    let snapshot_view = rsn3::validate_snapshot(&snapshot_account.data)?;
    if snapshot_view.cursor != cursor
        || snapshot_view.gametic != cursor
        || snapshot_view.state_root != state_root
    {
        return Err("RSN/3 header does not bind the captured native tic".into());
    }
    fs::write(receipt.join("snapshot-rsn3.bin"), &snapshot_account.data)?;
    let snapshot_hash = snapshot_view.snapshot_hash;
    let resource_hash = snapshot_view.resource_hash;
    let wad_hash = snapshot_view.wad_hash;
    let snapshot_payload_bytes = snapshot_view.payload.len();
    drop(snapshot_account);

    let mut strip_results = Vec::new();
    let mut reconstructed = vec![0u8; resource_sbf::RI2_FRAME_BYTES];
    let mut render_units_total = 0u64;
    for strip_id in 0..8u8 {
        let request = rsn3::RenderRequest {
            strip_id,
            cursor,
            snapshot_hash,
        }
        .encode();
        let (units, logs) = submit(
            &mut client.banks_client,
            &payer,
            render_instruction(program, snapshot, strip_id, &request),
        )
        .await?;
        fs::write(
            receipt.join(format!("render-strip-{strip_id}-logs.txt")),
            logs,
        )?;
        let strip_key = rsn3::strip_address(&program, &snapshot, strip_id).0;
        let strip_account = client
            .banks_client
            .get_account(strip_key)
            .await?
            .ok_or("strip account missing")?;
        let view = rsn3::validate_strip(&strip_account.data)?;
        let expected = slice_frame(&native_frame, usize::from(strip_id));
        let equal = view.strip_id == u32::from(strip_id)
            && view.cursor == cursor
            && view.gametic == cursor
            && view.snapshot_hash == snapshot_hash
            && view.state_root == state_root
            && view.pixels == expected;
        for row in 0..200 {
            let source = row * 40;
            let target = row * 320 + usize::from(strip_id) * 40;
            reconstructed[target..target + 40].copy_from_slice(&view.pixels[source..source + 40]);
        }
        fs::write(
            receipt.join(format!("render-strip-{strip_id}.px8")),
            view.pixels,
        )?;
        render_units_total += units;
        strip_results.push(json!({
            "strip_id": strip_id,
            "compute_units": units,
            "pixel_parity_native": equal,
            "strip_hash": hex(&view.strip_hash),
            "transaction_ok": true
        }));
        if !equal {
            return Err(format!("RSN/3 native pixel mismatch in strip {strip_id}").into());
        }
    }
    let full_parity = reconstructed == native_frame;
    if !full_parity {
        return Err("reconstructed RSN/3 framebuffer differs from native oracle".into());
    }
    fs::write(receipt.join("reconstructed-frame.fb1"), &reconstructed)?;

    let mut wrong_snapshot_hash = snapshot_hash;
    wrong_snapshot_hash[0] ^= 1;
    let wrong_request = rsn3::RenderRequest {
        strip_id: 0,
        cursor,
        snapshot_hash: wrong_snapshot_hash,
    }
    .encode();
    let blockhash = client.banks_client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[&payer],
        Message::new_with_blockhash(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_LIMIT),
                render_instruction(program, snapshot, 0, &wrong_request),
            ],
            Some(&payer.pubkey()),
            &blockhash,
        ),
        blockhash,
    );
    let bad_hash = client
        .banks_client
        .process_transaction_with_metadata(tx)
        .await?;
    let bad_hash_logs = bad_hash
        .metadata
        .as_ref()
        .map_or_else(String::new, |m| m.log_messages.join("\n"));
    let refusal = bad_hash
        .result
        .err()
        .ok_or("wrong snapshot hash was accepted")?;
    if !format!("{refusal:?}").contains("Custom(299)") {
        return Err(
            format!("wrong snapshot hash refused with unexpected error: {refusal:?}").into(),
        );
    }
    fs::write(
        receipt.join("malformed-snapshot-hash-logs.txt"),
        bad_hash_logs,
    )?;
    let total_frame_units = snapshot_units + render_units_total;
    let report = json!({
        "status": "measured-equal",
        "profile": "DOOM-RSN/3-SBF-PROGRAMTEST",
        "local_programtest_only": true,
        "network_calls": 0,
        "captured_demo_tic": cursor,
        "snapshot_account_preallocated_by_programtest": true,
        "snapshot_init_measured": false,
        "sim2_meta_fixture": "captured meta identity with cursor, state length, and root recomputed over retained native ST1/MP1",
        "sim2_meta_fixture_is_chain_readback": false,
        "sim2_original_meta_sha256": captured_meta_sha,
        "sim2_original_state_sha256": captured_state_sha,
        "sim2_original_mirror_sha256": captured_mirror_sha,
        "native_oracle_profile": native_report["profile"],
        "native_oracle_frame_sha256": sha(&native_frame),
        "native_state_sha256": sha(&native_state),
        "native_mirror_sha256": sha(&native_mirror),
        "resource_image_sha256": sha(&resource_image),
        "account_manifest_sha256": sha(&read(&capture.join("account/manifest.json"))?),
        "wad_accounts": resource_sbf::WAD_ACCOUNT_COUNT,
        "snapshot_compute_units": snapshot_units,
        "render_compute_units_total": render_units_total,
        "frame_compute_units_total": total_frame_units,
        "render_transactions": 8,
        "designed_transactions_per_frame": 9,
        "render_target_cu": 1_400_000,
        "strip_render_units_at_or_below_target": strip_results.iter().all(|r| r["compute_units"].as_u64().unwrap_or(u64::MAX) <= 1_400_000),
        "all_eight_strips_match_native": true,
        "reconstructed_frame_matches_native": full_parity,
        "malformed_snapshot_hash_refused_custom_299": true,
        "static_resource_account_step": "not implemented in this result; RENDER still imports RI2/WAD graph and renderer resources",
        "render_validates_resource_image_hash_each_call": true,
        "render_decodes_st1": false,
        "snapshot_hash": hex(&snapshot_hash),
        "snapshot_resource_hash": hex(&resource_hash),
        "snapshot_wad_hash": hex(&wad_hash),
        "snapshot_payload_bytes": snapshot_payload_bytes,
        "snapshot_account_bytes": rsn3::SNAPSHOT_BYTES,
        "pixel_parity": "exact byte equality",
        "strips": strip_results,
        "image_sha256": image_sha,
        "source_manifest_sha256": manifest_sha,
        "driver_source_sha256": driver_sha,
        "wad_manifest": wad_manifest
    });
    fs::write(
        receipt.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::write(
        receipt.join("identity.json"),
        serde_json::to_vec_pretty(&json!({
            "sbf_image_path": image.display().to_string(),
            "sbf_image_sha256": image_sha,
            "source_manifest_path": manifest.display().to_string(),
            "source_manifest_sha256": manifest_sha,
            "driver_source_sha256": driver_sha,
            "resource_image_sha256": sha(&resource_image),
            "snapshot_hash": hex(&snapshot_hash)
        }))?,
    )?;
    println!("RSN/3 ProgramTest measured: snapshot={snapshot_units} CU; renders={render_units_total} CU; 8/8 strips exact; receipt={}", receipt.display());
    Ok(())
}
