//! Load the exact finalized R15 account bytes into ProgramTest and exercise
//! the existing SIM/2 and RSR/2 paths under the candidate deployable image.
//!
//! The local clone substitutes only the authority public key in META and the
//! input-buffer header with ProgramTest's local payer. It never reads or needs
//! the R15 authority secret. SNAPSHOT-LIVE-BUFFERED and RENDER-BUFFERED are
//! read-only with respect to the live CONTEXT account; ADVANCE is then run on
//! the in-memory clone to prove the existing state can continue.

#![recursion_limit = "256"]

use std::{env, error::Error, fs, path::PathBuf, str::FromStr};

use doom_sbf::{render_snapshot_v3 as rsn3, sim2, static_resource as rsr};
use serde_json::Value;
use solana_account::Account;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_program::hash::hashv;
use solana_program_test::ProgramTest;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use sha2::{Digest, Sha256};

const COMPUTE_LIMIT: u32 = 1_400_000;
const RSR_ABI_DESCRIPTOR: &str = "DOOM-RSR/2|request=52|build=5-WAD-chunks+6-resumable-C-phases|operations=BUILD/0+RENDER/1+INIT/2+GROW/3+REBUILD/4+RENDER-BUFFERED/5+CLEAR/6|rebuild=4:accounts=program+static+programdata+authority+context+continuation:loader-authority+context-signer|clear=6:accounts=static:zeroes-header-refused-when-header-magic+version|each-tx<=1400000-target|account=DOOM_CTX+WAD+bootstrap|roots=u32-account-offsets|internal-pointers=pinned-DOOM_CTX-base|pin=WAD-SHA256+WAD-slice-SHA256+RI2-SHA256|dynamic=DOOM-RSN/3|account-provision=INIT+COMMUTATIVE-GROW/1|render=1:carries-snapshot-hash|refresh-render=5:buffer+strip+snapshot-hash:zero-name-refused-malformed:mismatched-name-refused-308";

fn read_json(path: &PathBuf) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn address(value: &Value, key: &str) -> Result<Pubkey, Box<dyn Error>> {
    Ok(Pubkey::from_str(value[key].as_str().ok_or("missing public address")?)?)
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn meta(key: Pubkey, writable: bool, signer: bool) -> AccountMeta {
    AccountMeta {
        pubkey: key,
        is_writable: writable,
        is_signer: signer,
    }
}

async fn submit(
    ctx: &mut solana_program_test::ProgramTestContext,
    instruction: Instruction,
    extra_signers: &[&Keypair],
) -> Result<u64, Box<dyn Error>> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await?;
    let instructions = [
        ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_LIMIT),
        instruction,
    ];
    let mut signers = vec![&ctx.payer];
    signers.extend_from_slice(extra_signers);
    let tx = Transaction::new(
        &signers,
        Message::new_with_blockhash(&instructions, Some(&ctx.payer.pubkey()), &blockhash),
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await?;
    if let Err(error) = result.result {
        let logs = result
            .metadata
            .as_ref()
            .map_or_else(String::new, |meta| meta.log_messages.join("\n"));
        return Err(format!("ProgramTest transaction failed: {error:?}\n{logs}").into());
    }
    Ok(result
        .metadata
        .ok_or("ProgramTest omitted transaction metadata")?
        .compute_units_consumed)
}

async fn account_data(
    ctx: &mut solana_program_test::ProgramTestContext,
    key: Pubkey,
) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(ctx
        .banks_client
        .get_account(key)
        .await?
        .ok_or("captured account missing from local bank")?
        .data)
}

fn role_address(identity: &Value, role: &str) -> Result<Pubkey, Box<dyn Error>> {
    if role == "program" {
        return address(identity, "program");
    }
    let value = identity["accounts"][role]
        .as_str()
        .ok_or_else(|| format!("missing account role {role}"))?;
    Ok(Pubkey::from_str(value)?)
}

fn check_layout(row: &Value) -> Result<(), Box<dyn Error>> {
    let bytes = row["bytes"].as_u64().ok_or("missing account bytes")? as usize;
    let roles = row["roles"].as_array().ok_or("missing account roles")?;
    for role in roles.iter().filter_map(Value::as_str) {
        let expected = match role {
            "accounts.authority" => Some(0),
            "accounts.buffer" => Some(sim2::BUFFER_BYTES),
            "accounts.context" | "static" => Some(rsr::ACCOUNT_BYTES),
            "accounts.continuation" => Some(doom_sbf::continuation_split_sbf::CONTEXT_CONTINUATION_BYTES),
            "accounts.controller" => Some(doom_sbf::resource_sbf::CONTROLLER_BYTES),
            "accounts.frame" => Some(doom_sbf::resource_sbf::RI2_FRAME_BYTES),
            "accounts.mirror" => Some(doom_sbf::resource_sbf::MIRROR_BYTES),
            "accounts.resource" => Some(doom_sbf::resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES),
            "accounts.state" => Some(doom_sbf::resource_sbf::STATE_MAX_BYTES),
            "accounts.wad-000" | "accounts.wad-001" | "accounts.wad-002"
            | "accounts.wad-003" | "accounts.wad-004" => {
                Some(doom_sbf::resource_sbf::WAD_ACCOUNT_BYTES)
            }
            "meta" => Some(sim2::META_BYTES),
            "legacy_snapshot" | "buffered_snapshots.0" | "buffered_snapshots.1"
            | "buffered_snapshots.2" | "buffered_snapshots.3" => Some(rsn3::SNAPSHOT_BYTES),
            role if role.starts_with("legacy_strips.") || role.starts_with("buffered_strips.") => {
                Some(rsn3::STRIP_ACCOUNT_BYTES)
            }
            _ => None,
        };
        if let Some(expected) = expected {
            if bytes != expected {
                return Err(format!("{role} is {bytes} bytes, expected {expected}").into());
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let capture = PathBuf::from(args.next().ok_or("usage: r15_upgrade_compat CAPTURE IMAGE")?);
    let image = PathBuf::from(args.next().ok_or("usage: r15_upgrade_compat CAPTURE IMAGE")?);
    let identity = read_json(&capture.join("r15-public-identity.json"))?;
    let manifest = read_json(&capture.join("r15-account-snapshot.json"))?;
    let program = address(&identity, "program")?;
    let context = address(&identity, "context")?;
    let static_key = address(&identity, "static")?;
    let meta_key = address(&identity, "meta")?;
    let state = role_address(&identity, "state")?;
    let mirror = role_address(&identity, "mirror")?;
    let buffer = role_address(&identity, "buffer")?;
    let snapshot = Pubkey::from_str(identity["buffered_snapshots"][0].as_str().ok_or("snapshot 0")?)?;
    let strip_keys: Vec<Pubkey> = identity["buffered_strips"][0]
        .as_array()
        .ok_or("buffer 0 strips")?
        .iter()
        .map(|key| Ok(Pubkey::from_str(key.as_str().ok_or("strip address")?)?))
        .collect::<Result<_, Box<dyn Error>>>()?;

    if manifest["program_id"].as_str() != Some(program.to_string().as_str())
        || manifest["context"].as_str() != Some(context.to_string().as_str())
    {
        return Err("capture identity and account snapshot do not agree".into());
    }
    if !image.is_file() {
        return Err(format!("SBF image does not exist: {}", image.display()).into());
    }
    for row in manifest["accounts"].as_array().ok_or("account rows")? {
        check_layout(row)?;
    }

    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(u64::from(COMPUTE_LIMIT));
    bank.add_program("doom_sbf", program, None);
    env::set_var("BPF_OUT_DIR", image.parent().ok_or("image parent")?);
    env::set_var("SBF_OUT_DIR", image.parent().ok_or("image parent")?);
    let local_authority = Keypair::new();

    let rows = manifest["accounts"].as_array().unwrap();
    let mut context_hash = String::new();
    let mut static_header: Option<Vec<u8>> = None;
    for row in rows {
        let key = Pubkey::from_str(row["address"].as_str().ok_or("row address")?)?;
        if key == program {
            continue; // ProgramTest installs the candidate SBF image here.
        }
        let data_path = capture.join(row["data_path"].as_str().ok_or("row data path")?);
        let mut data = fs::read(data_path)?;
        if key == meta_key {
            data[sim2::M_AUTHORITY..sim2::M_AUTHORITY + 32]
                .copy_from_slice(local_authority.pubkey().as_ref());
        }
        if key == buffer {
            data[sim2::B_AUTHORITY..sim2::B_AUTHORITY + 32]
                .copy_from_slice(local_authority.pubkey().as_ref());
        }
        if key == context {
            context_hash = hash(&data);
        }
        if key == static_key {
            static_header = Some(data[rsr::HEADER_OFFSET..rsr::HEADER_OFFSET + rsr::HEADER_BYTES].to_vec());
        }
        let owner = Pubkey::from_str(row["owner"].as_str().ok_or("row owner")?)?;
        bank.add_account(
            key,
            Account {
                lamports: row["lamports"].as_u64().unwrap_or(1),
                data,
                owner,
                executable: false,
                rent_epoch: row["rent_epoch"].as_u64().unwrap_or(0),
            },
        );
    }
    if context_hash.is_empty() {
        return Err("captured R15 context, meta, or buffer is missing or malformed".into());
    }
    bank.add_account(
        local_authority.pubkey(),
        Account {
            lamports: 1_000_000_000,
            data: Vec::new(),
            owner: solana_program::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let header = static_header.ok_or("captured static resource account is missing")?;
    if &header[..8] != rsr::MAGIC || u32_at(&header, 8) != rsr::VERSION
        || u32_at(&header, 16) as usize != rsr::ACCOUNT_BYTES
        || u32_at(&header, 32) as usize != rsr::HEADER_OFFSET
    {
        return Err("captured static resource version or layout does not match main".into());
    }
    let expected_abi = hashv(&[RSR_ABI_DESCRIPTOR.as_bytes()]).to_bytes();
    if header[120..152] != expected_abi {
        return Err(format!("static renderer ABI differs: stored={} expected={}", hash(&header[120..152]), hash(&expected_abi)).into());
    }

    let mut ctx = bank.start_with_context().await;
    let before_view = sim2::MetaView::parse(&account_data(&mut ctx, meta_key).await?)
        .ok_or("R15 SIM2 metadata failed to parse")?;
    let cursor = before_view.cursor;
    let snapshot_ix = Instruction {
        program_id: program,
        accounts: vec![
            meta(context, false, false), meta(meta_key, false, false), meta(state, false, false),
            meta(mirror, false, false), meta(static_key, false, false), meta(snapshot, true, false),
        ],
        data: rsn3::BufferedSnapshotRequest { buffer: 0 }.encode().to_vec(),
    };
    let snapshot_cu = submit(&mut ctx, snapshot_ix, &[]).await?;
    let snapshot_data = account_data(&mut ctx, snapshot).await?;
    let snapshot_view = rsn3::validate_buffered_snapshot_for(&snapshot_data, 0)?;
    if snapshot_view.cursor != cursor {
        return Err("buffered snapshot cursor differs from existing R15 meta".into());
    }
    if hash(&account_data(&mut ctx, context).await?) != context_hash {
        return Err("SNAPSHOT-LIVE-BUFFERED wrote the captured context account".into());
    }

    let mut render_cu = Vec::new();
    let mut first_strip: Option<Vec<u8>> = None;
    for strip in 0..8u8 {
        let ix = Instruction {
            program_id: program,
            accounts: vec![
                meta(static_key, true, false), meta(context, false, false),
                meta(snapshot, false, false), meta(strip_keys[strip as usize], true, false),
            ],
            data: rsr::Request::render_buffered(0, strip, snapshot_view.snapshot_hash).encode().to_vec(),
        };
        render_cu.push(submit(&mut ctx, ix, &[]).await?);
        let strip_data = account_data(&mut ctx, strip_keys[strip as usize]).await?;
        let strip_view = rsn3::validate_strip(&strip_data)?;
        if strip_view.cursor != cursor || strip_view.strip_id != strip {
            return Err(format!("rendered strip {strip} header does not match R15 cursor" ).into());
        }
        if strip == 0 { first_strip = Some(strip_data); }
    }
    let replay_ix = Instruction {
        program_id: program,
        accounts: vec![
            meta(static_key, true, false), meta(context, false, false),
            meta(snapshot, false, false), meta(strip_keys[0], true, false),
        ],
        data: rsr::Request::render_buffered(0, 0, snapshot_view.snapshot_hash).encode().to_vec(),
    };
    render_cu.push(submit(&mut ctx, replay_ix, &[]).await?);
    let replay_strip = account_data(&mut ctx, strip_keys[0]).await?;
    if first_strip.as_deref() != Some(replay_strip.as_slice()) {
        return Err("repeat rendering changed buffer 0 strip bytes".into());
    }
    if hash(&account_data(&mut ctx, context).await?) != context_hash {
        return Err("RENDER-BUFFERED wrote the captured context account".into());
    }

    // One existing slot may already be staged at the current cursor. If it is
    // empty, stage a neutral tic into this local clone using its substituted
    // authority, then run ADVANCE/5. No REBUILD operation is issued.
    let local_buffer = account_data(&mut ctx, buffer).await?;
    let slot_offset = sim2::BUFFER_HEADER_BYTES + cursor as usize * sim2::SLOT_BYTES;
    if cursor as usize >= before_view.capacity || slot_offset + sim2::SLOT_BYTES > local_buffer.len() {
        return Err("R15 cursor is outside its captured input buffer".into());
    }
    if local_buffer[slot_offset..slot_offset + sim2::SLOT_BYTES].iter().all(|b| *b == 0) {
        let deposit_ix = Instruction {
            program_id: program,
            accounts: vec![meta(buffer, true, false), meta(local_authority.pubkey(), false, true)],
            data: sim2::encode_deposit(cursor, &[0, 0, 0, 0]),
        };
        submit(&mut ctx, deposit_ix, &[&local_authority]).await?;
    }
    let advance_ix = Instruction {
        program_id: program,
        accounts: vec![
            meta(context, true, false), meta(state, true, false), meta(mirror, true, false),
            meta(meta_key, true, false), meta(buffer, false, false), meta(local_authority.pubkey(), false, true),
        ],
        data: sim2::encode_advance_starved(cursor, 1),
    };
    let advance_cu = submit(&mut ctx, advance_ix, &[&local_authority]).await?;
    let after_view = sim2::MetaView::parse(&account_data(&mut ctx, meta_key).await?)
        .ok_or("advanced SIM2 metadata failed to parse")?;
    if after_view.cursor != cursor + 1 {
        return Err(format!("ADVANCE/5 ended at {}, expected {}", after_view.cursor, cursor + 1).into());
    }

    println!("{{\"status\":\"measured\",\"accounts\":{},\"program\":\"{}\",\"context_bytes\":{},\"static_bytes\":{},\"rsr_version\":{},\"rsr_abi_id\":\"{}\",\"snapshot_cursor\":{},\"snapshot_cu\":{},\"render_strips\":8,\"render_cu\":{:?},\"repeat_strip_byte_identical\":true,\"context_unchanged_through_snapshot_and_render\":true,\"advance_from\":{},\"advance_to\":{},\"advance_cu\":{},\"rebuild_instructions\":0,\"local_only_authority_substitution\":true}}",
        rows.len(), program, rsr::ACCOUNT_BYTES, rsr::ACCOUNT_BYTES, rsr::VERSION,
        hash(&expected_abi), cursor, snapshot_cu, render_cu, cursor, after_view.cursor, advance_cu);
    Ok(())
}
