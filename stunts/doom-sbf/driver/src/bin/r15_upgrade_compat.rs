//! Load the exact finalized R15 account bytes into ProgramTest and exercise
//! the existing SIM/2 and RSR/2 paths under the candidate deployable image.
//!
//! The local clone substitutes only the authority public key in META and the
//! input-buffer header with ProgramTest's local payer. It never reads or needs
//! the R15 authority secret. SNAPSHOT-LIVE-BUFFERED and RENDER-BUFFERED are
//! read-only with respect to the live CONTEXT account; ADVANCE is then run on
//! the in-memory clone to prove the existing state can continue.

#![recursion_limit = "256"]

use std::{collections::HashMap, env, error::Error, fs, path::PathBuf, str::FromStr};

use doom_sbf::{lifecycle, render_snapshot_v3 as rsn3, sim2, static_resource as rsr};
use serde_json::Value;
use sha2::{Digest, Sha256};
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

// This local fixture raises the SVM budget so the first run can measure every
// bounded REINIT phase. The deployed transaction cap remains 1.4M CU and the
// runbook must not promote an over-budget phase.
const COMPUTE_LIMIT: u32 = 10_000_000;
const RSR_ABI_DESCRIPTOR: &str = "DOOM-RSR/2|request=52|build=5-WAD-chunks+6-resumable-C-phases|operations=BUILD/0+RENDER/1+INIT/2+GROW/3+REBUILD/4+RENDER-BUFFERED/5+CLEAR/6|rebuild=4:accounts=program+static+programdata+authority+context+continuation:loader-authority+context-signer|clear=6:accounts=static:zeroes-header-refused-when-header-magic+version|each-tx<=1400000-target|account=DOOM_CTX+WAD+bootstrap|roots=u32-account-offsets|internal-pointers=pinned-DOOM_CTX-base|pin=WAD-SHA256+WAD-slice-SHA256+RI2-SHA256|dynamic=DOOM-RSN/3|account-provision=INIT+COMMUTATIVE-GROW/1|render=1:carries-snapshot-hash|refresh-render=5:buffer+strip+snapshot-hash:zero-name-refused-malformed:mismatched-name-refused-308";

fn read_json(path: &PathBuf) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn address(value: &Value, key: &str) -> Result<Pubkey, Box<dyn Error>> {
    Ok(Pubkey::from_str(
        value[key].as_str().ok_or("missing public address")?,
    )?)
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
            "accounts.continuation" => Some(doom_sbf::continuation_split_sbf::CONTINUATION_BYTES),
            "accounts.controller" => Some(doom_sbf::resource_sbf::CONTROLLER_BYTES),
            "accounts.frame" => Some(doom_sbf::resource_sbf::RI2_FRAME_BYTES),
            "accounts.mirror" => Some(doom_sbf::resource_sbf::MIRROR_BYTES),
            "accounts.resource" => Some(doom_sbf::resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES),
            "accounts.state" => Some(doom_sbf::resource_sbf::STATE_MAX_BYTES),
            "accounts.wad-000" | "accounts.wad-001" | "accounts.wad-002" | "accounts.wad-003"
            | "accounts.wad-004" => Some(doom_sbf::resource_sbf::WAD_ACCOUNT_BYTES),
            "meta" => Some(sim2::META_BYTES),
            "legacy_snapshot"
            | "buffered_snapshots.0"
            | "buffered_snapshots.1"
            | "buffered_snapshots.2"
            | "buffered_snapshots.3" => Some(rsn3::SNAPSHOT_BYTES),
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

#[derive(Clone, Debug)]
struct LocalSet {
    context: Pubkey,
    resource: Pubkey,
    wads: Vec<Pubkey>,
    state: Pubkey,
    mirror: Pubkey,
    controller: Pubkey,
    frame: Pubkey,
    continuation: Pubkey,
    authority: Pubkey,
    meta: Pubkey,
    buffer: Pubkey,
    snapshots: Vec<Pubkey>,
    strips: Vec<Vec<Pubkey>>,
}

fn captured_set(identity: &Value, authority: Pubkey) -> Result<LocalSet, Box<dyn Error>> {
    let snapshots = identity["buffered_snapshots"]
        .as_array()
        .ok_or("buffered snapshots")?
        .iter()
        .map(|key| Ok(Pubkey::from_str(key.as_str().ok_or("snapshot address")?)?))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let strips = identity["buffered_strips"]
        .as_array()
        .ok_or("buffered strips")?
        .iter()
        .map(|buffer| {
            buffer
                .as_array()
                .ok_or("buffer strip list")?
                .iter()
                .map(|key| Ok(Pubkey::from_str(key.as_str().ok_or("strip address")?)?))
                .collect::<Result<Vec<_>, Box<dyn Error>>>()
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(LocalSet {
        context: address(identity, "context")?,
        resource: role_address(identity, "resource")?,
        wads: (0..5)
            .map(|index| role_address(identity, &format!("wad-{index:03}")))
            .collect::<Result<_, _>>()?,
        state: role_address(identity, "state")?,
        mirror: role_address(identity, "mirror")?,
        controller: role_address(identity, "controller")?,
        frame: role_address(identity, "frame")?,
        continuation: role_address(identity, "continuation")?,
        authority,
        meta: address(identity, "meta")?,
        buffer: role_address(identity, "buffer")?,
        snapshots,
        strips,
    })
}

fn owned_account(program: Pubkey, lamports: u64, data: Vec<u8>) -> Account {
    Account {
        lamports,
        data,
        owner: program,
        executable: false,
        rent_epoch: 0,
    }
}

fn copy_template(
    templates: &HashMap<Pubkey, Account>,
    key: Pubkey,
) -> Result<Account, Box<dyn Error>> {
    templates
        .get(&key)
        .cloned()
        .ok_or_else(|| format!("missing captured account template {key}").into())
}

fn add_fresh_reference(
    bank: &mut ProgramTest,
    program: Pubkey,
    template_set: &LocalSet,
    templates: &HashMap<Pubkey, Account>,
) -> Result<LocalSet, Box<dyn Error>> {
    let context = Pubkey::new_unique();
    let state = Pubkey::new_unique();
    let mirror = Pubkey::new_unique();
    let controller = Pubkey::new_unique();
    let frame = Pubkey::new_unique();
    let continuation = Pubkey::new_unique();
    let buffer = Pubkey::new_unique();
    let (meta, bump) = sim2::meta_address(&program, &context);
    let snapshots = (0..4u8)
        .map(|index| rsn3::buffered_snapshot_address(&program, &context, index).map(|pair| pair.0))
        .collect::<Option<Vec<_>>>()
        .ok_or("fresh snapshot PDA derivation failed")?;
    let strips = snapshots
        .iter()
        .map(|snapshot| {
            (0..8u8)
                .map(|strip| rsn3::strip_address(&program, snapshot, strip).0)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let old_context = copy_template(templates, template_set.context)?;
    bank.add_account(
        context,
        owned_account(
            program,
            old_context.lamports,
            vec![0; old_context.data.len()],
        ),
    );
    let old_state = copy_template(templates, template_set.state)?;
    bank.add_account(
        state,
        owned_account(program, old_state.lamports, vec![0; old_state.data.len()]),
    );
    let old_mirror = copy_template(templates, template_set.mirror)?;
    bank.add_account(
        mirror,
        owned_account(program, old_mirror.lamports, vec![0; old_mirror.data.len()]),
    );
    let old_controller = copy_template(templates, template_set.controller)?;
    bank.add_account(
        controller,
        owned_account(
            program,
            old_controller.lamports,
            vec![0; old_controller.data.len()],
        ),
    );
    let old_frame = copy_template(templates, template_set.frame)?;
    bank.add_account(
        frame,
        owned_account(program, old_frame.lamports, vec![0; old_frame.data.len()]),
    );
    let old_continuation = copy_template(templates, template_set.continuation)?;
    bank.add_account(
        continuation,
        owned_account(
            program,
            old_continuation.lamports,
            vec![0; old_continuation.data.len()],
        ),
    );

    let mut meta_data = copy_template(templates, template_set.meta)?.data;
    meta_data[sim2::M_CONTEXT..sim2::M_CONTEXT + 32].copy_from_slice(context.as_ref());
    meta_data[sim2::M_STATE..sim2::M_STATE + 32].copy_from_slice(state.as_ref());
    meta_data[sim2::M_MIRROR..sim2::M_MIRROR + 32].copy_from_slice(mirror.as_ref());
    meta_data[sim2::M_BUFFER..sim2::M_BUFFER + 32].copy_from_slice(buffer.as_ref());
    meta_data[sim2::M_AUTHORITY..sim2::M_AUTHORITY + 32]
        .copy_from_slice(template_set.authority.as_ref());
    meta_data[sim2::M_BUMP] = bump;
    let old_meta = copy_template(templates, template_set.meta)?;
    bank.add_account(meta, owned_account(program, old_meta.lamports, meta_data));

    let old_buffer = copy_template(templates, template_set.buffer)?;
    bank.add_account(
        buffer,
        owned_account(program, old_buffer.lamports, vec![0; old_buffer.data.len()]),
    );
    for (index, snapshot) in snapshots.iter().enumerate() {
        let old_snapshot = copy_template(templates, template_set.snapshots[index])?;
        bank.add_account(
            *snapshot,
            owned_account(
                program,
                old_snapshot.lamports,
                vec![0; rsn3::SNAPSHOT_BYTES],
            ),
        );
        for (strip, key) in strips[index].iter().enumerate() {
            let old_strip = copy_template(templates, template_set.strips[index][strip])?;
            bank.add_account(
                *key,
                owned_account(
                    program,
                    old_strip.lamports,
                    vec![0; rsn3::STRIP_ACCOUNT_BYTES],
                ),
            );
        }
    }
    Ok(LocalSet {
        context,
        resource: template_set.resource,
        wads: template_set.wads.clone(),
        state,
        mirror,
        controller,
        frame,
        continuation,
        authority: template_set.authority,
        meta,
        buffer,
        snapshots,
        strips,
    })
}

fn base_accounts(set: &LocalSet) -> Vec<AccountMeta> {
    let mut accounts = vec![
        meta(set.context, true, false),
        meta(set.resource, false, false),
    ];
    accounts.extend(set.wads.iter().map(|key| meta(*key, false, false)));
    accounts.extend([
        meta(set.state, true, false),
        meta(set.mirror, true, false),
        meta(set.controller, true, false),
        meta(set.frame, true, false),
        meta(set.continuation, true, false),
        meta(set.authority, false, true),
        meta(set.meta, true, false),
        meta(set.buffer, true, false),
    ]);
    accounts
}

fn clear_render_accounts(set: &LocalSet, target: Pubkey) -> Vec<AccountMeta> {
    vec![
        meta(set.context, false, false),
        meta(set.meta, true, false),
        meta(set.continuation, true, false),
        meta(target, true, false),
        meta(set.authority, true, true),
    ]
}

fn render_target(set: &LocalSet, index: usize) -> Pubkey {
    if index < 4 {
        set.snapshots[index]
    } else {
        let flat = index - 4;
        set.strips[flat / 8][flat % 8]
    }
}

async fn reinit(
    ctx: &mut solana_program_test::ProgramTestContext,
    program: Pubkey,
    set: &LocalSet,
    authority: &Keypair,
    label: &str,
) -> Result<Vec<u64>, Box<dyn Error>> {
    let mut units = Vec::new();
    let begin = Instruction {
        program_id: program,
        accounts: base_accounts(set),
        data: lifecycle::encode_begin().to_vec(),
    };
    units.push(submit(ctx, begin, &[authority])
        .await
        .map_err(|error| format!("{label} REINIT BEGIN failed: {error}"))?);
    for index in 0..36u8 {
        let target = render_target(set, index as usize);
        let clear = Instruction {
            program_id: program,
            accounts: clear_render_accounts(set, target),
            data: lifecycle::encode_clear_render(index).to_vec(),
        };
        units.push(submit(ctx, clear, &[authority])
            .await
            .map_err(|error| format!("{label} REINIT CLEAR_RENDER/{index} failed: {error}"))?);
    }
    for phase in 1..=6u8 {
        let step = Instruction {
            program_id: program,
            accounts: base_accounts(set),
            data: lifecycle::encode_step(phase).to_vec(),
        };
        units.push(submit(ctx, step, &[authority])
            .await
            .map_err(|error| format!("{label} REINIT STEP/{phase} failed: {error}"))?);
    }
    let view = sim2::MetaView::parse(&account_data(ctx, set.meta).await?)
        .ok_or_else(|| format!("{label} REINIT metadata failed to parse"))?;
    if view.cursor != 0 || view.first_tic != 0 {
        return Err(format!("{label} REINIT did not publish cursor/tic zero").into());
    }
    let controller = account_data(ctx, set.controller).await?;
    if u32_at(&controller, 20) != 0 {
        return Err(format!("{label} REINIT controller tic is not zero").into());
    }
    Ok(units)
}

struct RenderedFrame {
    snapshot: Vec<u8>,
    strips: Vec<Vec<u8>>,
    compute_units: Vec<u64>,
}

fn pixels(frame: &RenderedFrame) -> Result<Vec<u8>, Box<dyn Error>> {
    let views = frame
        .strips
        .iter()
        .map(|bytes| rsn3::validate_strip(bytes))
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(320 * 200);
    for y in 0..200 {
        for strip in &views {
            let start = y * 40;
            out.extend_from_slice(&strip.pixels[start..start + 40]);
        }
    }
    Ok(out)
}

async fn render_frame(
    ctx: &mut solana_program_test::ProgramTestContext,
    program: Pubkey,
    static_key: Pubkey,
    set: &LocalSet,
    cursor: u32,
) -> Result<RenderedFrame, Box<dyn Error>> {
    let snapshot = set.snapshots[0];
    let snapshot_ix = Instruction {
        program_id: program,
        accounts: vec![
            meta(set.context, false, false),
            meta(set.meta, false, false),
            meta(set.state, false, false),
            meta(set.mirror, false, false),
            meta(static_key, false, false),
            meta(snapshot, true, false),
        ],
        data: rsn3::BufferedSnapshotRequest { buffer: 0 }
            .encode()
            .to_vec(),
    };
    let mut compute_units = vec![submit(ctx, snapshot_ix, &[])
        .await
        .map_err(|error| format!("snapshot at cursor {cursor} failed: {error}"))?];
    let snapshot_data = account_data(ctx, snapshot).await?;
    let view = rsn3::validate_buffered_snapshot_for(&snapshot_data, 0)?;
    if view.cursor != cursor {
        return Err(format!(
            "snapshot cursor {} differs from expected {cursor}",
            view.cursor
        )
        .into());
    }
    let mut strips = Vec::new();
    let mut first_strip: Option<Vec<u8>> = None;
    for strip in 0..8u8 {
        let ix = Instruction {
            program_id: program,
            accounts: vec![
                meta(static_key, true, false),
                meta(set.context, false, false),
                meta(snapshot, false, false),
                meta(set.strips[0][strip as usize], true, false),
            ],
            data: rsr::Request::render_buffered(0, strip, view.snapshot_hash)
                .encode()
                .to_vec(),
        };
        compute_units.push(submit(ctx, ix, &[])
            .await
            .map_err(|error| format!("RENDER-BUFFERED cursor {cursor} strip {strip} failed: {error}"))?);
        let rendered = account_data(ctx, set.strips[0][strip as usize]).await?;
        let strip_view = rsn3::validate_strip(&rendered)?;
        if strip_view.cursor != cursor || strip_view.strip_id != u32::from(strip) {
            return Err(format!("strip {strip} header does not match cursor {cursor}").into());
        }
        if strip == 0 {
            first_strip = Some(rendered.clone());
        }
        strips.push(rendered);
    }
    let replay = Instruction {
        program_id: program,
        accounts: vec![
            meta(static_key, true, false),
            meta(set.context, false, false),
            meta(snapshot, false, false),
            meta(set.strips[0][0], true, false),
        ],
        data: rsr::Request::render_buffered(0, 0, view.snapshot_hash)
            .encode()
            .to_vec(),
    };
    compute_units.push(submit(ctx, replay, &[])
        .await
        .map_err(|error| format!("repeat RENDER-BUFFERED cursor {cursor} failed: {error}"))?);
    let replay_strip = account_data(ctx, set.strips[0][0]).await?;
    if first_strip.as_deref() != Some(replay_strip.as_slice()) {
        return Err(format!("repeat RENDER-BUFFERED changed cursor {cursor} strip 0").into());
    }
    Ok(RenderedFrame {
        snapshot: snapshot_data,
        strips,
        compute_units,
    })
}

async fn advance_one(
    ctx: &mut solana_program_test::ProgramTestContext,
    program: Pubkey,
    set: &LocalSet,
    authority: &Keypair,
    cursor: u32,
) -> Result<u64, Box<dyn Error>> {
    let buffer_data = account_data(ctx, set.buffer).await?;
    let slot = sim2::BUFFER_HEADER_BYTES + cursor as usize * sim2::SLOT_BYTES;
    if slot + sim2::SLOT_BYTES > buffer_data.len() {
        return Err("input buffer cursor is out of range".into());
    }
    if buffer_data[slot..slot + sim2::SLOT_BYTES]
        .iter()
        .all(|byte| *byte == 0)
    {
        let deposit = Instruction {
            program_id: program,
            accounts: vec![
                meta(set.buffer, true, false),
                meta(set.authority, false, true),
            ],
            data: sim2::encode_deposit(cursor, &[0, 0, 0, 0]),
        };
        submit(ctx, deposit, &[authority])
            .await
            .map_err(|error| format!("input deposit at cursor {cursor} failed: {error}"))?;
    }
    let advance = Instruction {
        program_id: program,
        accounts: vec![
            meta(set.context, true, false),
            meta(set.state, true, false),
            meta(set.mirror, true, false),
            meta(set.meta, true, false),
            meta(set.buffer, false, false),
            meta(set.authority, false, true),
        ],
        data: sim2::encode_advance_starved(cursor, 1),
    };
    submit(ctx, advance, &[authority])
        .await
        .map_err(|error| format!("ADVANCE at cursor {cursor} failed: {error}"))
}

fn close_accounts(set: &LocalSet) -> Vec<AccountMeta> {
    let mut accounts = vec![
        meta(set.context, true, false),
        meta(set.state, true, false),
        meta(set.mirror, true, false),
        meta(set.meta, true, false),
        meta(set.buffer, true, false),
        meta(set.controller, true, false),
        meta(set.frame, true, false),
        meta(set.continuation, true, false),
        meta(set.authority, false, true),
        meta(set.authority, true, false),
    ];
    accounts.extend(set.snapshots.iter().map(|key| meta(*key, true, false)));
    for strips in &set.strips {
        accounts.extend(strips.iter().map(|key| meta(*key, true, false)));
    }
    accounts
}

async fn close_set(
    ctx: &mut solana_program_test::ProgramTestContext,
    program: Pubkey,
    set: &LocalSet,
    authority: &Keypair,
) -> Result<(u64, u64), Box<dyn Error>> {
    let targets = [
        set.context,
        set.state,
        set.mirror,
        set.meta,
        set.buffer,
        set.controller,
        set.frame,
        set.continuation,
    ]
    .into_iter()
    .chain(set.snapshots.iter().copied())
    .chain(set.strips.iter().flat_map(|strips| strips.iter().copied()))
    .collect::<Vec<_>>();
    if targets.len() != 44 {
        return Err(format!(
            "CLOSE target set has {} accounts, expected 44",
            targets.len()
        )
        .into());
    }
    let mut balance_before = 0u64;
    for key in &targets {
        balance_before = balance_before
            .checked_add(
                ctx.banks_client
                    .get_account(*key)
                    .await?
                    .map_or(0, |account| account.lamports),
            )
            .ok_or("CLOSE refund sum overflow")?;
    }
    let payer_before = ctx
        .banks_client
        .get_account(set.authority)
        .await?
        .ok_or("CLOSE payer missing")?
        .lamports;
    let close = Instruction {
        program_id: program,
        accounts: close_accounts(set),
        data: lifecycle::encode_close().to_vec(),
    };
    let units = submit(ctx, close, &[authority]).await?;
    let payer_after = ctx
        .banks_client
        .get_account(set.authority)
        .await?
        .ok_or("CLOSE payer missing after close")?
        .lamports;
    let refunded = payer_after.saturating_sub(payer_before);
    if refunded != balance_before {
        return Err(
            format!("CLOSE refunded {refunded} lamports, expected {balance_before}").into(),
        );
    }
    for key in &targets {
        if let Some(account) = ctx.banks_client.get_account(*key).await? {
            if account.owner == program || account.lamports != 0 || !account.data.is_empty() {
                return Err(format!("CLOSE left a live account {key}").into());
            }
        }
    }
    let double_close = Instruction {
        program_id: program,
        accounts: close_accounts(set),
        data: lifecycle::encode_close().to_vec(),
    };
    if submit(ctx, double_close, &[authority]).await.is_ok() {
        return Err("double CLOSE unexpectedly succeeded".into());
    }
    Ok((balance_before, units))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let capture = PathBuf::from(
        args.next()
            .ok_or("usage: r15_upgrade_compat CAPTURE IMAGE")?,
    );
    let image = PathBuf::from(
        args.next()
            .ok_or("usage: r15_upgrade_compat CAPTURE IMAGE")?,
    );
    let identity_path = if capture.join("r15-public-identity.json").is_file() {
        capture.join("r15-public-identity.json")
    } else {
        capture.join("public-identity.json")
    };
    let identity = read_json(&identity_path)?;
    let manifest = read_json(&capture.join("r15-account-snapshot.json"))?;
    let program = address(&identity, "program")?;
    let context = address(&identity, "context")?;
    let static_key = address(&identity, "static")?;
    if manifest["program_id"].as_str() != Some(program.to_string().as_str())
        || manifest["context"].as_str() != Some(context.to_string().as_str())
    {
        return Err("capture identity and account snapshot do not agree".into());
    }
    if !image.is_file() {
        return Err(format!("SBF image does not exist: {}", image.display()).into());
    }
    let rows = manifest["accounts"].as_array().ok_or("account rows")?;
    for row in rows {
        check_layout(row)?;
    }

    let local_authority = Keypair::new();
    let stranger = Keypair::new();
    let set = captured_set(&identity, local_authority.pubkey())?;
    env::set_var("BPF_OUT_DIR", image.parent().ok_or("image parent")?);
    env::set_var("SBF_OUT_DIR", image.parent().ok_or("image parent")?);
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(u64::from(COMPUTE_LIMIT));
    bank.add_program("doom_sbf", program, None);

    let mut templates = HashMap::new();
    let mut context_hash = String::new();
    let mut static_header: Option<Vec<u8>> = None;
    for row in rows {
        let key = Pubkey::from_str(row["address"].as_str().ok_or("row address")?)?;
        if key == program {
            continue; // ProgramTest installs the candidate SBF image here.
        }
        let data_path = capture.join(row["data_path"].as_str().ok_or("row data path")?);
        let mut data = fs::read(data_path)?;
        if key == set.meta {
            data[sim2::M_AUTHORITY..sim2::M_AUTHORITY + 32]
                .copy_from_slice(local_authority.pubkey().as_ref());
        }
        if key == set.buffer {
            data[sim2::B_AUTHORITY..sim2::B_AUTHORITY + 32]
                .copy_from_slice(local_authority.pubkey().as_ref());
        }
        if key == context {
            context_hash = hash(&data);
        }
        if key == static_key {
            static_header =
                Some(data[rsr::HEADER_OFFSET..rsr::HEADER_OFFSET + rsr::HEADER_BYTES].to_vec());
        }
        let owner = Pubkey::from_str(row["owner"].as_str().ok_or("row owner")?)?;
        let account = Account {
            lamports: row["lamports"].as_u64().unwrap_or(1),
            data,
            owner,
            executable: false,
            rent_epoch: row["rent_epoch"].as_u64().unwrap_or(0),
        };
        templates.insert(key, account.clone());
        bank.add_account(key, account);
    }
    if context_hash.is_empty() {
        return Err("captured context account is missing or malformed".into());
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
    bank.add_account(
        stranger.pubkey(),
        Account {
            lamports: 1_000_000_000,
            data: Vec::new(),
            owner: solana_program::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let header = static_header.ok_or("captured static resource account is missing")?;
    if &header[..8] != rsr::MAGIC
        || u32_at(&header, 8) != rsr::VERSION
        || u32_at(&header, 16) as usize != rsr::ACCOUNT_BYTES
        || u32_at(&header, 32) as usize != rsr::HEADER_OFFSET
    {
        return Err("captured static resource version or layout does not match main".into());
    }
    let expected_abi = hashv(&[RSR_ABI_DESCRIPTOR.as_bytes()]).to_bytes();
    if header[120..152] != expected_abi {
        return Err(format!(
            "static renderer ABI differs: stored={} expected={}",
            hash(&header[120..152]),
            hash(&expected_abi)
        )
        .into());
    }

    let reference = add_fresh_reference(&mut bank, program, &set, &templates)?;
    let mut ctx = bank.start_with_context().await;

    // A stranger can sign, but cannot use this context's metadata to reset or
    // close it. These controls run before any account in the captured set is
    // changed by REINIT.
    let before_context = hash(&account_data(&mut ctx, set.context).await?);
    let mut stranger_begin_accounts = base_accounts(&set);
    stranger_begin_accounts[12] = meta(stranger.pubkey(), false, true);
    let stranger_begin = Instruction {
        program_id: program,
        accounts: stranger_begin_accounts,
        data: lifecycle::encode_begin().to_vec(),
    };
    if submit(&mut ctx, stranger_begin, &[&stranger]).await.is_ok() {
        return Err("stranger REINIT unexpectedly succeeded".into());
    }
    let mut stranger_close_accounts = close_accounts(&set);
    stranger_close_accounts[8] = meta(stranger.pubkey(), false, true);
    let stranger_close = Instruction {
        program_id: program,
        accounts: stranger_close_accounts,
        data: lifecycle::encode_close().to_vec(),
    };
    if submit(&mut ctx, stranger_close, &[&stranger]).await.is_ok() {
        return Err("stranger CLOSE unexpectedly succeeded".into());
    }
    if hash(&account_data(&mut ctx, set.context).await?) != before_context {
        return Err("stranger lifecycle controls changed the context".into());
    }

    let first_reinit_units = reinit(&mut ctx, program, &set, &local_authority, "captured").await?;
    println!("captured REINIT complete at cursor 0");
    let fresh_reinit_units = reinit(
        &mut ctx,
        program,
        &reference,
        &local_authority,
        "fresh reference",
    )
    .await?;
    println!("fresh reference REINIT complete at cursor 0");
    // The user specifically requires REINIT to accept an already fresh game.
    let fresh_again_units = reinit(
        &mut ctx,
        program,
        &reference,
        &local_authority,
        "already fresh",
    )
    .await?;
    println!("repeat REINIT complete at cursor 0");

    let captured_zero = render_frame(&mut ctx, program, static_key, &set, 0).await?;
    let fresh_zero = render_frame(&mut ctx, program, static_key, &reference, 0).await?;
    if pixels(&captured_zero)? != pixels(&fresh_zero)? {
        return Err("REINIT cursor-0 pixels differ from the fresh context reference".into());
    }
    let advance_cu = advance_one(&mut ctx, program, &set, &local_authority, 0).await?;
    let fresh_advance_cu = advance_one(&mut ctx, program, &reference, &local_authority, 0).await?;
    let captured_cursor = sim2::MetaView::parse(&account_data(&mut ctx, set.meta).await?)
        .ok_or("captured SIM2 metadata failed to parse after ADVANCE")?
        .cursor;
    let fresh_cursor = sim2::MetaView::parse(&account_data(&mut ctx, reference.meta).await?)
        .ok_or("fresh SIM2 metadata failed to parse after ADVANCE")?
        .cursor;
    if captured_cursor != 1 || fresh_cursor != 1 {
        return Err("ADVANCE after REINIT did not end both contexts at cursor 1".into());
    }
    let captured_one = render_frame(&mut ctx, program, static_key, &set, 1).await?;
    let fresh_one = render_frame(&mut ctx, program, static_key, &reference, 1).await?;
    if pixels(&captured_one)? != pixels(&fresh_one)? {
        return Err("REINIT cursor-1 pixels differ from the fresh context reference".into());
    }

    let (captured_refund, captured_close_cu) =
        close_set(&mut ctx, program, &set, &local_authority).await?;
    let (reference_refund, reference_close_cu) =
        close_set(&mut ctx, program, &reference, &local_authority).await?;
    let captured_zero_hash = hash(&pixels(&captured_zero)?);
    let captured_one_hash = hash(&pixels(&captured_one)?);
    println!(
        "{{\"status\":\"measured\",\"capture\":\"{}\",\"captured_accounts\":{},\"fresh_fixture_accounts\":44,\"renderer_abi\":\"{}\",\"reinit_cu\":{:?},\"fresh_reinit_cu\":{:?},\"already_fresh_reinit_cu\":{:?},\"cursor0_pixels_equal\":true,\"cursor0_sha256\":\"{}\",\"cursor0_strips\":8,\"cursor0_repeat_render_byte_identical\":true,\"advance_from\":0,\"advance_to\":1,\"advance_cu\":{},\"fresh_advance_cu\":{},\"cursor1_pixels_equal\":true,\"cursor1_sha256\":\"{}\",\"cursor1_strips\":8,\"cursor1_repeat_render_byte_identical\":true,\"stranger_reinit_refused\":true,\"stranger_close_refused\":true,\"reinit_already_fresh_succeeded\":true,\"close_accounts\":44,\"close_refund_lamports\":{},\"close_refund_fogo\":{:.9},\"close_cu\":{},\"close_left_program_owned_accounts\":0,\"double_close_refused\":true,\"fresh_reference_refund_lamports\":{},\"fresh_reference_close_cu\":{}}}",
        capture.display(), rows.len(), hash(&expected_abi), first_reinit_units, fresh_reinit_units,
        fresh_again_units, captured_zero_hash, advance_cu, fresh_advance_cu,
        captured_one_hash, captured_refund, captured_refund as f64 / 1_000_000_000.0,
        captured_close_cu, reference_refund, reference_close_cu
    );
    Ok(())
}
