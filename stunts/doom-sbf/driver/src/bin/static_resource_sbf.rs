#![recursion_limit = "256"]

//! Local SBF ProgramTest measurement for pinned-base DOOM-RSR/2 BUILD and RENDER.
//!
//! The RSN/3 dynamic snapshot and native framebuffer are retained outputs from
//! the R3 capture. ProgramTest preallocates the static account as a fixture;
//! this runner measures the BUILD write transaction, then eight zero-copy
//! RENDER transactions against the same captured payload and native oracle.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use doom_sbf::{render_snapshot_v3 as rsn3, resource_sbf, static_resource as rsr};
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

const COMPUTE_LIMIT: u32 = 1_400_000;
const DRIVER_SOURCE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/bin/static_resource_sbf.rs"
));
const RESOURCE: Pubkey = Pubkey::new_from_array([0xd1; 32]);

fn read(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    fs::read(path).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn retain_file(source: &Path, directory: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    fs::copy(source, directory.join(name))
        .map(|_| ())
        .map_err(|error| format!("retain {}: {error}", source.display()).into())
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
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

async fn submit(
    banks: &mut BanksClient,
    payer: &solana_keypair::Keypair,
    instruction: Instruction,
) -> Result<(u64, String), Box<dyn Error>> {
    let blockhash = banks.get_latest_blockhash().await?;
    let transaction = Transaction::new(
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
    let result = banks.process_transaction_with_metadata(transaction).await?;
    let logs = result
        .metadata
        .as_ref()
        .map_or_else(String::new, |metadata| metadata.log_messages.join("\n"));
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

fn build_instruction(program: Pubkey, static_key: Pubkey, cursor: u32) -> Instruction {
    let mut accounts = vec![meta(static_key, true), meta(RESOURCE, false)];
    if cursor < rsr::BUILD_WAD_CHUNKS {
        accounts.push(meta(
            Pubkey::new_from_array([0xe0 + cursor as u8; 32]),
            false,
        ));
    }
    Instruction {
        program_id: program,
        accounts,
        data: rsr::Request::build_chunk(cursor).encode().to_vec(),
    }
}

fn render_instruction(
    program: Pubkey,
    static_key: Pubkey,
    snapshot: Pubkey,
    strip_id: u8,
    request: &[u8],
) -> Instruction {
    let strip = rsn3::strip_address(&program, &snapshot, strip_id).0;
    Instruction {
        program_id: program,
        accounts: vec![
            meta(static_key, true),
            meta(snapshot, false),
            meta(strip, true),
        ],
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

fn check_wad_account(bytes: &[u8], ordinal: usize) -> Result<(), Box<dyn Error>> {
    if bytes.len() != resource_sbf::WAD_ACCOUNT_BYTES
        || &bytes[..8] != b"DOOM-WC1"
        || u32_at(bytes, 8) != 1
        || u32_at(bytes, 12) as usize != resource_sbf::WAD_HEADER_BYTES
        || u32_at(bytes, 16) as usize != resource_sbf::WAD_ACCOUNT_BYTES
        || u32_at(bytes, 20) as usize != ordinal
    {
        return Err(format!("invalid retained WAD account {ordinal}").into());
    }
    let payload = u32_at(bytes, 24) as usize;
    if payload > resource_sbf::WAD_ACCOUNT_BYTES - resource_sbf::WAD_HEADER_BYTES
        || bytes[resource_sbf::WAD_HEADER_BYTES + payload..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(format!("invalid retained WAD account padding {ordinal}").into());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let image = PathBuf::from(
        args.next()
            .ok_or("usage: static_resource_sbf IMAGE SOURCE_MANIFEST R3_RECEIPT_DIR RECEIPT_DIR")?,
    );
    let manifest = PathBuf::from(args.next().ok_or("missing source manifest")?);
    let r3_root = PathBuf::from(args.next().ok_or("missing R3 receipt root")?);
    let receipt = PathBuf::from(args.next().ok_or("missing receipt directory")?);
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    fs::create_dir_all(&receipt)?;
    let image_bytes = read(&image)?;
    let manifest_bytes = read(&manifest)?;
    let manifest_text = std::str::from_utf8(&manifest_bytes)?;
    if !manifest_text.contains("\"sbf\": true")
        || !manifest_text.contains("\"render_snapshot_v3_abi_descriptor\"")
    {
        return Err("SBF manifest does not identify the pinned DOOM-RSN/3 profile".into());
    }
    let image_sha = sha(&image_bytes);
    let manifest_sha = sha(&manifest_bytes);
    let driver_sha = sha(DRIVER_SOURCE);
    let retained = receipt.join("inputs");
    fs::create_dir_all(&retained)?;
    retain_file(&image, &retained, "doom_sbf.so")?;
    retain_file(&manifest, &retained, "doom_ex2_source_manifest.json")?;

    let input = r3_root.join("inputs");
    let resource_image = read(&input.join("resource-image.ri2"))?;
    let native_frame = read(&input.join("native-imported-frame.fb1"))?;
    let snapshot_image = read(&r3_root.join("snapshot-rsn3.bin"))?;
    let baseline_report: Value = serde_json::from_slice(&read(&r3_root.join("report.json"))?)?;
    let run: Value = serde_json::from_slice(&read(&input.join("sim2-run.json"))?)?;
    if resource_image.len() != resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES
        || native_frame.len() != resource_sbf::RI2_FRAME_BYTES
    {
        return Err("retained RI2 image or native framebuffer has the wrong size".into());
    }
    let snapshot_view = rsn3::validate_snapshot(&snapshot_image)?;
    if baseline_report["snapshot_hash"].as_str() != Some(hex(&snapshot_view.snapshot_hash).as_str())
        || baseline_report["native_oracle_frame_sha256"].as_str()
            != Some(sha(&native_frame).as_str())
        || baseline_report["all_eight_strips_match_native"] != true
    {
        return Err(
            "R3 snapshot or parity receipt does not match the retained RENDER input".into(),
        );
    }
    let program = run["program"]
        .as_str()
        .ok_or("sim2 run program")?
        .parse::<Pubkey>()?;
    let context = run["context"]
        .as_str()
        .ok_or("sim2 run context")?
        .parse::<Pubkey>()?;
    let snapshot_key = rsn3::snapshot_address(&program, &context).0;
    let static_key = rsr::static_address(&program).0;

    let mut wad_accounts = Vec::with_capacity(resource_sbf::WAD_ACCOUNT_COUNT);
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let path = input.join(format!("wad-{ordinal:03}.bin"));
        let bytes = read(&path)?;
        check_wad_account(&bytes, ordinal)?;
        retain_file(&path, &retained, &format!("wad-{ordinal:03}.bin"))?;
        wad_accounts.push(bytes);
    }
    for (source, name) in [
        (input.join("resource-image.ri2"), "resource-image.ri2"),
        (
            input.join("native-imported-frame.fb1"),
            "native-imported-frame.fb1",
        ),
        (r3_root.join("snapshot-rsn3.bin"), "snapshot-rsn3.bin"),
        (r3_root.join("report.json"), "baseline-r3-report.json"),
        (
            input.join("wad-account-manifest.json"),
            "wad-account-manifest.json",
        ),
        (input.join("sim2-run.json"), "sim2-run.json"),
    ] {
        retain_file(&source, &retained, name)?;
    }
    let wad_manifest: Value =
        serde_json::from_slice(&read(&input.join("wad-account-manifest.json"))?)?;

    env::set_var("BPF_OUT_DIR", image.parent().ok_or("image has no parent")?);
    env::set_var("SBF_OUT_DIR", image.parent().ok_or("image has no parent")?);
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(u64::from(COMPUTE_LIMIT));
    bank.add_program("doom_sbf", program, None);
    bank.add_account(static_key, account(program, vec![0; rsr::ACCOUNT_BYTES]));
    bank.add_account(RESOURCE, account(program, resource_image.clone()));
    for (ordinal, bytes) in wad_accounts.iter().enumerate() {
        bank.add_account(
            Pubkey::new_from_array([0xe0 + ordinal as u8; 32]),
            account(program, bytes.clone()),
        );
    }
    bank.add_account(snapshot_key, account(program, snapshot_image.clone()));
    for strip_id in 0..8u8 {
        let strip = rsn3::strip_address(&program, &snapshot_key, strip_id).0;
        bank.add_account(strip, account(program, vec![0; rsn3::STRIP_ACCOUNT_BYTES]));
    }

    let mut client = bank.start_with_context().await;
    let payer = client.payer.insecure_clone();
    let mut build_units = Vec::new();
    let mut build_logs = String::new();
    for cursor in 0..=rsr::BUILD_FINAL_CURSOR {
        let (units, logs) = submit(
            &mut client.banks_client,
            &payer,
            build_instruction(program, static_key, cursor),
        )
        .await?;
        if units > u64::from(COMPUTE_LIMIT) {
            return Err(
                format!("BUILD cursor {cursor} used {units} CU, over {COMPUTE_LIMIT}").into(),
            );
        }
        build_logs.push_str(&format!("=== BUILD cursor {cursor} ===\n{logs}\n"));
        build_units.push(units);
    }
    fs::write(receipt.join("build-logs.txt"), build_logs)?;

    let built = client
        .banks_client
        .get_account(static_key)
        .await?
        .ok_or("static resource account missing after BUILD")?;
    if built.owner != program
        || built.data.len() != rsr::ACCOUNT_BYTES
        || &built.data[rsr::HEADER_OFFSET..rsr::HEADER_OFFSET + 8] != rsr::MAGIC
        || u32_at(&built.data, rsr::HEADER_OFFSET + 40) as usize != rsr::ROOT_COUNT
    {
        return Err("BUILD readback did not contain a valid static resource header".into());
    }
    let static_account_sha_after_build = sha(&built.data);
    fs::write(
        receipt.join("static-resource-account-after-build.bin"),
        &built.data,
    )?;
    let build_high_water = u32_at(&built.data, rsr::HEADER_OFFSET + 152);
    let header_roots = &built.data[rsr::HEADER_OFFSET + rsr::ROOTS_OFFSET
        ..rsr::HEADER_OFFSET + rsr::ROOTS_OFFSET + rsr::ROOT_BYTES];
    if header_roots
        .chunks_exact(4)
        .all(|offset| u32::from_le_bytes(offset.try_into().unwrap()) == u32::MAX)
    {
        return Err("BUILD did not retain account-relative pointer roots".into());
    }
    drop(built);

    let mut strip_results = Vec::new();
    let mut reconstructed = vec![0u8; resource_sbf::RI2_FRAME_BYTES];
    let mut render_units_total = 0u64;
    for strip_id in 0..8u8 {
        let request =
            rsr::Request::render(strip_id, snapshot_view.cursor, snapshot_view.snapshot_hash)
                .encode();
        let (units, logs) = submit(
            &mut client.banks_client,
            &payer,
            render_instruction(program, static_key, snapshot_key, strip_id, &request),
        )
        .await?;
        fs::write(
            receipt.join(format!("render-strip-{strip_id}-logs.txt")),
            logs,
        )?;
        let strip_key = rsn3::strip_address(&program, &snapshot_key, strip_id).0;
        let strip_account = client
            .banks_client
            .get_account(strip_key)
            .await?
            .ok_or("strip account missing")?;
        let view = rsn3::validate_strip(&strip_account.data)?;
        let expected = slice_frame(&native_frame, usize::from(strip_id));
        let equal = view.strip_id == u32::from(strip_id)
            && view.cursor == snapshot_view.cursor
            && view.gametic == snapshot_view.gametic
            && view.snapshot_hash == snapshot_view.snapshot_hash
            && view.state_root == snapshot_view.state_root
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
            "baseline_r3_compute_units": baseline_report["strips"][usize::from(strip_id)]["compute_units"],
            "cu_drop": baseline_report["strips"][usize::from(strip_id)]["compute_units"].as_u64().unwrap_or(0).saturating_sub(units),
            "pixel_parity_native": equal,
            "strip_hash": hex(&view.strip_hash),
            "transaction_ok": true
        }));
        if !equal {
            return Err(format!("RSR/2 native pixel mismatch in strip {strip_id}").into());
        }
    }
    let full_parity = reconstructed == native_frame;
    if !full_parity {
        return Err("reconstructed RSR/2 frame differs from native oracle".into());
    }
    fs::write(receipt.join("reconstructed-frame.fb1"), &reconstructed)?;
    let static_account = client
        .banks_client
        .get_account(static_key)
        .await?
        .ok_or("static resource account missing after RENDER")?;
    let static_account_sha_after_render = sha(&static_account.data);
    fs::write(
        receipt.join("static-resource-account.bin"),
        &static_account.data,
    )?;
    drop(static_account);

    let before: Vec<u64> = baseline_report["strips"]
        .as_array()
        .ok_or("R3 baseline strip table")?
        .iter()
        .map(|strip| strip["compute_units"].as_u64().ok_or("R3 strip CU"))
        .collect::<Result<_, _>>()?;
    let before_total: u64 = before.iter().sum();
    let mean_before = before_total / before.len() as u64;
    let mean_after = render_units_total / strip_results.len() as u64;
    let report = json!({
        "status": "measured-equal",
        "profile": "DOOM-RSR/2-pinned-base-SBF-PROGRAMTEST",
        "local_programtest_only": true,
        "network_calls": 0,
        "baseline_profile": baseline_report["profile"],
        "baseline_image_sha256": baseline_report["image_sha256"],
        "image_sha256": image_sha,
        "source_manifest_sha256": manifest_sha,
        "driver_source_sha256": driver_sha,
        "resource_image_sha256": sha(&resource_image),
        "snapshot_hash": hex(&snapshot_view.snapshot_hash),
        "snapshot_payload_bytes": snapshot_view.payload.len(),
        "native_oracle_frame_sha256": sha(&native_frame),
        "wad_hash": hex(&snapshot_view.wad_hash),
        "static_resource_pda": static_key.to_string(),
        "static_resource_account_bytes": rsr::ACCOUNT_BYTES,
        "static_resource_header_bytes": rsr::HEADER_BYTES,
        "static_resource_layout_roots": rsr::ROOT_COUNT,
        "static_account_sha256_after_build": static_account_sha_after_build,
        "static_account_sha256_after_render": static_account_sha_after_render,
        "build_transaction_count": build_units.len(),
        "build_compute_units": build_units,
        "build_compute_units_max": build_units.iter().max(),
        "build_all_transactions_under_target": build_units.iter().all(|units| *units <= u64::from(COMPUTE_LIMIT)),
        "build_arena_high_water_bytes": build_high_water,
        "build_account_preallocated_by_programtest": true,
        "build_account_allocation_and_rent_measured": false,
        "build_resource_hash_pinned": true,
        "render_decodes_st1": false,
        "render_rebuilds_wad_graph_or_tables": false,
        "render_uses_zero_copy_static_account": true,
        "static_account_relocatable": false,
        "render_transaction_count": 8,
        "render_cu_before_total": before_total,
        "render_cu_after_total": render_units_total,
        "render_cu_drop_total": before_total.saturating_sub(render_units_total),
        "render_mean_cu_before": mean_before,
        "render_mean_cu_after": mean_after,
        "render_mean_cu_drop": mean_before.saturating_sub(mean_after),
        "render_target_cu": 1_400_000,
        "all_eight_strips_match_native": true,
        "reconstructed_frame_matches_native": full_parity,
        "pixel_parity": "exact byte equality",
        "snapshot_transaction_included": false,
        "snapshot_profile_boundary": "captured and hash-validated DOOM-RSN/3 account from the R3 ProgramTest receipt",
        "static_account_address_boundary": "the account is ordinal zero and the existing DOOM_CTX guard pins the data base; embedded C pointers require that base; the header stores 32 map/resource/cache root offsets; relocation is not supported",
        "account_provisioning_boundary": "ProgramTest genesis preallocated the program-owned static resource account; allocation or rent behavior is unmeasured",
        "wad_manifest": wad_manifest,
        "strips": strip_results
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
            "snapshot_hash": hex(&snapshot_view.snapshot_hash),
            "static_resource_account_sha256_after_build": static_account_sha_after_build,
            "static_resource_account_sha256_after_render": static_account_sha_after_render
        }))?,
    )?;
    println!(
        "RSR/2 ProgramTest measured: BUILD={} transactions, max={} CU; RENDER mean={mean_before} -> {mean_after} CU; 8/8 strips exact; receipts={}",
        build_units.len(),
        build_units.iter().max().copied().unwrap_or(0),
        receipt.display()
    );
    Ok(())
}
