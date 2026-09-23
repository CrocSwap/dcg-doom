//! DOOM-SIM/2 gate (b): the transport image on SBF under ProgramTest.
//!
//! `ProgramTest::prefer_bpf(true)` loads the pinned `doom_sbf.so`; every
//! instruction below is a real transaction through the BPF entry point.
//!
//! 1. DOOM-STG/1 stages ST1/MP1/CI2 into fresh program-owned accounts
//!    (the RI2 resource and WAD accounts are the retained captures, injected
//!    at genesis as in the prior gates).
//! 2. E2R/5 reconstructs the context; PREPARE without the context signature
//!    and re-PREPARE of a prepared/published context are refused.
//! 3. SIM/2 INIT binds the PDA meta; attacker, non-PDA, wrong-key and
//!    wrong-hash INITs are refused, and a pre-funded PDA is handled.
//! 4. Deposits land in the gate-(a) scrambled order with the same held late
//!    arrivals; ADVANCE sweeps; after every sweep that ran, ST1/MP1, the root
//!    and the input chain equal the sequential no-render oracle at that
//!    cursor.  Controls run before the first tic, mid-run and at the end.
//!
//! With `--profile` (diagnostic image) the phase markers are parsed into
//! per-tic step and per-sweep publication costs.

#[path = "continuation_split_sbf.rs"]
#[allow(dead_code)]
mod base;
#[path = "../sweep_common.rs"]
#[allow(dead_code)]
mod common;

use std::{collections::BTreeMap, env, error::Error, fs, io::Write, path::PathBuf};

use common::{hex, Capture, Rng};
use doom_sbf::{continuation_split_sbf, resource_sbf, sim2, stage};
use solana_account::Account;
use solana_banks_interface::BanksTransactionResultWithMetadata;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::{instruction as system_instruction, program as system_program};
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

const RESOURCE: Pubkey = Pubkey::new_from_array([0xe1; 32]);
const FRAME: Pubkey = Pubkey::new_from_array([0xe5; 32]);
const ZERO_BUFFER: Pubkey = Pubkey::new_from_array([0xb1; 32]);
const DECOY_META: Pubkey = Pubkey::new_from_array([0xb2; 32]);
const STAGE_CHUNK: usize = 900;
const HOLD: [u32; 2] = [37, 555];
const PHASE_MAGIC: &str = "0x444f4f4d50484153";
const SIM_TRANSPORT_E2R_VERSION: u32 = 5;

fn wad_key(ordinal: usize) -> Pubkey {
    Pubkey::new_from_array([0xf0 + ordinal as u8; 32])
}

fn meta(key: Pubkey, writable: bool, signer: bool) -> AccountMeta {
    AccountMeta {
        pubkey: key,
        is_signer: signer,
        is_writable: writable,
    }
}

struct Keys {
    authority: Keypair,
    attacker: Keypair,
    context: Keypair,
    state: Keypair,
    mirror: Keypair,
    controller: Keypair,
    decoy_state: Keypair,
    buffer: Keypair,
    meta: Pubkey,
    bump: u8,
}

struct Gate {
    ctx: ProgramTestContext,
    keys: Keys,
    nonce: u64,
    profile: bool,
}

fn custom_code(result: &BanksTransactionResultWithMetadata) -> Option<u32> {
    match &result.result {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(code))) => Some(*code),
        _ => None,
    }
}

fn logs(result: &BanksTransactionResultWithMetadata) -> Vec<String> {
    result
        .metadata
        .as_ref()
        .map(|m| m.log_messages.clone())
        .unwrap_or_default()
}

fn markers(result: &BanksTransactionResultWithMetadata) -> Vec<(u64, u64)> {
    logs(result)
        .iter()
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

/// The `SIM2-ADV` receipt ADVANCE logs with `sol_log_data`.
fn sweep_receipt(result: &BanksTransactionResultWithMetadata) -> Option<Vec<u8>> {
    use base64::Engine as _;
    logs(result).iter().find_map(|line| {
        let body = line.strip_prefix("Program data: ")?;
        let mut parts = body.split(' ');
        let tag = base64::engine::general_purpose::STANDARD
            .decode(parts.next()?)
            .ok()?;
        if tag != b"SIM2-ADV" {
            return None;
        }
        base64::engine::general_purpose::STANDARD
            .decode(parts.next()?)
            .ok()
    })
}

impl Gate {
    async fn send(
        &mut self,
        instructions: Vec<Instruction>,
        signers: &[&Keypair],
    ) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
        // A unique compute-unit price per transaction keeps otherwise
        // identical sweeps distinct; it does not change the compute limit.
        self.nonce += 1;
        let mut all = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(base::NORMAL_LIMIT),
            ComputeBudgetInstruction::set_compute_unit_price(self.nonce),
        ];
        all.extend(instructions);
        let blockhash = self.ctx.banks_client.get_latest_blockhash().await?;
        let mut keypairs: Vec<&Keypair> = vec![&self.ctx.payer];
        keypairs.extend_from_slice(signers);
        let tx = Transaction::new(
            &keypairs,
            Message::new_with_blockhash(&all, Some(&self.ctx.payer.pubkey()), &blockhash),
            blockhash,
        );
        Ok(self
            .ctx
            .banks_client
            .process_transaction_with_metadata(tx)
            .await?)
    }

    async fn data(&mut self, key: Pubkey) -> Result<Vec<u8>, Box<dyn Error>> {
        base::account_data(&self.ctx.banks_client, key).await
    }

    async fn durable(&mut self) -> Result<Vec<String>, Box<dyn Error>> {
        let mut out = Vec::new();
        for key in [
            self.keys.context.pubkey(),
            self.keys.state.pubkey(),
            self.keys.mirror.pubkey(),
            self.keys.meta,
            self.keys.buffer.pubkey(),
        ] {
            match self.ctx.banks_client.get_account(key).await? {
                Some(account) => out.push(base::sha(&account.data)),
                None => out.push("absent".into()),
            }
        }
        Ok(out)
    }

    fn e2r_ix(
        &self,
        authority: Pubkey,
        continuation: Pubkey,
        context_signs: bool,
        operation: u8,
        cursor: u32,
        state_len: usize,
    ) -> Instruction {
        let mut accounts = vec![
            meta(self.keys.context.pubkey(), true, context_signs),
            meta(RESOURCE, false, false),
        ];
        for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
            accounts.push(meta(wad_key(ordinal), false, false));
        }
        accounts.extend([
            meta(self.keys.state.pubkey(), true, false),
            meta(self.keys.mirror.pubkey(), true, false),
            meta(self.keys.controller.pubkey(), false, false),
            meta(FRAME, true, false),
            meta(continuation, true, false),
            meta(authority, false, true),
        ]);
        let mut data = continuation_split_sbf::Request {
            operation,
            state_len,
            cursor,
            fault: 0,
        }
        .encode()
        .to_vec();
        // The host harness is built with the provenance-pinned `ctx-engine`
        // feature only.  The loaded SBF image is DOOM-SIM/2 transport and
        // therefore accepts E2R/5; both request versions have the same 32-byte
        // field layout, with the version as the sole header difference.
        data[8..12].copy_from_slice(&SIM_TRANSPORT_E2R_VERSION.to_le_bytes());
        Instruction {
            program_id: base::PROGRAM,
            accounts,
            data,
        }
    }

    fn init_ix(
        &self,
        authority: Pubkey,
        meta_key: Pubkey,
        request: &sim2::InitRequest,
    ) -> Instruction {
        self.init_ix_with_buffer(authority, meta_key, self.keys.buffer.pubkey(), request)
    }

    fn init_ix_with_buffer(
        &self,
        authority: Pubkey,
        meta_key: Pubkey,
        buffer_key: Pubkey,
        request: &sim2::InitRequest,
    ) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.context.pubkey(), false, false),
                meta(self.keys.state.pubkey(), false, false),
                meta(self.keys.mirror.pubkey(), false, false),
                meta(meta_key, true, false),
                meta(buffer_key, true, false),
                meta(authority, true, true),
                meta(system_program::id(), false, false),
            ],
            data: request.encode(),
        }
    }

    fn deposit_ix(
        &self,
        buffer: Pubkey,
        authority: Pubkey,
        first: u32,
        commands: &[u8],
    ) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![meta(buffer, true, false), meta(authority, false, true)],
            data: sim2::encode_deposit(first, commands),
        }
    }

    fn grow_buffer_ix(&self, buffer: Pubkey, capacity: u32, expected_len: u32) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![meta(buffer, true, true)],
            data: sim2::encode_grow_buffer(capacity, expected_len),
        }
    }

    fn advance_ix(
        &self,
        state: Pubkey,
        meta_key: Pubkey,
        buffer: Pubkey,
        authority: Pubkey,
        max_tics: u32,
    ) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.context.pubkey(), true, false),
                meta(state, true, false),
                meta(self.keys.mirror.pubkey(), true, false),
                meta(meta_key, true, false),
                meta(buffer, false, false),
                meta(authority, false, true),
            ],
            data: sim2::encode_advance(max_tics),
        }
    }

    fn advance_expected_ix(
        &self,
        state: Pubkey,
        meta_key: Pubkey,
        buffer: Pubkey,
        authority: Pubkey,
        expected_cursor: u32,
        max_tics: u32,
    ) -> Instruction {
        let mut ix = self.advance_ix(state, meta_key, buffer, authority, max_tics);
        ix.data = sim2::encode_advance_expected(expected_cursor, max_tics);
        ix
    }

    fn snapshot_ix(&self, cursor: u32, root: &[u8; 32]) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.meta, false, false),
                meta(self.keys.state.pubkey(), false, false),
                meta(self.keys.mirror.pubkey(), false, false),
            ],
            data: sim2::encode_snapshot(cursor, root),
        }
    }

    fn stage_ix(target: Pubkey, signs: bool, offset: usize, bytes: &[u8]) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![meta(target, true, signs)],
            data: stage::encode(offset as u32, bytes),
        }
    }

    async fn deposit(
        &mut self,
        first: u32,
        commands: &[u8],
    ) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
        let ix = self.deposit_ix(
            self.keys.buffer.pubkey(),
            self.keys.authority.pubkey(),
            first,
            commands,
        );
        let authority = self.keys.authority.insecure_clone();
        self.send(vec![ix], &[&authority]).await
    }

    async fn advance(
        &mut self,
        max_tics: u32,
    ) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
        let ix = self.advance_ix(
            self.keys.state.pubkey(),
            self.keys.meta,
            self.keys.buffer.pubkey(),
            self.keys.authority.pubkey(),
            max_tics,
        );
        let authority = self.keys.authority.insecure_clone();
        self.send(vec![ix], &[&authority]).await
    }

    async fn advance_expected(
        &mut self,
        expected_cursor: u32,
        max_tics: u32,
    ) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
        let ix = self.advance_expected_ix(
            self.keys.state.pubkey(),
            self.keys.meta,
            self.keys.buffer.pubkey(),
            self.keys.authority.pubkey(),
            expected_cursor,
            max_tics,
        );
        let authority = self.keys.authority.insecure_clone();
        self.send(vec![ix], &[&authority]).await
    }

    async fn stage_account(
        &mut self,
        target: &Keypair,
        bytes: &[u8],
    ) -> Result<usize, Box<dyn Error>> {
        let chunks: Vec<(usize, &[u8])> = bytes
            .chunks(STAGE_CHUNK)
            .enumerate()
            .map(|(i, c)| (i * STAGE_CHUNK, c))
            .collect();
        let mut sent = 0;
        // Body first (skipping all-zero chunks: the account is zero-filled),
        // then verify, then the sealing header chunk.
        for &(offset, chunk) in chunks.iter().skip(1) {
            if chunk.iter().all(|&b| b == 0) {
                continue;
            }
            let result = self
                .send(
                    vec![Self::stage_ix(target.pubkey(), true, offset, chunk)],
                    &[target],
                )
                .await?;
            if result.result.is_err() {
                return Err(format!("stage body {offset}: {:?}", result.result).into());
            }
            sent += 1;
        }
        let body = self.data(target.pubkey()).await?;
        if body[STAGE_CHUNK.min(bytes.len())..] != bytes[STAGE_CHUNK.min(bytes.len())..] {
            return Err("staged body readback mismatch".into());
        }
        let (offset, chunk) = chunks[0];
        let result = self
            .send(
                vec![Self::stage_ix(target.pubkey(), true, offset, chunk)],
                &[target],
            )
            .await?;
        if result.result.is_err() {
            return Err(format!("stage header: {:?}", result.result).into());
        }
        if self.data(target.pubkey()).await? != bytes {
            return Err("staged account readback mismatch".into());
        }
        Ok(sent + 1)
    }
}

fn program_account(data: Vec<u8>) -> Account {
    base::account(data)
}

fn system_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: Vec::new(),
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    }
}

async fn refused(
    gate: &mut Gate,
    controls: &mut serde_json::Map<String, serde_json::Value>,
    label: &str,
    code: u32,
    instructions: Vec<Instruction>,
    signers: &[&Keypair],
) -> Result<(), Box<dyn Error>> {
    let before = gate.durable().await?;
    let cursor = sim2::MetaView::parse(&gate.data(gate.keys.meta).await.unwrap_or_default())
        .map(|v| v.cursor);
    let result = gate.send(instructions, signers).await?;
    let got = custom_code(&result);
    if got != Some(code) {
        return Err(format!("{label}: expected Custom({code}), got {:?}", result.result).into());
    }
    if gate.durable().await? != before {
        return Err(format!("{label}: refusal mutated durable accounts").into());
    }
    controls.insert(
        label.into(),
        serde_json::json!({"refused":true,"code":code,"cursor":cursor,"cu":base::units(&result)?,"durable_preserved":true}),
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let argv: Vec<String> = env::args().collect();
    let mut args = argv.iter().skip(1);
    let capture = PathBuf::from(args.next().ok_or("missing capture")?).canonicalize()?;
    let resource_path = PathBuf::from(args.next().ok_or("missing resource")?).canonicalize()?;
    let controller_path = PathBuf::from(args.next().ok_or("missing controller")?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let image = PathBuf::from(args.next().ok_or("missing image")?).canonicalize()?;
    let expected_image = args.next().ok_or("missing image SHA")?.clone();
    let manifest = PathBuf::from(args.next().ok_or("missing manifest")?).canonicalize()?;
    let expected_manifest = args.next().ok_or("missing manifest SHA")?.clone();
    let oracle_receipt =
        PathBuf::from(args.next().ok_or("missing gate-(a) receipt")?).canonicalize()?;
    let mut tics = 1000usize;
    let mut seed = 0x5eed_d00d_2026_0922u64;
    let mut profile = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--profile" => profile = true,
            "--tics" => tics = args.next().ok_or("--tics value")?.parse()?,
            "--seed" => {
                seed = u64::from_str_radix(
                    args.next().ok_or("--seed value")?.trim_start_matches("0x"),
                    16,
                )?
            }
            _ => return Err(format!("unknown flag {flag}").into()),
        }
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err("receipt directory is not empty".into());
    }
    fs::create_dir_all(&receipt)?;
    let image_bytes = base::read(&image)?;
    let manifest_bytes = base::read(&manifest)?;
    if base::sha(&image_bytes) != expected_image || base::sha(&manifest_bytes) != expected_manifest
    {
        return Err("image/manifest identity mismatch".into());
    }
    let manifest_json: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    if manifest_json["profile"] != "sim-transport" {
        return Err("image manifest is not the sim-transport profile".into());
    }
    env::set_var("BPF_OUT_DIR", image.parent().unwrap());
    env::set_var("SBF_OUT_DIR", image.parent().unwrap());

    let capture_data = Capture::load(&capture, &resource_path)?;
    let controller = base::read(&controller_path)?;
    let commands = capture_data.demo_commands(tics)?;
    let staged_state = capture_data.state_account();

    let authority = Keypair::new();
    let context_kp = Keypair::new();
    let (meta_key, bump) = sim2::meta_address(&base::PROGRAM, &context_kp.pubkey());
    let keys = Keys {
        authority,
        attacker: Keypair::new(),
        context: context_kp,
        state: Keypair::new(),
        mirror: Keypair::new(),
        controller: Keypair::new(),
        decoy_state: Keypair::new(),
        buffer: Keypair::new(),
        meta: meta_key,
        bump,
    };

    // Oracle in this process, tied to gate (a)'s retained per-tic roots.
    let oracle = common::sequential_oracle(&capture_data, &commands, &keys.context.pubkey())?;
    let retained = fs::read_to_string(oracle_receipt.join("oracle-per-tic.jsonl"))?;
    let retained_roots: Vec<String> = retained
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["state_root"].as_str().unwrap_or("").to_owned())
        })
        .collect::<Result<_, _>>()?;
    if retained_roots.len() < tics || oracle.tics.len() != tics {
        return Err(format!(
            "oracle lengths: retained {} computed {}",
            retained_roots.len(),
            oracle.tics.len()
        )
        .into());
    }
    if let Some(i) = (0..tics).find(|&i| retained_roots[i] != hex(&oracle.tics[i].state_root)) {
        return Err(format!(
            "in-process oracle differs from the retained gate-(a) oracle at seq {i}: retained {} computed {}",
            retained_roots[i],
            hex(&oracle.tics[i].state_root)
        )
        .into());
    }

    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(base::NORMAL_LIMIT as u64);
    bank.add_program("doom_sbf", base::PROGRAM, None);
    bank.add_account(
        keys.context.pubkey(),
        program_account(vec![0; resource_sbf::CONTEXT_BYTES]),
    );
    bank.add_account(RESOURCE, program_account(capture_data.resource.clone()));
    for (ordinal, bytes) in capture_data.wad.iter().enumerate() {
        bank.add_account(wad_key(ordinal), program_account(bytes.clone()));
    }
    for (key, len) in [
        (keys.state.pubkey(), resource_sbf::STATE_MAX_BYTES),
        (keys.mirror.pubkey(), resource_sbf::MIRROR_BYTES),
        (keys.controller.pubkey(), resource_sbf::CONTROLLER_BYTES),
        (keys.decoy_state.pubkey(), resource_sbf::STATE_MAX_BYTES),
    ] {
        bank.add_account(key, program_account(vec![0; len]));
    }
    bank.add_account(
        FRAME,
        program_account(vec![0; resource_sbf::RI2_FRAME_BYTES]),
    );
    bank.add_account(ZERO_BUFFER, program_account(vec![0; sim2::BUFFER_BYTES]));
    bank.add_account(DECOY_META, program_account(vec![0; sim2::META_BYTES]));
    bank.add_account(keys.authority.pubkey(), system_account(100_000_000_000));
    bank.add_account(keys.attacker.pubkey(), system_account(100_000_000_000));
    let ctx = bank.start_with_context().await;
    let mut gate = Gate {
        ctx,
        keys,
        nonce: 0,
        profile,
    };
    let mut controls = serde_json::Map::new();
    let authority = gate.keys.authority.insecure_clone();
    let attacker = gate.keys.attacker.insecure_clone();
    let context_kp = gate.keys.context.insecure_clone();

    // --- 1. DOOM-STG/1 --------------------------------------------------------
    let state_kp = gate.keys.state.insecure_clone();
    let mirror_kp = gate.keys.mirror.insecure_clone();
    let controller_kp = gate.keys.controller.insecure_clone();
    let decoy_kp = gate.keys.decoy_state.insecure_clone();
    for (label, code, target, ix, signers) in [
        (
            "stage_unsigned_target",
            stage::REFUSE_ACCOUNT,
            controller_kp.pubkey(),
            Gate::stage_ix(controller_kp.pubkey(), false, 0, &controller),
            vec![],
        ),
        (
            "stage_wrong_magic",
            stage::REFUSE_MAGIC,
            controller_kp.pubkey(),
            Gate::stage_ix(controller_kp.pubkey(), true, 0, b"DOOM-XX2\0\0\0\0\0\0\0\0"),
            vec![&controller_kp],
        ),
        (
            "stage_misaligned_header",
            stage::REFUSE_MAGIC,
            controller_kp.pubkey(),
            Gate::stage_ix(controller_kp.pubkey(), true, 3, &controller[3..16]),
            vec![&controller_kp],
        ),
        (
            "stage_range",
            stage::REFUSE_RANGE,
            controller_kp.pubkey(),
            Gate::stage_ix(controller_kp.pubkey(), true, 60, &controller[..16]),
            vec![&controller_kp],
        ),
        (
            "stage_context_not_stageable",
            stage::REFUSE_ACCOUNT,
            context_kp.pubkey(),
            Gate::stage_ix(context_kp.pubkey(), true, 100, &[1; 16]),
            vec![&context_kp],
        ),
    ] {
        let before = gate.data(target).await?;
        let result = gate.send(vec![ix], &signers).await?;
        if custom_code(&result) != Some(code) || gate.data(target).await? != before {
            return Err(format!("{label}: {:?}", result.result).into());
        }
        controls.insert(
            label.into(),
            serde_json::json!({"refused":true,"code":code,"target_preserved":true}),
        );
    }
    let mut stage_txs = 0;
    stage_txs += gate.stage_account(&state_kp, &staged_state).await?;
    stage_txs += gate.stage_account(&mirror_kp, &capture_data.mirror).await?;
    stage_txs += gate.stage_account(&controller_kp, &controller).await?;
    stage_txs += gate.stage_account(&decoy_kp, &staged_state).await?;
    let before = gate.data(state_kp.pubkey()).await?;
    let result = gate
        .send(
            vec![Gate::stage_ix(state_kp.pubkey(), true, 1000, &[7; 8])],
            &[&state_kp],
        )
        .await?;
    if custom_code(&result) != Some(stage::REFUSE_SEALED)
        || gate.data(state_kp.pubkey()).await? != before
    {
        return Err(format!("stage_sealed: {:?}", result.result).into());
    }
    controls.insert(
        "stage_sealed_before_reconstruction".into(),
        serde_json::json!({"refused":true,"code":stage::REFUSE_SEALED}),
    );

    // --- 2. E2R/5 reconstruction ---------------------------------------------
    let continuation = Keypair::new();
    base::create_continuation(&mut gate.ctx.banks_client, &gate.ctx.payer, &continuation).await?;
    let state_len = capture_data.state.len();
    let ix = gate.e2r_ix(
        authority.pubkey(),
        continuation.pubkey(),
        false,
        continuation_split_sbf::PREPARE,
        0,
        state_len,
    );
    refused(
        &mut gate,
        &mut controls,
        "prepare_without_context_signer",
        continuation_split_sbf::REFUSE_PREPARE_CONTEXT_SIGNER,
        vec![ix],
        &[&authority],
    )
    .await?;
    let mut phases = Vec::new();
    for cursor in 0..=7u32 {
        let (operation, signs) = if cursor == 0 {
            (continuation_split_sbf::PREPARE, true)
        } else {
            (continuation_split_sbf::STEP, false)
        };
        let ix = gate.e2r_ix(
            authority.pubkey(),
            continuation.pubkey(),
            signs,
            operation,
            cursor,
            state_len,
        );
        let signers: Vec<&Keypair> = if signs {
            vec![&authority, &context_kp]
        } else {
            vec![&authority]
        };
        let result = gate.send(vec![ix], &signers).await?;
        if result.result.is_err() {
            return Err(format!("E2R/5 cursor {cursor}: {:?}", result.result).into());
        }
        phases.push(base::units(&result)?);
        if cursor == 1 {
            // Mid-reconstruction: a second PREPARE (owner, context signs,
            // fresh continuation) must refuse; the checkpoint exists.
            let second = Keypair::new();
            base::create_continuation(&mut gate.ctx.banks_client, &gate.ctx.payer, &second).await?;
            let ix = gate.e2r_ix(
                authority.pubkey(),
                second.pubkey(),
                true,
                continuation_split_sbf::PREPARE,
                0,
                state_len,
            );
            refused(
                &mut gate,
                &mut controls,
                "reprepare_mid_reconstruction",
                continuation_split_sbf::REFUSE_PREPARE_CHECKPOINT_EXISTS,
                vec![ix],
                &[&authority, &context_kp],
            )
            .await?;
        }
    }
    if gate.data(state_kp.pubkey()).await? != staged_state
        || gate.data(mirror_kp.pubkey()).await? != capture_data.mirror
    {
        return Err("E2R/5 publication changed the staged ST1/MP1".into());
    }
    let context_bytes = gate.data(context_kp.pubkey()).await?;
    let checkpoint = context_bytes[continuation_split_sbf::CONTEXT_CHECKPOINT_OFFSET..].to_vec();
    fs::write(receipt.join("published-checkpoint.bin"), &checkpoint)?;
    drop(context_bytes);
    let fresh = Keypair::new();
    base::create_continuation(&mut gate.ctx.banks_client, &gate.ctx.payer, &fresh).await?;
    let ix = gate.e2r_ix(
        attacker.pubkey(),
        fresh.pubkey(),
        false,
        continuation_split_sbf::PREPARE,
        0,
        state_len,
    );
    refused(
        &mut gate,
        &mut controls,
        "outsider_reprepare_published",
        continuation_split_sbf::REFUSE_PREPARE_CONTEXT_SIGNER,
        vec![ix],
        &[&attacker],
    )
    .await?;
    let ix = gate.e2r_ix(
        authority.pubkey(),
        fresh.pubkey(),
        true,
        continuation_split_sbf::PREPARE,
        0,
        state_len,
    );
    refused(
        &mut gate,
        &mut controls,
        "owner_reprepare_published",
        continuation_split_sbf::REFUSE_PREPARE_CHECKPOINT_EXISTS,
        vec![ix],
        &[&authority, &context_kp],
    )
    .await?;

    // --- 3. SIM/2 INIT --------------------------------------------------------
    let buffer_kp = gate.keys.buffer.insecure_clone();
    let buffer_capacity = sim2::DEFAULT_BUFFER_CAPACITY as u32;
    let buffer_total = sim2::buffer_bytes(buffer_capacity as usize).ok_or("buffer capacity")?;
    let mut buffer_len =
        sim2::initial_buffer_bytes(buffer_capacity as usize).ok_or("initial buffer capacity")?;
    let buffer_rent = solana_program::rent::Rent::default().minimum_balance(buffer_total);
    {
        let blockhash = gate.ctx.banks_client.get_latest_blockhash().await?;
        let tx = Transaction::new(
            &[&gate.ctx.payer, &buffer_kp],
            Message::new_with_blockhash(
                &[system_instruction::create_account(
                    &gate.ctx.payer.pubkey(),
                    &buffer_kp.pubkey(),
                    buffer_rent,
                    buffer_len as u64,
                    &base::PROGRAM,
                )],
                Some(&gate.ctx.payer.pubkey()),
                &blockhash,
            ),
            blockhash,
        );
        gate.ctx.banks_client.process_transaction(tx).await?;
    }
    let invalid_grow =
        gate.grow_buffer_ix(buffer_kp.pubkey(), buffer_capacity, buffer_len as u32 - 1);
    let invalid_grow_result = gate.send(vec![invalid_grow], &[&buffer_kp]).await?;
    if custom_code(&invalid_grow_result) != Some(sim2::REFUSE_BUFFER_GROWTH)
        || gate.data(buffer_kp.pubkey()).await?.len() != buffer_len
    {
        return Err(format!(
            "wrong-size SIM2 GROW-BUFFER was not refused atomically: {:?}",
            invalid_grow_result.result
        )
        .into());
    }
    let invalid_grow_units = base::units(&invalid_grow_result)?;
    let mut buffer_grow_units = Vec::new();
    while buffer_len < buffer_total {
        let ix = gate.grow_buffer_ix(buffer_kp.pubkey(), buffer_capacity, buffer_len as u32);
        let grown = gate.send(vec![ix], &[&buffer_kp]).await?;
        if grown.result.is_err() {
            return Err(format!("SIM2 GROW-BUFFER at {buffer_len}: {:?}", grown.result).into());
        }
        buffer_grow_units.push(base::units(&grown)?);
        buffer_len = sim2::next_buffer_len(buffer_len, buffer_capacity as usize)
            .ok_or("buffer growth stalled")?;
    }
    if gate.data(buffer_kp.pubkey()).await?.len() != buffer_total {
        return Err("SIM2 input buffer did not reach its declared length".into());
    }
    let mut e2r_keys = [Pubkey::default(); sim2::INIT_KEY_COUNT];
    e2r_keys[0] = RESOURCE;
    for ordinal in 0..5 {
        e2r_keys[1 + ordinal] = wad_key(ordinal);
    }
    e2r_keys[6] = controller_kp.pubkey();
    e2r_keys[7] = FRAME;
    e2r_keys[8] = continuation.pubkey();
    let init = sim2::InitRequest {
        bump: gate.keys.bump,
        capacity: buffer_capacity,
        e2r_keys,
        resource_hash: solana_program::hash::hash(&capture_data.resource).to_bytes(),
        controller_hash: solana_program::hash::hash(&controller).to_bytes(),
        frame_hash: solana_program::hash::hash(&vec![0u8; resource_sbf::RI2_FRAME_BYTES])
            .to_bytes(),
    };
    let ix = gate.init_ix(attacker.pubkey(), meta_key, &init);
    refused(
        &mut gate,
        &mut controls,
        "init_hijack_attacker_signer",
        sim2::REFUSE_E2R_BINDING,
        vec![ix],
        &[&attacker],
    )
    .await?;
    let not_pda = Pubkey::new_from_array([0xb3; 32]);
    let ix = gate.init_ix(authority.pubkey(), not_pda, &init);
    refused(
        &mut gate,
        &mut controls,
        "init_non_pda_meta",
        sim2::REFUSE_BINDING,
        vec![ix],
        &[&authority],
    )
    .await?;
    let (noncanonical_meta, noncanonical_bump) = (0..gate.keys.bump)
        .find_map(|candidate| {
            Pubkey::create_program_address(
                &[sim2::META_SEED, context_kp.pubkey().as_ref(), &[candidate]],
                &base::PROGRAM,
            )
            .ok()
            .map(|key| (key, candidate))
        })
        .ok_or("no valid non-canonical PDA bump")?;
    let mut noncanonical = init.clone();
    noncanonical.bump = noncanonical_bump;
    let ix = gate.init_ix_with_buffer(
        authority.pubkey(),
        noncanonical_meta,
        ZERO_BUFFER,
        &noncanonical,
    );
    refused(
        &mut gate,
        &mut controls,
        "init_noncanonical_bump",
        sim2::REFUSE_BINDING,
        vec![ix],
        &[&authority],
    )
    .await?;
    let mut swapped = init.clone();
    swapped.e2r_keys.swap(6, 7);
    let ix = gate.init_ix(authority.pubkey(), meta_key, &swapped);
    refused(
        &mut gate,
        &mut controls,
        "init_wrong_e2r_keys",
        sim2::REFUSE_E2R_BINDING,
        vec![ix],
        &[&authority],
    )
    .await?;
    let mut bad_hash = init.clone();
    bad_hash.resource_hash[0] ^= 1;
    let ix = gate.init_ix(authority.pubkey(), meta_key, &bad_hash);
    refused(
        &mut gate,
        &mut controls,
        "init_wrong_component_hash",
        sim2::REFUSE_E2R_BINDING,
        vec![ix],
        &[&authority],
    )
    .await?;
    // Pre-fund the PDA (anyone can): INIT must still bind it.
    let prefund = system_instruction::transfer(&gate.ctx.payer.pubkey(), &meta_key, 1_000_000);
    let result = gate.send(vec![prefund], &[]).await?;
    if result.result.is_err() {
        return Err(format!("prefund {:?}", result.result).into());
    }
    let ix = gate.init_ix(authority.pubkey(), meta_key, &init);
    let init_result = gate.send(vec![ix], &[&authority]).await?;
    if init_result.result.is_err() {
        return Err(format!("SIM2 INIT {:?}", init_result.result).into());
    }
    let init_cu = base::units(&init_result)?;
    let meta_bytes = gate.data(meta_key).await?;
    let view = sim2::MetaView::parse(&meta_bytes).ok_or("meta parse")?;
    if view.cursor != 0 || view.state_root != oracle.initial_root || view.halt != 0 {
        return Err("INIT meta does not commit the published state".into());
    }
    controls.insert(
        "init_prefunded_pda".into(),
        serde_json::json!({"ok":true,"cu":init_cu,"prefund_lamports":1_000_000}),
    );
    controls.insert(
        "buffer_growth".into(),
        serde_json::json!({
            "capacity": buffer_capacity,
            "bytes": buffer_total,
            "initial_bytes": sim2::initial_buffer_bytes(buffer_capacity as usize),
            "grow_instructions": buffer_grow_units.len(),
            "grow_cu_max": buffer_grow_units.iter().max(),
            "grow_cu_total": buffer_grow_units.iter().sum::<u64>(),
            "wrong_size_refusal_code": sim2::REFUSE_BUFFER_GROWTH,
            "wrong_size_refusal_cu": invalid_grow_units,
            "rent_lamports": buffer_rent,
        }),
    );
    let page_first = buffer_capacity - (sim2::MAX_DEPOSIT_SLOTS as u32 + 1);
    let page_commands = commands[..4].repeat(sim2::MAX_DEPOSIT_SLOTS);
    let page_deposit = gate.deposit_ix(
        buffer_kp.pubkey(),
        authority.pubkey(),
        page_first,
        &page_commands,
    );
    let page_result = gate.send(vec![page_deposit], &[&authority]).await?;
    if page_result.result.is_err() {
        return Err(format!("64-slot DEPOSIT near buffer end {:?}", page_result.result).into());
    }
    controls.insert(
        "deposit_max_page_near_end".into(),
        serde_json::json!({
            "first_seq": page_first,
            "count": sim2::MAX_DEPOSIT_SLOTS,
            "cu": base::units(&page_result)?,
            "readback_equal": gate.data(buffer_kp.pubkey()).await?
                [sim2::BUFFER_HEADER_BYTES + page_first as usize * sim2::SLOT_BYTES
                    ..sim2::BUFFER_HEADER_BYTES + (page_first as usize + sim2::MAX_DEPOSIT_SLOTS) * sim2::SLOT_BYTES]
                .chunks_exact(sim2::SLOT_BYTES)
                .all(|slot| slot[..4] == commands[..4] && slot[4] == sim2::SLOT_WRITTEN),
        }),
    );
    let far_seq = buffer_capacity - 1;
    let far_deposit = gate.deposit_ix(
        buffer_kp.pubkey(),
        authority.pubkey(),
        far_seq,
        &commands[..4],
    );
    let far_result = gate.send(vec![far_deposit], &[&authority]).await?;
    if far_result.result.is_err() {
        return Err(format!("DEPOSIT at final input slot {:?}", far_result.result).into());
    }
    let far_buffer = gate.data(buffer_kp.pubkey()).await?;
    let far_offset = sim2::BUFFER_HEADER_BYTES + far_seq as usize * sim2::SLOT_BYTES;
    if far_buffer[far_offset..far_offset + 4] != commands[..4]
        || far_buffer[far_offset + 4] != sim2::SLOT_WRITTEN
    {
        return Err("far-end input slot did not persist".into());
    }
    controls.insert(
        "deposit_far_end".into(),
        serde_json::json!({"seq":far_seq,"cu":base::units(&far_result)?,"readback_equal":true}),
    );
    let ix = gate.init_ix(authority.pubkey(), meta_key, &init);
    refused(
        &mut gate,
        &mut controls,
        "init_again",
        sim2::REFUSE_ALREADY_INIT,
        vec![ix],
        &[&authority],
    )
    .await?;
    let ix = gate.init_ix(attacker.pubkey(), meta_key, &init);
    refused(
        &mut gate,
        &mut controls,
        "init_again_attacker",
        sim2::REFUSE_ALREADY_INIT,
        vec![ix],
        &[&attacker],
    )
    .await?;

    // --- controls before the first tic -----------------------------------------
    let result = gate.advance(0).await?;
    if result.result.is_err() || gate.data(meta_key).await? != meta_bytes {
        return Err(format!("initial sweep-with-nothing {:?}", result.result).into());
    }
    controls.insert(
        "sweep_with_nothing_initial".into(),
        serde_json::json!({"ok":true,"cu":base::units(&result)?}),
    );
    let buffer_key = buffer_kp.pubkey();
    let ix = gate.advance_ix(
        state_kp.pubkey(),
        meta_key,
        buffer_key,
        attacker.pubkey(),
        0,
    );
    refused(
        &mut gate,
        &mut controls,
        "wrong_authority_sweep_initial",
        sim2::REFUSE_AUTHORITY,
        vec![ix],
        &[&attacker],
    )
    .await?;
    let ix = gate.deposit_ix(buffer_key, attacker.pubkey(), 0, &commands[..4]);
    refused(
        &mut gate,
        &mut controls,
        "wrong_authority_deposit_initial",
        sim2::REFUSE_AUTHORITY,
        vec![ix],
        &[&attacker],
    )
    .await?;
    let ix = gate.deposit_ix(buffer_key, authority.pubkey(), 0, &[0, 0, 0, 0x82]);
    refused(
        &mut gate,
        &mut controls,
        "special_button_savegame",
        sim2::REFUSE_COMMAND,
        vec![ix],
        &[&authority],
    )
    .await?;
    let ix = gate.deposit_ix(
        buffer_key,
        authority.pubkey(),
        sim2::BUFFER_CAPACITY as u32,
        &commands[..4],
    );
    refused(
        &mut gate,
        &mut controls,
        "slot_beyond_capacity",
        sim2::REFUSE_SLOT_RANGE,
        vec![ix],
        &[&authority],
    )
    .await?;
    let mut bad = gate.deposit_ix(buffer_key, authority.pubkey(), 0, &commands[..4]);
    bad.data[20] = 0;
    refused(
        &mut gate,
        &mut controls,
        "malformed_deposit",
        sim2::REFUSE_MALFORMED,
        vec![bad],
        &[&authority],
    )
    .await?;

    // --- 4. scrambled deposits and sweeps ----------------------------------------
    let mut rng = Rng(seed);
    let runs = common::scrambled_runs(&mut rng, tics as u32, 4, &HOLD);
    let mut until_sweep = 1 + rng.below(12);
    let mut sweeps = fs::File::create(receipt.join("sweeps.jsonl"))?;
    let mut sweep_units = Vec::new();
    let mut tics_per_sweep = Vec::new();
    let mut noop_units = Vec::new();
    let mut deposit_units = Vec::new();
    let mut profiles = Vec::new();
    let mut last_cursor = 0u32;
    let mut sweep_count = 0usize;

    async fn run_sweep(
        gate: &mut Gate,
        oracle: &common::Oracle,
        sweeps: &mut fs::File,
        max_tics: u32,
    ) -> Result<(BanksTransactionResultWithMetadata, sim2::MetaView), Box<dyn Error>> {
        let result = gate.advance(max_tics).await?;
        if result.result.is_err() {
            return Err(format!("ADVANCE failed {:?}", result.result).into());
        }
        let meta_key = gate.keys.meta;
        let state_key = gate.keys.state.pubkey();
        let mirror_key = gate.keys.mirror.pubkey();
        let view = sim2::MetaView::parse(&gate.data(meta_key).await?).ok_or("meta parse")?;
        let report = sweep_receipt(&result).ok_or("missing SIM2-ADV receipt")?;
        let equal = if view.cursor == 0 {
            true
        } else {
            let tic = &oracle.tics[view.cursor as usize - 1];
            gate.data(state_key).await? == tic.state
                && gate.data(mirror_key).await? == tic.mirror
                && view.state_root == tic.state_root
                && view.input_chain == tic.chain
        };
        writeln!(
            sweeps,
            "{}",
            serde_json::json!({
                "cu": base::units(&result)?,
                "before": u32::from_le_bytes(report[0..4].try_into().unwrap()),
                "after": view.cursor,
                "ran": u32::from_le_bytes(report[8..12].try_into().unwrap()),
                "stop": report[12],
                "max_tics": max_tics,
                "oracle_equal": equal,
                "markers": if gate.profile { serde_json::json!(markers(&result)) } else { serde_json::Value::Null },
            })
        )?;
        if !equal {
            return Err(format!(
                "SBF state diverged from the oracle at cursor {}",
                view.cursor
            )
            .into());
        }
        Ok((result, view))
    }

    let mut record = |result: &BanksTransactionResultWithMetadata,
                      view: &sim2::MetaView,
                      last: &mut u32|
     -> Result<(), Box<dyn Error>> {
        let cu = base::units(result)?;
        let ran = view.cursor - *last;
        if ran > 0 {
            sweep_units.push(cu);
            tics_per_sweep.push(ran);
        } else {
            noop_units.push(cu);
        }
        if profile {
            profiles.push((ran, markers(result)));
        }
        *last = view.cursor;
        Ok(())
    };

    for (first, len) in &runs {
        let result = gate
            .deposit(
                *first,
                &commands[*first as usize * 4..(*first + *len) as usize * 4],
            )
            .await?;
        if result.result.is_err() {
            return Err(format!("deposit {first}+{len}: {:?}", result.result).into());
        }
        deposit_units.push(base::units(&result)?);
        until_sweep -= 1;
        if until_sweep > 0 {
            continue;
        }
        until_sweep = 1 + rng.below(12);
        let (result, view) = run_sweep(&mut gate, &oracle, &mut sweeps, 0).await?;
        sweep_count += 1;
        record(&result, &view, &mut last_cursor)?;
    }
    loop {
        let (result, view) = run_sweep(&mut gate, &oracle, &mut sweeps, 0).await?;
        sweep_count += 1;
        let progressed = view.cursor > last_cursor;
        record(&result, &view, &mut last_cursor)?;
        if !progressed {
            break;
        }
    }
    if last_cursor != HOLD[0] {
        return Err(format!("expected to stall at {}, cursor {last_cursor}", HOLD[0]).into());
    }

    // Exercise a future cursor arriving before its predecessor.  The first
    // transaction must be refused atomically.  Once the current cursor is
    // advanced, a fresh transaction for the same expected cursor succeeds.
    let c = |seq: u32| commands[seq as usize * 4..seq as usize * 4 + 4].to_vec();
    let future_cursor = HOLD[0] + 1;
    let future = gate.advance_expected_ix(
        state_kp.pubkey(),
        meta_key,
        buffer_key,
        authority.pubkey(),
        future_cursor,
        1,
    );
    refused(
        &mut gate,
        &mut controls,
        "advance_out_of_order_cursor",
        sim2::REFUSE_CURSOR_MISMATCH,
        vec![future],
        &[&authority],
    )
    .await?;

    let predecessor = gate.deposit(HOLD[0], &c(HOLD[0])).await?;
    if predecessor.result.is_err() {
        return Err(format!("deposit held predecessor {:?}", predecessor.result).into());
    }
    let predecessor_sweep = gate.advance_expected(HOLD[0], 1).await?;
    if predecessor_sweep.result.is_err() {
        return Err(format!(
            "expected-cursor predecessor sweep {:?}",
            predecessor_sweep.result
        )
        .into());
    }
    let predecessor_view =
        sim2::MetaView::parse(&gate.data(meta_key).await?).ok_or("meta after predecessor")?;
    let predecessor_oracle = &oracle.tics[predecessor_view.cursor as usize - 1];
    if predecessor_view.cursor != future_cursor
        || gate.data(state_kp.pubkey()).await? != predecessor_oracle.state
        || gate.data(gate.keys.mirror.pubkey()).await? != predecessor_oracle.mirror
        || predecessor_view.state_root != predecessor_oracle.state_root
        || predecessor_view.input_chain != predecessor_oracle.chain
    {
        return Err("cursor-bound predecessor sweep differs from the oracle".into());
    }
    controls.insert(
        "advance_expected_cursor_predecessor".into(),
        serde_json::json!({"cursor":predecessor_view.cursor,"cu":base::units(&predecessor_sweep)?,"oracle_equal":true}),
    );

    let resent = gate.advance_expected(future_cursor, 1).await?;
    if resent.result.is_err() {
        return Err(format!("re-sent expected-cursor sweep {:?}", resent.result).into());
    }
    let resent_view =
        sim2::MetaView::parse(&gate.data(meta_key).await?).ok_or("meta after re-send")?;
    let resent_oracle = &oracle.tics[resent_view.cursor as usize - 1];
    if resent_view.cursor != future_cursor + 1
        || gate.data(state_kp.pubkey()).await? != resent_oracle.state
        || gate.data(gate.keys.mirror.pubkey()).await? != resent_oracle.mirror
        || resent_view.state_root != resent_oracle.state_root
        || resent_view.input_chain != resent_oracle.chain
    {
        return Err("re-sent cursor-bound sweep differs from the oracle".into());
    }
    controls.insert(
        "advance_refusal_resend".into(),
        serde_json::json!({"refusal_code":sim2::REFUSE_CURSOR_MISMATCH,"cursor":resent_view.cursor,"cu":base::units(&resent)?,"fresh_transaction":true,"oracle_equal":true}),
    );
    last_cursor = resent_view.cursor;

    // Continue to the later held slot, preserving a second gap for the
    // existing no-op, duplicate and malformed-input controls below.
    loop {
        let (result, view) = run_sweep(&mut gate, &oracle, &mut sweeps, 0).await?;
        sweep_count += 1;
        let progressed = view.cursor > last_cursor;
        record(&result, &view, &mut last_cursor)?;
        if !progressed {
            break;
        }
    }
    if last_cursor != HOLD[1] {
        return Err(format!("expected to stall at {}, cursor {last_cursor}", HOLD[1]).into());
    }

    // --- mid-run controls (stalled at the first held slot) ----------------------
    let before = gate.durable().await?;
    let gap = gate.advance(0).await?;
    if gap.result.is_err() || gate.durable().await? != before {
        return Err(format!("sweep-during-gap {:?}", gap.result).into());
    }
    controls.insert("sweep_during_gap".into(), serde_json::json!({"ok":true,"cursor":HOLD[1],"cu":base::units(&gap)?,"durable_preserved":true}));
    let ix = gate.deposit_ix(buffer_key, authority.pubkey(), 5, &c(5));
    refused(
        &mut gate,
        &mut controls,
        "duplicate_slot_consumed",
        sim2::REFUSE_DUPLICATE_SLOT,
        vec![ix],
        &[&authority],
    )
    .await?;
    let ix = gate.deposit_ix(buffer_key, authority.pubkey(), HOLD[1] + 1, &c(HOLD[1] + 1));
    refused(
        &mut gate,
        &mut controls,
        "duplicate_slot_staged",
        sim2::REFUSE_DUPLICATE_SLOT,
        vec![ix],
        &[&authority],
    )
    .await?;
    let ix = gate.deposit_ix(buffer_key, attacker.pubkey(), HOLD[1], &c(HOLD[1]));
    refused(
        &mut gate,
        &mut controls,
        "wrong_authority_fills_gap",
        sim2::REFUSE_AUTHORITY,
        vec![ix],
        &[&attacker],
    )
    .await?;
    let ix = gate.advance_ix(
        state_kp.pubkey(),
        meta_key,
        buffer_key,
        attacker.pubkey(),
        0,
    );
    refused(
        &mut gate,
        &mut controls,
        "wrong_authority_sweep_midrun",
        sim2::REFUSE_AUTHORITY,
        vec![ix],
        &[&attacker],
    )
    .await?;
    let mut bad = gate.advance_ix(
        state_kp.pubkey(),
        meta_key,
        buffer_key,
        authority.pubkey(),
        0,
    );
    bad.data[21] = 1;
    refused(
        &mut gate,
        &mut controls,
        "malformed_advance",
        sim2::REFUSE_MALFORMED,
        vec![bad],
        &[&authority],
    )
    .await?;
    // Substitutions, all with a staged slot at the cursor so the sweep would
    // otherwise run: stage the held slot, attempt, and confirm the refusal
    // left the committed state alone (the slot write itself is the one
    // durable change and is part of the schedule).
    let result = gate.deposit(HOLD[1], &c(HOLD[1])).await?;
    if result.result.is_err() {
        return Err(format!("late deposit {:?}", result.result).into());
    }
    let ix = gate.advance_ix(
        decoy_kp.pubkey(),
        meta_key,
        buffer_key,
        authority.pubkey(),
        0,
    );
    refused(
        &mut gate,
        &mut controls,
        "substituted_state_account",
        sim2::REFUSE_BINDING,
        vec![ix],
        &[&authority],
    )
    .await?;
    let ix = gate.advance_ix(
        state_kp.pubkey(),
        meta_key,
        ZERO_BUFFER,
        authority.pubkey(),
        0,
    );
    refused(
        &mut gate,
        &mut controls,
        "substituted_unbound_buffer",
        sim2::REFUSE_BINDING,
        vec![ix],
        &[&authority],
    )
    .await?;
    let ix = gate.advance_ix(
        state_kp.pubkey(),
        DECOY_META,
        buffer_key,
        authority.pubkey(),
        0,
    );
    refused(
        &mut gate,
        &mut controls,
        "substituted_meta",
        sim2::REFUSE_BINDING,
        vec![ix],
        &[&authority],
    )
    .await?;
    let ix = gate.init_ix(attacker.pubkey(), meta_key, &init);
    refused(
        &mut gate,
        &mut controls,
        "init_hijack_midrun",
        sim2::REFUSE_ALREADY_INIT,
        vec![ix],
        &[&attacker],
    )
    .await?;
    let ix = gate.e2r_ix(
        attacker.pubkey(),
        fresh.pubkey(),
        false,
        continuation_split_sbf::PREPARE,
        0,
        state_len,
    );
    refused(
        &mut gate,
        &mut controls,
        "outsider_reprepare_midrun",
        continuation_split_sbf::REFUSE_PREPARE_CONTEXT_SIGNER,
        vec![ix],
        &[&attacker],
    )
    .await?;
    let ix = gate.e2r_ix(
        authority.pubkey(),
        fresh.pubkey(),
        true,
        continuation_split_sbf::PREPARE,
        0,
        state_len,
    );
    refused(
        &mut gate,
        &mut controls,
        "owner_reprepare_midrun",
        continuation_split_sbf::REFUSE_PREPARE_CHECKPOINT_EXISTS,
        vec![ix],
        &[&authority, &context_kp],
    )
    .await?;
    let ix = Gate::stage_ix(state_kp.pubkey(), true, 1000, &[7; 8]);
    refused(
        &mut gate,
        &mut controls,
        "stage_live_state_sealed",
        stage::REFUSE_SEALED,
        vec![ix],
        &[&state_kp],
    )
    .await?;
    // Atomic rollback: a real sweep followed by a failing instruction in the
    // same transaction leaves every durable account unchanged.
    let sweep_ix = gate.advance_ix(
        state_kp.pubkey(),
        meta_key,
        buffer_key,
        authority.pubkey(),
        0,
    );
    let mut failing = gate.deposit_ix(buffer_key, authority.pubkey(), 0, &c(0));
    failing.data[20] = 0;
    let before = gate.durable().await?;
    let result = gate.send(vec![sweep_ix, failing], &[&authority]).await?;
    if custom_code(&result) != Some(sim2::REFUSE_MALFORMED) || gate.durable().await? != before {
        return Err(format!("rollback control {:?}", result.result).into());
    }
    controls.insert(
        "rollback_sweep_then_failing_instruction".into(),
        serde_json::json!({"refused":true,"code":sim2::REFUSE_MALFORMED,"durable_preserved":true}),
    );
    let view = sim2::MetaView::parse(&gate.data(meta_key).await?).unwrap();
    let ok = gate.snapshot_ix(view.cursor, &view.state_root);
    let result = gate.send(vec![ok], &[]).await?;
    if result.result.is_err() {
        return Err(format!("snapshot at committed cursor {:?}", result.result).into());
    }
    controls.insert(
        "snapshot_committed_cursor".into(),
        serde_json::json!({"ok":true,"cursor":view.cursor,"cu":base::units(&result)?}),
    );
    let stale = gate.snapshot_ix(view.cursor - 1, &view.state_root);
    refused(
        &mut gate,
        &mut controls,
        "snapshot_stale_cursor",
        sim2::REFUSE_SNAPSHOT,
        vec![stale],
        &[],
    )
    .await?;
    let wrong = gate.snapshot_ix(view.cursor, &oracle.tics[0].state_root);
    refused(
        &mut gate,
        &mut controls,
        "snapshot_wrong_root",
        sim2::REFUSE_SNAPSHOT,
        vec![wrong],
        &[],
    )
    .await?;

    // --- late arrivals; the long staged run measures budget-bound sweeps ------------
    let mut late = Vec::new();
    for (index, &held) in HOLD.iter().enumerate() {
        if index > 0 {
            let result = gate.deposit(held, &c(held)).await?;
            if result.result.is_err() {
                return Err(format!("late deposit {held}: {:?}", result.result).into());
            }
        }
        let target = HOLD.get(index + 1).copied().unwrap_or(tics as u32);
        let mut first_after = None;
        loop {
            let (result, view) = run_sweep(&mut gate, &oracle, &mut sweeps, 0).await?;
            sweep_count += 1;
            let progressed = view.cursor > last_cursor;
            first_after.get_or_insert(view.cursor);
            record(&result, &view, &mut last_cursor)?;
            if !progressed || view.cursor >= target {
                break;
            }
        }
        late.push(
            serde_json::json!({"seq":held,"stalled_cursor":held,"first_sweep_to":first_after}),
        );
        if last_cursor != target {
            return Err(
                format!("late arrival {held}: cursor {last_cursor}, expected {target}").into(),
            );
        }
    }
    controls.insert("late_arrival".into(), serde_json::Value::Array(late));
    let before = gate.durable().await?;
    let end = gate.advance(0).await?;
    if end.result.is_err() || gate.durable().await? != before {
        return Err(format!("final sweep-with-nothing {:?}", end.result).into());
    }
    controls.insert(
        "sweep_with_nothing_final".into(),
        serde_json::json!({"ok":true,"cu":base::units(&end)?}),
    );
    let ix = gate.deposit_ix(
        buffer_key,
        authority.pubkey(),
        tics as u32 - 1,
        &c(tics as u32 - 1),
    );
    refused(
        &mut gate,
        &mut controls,
        "duplicate_slot_final",
        sim2::REFUSE_DUPLICATE_SLOT,
        vec![ix],
        &[&authority],
    )
    .await?;
    let ix = gate.advance_ix(
        state_kp.pubkey(),
        meta_key,
        buffer_key,
        attacker.pubkey(),
        0,
    );
    refused(
        &mut gate,
        &mut controls,
        "wrong_authority_sweep_final",
        sim2::REFUSE_AUTHORITY,
        vec![ix],
        &[&attacker],
    )
    .await?;

    // --- final equality ----------------------------------------------------------
    let final_meta = gate.data(meta_key).await?;
    let view = sim2::MetaView::parse(&final_meta).unwrap();
    let last = oracle.tics.last().unwrap();
    let final_state = gate.data(state_kp.pubkey()).await?;
    let final_mirror = gate.data(mirror_kp.pubkey()).await?;
    if view.cursor != tics as u32
        || final_state != last.state
        || final_mirror != last.mirror
        || view.state_root != last.state_root
        || view.input_chain != last.chain
    {
        return Err("final SBF state differs from the sequential oracle".into());
    }
    let first_grow_boundary =
        ((sim2::MAX_BUFFER_GROWTH - sim2::BUFFER_HEADER_BYTES) / sim2::SLOT_BYTES) as u32;
    let crossed_grow_boundary = fs::read_to_string(receipt.join("sweeps.jsonl"))?
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .any(|row| {
            row["before"].as_u64().unwrap_or(0) < first_grow_boundary as u64
                && row["after"].as_u64().unwrap_or(0) > first_grow_boundary as u64
                && row["oracle_equal"].as_bool() == Some(true)
        });
    if tics as u32 > first_grow_boundary && !crossed_grow_boundary {
        return Err("ADVANCE did not cross the first grown-buffer slot boundary".into());
    }
    controls.insert(
        "advance_across_grown_buffer_boundary".into(),
        serde_json::json!({"slot_boundary":first_grow_boundary,"crossed":crossed_grow_boundary,"oracle_equal":crossed_grow_boundary}),
    );
    let ok = gate.snapshot_ix(view.cursor, &view.state_root);
    let result = gate.send(vec![ok], &[]).await?;
    if result.result.is_err() {
        return Err(format!("final snapshot {:?}", result.result).into());
    }
    fs::write(receipt.join("final-state.st1"), &final_state)?;
    fs::write(receipt.join("final-mirror.mp1"), &final_mirror)?;
    fs::write(receipt.join("final-meta.bin"), &final_meta)?;
    fs::write(receipt.join("commands.bin"), &commands)?;

    // --- compute summary -------------------------------------------------------------
    let mut hist = BTreeMap::<u32, usize>::new();
    for &n in &tics_per_sweep {
        *hist.entry(n).or_default() += 1;
    }
    // Sweeps that stopped with slots still staged: on the budget (metered
    // profile) or on the tic cap (count-capped profile).
    let mut budget_bound = Vec::new();
    let mut cap_bound = Vec::new();
    let mut stops = BTreeMap::<u8, usize>::new();
    for line in fs::read_to_string(receipt.join("sweeps.jsonl"))?.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        let stop = v["stop"].as_u64().unwrap() as u8;
        *stops.entry(stop).or_default() += 1;
        let pair = (v["ran"].as_u64().unwrap(), v["cu"].as_u64().unwrap());
        if stop == sim2::Stop::Budget as u8 {
            budget_bound.push(pair);
        } else if stop == sim2::Stop::MaxTics as u8 {
            cap_bound.push(pair);
        }
    }
    let mut profile_summary = serde_json::Value::Null;
    if profile {
        let mut step_costs = Vec::new();
        let mut publish = Vec::new();
        let mut fixed_pre = Vec::new();
        let mut post = Vec::new();
        for (ran, marks) in &profiles {
            if *ran == 0 {
                continue;
            }
            let find = |phase: u64| {
                marks
                    .iter()
                    .filter(|(p, _)| *p == phase)
                    .map(|(_, r)| *r)
                    .collect::<Vec<_>>()
            };
            let starts = find(900);
            let loop_end = find(923);
            let pre = find(922);
            let pub_end = find(924);
            let meta_end = find(925);
            if let (Some(&p), Some(&e), Some(&pe), Some(&me)) = (
                pre.first(),
                loop_end.first(),
                pub_end.first(),
                meta_end.first(),
            ) {
                fixed_pre.push(base::NORMAL_LIMIT as u64 - p);
                for w in starts.windows(2) {
                    step_costs.push(w[0] - w[1]);
                }
                if let Some(&s) = starts.last() {
                    step_costs.push(s - e);
                }
                publish.push(e - pe);
                post.push(pe - me);
            }
        }
        let stats = |v: &mut Vec<u64>| -> serde_json::Value {
            v.sort();
            if v.is_empty() {
                return serde_json::Value::Null;
            }
            serde_json::json!({"n": v.len(), "min": v[0], "median": v[v.len()/2], "p99": v[(v.len()*99)/100], "max": v[v.len()-1]})
        };
        profile_summary = serde_json::json!({
            "per_tic_step_including_slot_and_chain": stats(&mut step_costs),
            "publish_fill_pool_encode_copy": stats(&mut publish),
            "post_publish_shape_root_meta": stats(&mut post),
            "pre_sweep_fixed_to_first_tic": stats(&mut fixed_pre),
        });
    }
    let max_sweep = sweep_units.iter().max().copied().unwrap_or(0);
    common::write_json(
        &receipt.join("controls.json"),
        &serde_json::Value::Object(controls),
    )?;
    common::write_json(
        &receipt.join("summary.json"),
        &serde_json::json!({
            "gate": "DOOM-SIM/2 (b) SBF ProgramTest prefer_bpf, scrambled deposits + ADVANCE sweeps vs sequential no-render oracle",
            "tics": tics,
            "e2r5_phase_units": phases,
            "stage_transactions": stage_txs,
            "init_cu": init_cu,
            "buffer_capacity": view.capacity,
            "buffer_bytes": buffer_total,
            "buffer_grow_instructions": buffer_grow_units.len(),
            "buffer_grow_cu_max": buffer_grow_units.iter().max(),
            "deposit_transactions": deposit_units.len(),
            "deposit_cu_max": deposit_units.iter().max(),
            "sweeps_sent": sweep_count,
            "running_sweeps": sweep_units.len(),
            "max_cu_per_sweep": max_sweep,
            "normal_limit": base::NORMAL_LIMIT,
            "tics_per_sweep_histogram": hist,
            "tics_per_sweep_max": tics_per_sweep.iter().max(),
            "budget_bound_sweeps_ran_cu": budget_bound,
            "cap_bound_sweeps_ran_cu": cap_bound,
            "stop_histogram_0none_1gap_2budget_3cap_4capacity_5halt_6halted": stops,
            "effective_default_max_tics": sim2::effective_max_tics(0),
            "noop_sweep_cu": {"min": noop_units.iter().min(), "max": noop_units.iter().max(), "n": noop_units.len()},
            "final_cursor": view.cursor,
            "final_state_root": hex(&view.state_root),
            "final_input_chain": hex(&view.input_chain),
            "final_st1_account_sha256": base::sha(&final_state),
            "final_mp1_sha256": base::sha(&final_mirror),
            "oracle_equal_every_running_sweep": true,
            "tic_reserve_cu": sim2::TIC_RESERVE_CU,
            "publish_reserve_cu": sim2::PUBLISH_RESERVE_CU,
            "profile": profile_summary,
        }),
    )?;
    common::write_json(
        &receipt.join("identity.json"),
        &serde_json::json!({
            "profile": "DOOM-SIM/2 transport image under ProgramTest prefer_bpf(true)",
            "image": image.display().to_string(),
            "image_sha256": base::sha(&image_bytes),
            "manifest_sha256": base::sha(&manifest_bytes),
            "manifest_source_tree": manifest_json["source_tree"],
            "diagnostic_profile_run": profile,
            "command_line": argv,
            "capture": capture.display().to_string(),
            "resource_sha256": base::sha(&capture_data.resource),
            "controller_sha256": base::sha(&controller),
            "wad_payload_sha256_recomputed": capture_data.wad_payload_sha256,
            "oracle_receipt": oracle_receipt.display().to_string(),
            "abi": {"sim2": sim2::ABI_DESCRIPTOR, "stage": stage::ABI_DESCRIPTOR, "e2r": continuation_split_sbf::ABI_DESCRIPTOR},
            "seed": format!("{seed:#x}"),
            "sources": common::driver_identity(&[
                ("driver/src/bin/sweep_gate.rs", include_bytes!("sweep_gate.rs")),
                ("driver/src/sweep_common.rs", include_bytes!("../sweep_common.rs")),
            ]),
        }),
    )?;
    println!(
        "DOOM-SIM/2 gate (b) passed: tics={tics} running_sweeps={} max_sweep_cu={max_sweep} max_tics_per_sweep={:?} receipt={}",
        sweep_units.len(),
        tics_per_sweep.iter().max(),
        receipt.display()
    );
    Ok(())
}
