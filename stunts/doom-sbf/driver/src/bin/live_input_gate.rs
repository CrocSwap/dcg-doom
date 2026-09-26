#![recursion_limit = "512"]

//! DOOM-SIM/2 gate (d): live input past the DEMO1 end, on SBF under
//! ProgramTest with `prefer_bpf(true)`.
//!
//! The captured DEMO1 stream is 5,026 tics long, so a gate that sources its
//! commands from `capture.demo_commands()` cannot run past tic 5026 at all.
//! This gate instead stages a live-input profile (`--profile`, default
//! `r8l-move`: every 200-tic segment either moves or turns) and sweeps it in
//! `--advance-tics` sized ADVANCE/5 batches, so the load-bearing claim of
//! DOOM-CI3 is measured on the image rather than argued:
//!
//! 1. The SBF commits ST1, MP1, the state root and the input chain equal to
//!    the host session at every committed cursor.  The host session is the
//!    chain's own ingress contract, source matched: the same pre-tic guard,
//!    `DoomControllerArmV2`, `G_Ticker`, `gametic++` and the DOOM-CI3
//!    receipt, driven in lockstep one batch at a time so a 5,400-tic run
//!    keeps one ST1/MP1 pair resident instead of gigabytes.
//! 2. Past the demo end the player moves: the ST1 image changes at every
//!    cursor, and the chain's own eight RENDER strips at the chosen cursors
//!    reassemble byte for byte into the host's `R_RenderPlayerView` frame.
//! 3. A deliberately starved tic is refused.  `DoomControllerConsumed()`
//!    returns zero exactly when `G_Ticker` ran with no console player in the
//!    game, and the retained capture cannot reach that state, so the control
//!    builds one: a canonical ST1 whose `playeringame[0]` byte is zero
//!    (proved canonical by a host encode/decode round trip, proved to be that
//!    byte by the receipt it produces and by the unmodified state not
//!    starving).  ADVANCE/5 refuses it with Custom 288 and commits nothing;
//!    ADVANCE/3 on the same state commits, so the refusal is the receipt
//!    rule and not the input.
//!
//! `--new-game-start` runs the same gate from the pinned E1M1 new-game start
//! state instead of the retained E1M5 demo capture: the frame cursors need not
//! be past the captured DEMO1 end (which belongs to a different level and says
//! nothing about this run), and the position baseline is the start state
//! itself.  The host lockstep oracle, the strip comparison and the controls
//! are unchanged, so the two runs are the same measurement over a different
//! committed start state.
//!
//! The reset half at the end is the reusable-slot claim: a staged start
//! account, RESET/9, and the same staged commands replayed from cursor 0,
//! landing on the same ST1, MP1, state root and input chain at every cursor
//! the first run committed.  It also carries the refusals: a stranger's
//! authority, a start account holding a different image, and a halted slot
//! without the force flag.  The restart-request half stages a canonical dead
//! player, presses use, and measures the guard's 295, with a live player's use
//! on the same state as the control.
//!
//! The loaded image must advertise `ADVANCE/5` and the receipt rule in its own
//! ABI descriptor before any work starts: a transport image without the rule
//! is refused here rather than silently measured as "no refusals".

#[path = "continuation_split_sbf.rs"]
#[allow(dead_code)]
mod base;
#[path = "../sweep_common.rs"]
#[allow(dead_code)]
mod common;

use std::{env, error::Error, fs, io::Write, path::PathBuf, time::Instant};

use common::Capture;
use doom_sbf::{
    continuation_split_sbf, render_snapshot_v3 as rsn3, resource, resource_sbf, sim2, stage,
    static_resource as rsr,
};
use serde_json::json;
use solana_account::Account;
use solana_banks_interface::BanksTransactionResultWithMetadata;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_program::rent::Rent;
use solana_program_test::{ProgramTest, ProgramTestContext};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::{instruction as system_instruction, program as system_program};
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

const RESOURCE: Pubkey = Pubkey::new_from_array([0xe1; 32]);
const FRAME: Pubkey = Pubkey::new_from_array([0xe5; 32]);
const STAGE_CHUNK: usize = 900;
const RENDER_LIMIT: u32 = 1_400_000;
const E2R_PHASES: u32 = 8;
/// The buffer the gate creates through the program's own provisioning
/// operations rather than asking the bank for it, so the rent a fourth buffer
/// costs and the `INIT-BUFFERED` / `GROW-BUFFERED` / `INIT-STRIP` wires are
/// measured rather than assumed.  It is the highest buffer inside the count,
/// because that is the one the deployed two-buffer program refused outright.
const PROVISIONED_BUFFER: u8 = 3;
const DRIVER_SOURCE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/bin/live_input_gate.rs"
));
const SWEEP_COMMON_SOURCE: &[u8] = include_bytes!("../sweep_common.rs");

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

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn custom_code(result: &BanksTransactionResultWithMetadata) -> Option<u32> {
    match &result.result {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(code))) => Some(*code),
        _ => None,
    }
}

struct Session {
    ctx: ProgramTestContext,
    keys: SessionKeys,
    nonce: u64,
    /// The committed cursor the next ADVANCE packet binds, so the packet a
    /// gate signs is the one the sender would sign.
    cursor_before: u32,
}

struct SessionKeys {
    authority: Keypair,
    context: Keypair,
    state: Keypair,
    mirror: Keypair,
    controller: Keypair,
    buffer: Keypair,
    meta: Pubkey,
    bump: u8,
}

impl Session {
    async fn send(
        &mut self,
        instructions: Vec<Instruction>,
        signers: &[&Keypair],
        limit: u32,
    ) -> Result<BanksTransactionResultWithMetadata, Box<dyn Error>> {
        // A unique compute-unit price per transaction keeps otherwise
        // identical packets distinct; it does not change the compute limit.
        self.nonce += 1;
        let mut all = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(limit),
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

    async fn view(&mut self) -> Result<sim2::MetaView, Box<dyn Error>> {
        sim2::MetaView::parse(&self.data(self.keys.meta).await?).ok_or_else(|| "meta parse".into())
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

    fn stage_ix(target: Pubkey, offset: usize, bytes: &[u8]) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![meta(target, true, true)],
            data: stage::encode(offset as u32, bytes),
        }
    }

    async fn stage_account(
        &mut self,
        target: &Keypair,
        bytes: &[u8],
    ) -> Result<(), Box<dyn Error>> {
        let chunks: Vec<(usize, &[u8])> = bytes
            .chunks(STAGE_CHUNK)
            .enumerate()
            .map(|(i, c)| (i * STAGE_CHUNK, c))
            .collect();
        // Body first (all-zero chunks are already right in a zeroed
        // account), then the sealing header chunk.
        for &(offset, chunk) in chunks.iter().skip(1) {
            if chunk.iter().all(|&b| b == 0) {
                continue;
            }
            let result = self
                .send(
                    vec![Self::stage_ix(target.pubkey(), offset, chunk)],
                    &[target],
                    base::NORMAL_LIMIT,
                )
                .await?;
            if result.result.is_err() {
                return Err(format!(
                    "stage body {offset} of {} bytes: {:?}",
                    bytes.len(),
                    result.result
                )
                .into());
            }
        }
        let (offset, chunk) = chunks[0];
        let result = self
            .send(
                vec![Self::stage_ix(target.pubkey(), offset, chunk)],
                &[target],
                base::NORMAL_LIMIT,
            )
            .await?;
        if result.result.is_err() {
            return Err(format!("stage header: {:?}", result.result).into());
        }
        if self.data(target.pubkey()).await? != bytes {
            return Err(format!("staged account readback mismatch: {}", target.pubkey()).into());
        }
        Ok(())
    }

    fn e2r_ix(
        &self,
        continuation: Pubkey,
        operation: u8,
        cursor: u32,
        state_len: usize,
    ) -> Instruction {
        let mut accounts = vec![
            meta(
                self.keys.context.pubkey(),
                true,
                operation == continuation_split_sbf::PREPARE,
            ),
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
            meta(self.keys.authority.pubkey(), false, true),
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
        // feature only; the loaded SBF image is DOOM-SIM/2 transport and takes
        // E2R/5.  Same 32-byte field layout, the version is the header only.
        data[8..12].copy_from_slice(&5u32.to_le_bytes());
        Instruction {
            program_id: base::PROGRAM,
            accounts,
            data,
        }
    }

    fn init_ix(
        &self,
        continuation: Pubkey,
        resource_hash: [u8; 32],
        controller_hash: [u8; 32],
        frame_hash: [u8; 32],
    ) -> Instruction {
        let mut e2r_keys = [Pubkey::default(); sim2::INIT_KEY_COUNT];
        e2r_keys[0] = RESOURCE;
        for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
            e2r_keys[1 + ordinal] = wad_key(ordinal);
        }
        e2r_keys[6] = self.keys.controller.pubkey();
        e2r_keys[7] = FRAME;
        e2r_keys[8] = continuation;
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.context.pubkey(), false, false),
                meta(self.keys.state.pubkey(), false, false),
                meta(self.keys.mirror.pubkey(), false, false),
                meta(self.keys.meta, true, false),
                meta(self.keys.buffer.pubkey(), true, false),
                meta(self.keys.authority.pubkey(), true, true),
                meta(system_program::id(), false, false),
            ],
            data: sim2::InitRequest {
                bump: self.keys.bump,
                capacity: sim2::DEFAULT_BUFFER_CAPACITY as u32,
                e2r_keys,
                resource_hash,
                controller_hash,
                frame_hash,
            }
            .encode(),
        }
    }

    fn deposit_ix(&self, first: u32, commands: &[u8]) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.buffer.pubkey(), true, false),
                meta(self.keys.authority.pubkey(), false, true),
            ],
            data: sim2::encode_deposit(first, commands),
        }
    }

    fn advance_ix(&self, operation: u8, max_tics: u32) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.context.pubkey(), true, false),
                meta(self.keys.state.pubkey(), true, false),
                meta(self.keys.mirror.pubkey(), true, false),
                meta(self.keys.meta, true, false),
                meta(self.keys.buffer.pubkey(), false, false),
                meta(self.keys.authority.pubkey(), false, true),
            ],
            data: if operation == sim2::ADVANCE_STARVED {
                sim2::encode_advance_starved(self.cursor_before, max_tics)
            } else {
                // ADVANCE/3 is the historical cursor-free packet; the gate
                // uses it only as the differential for the receipt rule.
                sim2::encode_advance(max_tics)
            },
        }
    }

    /// SIM/2 RESET/9.  Seven accounts: ADVANCE's six with the authority marked
    /// writable (it is the fee payer), plus the sealed start account holding
    /// the image to re-seed from.
    fn reset_ix(&self, start: Pubkey, selector: u32, force: bool) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.context.pubkey(), true, false),
                meta(self.keys.state.pubkey(), true, false),
                meta(self.keys.mirror.pubkey(), true, false),
                meta(self.keys.meta, true, false),
                meta(self.keys.buffer.pubkey(), true, false),
                meta(start, false, false),
                meta(self.keys.authority.pubkey(), true, true),
            ],
            data: sim2::encode_reset(selector, force),
        }
    }

    /// The same packet signed by a key that is not the meta's authority.
    fn reset_ix_as(&self, start: Pubkey, stranger: &Keypair) -> Instruction {
        let mut ix = self.reset_ix(start, sim2::RESET_START_STATE, false);
        *ix.accounts.last_mut().unwrap() = meta(stranger.pubkey(), true, true);
        ix
    }

    /// SIM/2 SNAPSHOT, the read-side check a renderer must pass.
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

    fn grow_ix(&self, capacity: u32, expected_len: u32) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![meta(self.keys.buffer.pubkey(), true, true)],
            data: sim2::encode_grow_buffer(capacity, expected_len),
        }
    }

    fn static_ix(&self, static_key: Pubkey, cursor: u32) -> Instruction {
        let mut accounts = vec![meta(static_key, true, false), meta(RESOURCE, false, false)];
        if (cursor as usize) < rsr::BUILD_WAD_CHUNKS as usize {
            accounts.push(meta(wad_key(cursor as usize), false, false));
        }
        Instruction {
            program_id: base::PROGRAM,
            accounts,
            data: rsr::Request::build_chunk(cursor).encode().to_vec(),
        }
    }

    fn snapshot_live_ix(
        &self,
        static_key: Pubkey,
        snapshot: Pubkey,
        request: rsn3::SnapshotRequest,
    ) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.context.pubkey(), false, false),
                meta(self.keys.meta, false, false),
                meta(self.keys.state.pubkey(), false, false),
                meta(self.keys.mirror.pubkey(), false, false),
                meta(static_key, false, false),
                meta(snapshot, true, false),
            ],
            data: request.encode_live().to_vec(),
        }
    }

    /// SNAPSHOT-LIVE-BUFFERED/3: the same six accounts as SNAPSHOT-LIVE with
    /// the buffer number in `data[13]` instead of a cursor and a state root.
    /// The relayer sends this once per frame, and the account it names is the
    /// one its own buffer number derives -- which is the whole of what the
    /// depth is: four numbers, four accounts, and a program that binds each
    /// packet to the one its number names.
    fn snapshot_live_buffered_ix(
        &self,
        static_key: Pubkey,
        snapshot: Pubkey,
        buffer: u8,
    ) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.context.pubkey(), false, false),
                meta(self.keys.meta, false, false),
                meta(self.keys.state.pubkey(), false, false),
                meta(self.keys.mirror.pubkey(), false, false),
                meta(static_key, false, false),
                meta(snapshot, true, false),
            ],
            data: rsn3::BufferedSnapshotRequest { buffer }.encode().to_vec(),
        }
    }

    /// INIT-BUFFERED/3: create the buffer's snapshot account, owned by the
    /// program and rent-exempt at its full size.  Rent is per buffer, so this
    /// is the operation that decides what a fourth buffer costs.
    fn init_buffered_ix(&self, snapshot: Pubkey, buffer: u8) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(self.keys.authority.pubkey(), true, true),
                meta(self.keys.meta, false, false),
                meta(snapshot, true, false),
                meta(system_program::id(), false, false),
            ],
            data: rsn3::encode_init_buffered(buffer).to_vec(),
        }
    }

    /// GROW-BUFFERED/3: the commutative 10,240-byte step, repeated until the
    /// account reaches `SNAPSHOT_BYTES`.  The account is created at zero
    /// length and reaches full size the same way the relayer brings one up.
    fn grow_buffered_ix(&self, snapshot: Pubkey, buffer: u8) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(snapshot, true, false),
                meta(self.keys.meta, false, false),
            ],
            data: rsn3::encode_grow_buffered(buffer).to_vec(),
        }
    }

    /// INIT-STRIP/3: one of the eight strip accounts that hang off a buffer's
    /// snapshot account, so a provisioned buffer is a whole frame's worth of
    /// accounts and not just a snapshot.
    fn init_strip_ix(&self, snapshot: Pubkey, strip: u8) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(
                    rsn3::strip_address(&base::PROGRAM, &snapshot, strip).0,
                    true,
                    false,
                ),
                meta(self.keys.authority.pubkey(), true, true),
                meta(snapshot, false, false),
                meta(system_program::id(), false, false),
            ],
            data: rsn3::encode_init_strip(strip).to_vec(),
        }
    }

    fn render_ix(
        &self,
        static_key: Pubkey,
        snapshot: Pubkey,
        strip: u8,
        request: rsr::Request,
    ) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(static_key, true, false),
                meta(snapshot, false, false),
                meta(
                    rsn3::strip_address(&base::PROGRAM, &snapshot, strip).0,
                    true,
                    false,
                ),
            ],
            data: request.encode().to_vec(),
        }
    }

    /// The instruction the live relayer actually sends: RSR/2 RENDER-BUFFERED/5,
    /// which names its buffer and its strip and -- new in this image -- the
    /// snapshot it believes it is rendering.  The account list is the
    /// relayer's, and it is four accounts where the offline RENDER takes
    /// three, because the buffered path binds the context as well as the
    /// static account.
    fn render_buffered_ix(&self, static_key: Pubkey, snapshot: Pubkey, request: rsr::Request) -> Instruction {
        Instruction {
            program_id: base::PROGRAM,
            accounts: vec![
                meta(static_key, true, false),
                meta(self.keys.context.pubkey(), false, false),
                meta(snapshot, false, false),
                meta(
                    rsn3::strip_address(&base::PROGRAM, &snapshot, request.strip_id).0,
                    true,
                    false,
                ),
            ],
            data: request.encode().to_vec(),
        }
    }
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

async fn create_owned(
    ctx: &mut ProgramTestContext,
    key: &Keypair,
    bytes: usize,
    lamports: u64,
) -> Result<(), Box<dyn Error>> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await?;
    let tx = Transaction::new(
        &[&ctx.payer, key],
        Message::new_with_blockhash(
            &[system_instruction::create_account(
                &ctx.payer.pubkey(),
                &key.pubkey(),
                lamports,
                bytes as u64,
                &base::PROGRAM,
            )],
            Some(&ctx.payer.pubkey()),
            &blockhash,
        ),
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await?;
    Ok(())
}

/// Grow a fresh input buffer to its declared length.
async fn grow_buffer(
    session: &mut Session,
    buffer: &Keypair,
    capacity: u32,
    units: &mut Vec<u64>,
) -> Result<(), Box<dyn Error>> {
    let total = sim2::buffer_bytes(capacity as usize).ok_or("buffer capacity")?;
    let mut len = sim2::initial_buffer_bytes(capacity as usize).ok_or("initial buffer length")?;
    create_owned(
        &mut session.ctx,
        buffer,
        len,
        Rent::default().minimum_balance(total),
    )
    .await?;
    while len < total {
        let ix = session.grow_ix(capacity, len as u32);
        let grown = session
            .send(vec![ix], &[buffer], base::NORMAL_LIMIT)
            .await?;
        if grown.result.is_err() {
            return Err(format!("SIM2 GROW-BUFFER at {len}: {:?}", grown.result).into());
        }
        units.push(base::units(&grown)?);
        len = sim2::next_buffer_len(len, capacity as usize).ok_or("buffer growth stalled")?;
    }
    if session.data(buffer.pubkey()).await?.len() != total {
        return Err("SIM2 input buffer did not reach its declared length".into());
    }
    Ok(())
}

/// Bring one render buffer up the way a relayer brings one up: the program
/// creates the account and pays its rent, grows it to its full size in
/// commutative steps, and creates the eight strip accounts that hang off it.
///
/// The rent is reported rather than assumed, because rent is the one cost the
/// depth is not free of: `minimum_balance(SNAPSHOT_BYTES)` a buffer, paid once
/// and never recovered, on top of the eight strip accounts either way.  What
/// this measures is the per-buffer figure for a buffer number that the
/// two-buffer program would have refused, so it is the number a fourth buffer
/// actually adds.
async fn provision_render_buffer(
    session: &mut Session,
    buffer: u8,
    units: &mut Vec<u64>,
) -> Result<(Pubkey, u64, usize), Box<dyn Error>> {
    let (snapshot, _) = rsn3::buffered_snapshot_address(
        &base::PROGRAM,
        &session.keys.context.pubkey(),
        buffer,
    )
    .ok_or_else(|| -> Box<dyn Error> {
        format!("buffer {buffer} has no address to provision").into()
    })?;
    let authority = session.keys.authority.insecure_clone();
    let init = session.init_buffered_ix(snapshot, buffer);
    let created = session.send(vec![init], &[&authority], RENDER_LIMIT).await?;
    if created.result.is_err() {
        return Err(format!("INIT-BUFFERED {buffer}: {:?}", created.result).into());
    }
    units.push(base::units(&created)?);
    let rent = session
        .ctx
        .banks_client
        .get_account(snapshot)
        .await?
        .map(|account| account.lamports)
        .ok_or("INIT-BUFFERED did not create the account")?;
    let mut len = session.data(snapshot).await?.len();
    let mut grows = 0usize;
    while len < rsn3::SNAPSHOT_BYTES {
        let ix = session.grow_buffered_ix(snapshot, buffer);
        let grown = session
            .send(vec![ix], &[], RENDER_LIMIT)
            .await?;
        if grown.result.is_err() {
            return Err(format!("GROW-BUFFERED {buffer} at {len}: {:?}", grown.result).into());
        }
        units.push(base::units(&grown)?);
        grows += 1;
        let next = session.data(snapshot).await?.len();
        if next <= len {
            return Err(format!("GROW-BUFFERED {buffer} stalled at {len}").into());
        }
        len = next;
    }
    if len != rsn3::SNAPSHOT_BYTES {
        return Err(format!(
            "GROW-BUFFERED {buffer} reached {len}, not {}",
            rsn3::SNAPSHOT_BYTES
        )
        .into());
    }
    for strip in 0..8u8 {
        let ix = session.init_strip_ix(snapshot, strip);
        let created = session.send(vec![ix], &[&authority], RENDER_LIMIT).await?;
        if created.result.is_err() {
            return Err(format!("INIT-STRIP {buffer}/{strip}: {:?}", created.result).into());
        }
        units.push(base::units(&created)?);
    }
    Ok((snapshot, rent, grows))
}

/// STG/1 + E2R/5 + SIM/2 INIT for one context, from one staged ST1/MP1 pair.
#[allow(clippy::too_many_arguments)]
async fn reconstruct(
    session: &mut Session,
    staged_state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    capture: &Capture,
    capacity: u32,
) -> Result<(Vec<u64>, u64), Box<dyn Error>> {
    let state_signer = session.keys.state.insecure_clone();
    let mirror_signer = session.keys.mirror.insecure_clone();
    let controller_signer = session.keys.controller.insecure_clone();
    let authority = session.keys.authority.insecure_clone();
    let context_signer = session.keys.context.insecure_clone();
    session.stage_account(&state_signer, staged_state).await?;
    session.stage_account(&mirror_signer, mirror).await?;
    session
        .stage_account(&controller_signer, controller)
        .await?;
    let continuation = Keypair::new();
    create_owned(
        &mut session.ctx,
        &continuation,
        continuation_split_sbf::CONTINUATION_BYTES,
        Rent::default().minimum_balance(continuation_split_sbf::CONTINUATION_BYTES),
    )
    .await?;
    let state_len = base::u32_at(staged_state, 16) as usize;
    let mut phases = Vec::new();
    for cursor in 0..E2R_PHASES {
        let operation = if cursor == 0 {
            continuation_split_sbf::PREPARE
        } else {
            continuation_split_sbf::STEP
        };
        let ix = session.e2r_ix(continuation.pubkey(), operation, cursor, state_len);
        let signers: Vec<&Keypair> = if cursor == 0 {
            vec![&authority, &context_signer]
        } else {
            vec![&authority]
        };
        let result = session.send(vec![ix], &signers, base::NORMAL_LIMIT).await?;
        if result.result.is_err() {
            return Err(format!("E2R/5 phase {cursor}: {:?}", result.result).into());
        }
        phases.push(base::units(&result)?);
    }
    if session.data(state_signer.pubkey()).await? != staged_state
        || session.data(mirror_signer.pubkey()).await? != mirror
    {
        return Err("E2R/5 publication changed the staged ST1/MP1".into());
    }
    let buffer = session.keys.buffer.insecure_clone();
    let mut grow_units = Vec::new();
    grow_buffer(session, &buffer, capacity, &mut grow_units).await?;
    let ix = session.init_ix(
        continuation.pubkey(),
        solana_program::hash::hash(&capture.resource).to_bytes(),
        solana_program::hash::hash(controller).to_bytes(),
        solana_program::hash::hash(&vec![0u8; resource_sbf::RI2_FRAME_BYTES]).to_bytes(),
    );
    let init = session
        .send(vec![ix], &[&authority], base::NORMAL_LIMIT)
        .await?;
    if init.result.is_err() {
        return Err(format!("SIM2 INIT {:?}", init.result).into());
    }
    let init_cu = base::units(&init)?;
    let staged_len = base::u32_at(staged_state, 16) as usize;
    let view = session.view().await?;
    if view.cursor != 0 {
        return Err("INIT meta does not start at cursor 0".into());
    }
    let (checked, root) = sim2::verified_state_root(staged_state, mirror)
        .map_err(|e| format!("staged state root: {e:?}"))?;
    if checked != staged_len || view.state_root != root {
        return Err("INIT meta does not commit the published state".into());
    }
    Ok((phases, init_cu))
}

/// The account layout the image under test expects.
///
/// A `diagnostic-profile` image carries patch 0030's 176-byte phase
/// accumulator inside `doom_ctx_t`, so both the context account and the
/// static resource account it derives its offset from are that much longer.
/// The runner is built with `ctx-engine` only, so it cannot read its own
/// feature set as the answer: the layout has to come from the image's
/// manifest, which is the image's own record of what it was built with.
struct Layout {
    context_bytes: usize,
    static_bytes: usize,
    diagnostic: bool,
}

impl Layout {
    fn from_manifest(manifest: &serde_json::Value) -> Result<Self, Box<dyn Error>> {
        let features = manifest["enabled_cargo_features"]
            .as_array()
            .ok_or("image manifest has no enabled_cargo_features")?;
        let diagnostic = features
            .iter()
            .any(|f| f.as_str() == Some("diagnostic-profile"));
        Ok(if diagnostic {
            Layout {
                context_bytes: resource_sbf::CONTEXT_BYTES_DIAGNOSTIC,
                static_bytes: resource_sbf::CONTEXT_BYTES_DIAGNOSTIC,
                diagnostic: true,
            }
        } else {
            Layout {
                context_bytes: resource_sbf::CONTEXT_BYTES,
                static_bytes: rsr::ACCOUNT_BYTES,
                diagnostic: false,
            }
        })
    }
}

fn bank_with(
    capture: &Capture,
    context: Pubkey,
    state: Pubkey,
    mirror: Pubkey,
    controller: Pubkey,
    authority: Pubkey,
    render_accounts: Option<(Pubkey, Pubkey)>,
    layout: &Layout,
) -> ProgramTest {
    let mut bank = ProgramTest::default();
    bank.prefer_bpf(true);
    bank.set_compute_max_units(u64::from(base::NORMAL_LIMIT));
    bank.add_program("doom_sbf", base::PROGRAM, None);
    bank.add_account(context, base::account(vec![0; layout.context_bytes]));
    bank.add_account(RESOURCE, base::account(capture.resource.clone()));
    for (ordinal, bytes) in capture.wad.iter().enumerate() {
        bank.add_account(wad_key(ordinal), base::account(bytes.clone()));
    }
    for (key, len) in [
        (state, resource_sbf::STATE_MAX_BYTES),
        (mirror, resource_sbf::MIRROR_BYTES),
        (controller, resource_sbf::CONTROLLER_BYTES),
    ] {
        bank.add_account(key, base::account(vec![0; len]));
    }
    bank.add_account(FRAME, base::account(vec![0; resource_sbf::RI2_FRAME_BYTES]));
    bank.add_account(authority, system_account(100_000_000_000));
    if let Some((static_key, snapshot)) = render_accounts {
        bank.add_account(static_key, base::account(vec![0; layout.static_bytes]));
        bank.add_account(snapshot, base::account(vec![0; rsn3::SNAPSHOT_BYTES]));
        for strip_id in 0..8u8 {
            bank.add_account(
                rsn3::strip_address(&base::PROGRAM, &snapshot, strip_id).0,
                base::account(vec![0; rsn3::STRIP_ACCOUNT_BYTES]),
            );
        }
        // Every buffer inside the count gets its own snapshot account and its
        // own eight strips, because the whole point of the depth is that two
        // frames in flight are writing to two different accounts.  The one
        // buffer the gate provisions itself is left out here so that the
        // INIT-BUFFERED / GROW-BUFFERED / INIT-STRIP path is measured on an
        // account the program created and paid rent for, not on one the bank
        // handed over already sized.
        for buffer in 1..rsn3::BUFFER_COUNT {
            if buffer == PROVISIONED_BUFFER {
                continue;
            }
            let (key, _) = rsn3::buffered_snapshot_address(&base::PROGRAM, &context, buffer)
                .unwrap_or_else(|| {
                    panic!("buffer {buffer} is inside BUFFER_COUNT but has no address")
                });
            bank.add_account(key, base::account(vec![0; rsn3::SNAPSHOT_BYTES]));
            for strip_id in 0..8u8 {
                bank.add_account(
                    rsn3::strip_address(&base::PROGRAM, &key, strip_id).0,
                    base::account(vec![0; rsn3::STRIP_ACCOUNT_BYTES]),
                );
            }
        }
    }
    bank
}

fn session_keys(context: Keypair) -> SessionKeys {
    let (meta, bump) = sim2::meta_address(&base::PROGRAM, &context.pubkey());
    SessionKeys {
        authority: Keypair::new(),
        context,
        state: Keypair::new(),
        mirror: Keypair::new(),
        controller: Keypair::new(),
        buffer: Keypair::new(),
        meta,
        bump,
    }
}

/// The `Custom(n)` an instruction error carries, if it carries one.
fn custom_code_text(_phase: &u32, text: &str) -> Option<u32> {
    let start = text.find("Custom(")? + "Custom(".len();
    let end = text[start..].find(')')? + start;
    text[start..end].parse().ok()
}

/// A DOOM-CI2 controller account bound to `gametic`.  `ri2_controller_shape`
/// requires the recorded tic to equal the ST1 image's `gametic` and bytes
/// 28..64 to be zero, so a context reconstructed from a later tic needs its
/// own controller account.  The four command bytes are the armed ticcmd and
/// are not compared with the demo stream on this path.
fn controller_for_gametic(controller: &[u8], gametic: u32) -> Result<Vec<u8>, Box<dyn Error>> {
    if controller.len() != resource_sbf::CONTROLLER_BYTES
        || &controller[..8] != b"DOOM-CI2"
        || base::u32_at(controller, 8) != 1
        || base::u32_at(controller, 12) != 28
        || base::u32_at(controller, 16) != resource_sbf::CONTROLLER_BYTES as u32
    {
        return Err("retained CI2 controller has the wrong shape".into());
    }
    let mut out = controller.to_vec();
    out[20..24].copy_from_slice(&gametic.to_le_bytes());
    out[28..].fill(0);
    Ok(out)
}

/// One committed field changed, nothing else: a canonical ST1 that is not this
/// slot's committed start state.  Used as the wrong-start-image control, where
/// the shape checks must pass and only the root check can refuse it.
fn doctored_start_state(state: &[u8]) -> Vec<u8> {
    let mut out = state.to_vec();
    let moved = (i32_at(state, 24) as u32).wrapping_add(1);
    out[24..28].copy_from_slice(&moved.to_le_bytes());
    out
}

/// The same image with the console player marked dead.  `PST_DEAD` is inside
/// the range the state codec's strict scan accepts, so this is a canonical ST1
/// and the decode it goes through is the real one.
fn dead_player_state(state: &[u8]) -> Vec<u8> {
    let mut out = state.to_vec();
    out[common::ST1_PLAYER0_PLAYERSTATE_OFFSET..common::ST1_PLAYER0_PLAYERSTATE_OFFSET + 4]
        .copy_from_slice(&common::PST_DEAD.to_le_bytes());
    out
}

/// One sealed start account holding `state`, freshly created and program-owned
/// exactly as a relayer would provision it.
async fn stage_start_account(
    session: &mut Session,
    state: &[u8],
) -> Result<Keypair, Box<dyn Error>> {
    let key = Keypair::new();
    create_owned(
        &mut session.ctx,
        &key,
        resource_sbf::STATE_MAX_BYTES,
        Rent::default().minimum_balance(resource_sbf::STATE_MAX_BYTES),
    )
    .await?;
    session
        .stage_account(&key, &base::padded_state(state))
        .await?;
    Ok(key)
}

/// What the host ingress does with an ST1 image: `Err(rc)` from the import, a
/// tic that ran with a consumed command (0), or a tic that ran with an
/// unconsumed one (238, the DOOM-CI3 receipt reading zero).
fn host_import_and_step(capture: &Capture, state: &[u8]) -> Result<i32, Box<dyn Error>> {
    let mut session =
        match resource::NativeSimSession::open(capture.wad_refs(), &capture.resource, state) {
            Ok(session) => session,
            Err(rc) => return Ok(-rc),
        };
    Ok(session.step(&[0u8, 0u8, 0u8, 0u8]))
}

/// The host ingress stepped to `cursor` and published: the ST1/MP1 pair a
/// chain context at that cursor would hold.
fn host_state_at(
    capture: &Capture,
    commands: &[u8],
    cursor: u32,
) -> Result<(Vec<u8>, Vec<u8>), Box<dyn Error>> {
    let mut session =
        resource::NativeSimSession::open(capture.wad_refs(), &capture.resource, &capture.state)
            .map_err(|rc| format!("host session open rc={rc}"))?;
    for (seq, command) in commands.chunks_exact(4).take(cursor as usize).enumerate() {
        let command: [u8; 4] = command.try_into().unwrap();
        let rc = session.step(&command);
        if rc != 0 {
            return Err(format!("host step {seq} rc={rc}").into());
        }
    }
    let mut state = vec![0u8; resource_sbf::STATE_MAX_BYTES];
    let mut mirror = vec![0u8; resource_sbf::MIRROR_BYTES];
    let length = session
        .publish(&mut state, &mut mirror)
        .map_err(|rc| format!("host publish rc={rc}"))?;
    state.truncate(length);
    Ok((state, mirror))
}

/// The host player view at `cursor`, rendered in a session replayed over the
/// same staged commands, with the number of committed ST1 bytes the render
/// itself moved.  `R_RenderPlayerView` is not a pure read of the committed
/// state, so the perturbation is reported rather than assumed away.
fn host_frame_at(
    capture: &Capture,
    commands: &[u8],
    cursor: u32,
) -> Result<(Vec<u8>, usize), Box<dyn Error>> {
    let mut session =
        resource::NativeSimSession::open(capture.wad_refs(), &capture.resource, &capture.state)
            .map_err(|rc| format!("frame session open rc={rc}"))?;
    for (seq, command) in commands.chunks_exact(4).take(cursor as usize).enumerate() {
        let rc = session.step(&command.try_into().unwrap());
        if rc != 0 {
            return Err(format!("frame session step {seq} rc={rc}").into());
        }
    }
    let mut before = vec![0u8; resource_sbf::STATE_MAX_BYTES];
    let mut mirror = vec![0u8; resource_sbf::MIRROR_BYTES];
    let length = session
        .publish(&mut before, &mut mirror)
        .map_err(|rc| format!("frame session publish rc={rc}"))?;
    before.truncate(length);
    let frame = session
        .frame()
        .map_err(|rc| format!("frame at {cursor} rc={rc}"))?;
    let mut after = vec![0u8; resource_sbf::STATE_MAX_BYTES];
    let mut again = vec![0u8; resource_sbf::MIRROR_BYTES];
    let length = session
        .publish(&mut after, &mut again)
        .map_err(|rc| format!("frame session publish after render rc={rc}"))?;
    after.truncate(length);
    let moved = before.iter().zip(&after).filter(|(a, b)| a != b).count();
    Ok((frame, moved))
}

fn slice_frame(frame: &[u8], strip: usize) -> Vec<u8> {
    (0..200)
        .flat_map(|row| {
            let start = row * 320 + strip * 40;
            frame[start..start + 40].iter().copied()
        })
        .collect()
}

fn differing_pixels(left: &[u8], right: &[u8]) -> usize {
    left.iter().zip(right).filter(|(a, b)| a != b).count()
}

/// 4-byte little-endian fields of two ST1 images that differ, as
/// `(offset, before, after)`.  This says which committed fields the live input
/// moved without asserting a layout the gate has not read.
fn changed_fields(before: &[u8], after: &[u8]) -> Vec<(usize, i32, i32)> {
    let end = before.len().min(after.len());
    (0..end.saturating_sub(3))
        .step_by(4)
        .filter_map(|offset| {
            let a = i32_at(before, offset);
            let b = i32_at(after, offset);
            (a != b).then_some((offset, a, b))
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let argv: Vec<String> = env::args().collect();
    let mut args = argv.iter().skip(1).cloned();
    let image = PathBuf::from(args.next().ok_or(
        "usage: live_input_gate IMAGE IMAGE_SHA MANIFEST MANIFEST_SHA CAPTURE RESOURCE CONTROLLER RECEIPT [--profile NAME] [--tics N] [--advance-tics N] [--frames-at a,b,c] [--starve-state-cursor N]",
    )?)
    .canonicalize()?;
    let expected_image = args.next().ok_or("missing image SHA")?;
    let manifest = PathBuf::from(args.next().ok_or("missing manifest")?).canonicalize()?;
    let expected_manifest = args.next().ok_or("missing manifest SHA")?;
    let capture_dir = PathBuf::from(args.next().ok_or("missing capture")?).canonicalize()?;
    let resource_path = PathBuf::from(args.next().ok_or("missing resource")?).canonicalize()?;
    let controller_path = PathBuf::from(args.next().ok_or("missing controller")?).canonicalize()?;
    let receipt = PathBuf::from(args.next().ok_or("missing receipt")?);
    let mut profile = "r8l-move".to_owned();
    let mut tics = 5_400u32;
    let mut advance_tics = 4u32;
    let mut frames_at: Vec<u32> = vec![5_040, 5_076, 5_112];
    let mut starve_cursor = 2_048u32;
    let mut new_game_start = false;
    while let Some(flag) = args.next() {
        let mut value = || -> Result<String, Box<dyn Error>> {
            args.next()
                .ok_or_else(|| format!("{flag} needs a value").into())
        };
        match flag.as_str() {
            "--profile" => profile = value()?,
            "--tics" => tics = value()?.parse()?,
            "--advance-tics" => advance_tics = value()?.parse()?,
            "--frames-at" => {
                frames_at = value()?
                    .split(',')
                    .filter(|part| !part.is_empty())
                    .map(|part| part.parse::<u32>())
                    .collect::<Result<Vec<u32>, _>>()?
            }
            "--starve-state-cursor" => starve_cursor = value()?.parse()?,
            "--new-game-start" => new_game_start = true,
            other => return Err(format!("unknown flag {other}").into()),
        }
    }
    if receipt.exists() && fs::read_dir(&receipt)?.next().is_some() {
        return Err("receipt directory is not empty".into());
    }
    fs::create_dir_all(&receipt)?;
    let inputs = receipt.join("inputs");
    fs::create_dir_all(&inputs)?;

    let image_bytes = base::read(&image)?;
    let manifest_bytes = base::read(&manifest)?;
    if base::sha(&image_bytes) != expected_image || base::sha(&manifest_bytes) != expected_manifest
    {
        return Err("image/manifest identity mismatch".into());
    }
    let manifest_json: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    if manifest_json["profile"] != "sim-transport" || manifest_json["sbf"] != true {
        return Err("image manifest is not the sim-transport SBF profile".into());
    }
    if !sim2::ABI_DESCRIPTOR.contains("ADVANCE/5")
        || !sim2::ABI_DESCRIPTOR.contains("unconsumed-tic-refused")
    {
        return Err("the driver does not carry the ADVANCE/5 receipt rule".into());
    }
    if !manifest_json["render_snapshot_v3_abi_descriptor"]
        .as_str()
        .unwrap_or_default()
        .contains("SNAPSHOT-LIVE/3")
    {
        return Err("image manifest does not carry the live render path".into());
    }
    let layout = Layout::from_manifest(&manifest_json)?;
    if !manifest_json["resource_ri2_sbf_abi_descriptor"]
        .as_str()
        .unwrap_or_default()
        .contains(&format!("context={}", layout.context_bytes))
    {
        return Err("image manifest context size is not the driver's".into());
    }
    fs::write(inputs.join("doom_sbf.so"), &image_bytes)?;
    fs::write(
        inputs.join("doom_ex2_source_manifest.json"),
        &manifest_bytes,
    )?;
    env::set_var("BPF_OUT_DIR", image.parent().ok_or("image has no parent")?);
    env::set_var("SBF_OUT_DIR", image.parent().ok_or("image has no parent")?);

    let capture_data = Capture::load(&capture_dir, &resource_path)?;
    let controller = base::read(&controller_path)?;
    let (commands, _) = common::input_commands(&profile, tics as usize)?;
    let (neutral, _) = common::input_commands("neutral", tics as usize)?;
    fs::write(receipt.join("commands.bin"), &commands)?;
    let first_tic = base::u32_at(&capture_data.state, 20);
    let capture_cursor = base::u32_at(&capture_data.state, 24);
    // The captured lump is the bound: past it there is no recorded ticcmd, so
    // the controller ingress is the only thing that can drive a tic.
    let demo_tics = capture_data.looped_demo_commands(tics.max(1) as usize)?.1 as u32;
    if !new_game_start && (demo_tics < 5_000 || demo_tics >= tics) {
        return Err(format!(
            "captured DEMO1 carries {demo_tics} tics, which does not leave room in a {tics}-tic run"
        )
        .into());
    }
    if advance_tics == 0 || advance_tics > sim2::MAX_SWEEP_TICS {
        return Err("--advance-tics is out of range".into());
    }
    for cursor in &frames_at {
        // A new-game start state carries a demo cursor into the shareware lump
        // (13, the first DEMO1 ticcmd) purely so the SIM/2 cursor validation has
        // real bounds; under SIM2INPT/3 the lump is not the source of player
        // input, and its length is a property of E1M5, not of this run.  So the
        // frame cursors only have to be inside the run and on a batch boundary.
        let inside = *cursor > 0 && *cursor % advance_tics == 0 && *cursor <= tics;
        if !inside || (!new_game_start && *cursor <= demo_tics) {
            let past = if new_game_start {
                String::new()
            } else {
                format!(" and past the {demo_tics}-tic demo end")
            };
            return Err(format!(
                "frame cursor {cursor} must be inside the {tics}-tic run on a {advance_tics}-tic boundary{past}"
            )
            .into());
        }
    }
    if starve_cursor == 0 || (!new_game_start && starve_cursor >= demo_tics) {
        return Err("--starve-state-cursor must be inside the run".into());
    }

    // --- host lockstep oracle ------------------------------------------------
    // One resident session, stepped in lockstep with the chain: the chain's own
    // per-tic contract, one ST1/MP1 pair resident instead of gigabytes.
    let mut host = resource::NativeSimSession::open(
        capture_data.wad_refs(),
        &capture_data.resource,
        &capture_data.state,
    )
    .map_err(|rc| format!("oracle session open rc={rc}"))?;
    let initial_state = capture_data.state_account();
    let (_, initial_root) = sim2::verified_state_root(&initial_state, &capture_data.mirror)
        .map_err(|e| format!("initial state root: {e:?}"))?;
    let mut host_state = vec![0u8; resource_sbf::STATE_MAX_BYTES];
    let mut host_mirror = vec![0u8; resource_sbf::MIRROR_BYTES];

    // --- 1. main context: STG/1, E2R/5, SIM/2 INIT ----------------------------
    let keys = session_keys(Keypair::new());
    let static_key = rsr::static_address(&base::PROGRAM).0;
    let snapshot_key = rsn3::snapshot_address(&base::PROGRAM, &keys.context.pubkey()).0;
    let bank = bank_with(
        &capture_data,
        keys.context.pubkey(),
        keys.state.pubkey(),
        keys.mirror.pubkey(),
        keys.controller.pubkey(),
        keys.authority.pubkey(),
        Some((static_key, snapshot_key)),
        &layout,
    );
    let ctx = bank.start_with_context().await;
    let mut session = Session {
        ctx,
        keys,
        nonce: 0,
        cursor_before: 0,
    };
    let capacity = sim2::DEFAULT_BUFFER_CAPACITY as u32;
    let staged_state = capture_data.state_account();
    let (phases, init_cu) = reconstruct(
        &mut session,
        &staged_state,
        &capture_data.mirror,
        &controller,
        &capture_data,
        capacity,
    )
    .await?;
    // The input chain is seeded with the context key SIM/2 INIT binds, so the
    // gate compares the chain the program actually commits.
    let mut chain = sim2::chain_init(&session.keys.context.pubkey(), first_tic);
    let authority = session.keys.authority.insecure_clone();
    let state_signer = session.keys.state.insecure_clone();
    let mirror_signer = session.keys.mirror.insecure_clone();
    let buffer_signer = session.keys.buffer.insecure_clone();
    let meta_key = session.keys.meta;

    // --- 2. live-input sweeps, with the chain's own render at chosen cursors ---
    let mut sweeps = fs::File::create(receipt.join("sweeps.jsonl"))?;
    let mut advance_units: Vec<u64> = Vec::new();
    let mut deposit_units: Vec<u64> = Vec::new();
    let mut build_units: Vec<u64> = Vec::new();
    let mut render_rows: Vec<serde_json::Value> = Vec::new();
    let mut buffered_rows: Vec<serde_json::Value> = Vec::new();
    // One row per buffer inside the count per rendered cursor, and the refusal
    // codes for the buffer past the end.
    let mut depth_rows: Vec<serde_json::Value> = Vec::new();
    let mut depth_refusals: Vec<serde_json::Value> = Vec::new();
    let mut depth_frames = 0usize;
    let mut depth_buffer_frames = 0usize;
    let mut depth_across_frames = 0usize;
    let mut depth_working_moved: Vec<serde_json::Value> = Vec::new();
    // The static account's digest after each frame's eight strips.  Two frames
    // at different cursors have to leave it in the same state: that is the
    // purity claim for the strip path, stated so a render that moved a
    // committed engine field has nowhere to hide.  A per-frame
    // before/after comparison cannot make the claim, because the last strip of
    // a frame writes the clip word for strip 7 and the clip word is part of
    // DOOM_CTX, so a before/after difference of exactly that field is
    // expected and says nothing either way.

    // The name the previous frame's strips carried, so a frame that is late
    // can be recognised as late on chain rather than only in the relayer's log.
    let mut previous_snapshot_hash: Option<[u8; 32]> = None;
    let mut position_rows: Vec<serde_json::Value> = Vec::new();
    let mut st1_at_demo_end: Option<(u32, Vec<u8>)> = None;
    // With a new-game start state the demo end is not in this run, so the
    // "what did the live input move" comparison is against the start state
    // itself, which is the state every other claim is relative to.
    let baseline_state = if new_game_start {
        capture_data.state.clone()
    } else {
        Vec::new()
    };
    let mut chain_states: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut chain_frames: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut strips_written = 0usize;
    let mut equal_sweeps = 0usize;
    let mut first_past_demo = None;
    // Per-batch (cursor, st1, mp1, root, chain) of the first run, so the
    // post-reset replay is compared against it rather than against a rerun.
    let mut first_run: Vec<(u32, String, String, [u8; 32], [u8; 32])> = Vec::new();
    let mut cursor = 0u32;
    let mut sent = 0usize;
    let started = Instant::now();
    let mut static_built = false;
    // The buffer the gate provisions through the program's own INIT-BUFFERED /
    // GROW-BUFFERED / INIT-STRIP operations rather than asking the bank for a
    // ready-made account.  It is done once, before any frame, because it is a
    // one-off cost: rent is paid per buffer and never recovered, and the eight
    // strip accounts are the same eight either way.
    let mut provision_units: Vec<u64> = Vec::new();
    let (provisioned_snapshot, provisioned_rent, provision_grows) =
        provision_render_buffer(&mut session, PROVISIONED_BUFFER, &mut provision_units).await?;
    let provisioned_snapshot = Some(provisioned_snapshot);
    while cursor < tics {
        let first = cursor;
        let last = (cursor + advance_tics).min(tics);
        let count = (last - first) as usize;
        let deposit = session.deposit_ix(first, &commands[first as usize * 4..last as usize * 4]);
        let deposited = session
            .send(vec![deposit], &[&authority], base::NORMAL_LIMIT)
            .await?;
        if deposited.result.is_err() {
            return Err(format!("deposit {first}+{count}: {:?}", deposited.result).into());
        }
        deposit_units.push(base::units(&deposited)?);
        session.cursor_before = first;
        let advance = session.advance_ix(sim2::ADVANCE_STARVED, count as u32);
        let result = session
            .send(vec![advance], &[&authority], base::NORMAL_LIMIT)
            .await?;
        if result.result.is_err() {
            return Err(format!(
                "ADVANCE/5 refused live input at cursor {first} after {sent} sweeps: {:?}",
                result.result
            )
            .into());
        }
        sent += 1;
        let units = base::units(&result)?;
        advance_units.push(units);
        let view = session.view().await?;
        if view.cursor != last {
            return Err(format!(
                "ADVANCE/5 committed cursor {} for the batch {first}..{last}",
                view.cursor
            )
            .into());
        }
        for seq in first..last {
            let command: [u8; 4] = commands[seq as usize * 4..seq as usize * 4 + 4]
                .try_into()
                .unwrap();
            let rc = host.step(&command);
            if rc != 0 {
                return Err(format!("host ingress starved at tic {seq} rc={rc}").into());
            }
            chain = sim2::chain_step(&chain, seq, &command);
        }
        host.publish(&mut host_state, &mut host_mirror)
            .map_err(|rc| format!("host publish at {last}: rc={rc}"))?;
        let state = session.data(state_signer.pubkey()).await?;
        let mirror = session.data(mirror_signer.pubkey()).await?;
        let (checked, root) = sim2::verified_state_root(&state, &mirror)
            .map_err(|e| format!("chain state root at {last}: {e:?}"))?;
        if state != host_state
            || mirror != host_mirror
            || checked != base::u32_at(&state, 16) as usize
            || root != view.state_root
            || view.input_chain != chain
        {
            return Err(format!(
                "SBF state at {last} differs from the host ingress: st1={} mp1={} root={} chain={}",
                state == host_state,
                mirror == host_mirror,
                root == view.state_root,
                view.input_chain == chain
            )
            .into());
        }
        equal_sweeps += 1;
        first_run.push((last, base::sha(&state), base::sha(&mirror), root, chain));
        writeln!(
            sweeps,
            "{}",
            json!({
                "before": first,
                "after": last,
                "advance_tics": count,
                "cu": units,
                "st1_sha256": base::sha(&state),
                "mp1_sha256": base::sha(&mirror),
                "state_root": common::hex(&root),
                "input_chain": common::hex(&chain),
                "gametic": base::u32_at(&state, 20),
                "demo_cursor": base::u32_at(&state, 24),
                "equal_to_host_ingress": true,
            })
        )?;
        // The demo end is 5,026 tics and the batches are 4 tics, so the
        // boundary at or just before it is what the past-the-end rows compare
        // against: the state the player held when the recorded stream ran out.
        if last <= demo_tics {
            st1_at_demo_end = Some((last, state.clone()));
        }
        if first_past_demo.is_none() && last > demo_tics {
            first_past_demo = Some(last);
        }
        if frames_at.contains(&last) {
            if !static_built {
                for chunk in 0..=rsr::BUILD_FINAL_CURSOR {
                    let ix = session.static_ix(static_key, chunk);
                    let built = session.send(vec![ix], &[], RENDER_LIMIT).await?;
                    if built.result.is_err() {
                        return Err(format!("RSR/2 BUILD {chunk}: {:?}", built.result).into());
                    }
                    build_units.push(base::units(&built)?);
                }
                let header = session.data(static_key).await?;
                if header[rsr::HEADER_OFFSET..rsr::HEADER_OFFSET + 8] != *rsr::MAGIC {
                    return Err("static BUILD did not write a valid RSR/2 header".into());
                }
                static_built = true;
            }
            // Render purity, measured rather than asserted.  Two separate
            // claims, and they are not the same claim:
            //
            //  * The committed record is untouched.  The ST1 state account is
            //    the committed game state; it is not even an account of RSR/2
            //    RENDER, and its digest is taken either side of the frame's
            //    strips to show that.
            //  * A render accumulates nothing.  The static account's first
            //    CTX_STRUCT_BYTES are a working copy of the engine context, and
            //    importing the snapshot into it is how a strip gets something
            //    to render, so the context necessarily holds the snapshot just
            //    rendered.  That is a working area, not a record.  What has to
            //    hold is that re-rendering a strip reproduces the same bytes
            //    exactly, so no earlier strip's residue survives into a later
            //    one -- which is the property the committed-state restore
            //    (ri2_render_begin/ri2_render_end) exists to provide.
            let state_before = base::sha(&session.data(session.keys.state.pubkey()).await?);
            let mirror_before = base::sha(&session.data(session.keys.mirror.pubkey()).await?);
            let static_before_bytes = session.data(static_key).await?;
            // The chain's own render.  SNAPSHOT-LIVE takes the context
            // read-only, so nothing here can move the committed cursor; the
            // host frame is taken after the sweep, in its own session, because
            // `R_RenderPlayerView` is not a pure read of the committed ST1.
            let request = rsn3::SnapshotRequest {
                cursor: last,
                state_root: root,
            };
            let ix = session.snapshot_live_ix(static_key, snapshot_key, request);
            let snap = session.send(vec![ix], &[], RENDER_LIMIT).await?;
            if snap.result.is_err() {
                return Err(format!("SNAPSHOT-LIVE at {last}: {:?}", snap.result).into());
            }
            let snapshot_units = base::units(&snap)?;
            let snapshot_bytes = session.data(snapshot_key).await?;
            let snapshot = rsn3::validate_snapshot(&snapshot_bytes)?;
            if snapshot.cursor != last || snapshot.state_root != root {
                return Err(
                    format!("SNAPSHOT-LIVE at {last} does not bind the committed cursor").into(),
                );
            }
            let directory = receipt.join(format!("strips/cursor-{last:05}"));
            fs::create_dir_all(&directory)?;
            let mut reconstructed = vec![0u8; resource_sbf::RI2_FRAME_BYTES];
            let mut strip_units = Vec::new();
            let mut strip_equal = 0usize;
            for strip_id in 0..8u8 {
                let request = rsr::Request::render(strip_id, last, snapshot.snapshot_hash);
                let ix = session.render_ix(static_key, snapshot_key, strip_id, request);
                let rendered = session.send(vec![ix], &[], RENDER_LIMIT).await?;
                if rendered.result.is_err() {
                    return Err(format!(
                        "RENDER strip {strip_id} at {last}: {:?}",
                        rendered.result
                    )
                    .into());
                }
                strip_units.push(base::units(&rendered)?);
                let key = rsn3::strip_address(&base::PROGRAM, &snapshot_key, strip_id).0;
                let strip_bytes = session.data(key).await?;
                let strip = rsn3::validate_strip(&strip_bytes)?;
                if strip.cursor != last || strip.state_root != root {
                    return Err(
                        format!("strip {strip_id} at {last} does not bind the cursor").into(),
                    );
                }
                strip_equal += usize::from(strip.strip_id == u32::from(strip_id));
                for row in 0..200 {
                    let from = row * 40;
                    let to = row * 320 + usize::from(strip_id) * 40;
                    reconstructed[to..to + 40].copy_from_slice(&strip.pixels[from..from + 40]);
                }
                fs::write(
                    directory.join(format!("strip-{strip_id}.px8")),
                    &strip.pixels,
                )?;
                strips_written += 1;
            }
            fs::write(directory.join("chain-frame.fb1"), &reconstructed)?;

            // --- the strip names its snapshot --------------------------------
            //
            // Four RENDER-BUFFERED/5 packets against the accounts the relayer
            // uses, in the order a relayer meets them.  The first is the
            // positive control: the same strip, named correctly, has to render
            // and has to be byte-identical to the offline RENDER's strip 7, so
            // the buffered path is not being waved through on a weaker check
            // than the gate above applies to it.  The next two are the
            // malformed and stale shapes.  The fourth is the real thing: a
            // strip built for the previous frame, arriving after this frame's
            // SNAPSHOT-LIVE has already replaced the buffer.
            let probe_strip = 7u8;
            let positive_ix = session.render_buffered_ix(
                    static_key,
                    snapshot_key,
                    rsr::Request::render_buffered(
                            0,
                            probe_strip,
                            snapshot.snapshot_hash,
                        ),
                );
                let positive = session.send(vec![positive_ix], &[], RENDER_LIMIT).await?;
            if positive.result.is_err() {
                return Err(format!(
                    "RENDER-BUFFERED positive control at {last}: {:?}",
                    positive.result
                )
                .into());
            }
            let buffered_units = base::units(&positive)?;
            let probe_key = rsn3::strip_address(&base::PROGRAM, &snapshot_key, probe_strip).0;
            // `reconstructed` is the frame RENDER/1 just assembled, and
            // slice_frame pulls one strip's 40 columns out of a 320-wide
            // frame, so this compares the buffered strip against the offline
            // strip of the same snapshot.  The host frame is not in scope here
            // -- it is taken in its own session after the sweep, because
            // R_RenderPlayerView is not a pure read of the committed ST1 -- and
            // section 2b below compares the offline strips against the host.
            // Equal-to-RENDER plus RENDER-equal-to-host is equal-to-host.
            let probe_bytes = session.data(probe_key).await?;
            let probe_pixels = rsn3::validate_strip(&probe_bytes)?.pixels.to_vec();
            let positive_equal =
                probe_pixels == slice_frame(&reconstructed, usize::from(probe_strip));

            let zero_name_ix = session.render_buffered_ix(
                    static_key,
                    snapshot_key,
                    rsr::Request::render_buffered(0, probe_strip, [0; 32]),
                );
                let zero_name = session.send(vec![zero_name_ix], &[], RENDER_LIMIT).await?;
            let zero_name_code = custom_code(&zero_name);

            // One bit flipped in the name the account is holding: same length,
            // same accounts, same owner, same everything except that it is not
            // this snapshot.
            let mut wrong_name = snapshot.snapshot_hash;
            wrong_name[17] ^= 0x40;
            let stale_name_ix = session.render_buffered_ix(
                    static_key,
                    snapshot_key,
                    rsr::Request::render_buffered(0, probe_strip, wrong_name),
                );
                let stale_name = session.send(vec![stale_name_ix], &[], RENDER_LIMIT).await?;
            let stale_name_code = custom_code(&stale_name);

            // The relayer's actual failure mode, on chain: last frame's name,
            // this frame's buffer.  Only available from the second frame on,
            // because the first frame has no predecessor to be late against.
            let late_frame = if let Some(previous) = previous_snapshot_hash {
                let late_ix = session.render_buffered_ix(
                        static_key,
                        snapshot_key,
                        rsr::Request::render_buffered(0, probe_strip, previous),
                    );
                    let late = session.send(vec![late_ix], &[], RENDER_LIMIT).await?;
                custom_code(&late)
            } else {
                None
            };
            // The refusals must not have touched the strip: a refused strip
            // that still rewrote its pixels would be worse than one that did
            // not render at all, because the sender would read a frame it
            // believes it just published.
            let after_bytes = session.data(probe_key).await?;
            let after_refusals = rsn3::validate_strip(&after_bytes)?.pixels.to_vec();
            let refusals_left_the_strip_alone = after_refusals == probe_pixels;

            let static_after_bytes = session.data(static_key).await?;
            let changed_offsets: Vec<usize> = static_before_bytes
                .iter()
                .zip(&static_after_bytes)
                .enumerate()
                .filter(|(_, (before, after))| before != after)
                .map(|(offset, _)| offset)
                .collect();
            // Re-render the same strip of the same snapshot.  Byte-identical
            // account bytes the second time is the no-accumulation claim: if a
            // render left anything behind -- a frame counter, a per-sector
            // mark, a linedef's ML_MAPPED, a stale subsector identity -- the
            // second render would start from it and the two digests would
            // differ.  This is the check that would have caught the class the
            // purity restore exists for, and it is cheap.
            let replay_ix = session.render_ix(
                static_key,
                snapshot_key,
                probe_strip,
                rsr::Request::render(probe_strip, last, snapshot.snapshot_hash),
            );
            let replay = session.send(vec![replay_ix], &[], RENDER_LIMIT).await?;
            if replay.result.is_err() {
                return Err(format!("RENDER replay strip {probe_strip} at {last}: {:?}", replay.result).into());
            }
            let after_replay = session.data(static_key).await?;
            let replay_is_idempotent = base::sha(&after_replay) == base::sha(&static_after_bytes);
            buffered_rows.push(json!({
                "cursor": last,
                "positive_control_rendered": true,
                "positive_control_pixels_match_offline_render": positive_equal,
                "buffered_strip_cu": buffered_units,
                "zero_name_refusal": zero_name_code,
                "one_bit_wrong_name_refusal": stale_name_code,
                "previous_frame_name_refusal": late_frame,
                "refusals_left_the_strip_account_alone": refusals_left_the_strip_alone,
                "committed_state_sha256_before_strips": state_before,
                "committed_state_unchanged_by_the_strips": state_before
                    == base::sha(&session.data(session.keys.state.pubkey()).await?),
                "committed_mirror_unchanged_by_the_strips": mirror_before
                    == base::sha(&session.data(session.keys.mirror.pubkey()).await?),
                "replaying_the_last_strip_is_byte_identical": replay_is_idempotent,
                "working_context_bytes_moved_by_the_frame": changed_offsets.len(),
                "working_context_first_moved_offsets": changed_offsets.iter().take(16).collect::<Vec<_>>(),
            }));
            if !replay_is_idempotent {
                return Err(format!(
                    "re-rendering strip {probe_strip} at {last} did not reproduce the same static \
                     account bytes, so a render accumulated something"
                )
                .into());
            }
            if state_before != base::sha(&session.data(session.keys.state.pubkey()).await?) {
                return Err(format!("the strips at {last} changed the committed ST1 state").into());
            }
            previous_snapshot_hash = Some(snapshot.snapshot_hash);

            if !positive_equal {
                return Err(format!(
                    "RENDER-BUFFERED positive control at {last} does not match the native strip"
                )
                .into());
            }
            if zero_name_code != Some(rsr::REFUSE_MALFORMED) {
                return Err(format!(
                    "a buffered strip with no snapshot name was refused {zero_name_code:?}, not {}",
                    rsr::REFUSE_MALFORMED
                )
                .into());
            }
            if stale_name_code != Some(rsr::REFUSE_SNAPSHOT_STALE) {
                return Err(format!(
                    "a buffered strip naming the wrong snapshot was refused {stale_name_code:?}, not {}",
                    rsr::REFUSE_SNAPSHOT_STALE
                )
                .into());
            }
            if late_frame.is_some() && late_frame != Some(rsr::REFUSE_SNAPSHOT_STALE) {
                return Err(format!(
                    "a late frame's buffered strip was refused {late_frame:?}, not {}",
                    rsr::REFUSE_SNAPSHOT_STALE
                )
                .into());
            }
            if !refusals_left_the_strip_alone {
                return Err(format!("a refused buffered strip rewrote its pixels at {last}").into());
            }


            // --- the depth: every buffer inside the count, and the one past it
            //
            // Four buffers is the arithmetic's answer *before* the serial
            // submit loop is fixed rather than after: the depth note measures
            // no gain at all from four today, the knee of
            // `min(1 / cadence, buffers / occupancy)` is 1.72 buffers on the
            // `r9s64b` cycle, and halving the loop's own 0.216 s a frame puts
            // the knee at 3.1 -- so four holds until the cadence drops below
            // 0.105 s.  What has to be true of the *program* is therefore not
            // that four is faster.  It is that a number past the end is
            // refused and a number inside it names its own account, its own
            // strips and its own picture, and that all of it is the picture the
            // offline RENDER already drew.
            //
            // Three claims per buffer, and each of them is a claim the two
            // buffers could already make:
            //
            //  * the buffered snapshot binds the committed cursor and root and
            //    carries the ABI identifier its own number selects -- a buffer
            //    2 snapshot that carried buffer 0's ABI would be a second name
            //    for buffer 0 with a different address, and the strip that came
            //    out of it would still be right, which is exactly the shape of
            //    bug that is invisible until two frames are in flight;
            //  * all eight strips are byte-identical to the eight the offline
            //    RENDER produced from the same committed cursor, so the picture
            //    does not depend on which buffer it was drawn into;
            //  * the render is a function of the strip and not of the order the
            //    buffers were visited, which the replay below re-checks with
            //    the whole depth sweep already behind it.
            let mut depth_equal = 0usize;
            let mut depth_idempotent = 0usize;
            // The static account's digest after each buffer's eight strips, so
            // the first buffer's can be re-rendered against it after the others
            // have been visited.
            let mut depth_static_digests: Vec<String> = Vec::new();
            for buffer in 1..rsn3::BUFFER_COUNT {
                let buffer_snapshot = if buffer == PROVISIONED_BUFFER {
                    provisioned_snapshot.ok_or("the provisioned buffer has no address")?
                } else {
                    rsn3::buffered_snapshot_address(
                        &base::PROGRAM,
                        &session.keys.context.pubkey(),
                        buffer,
                    )
                    .ok_or_else(|| -> Box<dyn Error> {
                        format!("buffer {buffer} has no address").into()
                    })?
                    .0
                };
                let snap_ix =
                    session.snapshot_live_buffered_ix(static_key, buffer_snapshot, buffer);
                let snap = session.send(vec![snap_ix], &[], RENDER_LIMIT).await?;
                if snap.result.is_err() {
                    return Err(format!(
                        "SNAPSHOT-LIVE-BUFFERED buffer {buffer} at {last}: {:?}",
                        snap.result
                    )
                    .into());
                }
                let snapshot_cu = base::units(&snap)?;
                let buffer_bytes = session.data(buffer_snapshot).await?;
                let view = rsn3::validate_buffered_snapshot_for(&buffer_bytes, buffer)?;
                if view.cursor != last || view.state_root != root {
                    return Err(format!(
                        "buffer {buffer}'s snapshot binds cursor {} and root {}, not {last} and the \
                         committed root",
                        view.cursor,
                        common::hex(&view.state_root)
                    )
                    .into());
                }
                let mut frame = vec![0u8; resource_sbf::RI2_FRAME_BYTES];
                let mut strip_cu: Vec<u64> = Vec::new();
                let mut well_formed = 0usize;
                for strip_id in 0..8u8 {
                    let request =
                        rsr::Request::render_buffered(buffer, strip_id, view.snapshot_hash);
                    let ix = session.render_buffered_ix(static_key, buffer_snapshot, request);
                    let rendered = session.send(vec![ix], &[], RENDER_LIMIT).await?;
                    if rendered.result.is_err() {
                        return Err(format!(
                            "RENDER-BUFFERED buffer {buffer} strip {strip_id} at {last}: {:?}",
                            rendered.result
                        )
                        .into());
                    }
                    strip_cu.push(base::units(&rendered)?);
                    let key = rsn3::strip_address(&base::PROGRAM, &buffer_snapshot, strip_id).0;
                    let strip_bytes = session.data(key).await?;
                    let strip = rsn3::validate_strip(&strip_bytes)?;
                    if strip.cursor != last
                        || strip.state_root != root
                        || strip.snapshot_hash != view.snapshot_hash
                    {
                        return Err(format!(
                            "buffer {buffer} strip {strip_id} at {last} does not bind its own \
                             snapshot"
                        )
                        .into());
                    }
                    well_formed += usize::from(strip.strip_id == u32::from(strip_id));
                    for row in 0..200 {
                        let from = row * 40;
                        let to = row * 320 + usize::from(strip_id) * 40;
                        frame[to..to + 40].copy_from_slice(&strip.pixels[from..from + 40]);
                    }
                }
                let frame_equal = frame == reconstructed;
                let frame_sha = base::sha(&frame);
                if !frame_equal {
                    return Err(format!(
                        "buffer {buffer}'s eight strips at {last} reassemble to a different frame \
                         than the offline RENDER's ({frame_sha} against {})",
                        base::sha(&reconstructed)
                    )
                    .into());
                }
                depth_equal += usize::from(frame_equal);
                depth_frames += usize::from(frame_equal);
                depth_buffer_frames += 1;
                let before_replay = session.data(static_key).await?;
                depth_static_digests.push(base::sha(&before_replay));
                // The purity claim, made again for this buffer: re-render the
                // frame's last strip with nothing in between and require the
                // *strip account* -- header and all 8,000 pixel bytes -- to come
                // back identical.  A new buffer number has to get the same answer
                // as buffer 0, because the answer is a property of the render and
                // not of which account the snapshot went to.
                //
                // The comparison is on the strip rather than on the static
                // resource account, and the reason is measured rather than
                // assumed.  The working copy of the engine context inside the
                // static account is not byte-stable across transactions: the
                // strip render allocates its framebuffer and leaves viewimage,
                // ylookup and screens[] pointing into that block, so a re-render
                // two transactions later leaves a different address there.  How
                // much moves, and at which offsets, is recorded below rather
                // than argued.  The bytes the gate can compare across the depth
                // are the strips and the committed ST1 and MP1 accounts.
                let probe_key = rsn3::strip_address(&base::PROGRAM, &buffer_snapshot, probe_strip).0;
                let strip_before = session.data(probe_key).await?;
                let replay_ix = session.render_buffered_ix(
                    static_key,
                    buffer_snapshot,
                    rsr::Request::render_buffered(buffer, probe_strip, view.snapshot_hash),
                );
                let replayed = session.send(vec![replay_ix], &[], RENDER_LIMIT).await?;
                if replayed.result.is_err() {
                    return Err(format!(
                        "RENDER-BUFFERED buffer {buffer} strip {probe_strip} replay at {last}: {:?}",
                        replayed.result
                    )
                    .into());
                }
                let strip_after = session.data(probe_key).await?;
                // Read after the replay, not before: this is the measurement of
                // what a re-render leaves in the working copy of the context, and
                // reading it on the other side of the transaction would report
                // the program back to itself.
                let after_replay_bytes = session.data(static_key).await?;
                let buffer_is_idempotent = strip_after == strip_before;
                let moved: Vec<usize> = before_replay
                    .iter()
                    .zip(&after_replay_bytes)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(offset, _)| offset)
                    .collect();
                if !buffer_is_idempotent {
                    return Err(format!(
                        "re-rendering buffer {buffer}'s strip {probe_strip} at {last} did not \
                         reproduce the strip account byte for byte"
                    )
                    .into());
                }
                depth_working_moved.push(json!({
                    "cursor": last,
                    "buffer": buffer,
                    "working_context_bytes_moved_by_the_replay": moved.len(),
                    "working_context_offsets": moved,
                }));
                depth_idempotent += usize::from(buffer_is_idempotent);
                depth_rows.push(json!({
                    "cursor": last,
                    "buffer": buffer,
                    "provisioned_by_the_program": buffer == PROVISIONED_BUFFER,
                    "abi": if buffer == 0 { "DOOM-RSN/3" } else { "buffered" },
                    "snapshot_sha256": common::hex(&view.snapshot_hash),
                    "snapshot_cu": snapshot_cu,
                    "strip_cu": strip_cu.clone(),
                    "strip_cu_max": strip_cu.iter().max(),
                    "strips_well_formed": well_formed,
                    "frame_sha256": frame_sha,
                    "frame_matches_the_offline_render": frame_equal,
                    "static_account_sha256_after_this_buffer": depth_static_digests
                        .last()
                        .cloned()
                        .unwrap_or_default(),
                }));
            }

            // The fifth buffer, on every wire that carries a buffer number, with
            // accounts that are otherwise exactly what the accepted packet used.
            // Each of these is refused by the number alone, before the program
            // looks at an account, so the refusal is the bound and not a
            // complaint about the accounts -- which is what makes it a bound the
            // relayer can rely on rather than a coincidence.
            let past = rsn3::BUFFER_COUNT;
            let buffer0_snapshot_before_refusals = base::sha(&session.data(snapshot_key).await?);
            let snapshot_past_ix =
                session.snapshot_live_buffered_ix(static_key, snapshot_key, past);
            let snapshot_past = session.send(vec![snapshot_past_ix], &[], RENDER_LIMIT).await?;
            let snapshot_past_code = custom_code(&snapshot_past);
            let snapshot_past_cu = base::units(&snapshot_past).ok();
            let render_past_ix = session.render_buffered_ix(
                static_key,
                snapshot_key,
                rsr::Request::render_buffered(past, probe_strip, snapshot.snapshot_hash),
            );
            let render_past = session.send(vec![render_past_ix], &[], RENDER_LIMIT).await?;
            let render_past_code = custom_code(&render_past);
            let render_past_cu = base::units(&render_past).ok();
            let init_past_ix = session.init_buffered_ix(snapshot_key, past);
            let init_past = session
                .send(vec![init_past_ix], &[&authority], RENDER_LIMIT)
                .await?;
            let init_past_code = custom_code(&init_past);
            let init_past_cu = base::units(&init_past).ok();
            let grow_past_ix = session.grow_buffered_ix(snapshot_key, past);
            let grow_past = session.send(vec![grow_past_ix], &[], RENDER_LIMIT).await?;
            let grow_past_code = custom_code(&grow_past);
            let grow_past_cu = base::units(&grow_past).ok();
            depth_refusals.push(json!({
                "cursor": last,
                "buffer": past,
                "buffer_count": rsn3::BUFFER_COUNT,
                "note": "the three RSN/3 wires refuse 297 (rsn3::REFUSE_MALFORMED) and RSR/2 RENDER-BUFFERED refuses 301 (rsr::REFUSE_MALFORMED); the bound is one constant read by both modules, but the two have their own refusal numbering",
                "snapshot_live_buffered_refusal": snapshot_past_code,
                "snapshot_live_buffered_cu": snapshot_past_cu,
                "render_buffered_refusal": render_past_code,
                "render_buffered_cu": render_past_cu,
                "init_buffered_refusal": init_past_code,
                "init_buffered_cu": init_past_cu,
                "grow_buffered_refusal": grow_past_code,
                "grow_buffered_cu": grow_past_cu,
                "buffer_0_snapshot_sha256_before": buffer0_snapshot_before_refusals,
                "refused_packets_left_buffer_0_alone": true,
            }));

            // The depth's own byte-exactness claim, across buffers.  Buffer 1's
            // last strip, re-rendered after buffers 2 and 3 have been visited,
            // has to be the bytes the offline RENDER produced at this cursor --
            // the picture may not depend on which buffer it was drawn into or on
            // what was drawn since.
            //
            // What is deliberately *not* claimed here is that the static account
            // comes back to the same digest.  It cannot, and the reason is worth
            // writing down: the strip render `calloc`s its framebuffer and
            // leaves `viewimage`, `ylookup` and `screens[]` pointing into that
            // block in the working context, so the account's bytes are a
            // function of where in the transaction sequence the read was taken,
            // not only of the picture.  The per-buffer replay above is taken with
            // nothing in between, which is why that one can compare digests, and
            // the byte delta across the depth sweep is reported rather than
            // asserted so the size of the effect is on the record.
            let first_buffer = 1u8;
            let (first_snapshot, _) = rsn3::buffered_snapshot_address(
                &base::PROGRAM,
                &session.keys.context.pubkey(),
                first_buffer,
            )
            .ok_or("buffer 1 has no address")?;
            let first_bytes = session.data(first_snapshot).await?;
            let first_view = rsn3::validate_buffered_snapshot_for(&first_bytes, first_buffer)?;
            let first_replay_ix = session.render_buffered_ix(
                static_key,
                first_snapshot,
                rsr::Request::render_buffered(
                    first_buffer,
                    probe_strip,
                    first_view.snapshot_hash,
                ),
            );
            let first_replay = session
                .send(vec![first_replay_ix], &[], RENDER_LIMIT)
                .await?;
            if first_replay.result.is_err() {
                return Err(format!(
                    "RENDER-BUFFERED buffer {first_buffer} strip {probe_strip} replay at {last}: \
                     {:?}",
                    first_replay.result
                )
                .into());
            }
            let first_strip_key =
                rsn3::strip_address(&base::PROGRAM, &first_snapshot, probe_strip).0;
            let first_pixels =
                rsn3::validate_strip(&session.data(first_strip_key).await?)?.pixels.to_vec();
            let first_stable = first_pixels == slice_frame(&reconstructed, usize::from(probe_strip));
            if !first_stable {
                return Err(format!(
                    "buffer {first_buffer}'s strip {probe_strip} at {last} is not the offline strip \
                     after buffers 2 and 3 had been rendered, so the picture depends on the buffer"
                )
                .into());
            }
            // The strip- byte-exactness claim across buffers: buffer 1's strips,
            // re-rendered after buffers 2 and 3 have been drawn into their own
            // accounts, are byte-identical to the eight the offline RENDER
            // produced at this cursor.  The picture may not depend on which
            // buffer it went to or on what has been drawn since.
            let first_buffer = 1u8;
            let (first_snapshot, _) = rsn3::buffered_snapshot_address(
                &base::PROGRAM,
                &session.keys.context.pubkey(),
                first_buffer,
            )
            .ok_or("buffer 1 has no address")?;
            let first_bytes = session.data(first_snapshot).await?;
            let first_view = rsn3::validate_buffered_snapshot_for(&first_bytes, first_buffer)?;
            for strip_id in 0..8u8 {
                let ix = session.render_buffered_ix(
                    static_key,
                    first_snapshot,
                    rsr::Request::render_buffered(
                        first_buffer,
                        strip_id,
                        first_view.snapshot_hash,
                    ),
                );
                let again = session.send(vec![ix], &[], RENDER_LIMIT).await?;
                if again.result.is_err() {
                    return Err(format!(
                        "RENDER-BUFFERED buffer {first_buffer} strip {strip_id} re-sweep at {last}: \
                         {:?}",
                        again.result
                    )
                    .into());
                }
            }
            let first_strip_key =
                rsn3::strip_address(&base::PROGRAM, &first_snapshot, probe_strip).0;
            let first_pixels =
                rsn3::validate_strip(&session.data(first_strip_key).await?)?.pixels.to_vec();
            let first_stable = first_pixels == slice_frame(&reconstructed, usize::from(probe_strip));
            if !first_stable {
                return Err(format!(
                    "buffer {first_buffer}'s strip {probe_strip} at {last} is not the offline strip \
                     after buffers 2 and 3 had been rendered, so the picture depends on the buffer"
                )
                .into());
            }
            depth_across_frames += usize::from(first_stable);
            depth_rows.push(json!({
                "cursor": last,
                "buffer": first_buffer,
                "across_depth_strip_sha256": base::sha(&first_pixels),
                "across_depth_strip_matches_the_offline_render": first_stable,
            }));
            if depth_idempotent != usize::from(rsn3::BUFFER_COUNT - 1) {
                return Err(format!(
                    "{depth_idempotent} of {} buffers re-rendered their last strip to the same \
                     static account bytes at {last}",
                    rsn3::BUFFER_COUNT - 1
                )
                .into());
            }
            if depth_equal != usize::from(rsn3::BUFFER_COUNT - 1) {
                return Err(format!(
                    "{depth_equal} of {} buffers reproduced the offline frame at {last}",
                    rsn3::BUFFER_COUNT - 1
                )
                .into());
            }
            // Every wire that carries a buffer number refuses the one past the
            // end, and refuses it as malformed rather than as an account
            // complaint: the accounts on all four packets are the ones the
            // accepted packet used, so the number is the only thing that differs.
            //
            // Two numbers, because the bound lives in two modules and each has
            // its own: the three RSN/3 wires refuse 297 and RSR/2's
            // RENDER-BUFFERED refuses 301.  A single "297" here would be wrong
            // for one of the four, and a relayer matching on the number needs to
            // know which is which.
            for (wire, code, expected) in [
                ("SNAPSHOT-LIVE-BUFFERED", snapshot_past_code, rsn3::REFUSE_MALFORMED),
                ("RENDER-BUFFERED", render_past_code, rsr::REFUSE_MALFORMED),
                ("INIT-BUFFERED", init_past_code, rsn3::REFUSE_MALFORMED),
                ("GROW-BUFFERED", grow_past_code, rsn3::REFUSE_MALFORMED),
            ] {
                if code != Some(expected) {
                    return Err(format!(
                        "buffer {past} on {wire} was refused {code:?}, not {expected}"
                    )
                    .into());
                }
            }
            if buffer0_snapshot_before_refusals
                != base::sha(&session.data(snapshot_key).await?)
            {
                return Err(format!(
                    "a refused buffer-{past} packet changed buffer 0's snapshot at {last}"
                )
                .into());
            }

            let (segment, forward, strafe) = common::r8l_segment(last - 1);
            let (demo_end_cursor, base_state) = if new_game_start {
                (0u32, baseline_state.clone())
            } else {
                st1_at_demo_end
                    .clone()
                    .ok_or("the run never committed a cursor at or before the demo end")?
            };
            let changed = changed_fields(&base_state, &state);
            let chain_frame_sha = base::sha(&reconstructed);
            chain_frames.push((last, reconstructed));
            chain_states.push((last, state.clone()));
            let bytes_changed = base_state
                .iter()
                .zip(&state)
                .filter(|(a, b)| a != b)
                .count();
            position_rows.push(json!({
                "cursor": last,
                "tics_past_demo_end": if new_game_start { json!(null) } else { json!(last - demo_tics) },
                "compared_against_cursor": demo_end_cursor,
                "input_segment": segment,
                "segment_moves_forward": forward,
                "segment_strafes": strafe,
                "st1_changed_fields_vs_demo_end": changed.len(),
                "st1_changed_bytes_vs_demo_end": bytes_changed,
                "first_changed_fields": changed.iter().take(8).map(|(offset, before, after)| json!({"offset":offset,"before":before,"after":after})).collect::<Vec<_>>(),
                "chain_frame_sha256": chain_frame_sha,
            }));
            render_rows.push(json!({
                "cursor": last,
                "tics_past_demo_end": if new_game_start { json!(null) } else { json!(last - demo_tics) },
                "snapshot_live_cu": snapshot_units,
                "strip_cu": strip_units.clone(),
                "strip_cu_max": strip_units.iter().max(),
                "strips_well_formed": strip_equal,
                "chain_frame_sha256": chain_frame_sha,
            }));
        }
        cursor = last;
    }
    sweeps.flush()?;
    let sweep_seconds = started.elapsed().as_secs_f64();
    if position_rows
        .iter()
        .all(|row| row["st1_changed_bytes_vs_demo_end"] == json!(0))
    {
        return Err(if new_game_start {
            "the ST1 image did not change from the start state".into()
        } else {
            "the ST1 image did not change past the demo end".into()
        });
    }
    if chain_frames.len() < 2 {
        return Err("fewer than two rendered cursors past the demo end".into());
    }

    // --- 2b. the host frame at each rendered cursor ---------------------------
    // One fresh session per cursor: `R_RenderPlayerView` moves committed ST1
    // bytes, so a replay that rendered at an earlier cursor could not stand in
    // for the chain at a later one.  The perturbation is measured, not hidden.
    let mut previous_frame: Option<(u32, Vec<u8>)> = None;
    for (index, (at, chain_frame)) in chain_frames.iter().enumerate() {
        let chain_state = &chain_states[index].1;
        let (frame, moved_st1) = host_frame_at(&capture_data, &commands, *at)?;
        let directory = receipt.join(format!("strips/cursor-{at:05}"));
        fs::write(directory.join("host-frame.fb1"), &frame)?;
        let mut strip_equal = 0usize;
        for strip_id in 0..8u8 {
            let bytes = fs::read(directory.join(format!("strip-{strip_id}.px8")))?;
            if bytes == slice_frame(&frame, usize::from(strip_id)) {
                strip_equal += 1;
            }
        }
        let equal = chain_frame == &frame;
        let differing = previous_frame
            .as_ref()
            .map(|(cursor, previous)| {
                json!({
                    "from_cursor": cursor,
                    "differing_pixels": differing_pixels(previous, &frame),
                })
            })
            .unwrap_or(serde_json::Value::Null);
        // The null control: the same capture and cursors with every staged
        // command zeroed, so whatever the live-input run changed in committed
        // state and in the view is attributable to the input and not to the
        // level clock.
        let (neutral_frame, _) = host_frame_at(&capture_data, &neutral, *at)?;
        fs::write(directory.join("neutral-frame.fb1"), &neutral_frame)?;
        let (neutral_state, _) = host_state_at(&capture_data, &neutral, *at)?;
        let neutral_fields = changed_fields(&neutral_state, chain_state);
        let mut movers: Vec<(usize, i32, i32)> = neutral_fields.clone();
        movers.sort_by_key(|(_, before, after)| -i64::from(after.wrapping_sub(*before)).abs());
        render_rows[index]["neutral_control"] = json!({
            "frame_differing_pixels": differing_pixels(&neutral_frame, &frame),
            "committed_st1_differing_bytes": neutral_state
                .iter()
                .zip(&chain_states[index].1[..neutral_state.len()])
                .filter(|(a, b)| a != b)
                .count(),
            "committed_st1_differing_fields": neutral_fields.len(),
            "largest_field_moves": movers.iter().take(6).map(|(offset, before, after)| json!({"offset":offset,"before":before,"after":after})).collect::<Vec<_>>(),
        });
        render_rows[index]["strips_equal_to_host_pixels"] = json!(strip_equal);
        render_rows[index]["host_frame_sha256"] = json!(base::sha(&frame));
        render_rows[index]["reassembled_equals_host"] = json!(equal);
        render_rows[index]["host_render_moved_committed_st1_bytes"] = json!(moved_st1);
        render_rows[index]["differing_pixels_from_previous_cursor"] = differing;
        if index > 0 {
            let (previous_cursor, previous_state) = &chain_states[index - 1];
            render_rows[index]["st1_changed_fields_from_previous_cursor"] = json!({
                "from_cursor": previous_cursor,
                "fields": changed_fields(previous_state, &chain_states[index].1)
                    .iter()
                    .take(8)
                    .map(|(offset, before, after)| json!({"offset":offset,"before":before,"after":after}))
                    .collect::<Vec<_>>(),
                "field_count": changed_fields(previous_state, &chain_states[index].1).len(),
            });
        }
        if strip_equal != 8 || !equal {
            return Err(format!(
                "the chain frame at {at} is not the host frame: strips {strip_equal}/8, frame equal {equal}"
            )
            .into());
        }
        previous_frame = Some((*at, frame));
    }

    // --- 3. the deliberately starved tic, and why it cannot be staged -------
    // `DoomControllerConsumed()` reads zero exactly when G_Ticker ran with no
    // console player in the game, and that is the only way a tic runs without
    // consuming the command it was armed with: the pre-tic guard already
    // refuses a pending gameaction, a non-level gamestate, a paused tic and a
    // reborn player, and ArmV2 always arms.  The state codec then closes the
    // last door: `ds_scan_state` requires playeringame[consoleplayer], so no
    // ST1 image a context can be reconstructed from is starved either.  The
    // gate measures both halves of that instead of asserting it: the host
    // import refuses the doctored image, and so does E2R/5 on the image.
    let (source_state, source_mirror) = host_state_at(&capture_data, &commands, starve_cursor)?;
    if i32_at(&source_state, common::ST1_GAMESTATE_OFFSET) != common::GAMESTATE_LEVEL {
        return Err("the starved control needs a GS_LEVEL state".into());
    }
    let console_player = i32_at(&source_state, common::ST1_CONSOLEPLAYER_OFFSET);
    let flags: [u8; 4] = source_state
        [common::ST1_PLAYERINGAME_OFFSET..common::ST1_PLAYERINGAME_OFFSET + 4]
        .try_into()
        .unwrap();
    if flags != [1, 0, 0, 0] {
        return Err(format!(
            "unexpected playeringame bytes {flags:?} at offset {}",
            common::ST1_PLAYERINGAME_OFFSET
        )
        .into());
    }
    let mut starved_state = source_state.clone();
    starved_state[common::ST1_PLAYERINGAME_OFFSET + console_player as usize] = 0;
    let host_unmodified = host_import_and_step(&capture_data, &source_state)?;
    let host_starved = host_import_and_step(&capture_data, &starved_state)?;
    fs::write(receipt.join("starved.st1"), &starved_state)?;
    fs::write(receipt.join("starved-source.st1"), &source_state)?;
    if host_unmodified != 0 {
        return Err(
            format!("the unmodified state did not run a consumed tic: {host_unmodified}").into(),
        );
    }
    if host_starved >= 0 {
        return Err(format!(
            "the host import accepted a state with no console player in the game (step rc={host_starved})"
        )
        .into());
    }

    // The same doctored image, reconstructed by the image under test.
    let starve_keys = session_keys(Keypair::new());
    let starve_bank = bank_with(
        &capture_data,
        starve_keys.context.pubkey(),
        starve_keys.state.pubkey(),
        starve_keys.mirror.pubkey(),
        starve_keys.controller.pubkey(),
        starve_keys.authority.pubkey(),
        None,
        &layout,
    );
    let starve_state_signer = starve_keys.state.pubkey();
    let starve_ctx = starve_bank.start_with_context().await;
    let mut starve = Session {
        ctx: starve_ctx,
        keys: starve_keys,
        nonce: 0,
        cursor_before: 0,
    };
    let starve_authority = starve.keys.authority.insecure_clone();
    let starve_context_signer = starve.keys.context.insecure_clone();
    let starve_gametic = base::u32_at(&source_state, 20);
    let starve_controller = controller_for_gametic(&controller, starve_gametic)?;
    let starve_state_signer_kp = starve.keys.state.insecure_clone();
    let starve_mirror_signer_kp = starve.keys.mirror.insecure_clone();
    let starve_controller_signer_kp = starve.keys.controller.insecure_clone();
    let staged_starved = base::padded_state(&starved_state);
    starve
        .stage_account(&starve_state_signer_kp, &staged_starved)
        .await?;
    starve
        .stage_account(&starve_mirror_signer_kp, &source_mirror)
        .await?;
    starve
        .stage_account(&starve_controller_signer_kp, &starve_controller)
        .await?;
    let starve_continuation = Keypair::new();
    create_owned(
        &mut starve.ctx,
        &starve_continuation,
        continuation_split_sbf::CONTINUATION_BYTES,
        Rent::default().minimum_balance(continuation_split_sbf::CONTINUATION_BYTES),
    )
    .await?;
    // The reconstruction is eight phases; the decode lives in one of them, so
    // the refusal is wherever it lands.  Every phase is sent and the first
    // refusal is recorded.
    let starved_state_len = starved_state.len();
    let mut starve_refusal = None;
    let mut starve_phase_cu = Vec::new();
    for cursor in 0..E2R_PHASES {
        let operation = if cursor == 0 {
            continuation_split_sbf::PREPARE
        } else {
            continuation_split_sbf::STEP
        };
        let ix = starve.e2r_ix(
            starve_continuation.pubkey(),
            operation,
            cursor,
            starved_state_len,
        );
        let signers: Vec<&Keypair> = if cursor == 0 {
            vec![&starve_authority, &starve_context_signer]
        } else {
            vec![&starve_authority]
        };
        let result = starve.send(vec![ix], &signers, base::NORMAL_LIMIT).await?;
        starve_phase_cu.push(base::units(&result)?);
        if result.result.is_err() {
            starve_refusal = Some((cursor, format!("{:?}", result.result)));
            break;
        }
    }
    let prepare_code = starve_refusal
        .as_ref()
        .and_then(|(cursor, text)| custom_code_text(cursor, text));
    if starve_refusal.is_none() {
        return Err("E2R/5 reconstructed a state with no console player in the game".into());
    }
    if starve.data(starve_state_signer).await? != staged_starved {
        return Err("the refused reconstruction changed the staged ST1".into());
    }

    // The same PREPARE from the unmodified state is accepted, so the refusal
    // is the state and not the packet.
    let good_keys = session_keys(Keypair::new());
    let good_bank = bank_with(
        &capture_data,
        good_keys.context.pubkey(),
        good_keys.state.pubkey(),
        good_keys.mirror.pubkey(),
        good_keys.controller.pubkey(),
        good_keys.authority.pubkey(),
        None,
        &layout,
    );
    let good_state_signer = good_keys.state.pubkey();
    let good_ctx = good_bank.start_with_context().await;
    let mut good = Session {
        ctx: good_ctx,
        keys: good_keys,
        nonce: 0,
        cursor_before: 0,
    };
    let good_authority = good.keys.authority.insecure_clone();
    let good_context_signer = good.keys.context.insecure_clone();
    let good_state_signer = good.keys.state.pubkey();
    let (good_phases, good_init_cu) = reconstruct(
        &mut good,
        &base::padded_state(&source_state),
        &source_mirror,
        &controller_for_gametic(&controller, starve_gametic)?,
        &capture_data,
        capacity,
    )
    .await?;
    // The good context really does run live-input tics, with the receipt
    // checked on every one of them.
    let good_batch = &commands[..advance_tics as usize * 4];
    let good_deposit = good
        .send(
            vec![good.deposit_ix(0, good_batch)],
            &[&good_authority],
            base::NORMAL_LIMIT,
        )
        .await?;
    if good_deposit.result.is_err() {
        return Err(format!("control DEPOSIT {:?}", good_deposit.result).into());
    }
    good.cursor_before = 0;
    let good_advance = good
        .send(
            vec![good.advance_ix(sim2::ADVANCE_STARVED, advance_tics)],
            &[&good_authority],
            base::NORMAL_LIMIT,
        )
        .await?;
    if good_advance.result.is_err() {
        return Err(format!(
            "ADVANCE/5 refused a good first batch: {:?}",
            good_advance.result
        )
        .into());
    }
    let good_view = good.view().await?;
    if good_view.cursor != advance_tics {
        return Err("the control context did not commit its first batch".into());
    }
    let _ = (good_state_signer, good_init_cu, good_context_signer);
    let controls = json!({
        "starved_tic_is_unreachable_from_a_reconstructable_state": {
            "why": "G_Ticker consumes the armed command for every player in the game; the pre-tic guard refuses a pending gameaction, a non-level gamestate, paused and a reborn player, and ArmV2 always arms.  ds_scan_state then requires playeringame[consoleplayer], so no importable ST1 starves either.",
            "playeringame_offset": common::ST1_PLAYERINGAME_OFFSET,
            "console_player": console_player,
            "playeringame_bytes": flags,
            "host_import_and_step_unmodified": host_unmodified,
            "host_import_and_step_starved": host_starved,
            "host_starved_is_a_refused_import": host_starved < 0,
            "e2r5_prepare_refusal_code": prepare_code,
            "staged_state_unchanged_after_refusal": true,
        },
        "unmodified_state_reconstructs_and_runs_live_input": {
            "e2r5_phase_cu": good_phases,
            "advance5_first_batch_cursor": good_view.cursor,
            "advance5_first_batch_cu": base::units(&good_advance)?,
        },
        "advance5_on_the_image": {
            "operation_byte": sim2::ADVANCE_STARVED,
            "sweeps": sent,
            "tics": tics,
            "refusals": 0,
        },
    });

    // --- 3b. RESET/9: the same slot, the same roots, from a fresh start -------
    // Everything above measured one session.  This measures that the slot can
    // serve another one: a sealed start account, one RESET/9, and the same
    // staged commands replayed from cursor 0 landing on the same ST1, MP1, state
    // root and input chain at every cursor the first run committed.  The
    // refusals come first, on the played slot, so each one is measured against
    // a state that really has something to lose.
    let (final_cursor, _, _, final_root, _) = *first_run
        .last()
        .ok_or("the run committed no batch to replay over")?;
    let staged_start = capture_data.state_account();
    let start_key = stage_start_account(&mut session, &capture_data.state).await?;
    let wrong_key =
        stage_start_account(&mut session, &doctored_start_state(&capture_data.state)).await?;
    let stranger = Keypair::new();
    let before_reset = (
        session.data(session.keys.state.pubkey()).await?,
        session.data(session.keys.mirror.pubkey()).await?,
        session.data(session.keys.buffer.pubkey()).await?,
        session.data(session.keys.meta).await?,
    );
    let mut reset_refusals = Vec::new();
    for (name, ix, signers, want) in [
        (
            "stranger_authority",
            session.reset_ix_as(start_key.pubkey(), &stranger),
            vec![&stranger],
            sim2::REFUSE_AUTHORITY,
        ),
        (
            "not_the_committed_start_state",
            session.reset_ix(wrong_key.pubkey(), sim2::RESET_START_STATE, false),
            vec![&authority],
            sim2::REFUSE_STATE_ROOT,
        ),
    ] {
        let result = session.send(vec![ix], &signers, base::NORMAL_LIMIT).await?;
        let code = custom_code(&result);
        if result.result.is_err() {
            let units = base::units(&result)?;
            if code != Some(want) {
                return Err(format!("{name}: refused with {code:?}, wanted {want}").into());
            }
            reset_refusals.push(json!({"case": name, "refused_with": code, "cu": units}));
        } else {
            return Err(format!("{name}: the image accepted a reset it must refuse").into());
        }
        let after = (
            session.data(session.keys.state.pubkey()).await?,
            session.data(session.keys.mirror.pubkey()).await?,
            session.data(session.keys.buffer.pubkey()).await?,
            session.data(session.keys.meta).await?,
        );
        if after != before_reset {
            return Err(format!("{name}: a refused reset changed an account").into());
        }
    }

    let reset_ix = session.reset_ix(start_key.pubkey(), sim2::RESET_START_STATE, false);
    let reset = session
        .send(vec![reset_ix], &[&authority], base::NORMAL_LIMIT)
        .await?;
    if reset.result.is_err() {
        return Err(format!("RESET/9 {:?}", reset.result).into());
    }
    let reset_cu = base::units(&reset)?;
    let view = session.view().await?;
    let start_root = sim2::verified_state_root(&staged_start, &capture_data.mirror)
        .map_err(|e| format!("start state root: {e:?}"))?
        .1;
    let state_after_reset = session.data(session.keys.state.pubkey()).await?;
    let mirror_after_reset = session.data(session.keys.mirror.pubkey()).await?;
    let buffer_after_reset = session.data(session.keys.buffer.pubkey()).await?;
    if view.cursor != 0
        || view.halt != 0
        || view.sweeps != 0
        || view.resets != 1
        || view.state_root != start_root
        || view.start_root != start_root
        || view.input_chain != sim2::chain_init(&session.keys.context.pubkey(), view.first_tic)
        || state_after_reset != staged_start
        || mirror_after_reset != capture_data.mirror
        || !buffer_after_reset[sim2::BUFFER_HEADER_BYTES..]
            .iter()
            .all(|byte| *byte == 0)
        || &buffer_after_reset[..8] != sim2::BUFFER_MAGIC
    {
        return Err(format!(
            "RESET/9 did not return the slot to its start state: cursor={} halt={} sweeps={} resets={} root={} start_root={} chain={} state={} mirror={} slots_cleared={}",
            view.cursor,
            view.halt,
            view.sweeps,
            view.resets,
            common::hex(&view.state_root) == common::hex(&start_root),
            view.start_root == start_root,
            view.input_chain == sim2::chain_init(&session.keys.context.pubkey(), view.first_tic),
            state_after_reset == staged_start,
            mirror_after_reset == capture_data.mirror,
            buffer_after_reset[sim2::BUFFER_HEADER_BYTES..].iter().all(|byte| *byte == 0),
        )
        .into());
    }

    // A renderer holding a frame from before the reset is refused: its cursor
    // and root no longer equal the meta.  The same packet with the reset's own
    // cursor and root is accepted, so the refusal is the binding and not the
    // packet.
    let stale = session.snapshot_ix(final_cursor, &final_root);
    let stale_result = session.send(vec![stale], &[], base::NORMAL_LIMIT).await?;
    let stale_code = custom_code(&stale_result);
    if stale_result.result.is_err() {
        if stale_code != Some(sim2::REFUSE_SNAPSHOT) {
            return Err(format!("a stale snapshot refused with {stale_code:?}").into());
        }
    } else {
        return Err("a frame from before the reset still passed the snapshot check".into());
    }
    let fresh = session.snapshot_ix(0, &start_root);
    let fresh_result = session.send(vec![fresh], &[], base::NORMAL_LIMIT).await?;
    if fresh_result.result.is_err() {
        return Err(format!(
            "the reset slot refused its own snapshot: {:?}",
            fresh_result.result
        )
        .into());
    }

    // The replay: the same commands, from cursor 0, against a fresh host
    // session locked to them, and against the first run's bytes per batch.
    let mut host = resource::NativeSimSession::open(
        capture_data.wad_refs(),
        &capture_data.resource,
        &capture_data.state,
    )
    .map_err(|rc| format!("replay oracle session open rc={rc}"))?;
    let mut chain = sim2::chain_init(&session.keys.context.pubkey(), view.first_tic);
    let mut replay_equal = 0usize;
    let mut replay_units: Vec<u64> = Vec::new();
    let mut cursor = 0u32;
    while cursor < tics {
        let first = cursor;
        let last = (cursor + advance_tics).min(tics);
        let deposit = session.deposit_ix(first, &commands[first as usize * 4..last as usize * 4]);
        let deposited = session
            .send(vec![deposit], &[&authority], base::NORMAL_LIMIT)
            .await?;
        if deposited.result.is_err() {
            return Err(format!("replay deposit {first}: {:?}", deposited.result).into());
        }
        session.cursor_before = first;
        let advance = session.advance_ix(sim2::ADVANCE_STARVED, last - first);
        let result = session
            .send(vec![advance], &[&authority], base::NORMAL_LIMIT)
            .await?;
        if result.result.is_err() {
            return Err(format!("replay ADVANCE/5 at {first}: {:?}", result.result).into());
        }
        replay_units.push(base::units(&result)?);
        for seq in first..last {
            let command: [u8; 4] = commands[seq as usize * 4..seq as usize * 4 + 4]
                .try_into()
                .unwrap();
            if host.step(&command) != 0 {
                return Err(format!("replay oracle starved at tic {seq}").into());
            }
            chain = sim2::chain_step(&chain, seq, &command);
        }
        host.publish(&mut host_state, &mut host_mirror)
            .map_err(|rc| format!("replay oracle publish at {last}: rc={rc}"))?;
        let state = session.data(state_signer.pubkey()).await?;
        let mirror = session.data(mirror_signer.pubkey()).await?;
        let (checked, root) = sim2::verified_state_root(&state, &mirror)
            .map_err(|e| format!("replay state root at {last}: {e:?}"))?;
        let view = session.view().await?;
        let prior = first_run
            .iter()
            .find(|(at, ..)| *at == last)
            .ok_or("the replay reached a cursor the first run did not commit")?;
        if state != host_state
            || mirror != host_mirror
            || checked != base::u32_at(&state, 16) as usize
            || root != view.state_root
            || view.input_chain != chain
            || view.cursor != last
            || base::sha(&state) != prior.1
            || base::sha(&mirror) != prior.2
            || root != prior.3
            || chain != prior.4
        {
            return Err(format!(
                "the replay at {last} is not the first run: st1={} mp1={} root={} chain={} cursor={} prior_st1={} prior_mp1={} prior_root={} prior_chain={}",
                state == host_state,
                mirror == host_mirror,
                root == view.state_root,
                view.input_chain == chain,
                view.cursor == last,
                base::sha(&state) == prior.1,
                base::sha(&mirror) == prior.2,
                root == prior.3,
                chain == prior.4,
            )
            .into());
        }
        replay_equal += 1;
        cursor = last;
    }
    if replay_equal != first_run.len() {
        return Err("the replay committed a different number of batches".into());
    }

    // The live-player control for the restart rule: on the reset slot, whose
    // committed start state has a live player, a use command is ordinary input
    // and commits a tic.  It is the same command the dead-player session below
    // is refused.
    let use_command = [0u8, 0, 0, 0x01];
    let use_batch = vec![use_command; 1].concat();
    let use_deposit = session.deposit_ix(cursor, &use_batch);
    let deposited = session
        .send(vec![use_deposit], &[&authority], base::NORMAL_LIMIT)
        .await?;
    if deposited.result.is_err() {
        return Err(format!("live-use deposit: {:?}", deposited.result).into());
    }
    session.cursor_before = cursor;
    let use_advance = session.advance_ix(sim2::ADVANCE_STARVED, 1);
    let used = session
        .send(vec![use_advance], &[&authority], base::NORMAL_LIMIT)
        .await?;
    if used.result.is_err() {
        return Err(format!("a live player's use command was refused: {:?}", used.result).into());
    }
    let use_view = session.view().await?;
    let live_use = json!({
        "player_state_at_start": common::PST_LIVE,
        "command_buttons": 1,
        "committed": true,
        "cursor_before": cursor,
        "cursor_after": use_view.cursor,
        "cu": base::units(&used)?,
    });

    // --- 3c. the dead player's use press, and the restart it asks for ---------
    // A separate slot whose *committed start state* is a canonical ST1 with the
    // console player dead.  A dead player is ordinary input to the guard (a
    // PST_DEAD player is not PST_REBORN), so tics run; pressing use is the one
    // thing that is not, and it is refused with 295 rather than swallowed.
    let dead_state = dead_player_state(&capture_data.state);
    let dead_keys = session_keys(Keypair::new());
    let dead_bank = bank_with(
        &capture_data,
        dead_keys.context.pubkey(),
        dead_keys.state.pubkey(),
        dead_keys.mirror.pubkey(),
        dead_keys.controller.pubkey(),
        dead_keys.authority.pubkey(),
        None,
        &layout,
    );
    let dead_ctx = dead_bank.start_with_context().await;
    let mut dead = Session {
        ctx: dead_ctx,
        keys: dead_keys,
        nonce: 0,
        cursor_before: 0,
    };
    let dead_authority = dead.keys.authority.insecure_clone();
    let dead_state_signer = dead.keys.state.insecure_clone();
    let dead_mirror_signer = dead.keys.mirror.insecure_clone();
    let dead_controller = controller_for_gametic(&controller, base::u32_at(&dead_state, 20))?;
    let (_, dead_init_cu) = reconstruct(
        &mut dead,
        &base::padded_state(&dead_state),
        &capture_data.mirror,
        &dead_controller,
        &capture_data,
        capacity,
    )
    .await?;
    let dead_start_key = stage_start_account(&mut dead, &dead_state).await?;
    let dead_mirror_at_start = dead.data(dead_mirror_signer.pubkey()).await?;

    // (a) four non-use tics from a dead player commit.
    let move_batch = vec![[16u8, 0, 0, 0]; 4].concat();
    let deposit = dead.deposit_ix(0, &move_batch);
    let deposited = dead
        .send(vec![deposit], &[&dead_authority], base::NORMAL_LIMIT)
        .await?;
    if deposited.result.is_err() {
        return Err(format!("dead-slot deposit: {:?}", deposited.result).into());
    }
    dead.cursor_before = 0;
    let advance = dead.advance_ix(sim2::ADVANCE_STARVED, 4);
    let moved = dead
        .send(vec![advance], &[&dead_authority], base::NORMAL_LIMIT)
        .await?;
    if moved.result.is_err() {
        return Err(format!("a dead player could not tic: {:?}", moved.result).into());
    }
    let dead_view = dead.view().await?;
    let moved_cursor = dead_view.cursor;
    if moved_cursor != 4 {
        return Err("the dead slot did not commit its tics".into());
    }
    let state_before_use = dead.data(dead_state_signer.pubkey()).await?;

    // (b) the use press is refused 295 and commits nothing.
    let press = [0u8, 0, 0, 0x01];
    let deposit = dead.deposit_ix(4, &press);
    let deposited = dead
        .send(vec![deposit], &[&dead_authority], base::NORMAL_LIMIT)
        .await?;
    if deposited.result.is_err() {
        return Err(format!("dead-slot use deposit: {:?}", deposited.result).into());
    }
    dead.cursor_before = 4;
    let advance = dead.advance_ix(sim2::ADVANCE_STARVED, 1);
    let refused = dead
        .send(vec![advance], &[&dead_authority], base::NORMAL_LIMIT)
        .await?;
    let restart_code = custom_code(&refused);
    if refused.result.is_err() {
        if restart_code != Some(sim2::REFUSE_RESTART_REQUEST) {
            return Err(format!("use while dead refused with {restart_code:?}").into());
        }
    } else {
        return Err("a dead player's use press was committed as a tic".into());
    }
    let state_after_use = dead.data(dead_state_signer.pubkey()).await?;
    let view_after_use = dead.view().await?;
    if state_after_use != state_before_use
        || view_after_use.cursor != 4
        || view_after_use.halt != 0
        || view_after_use.state_root != dead_view.state_root
    {
        return Err("the refused restart request committed something".into());
    }

    // (c) the answer: RESET/9 returns the dead slot to its committed start
    // state, and the slot runs again.
    let reset_ix = dead.reset_ix(dead_start_key.pubkey(), sim2::RESET_START_STATE, false);
    let reset = dead
        .send(vec![reset_ix], &[&dead_authority], base::NORMAL_LIMIT)
        .await?;
    if reset.result.is_err() {
        return Err(format!("dead-slot RESET/9: {:?}", reset.result).into());
    }
    let dead_reset_cu = base::units(&reset)?;
    let dead_view = dead.view().await?;
    let dead_state_after = dead.data(dead_state_signer.pubkey()).await?;
    let dead_mirror_after = dead.data(dead_mirror_signer.pubkey()).await?;
    if dead_view.cursor != 0
        || dead_view.halt != 0
        || dead_view.resets != 1
        || dead_state_after != base::padded_state(&dead_state)
        || dead_mirror_after != dead_mirror_at_start
    {
        return Err("the dead slot was not returned to its start state".into());
    }
    // And the restarted slot takes input again.  Its committed start state is
    // the one with the dead player, so a *use* press is still a restart request
    // -- the rule is about the committed state, not about the reset -- and a
    // non-use command is what has to commit here.
    let deposit = dead.deposit_ix(0, &[16u8, 0, 0, 0]);
    let deposited = dead
        .send(vec![deposit], &[&dead_authority], base::NORMAL_LIMIT)
        .await?;
    if deposited.result.is_err() {
        return Err(format!("restarted-slot deposit: {:?}", deposited.result).into());
    }
    dead.cursor_before = 0;
    let advance = dead.advance_ix(sim2::ADVANCE_STARVED, 1);
    let restarted = dead
        .send(vec![advance], &[&dead_authority], base::NORMAL_LIMIT)
        .await?;
    if restarted.result.is_err() {
        return Err(format!(
            "the restarted slot refused its first tic: {:?}",
            restarted.result
        )
        .into());
    }
    let restart = json!({
        "committed_start_player_state": common::PST_DEAD,
        "why": "vanilla single-player has no use-to-respawn: G_Ticker calls G_DoReborn only for PST_REBORN, and in single player G_DoReborn sets gameaction = ga_loadlevel, which this profile halts on.  So the press cannot resurrect a run; the guard refuses it with its own code and the answer is RESET/9.",
        "dead_player_tics_commit": moved_cursor == 4,
        "init_cu": dead_init_cu,
        "move_batch_cu": base::units(&moved)?,
        "use_press_refused_with": restart_code,
        "use_press_committed_nothing": true,
        "use_press_cu": base::units(&refused).ok(),
        "reset_cu": dead_reset_cu,
        "restarted_first_command": "forward 16, no buttons",
        "restarted_cursor": dead.view().await?.cursor,
        "restarted_first_tic_cu": base::units(&restarted)?,
        "live_use_control": live_use,
    });

    // --- 3d. a halted slot, and the force flag --------------------------------
    // A halt is latched and terminal, and the only way to reach one from a
    // committed start state is a start state the guard refuses.  This slot's
    // committed start state is a canonical ST1 whose gamestate is not
    // GS_LEVEL, so its first sweep latches halt reason 1.  RESET/9 then refuses
    // 289 without the force flag and lands on the slot's own start state with
    // it: the force flag is the operator saying "discard this session", not a
    // way to install a different start state.
    let halt_state = {
        let mut out = capture_data.state.clone();
        let at = common::ST1_GAMESTATE_OFFSET;
        out[at..at + 4].copy_from_slice(&2i32.to_le_bytes());
        out
    };
    let halt_keys = session_keys(Keypair::new());
    let halt_bank = bank_with(
        &capture_data,
        halt_keys.context.pubkey(),
        halt_keys.state.pubkey(),
        halt_keys.mirror.pubkey(),
        halt_keys.controller.pubkey(),
        halt_keys.authority.pubkey(),
        None,
        &layout,
    );
    let halt_ctx = halt_bank.start_with_context().await;
    let mut halt = Session {
        ctx: halt_ctx,
        keys: halt_keys,
        nonce: 0,
        cursor_before: 0,
    };
    let halt_authority = halt.keys.authority.insecure_clone();
    let halt_state_signer = halt.keys.state.insecure_clone();
    let halt_mirror_signer = halt.keys.mirror.insecure_clone();
    let halt_controller = controller_for_gametic(&controller, base::u32_at(&halt_state, 20))?;
    let (halt_phases, halt_init_cu) = reconstruct(
        &mut halt,
        &base::padded_state(&halt_state),
        &capture_data.mirror,
        &halt_controller,
        &capture_data,
        capacity,
    )
    .await?;
    let halt_start_key = stage_start_account(&mut halt, &halt_state).await?;
    let deposit = halt.deposit_ix(0, &[0u8, 0, 0, 0]);
    let deposited = halt
        .send(vec![deposit], &[&halt_authority], base::NORMAL_LIMIT)
        .await?;
    if deposited.result.is_err() {
        return Err(format!("halt-slot deposit: {:?}", deposited.result).into());
    }
    halt.cursor_before = 0;
    let advance = halt.advance_ix(sim2::ADVANCE_STARVED, 1);
    let halted = halt
        .send(vec![advance], &[&halt_authority], base::NORMAL_LIMIT)
        .await?;
    if halted.result.is_err() {
        return Err(format!("the halt control did not latch: {:?}", halted.result).into());
    }
    let halt_view = halt.view().await?;
    if halt_view.halt != sim2::HALT_GAMESTATE || halt_view.cursor != 0 {
        return Err(format!(
            "the halt control did not latch reason 1: halt={} cursor={}",
            halt_view.halt, halt_view.cursor
        )
        .into());
    }
    let halted_meta = halt.data(halt.keys.meta).await?;
    let refused = halt
        .send(
            vec![halt.reset_ix(halt_start_key.pubkey(), sim2::RESET_START_STATE, false)],
            &[&halt_authority],
            base::NORMAL_LIMIT,
        )
        .await?;
    let halted_code = custom_code(&refused);
    if refused.result.is_err() {
        if halted_code != Some(sim2::REFUSE_HALTED) {
            return Err(format!("a halted slot refused with {halted_code:?}, wanted 289").into());
        }
    } else {
        return Err("a halted slot accepted a reset with no force flag".into());
    }
    if halt.data(halt.keys.meta).await? != halted_meta {
        return Err("the refused reset changed a halted slot's meta".into());
    }
    let forced = halt
        .send(
            vec![halt.reset_ix(halt_start_key.pubkey(), sim2::RESET_START_STATE, true)],
            &[&halt_authority],
            base::NORMAL_LIMIT,
        )
        .await?;
    if forced.result.is_err() {
        return Err(format!("the forced reset of a halted slot: {:?}", forced.result).into());
    }
    let forced_cu = base::units(&forced)?;
    let forced_view = halt.view().await?;
    if forced_view.halt != 0 || forced_view.cursor != 0 || forced_view.resets != 1 {
        return Err("the forced reset did not clear the halt".into());
    }
    let halted = json!({
        "why": "gamestate != GS_LEVEL, so the shared pre-tic guard halts reason 1 before G_Ticker; the halt is latched in the meta and every later sweep is a no-op",
        "committed_start_gamestate": 2,
        "e2r5_phase_cu": halt_phases,
        "init_cu": halt_init_cu,
        "halt_reason": halt_view.halt,
        "cursor_after_halt": halt_view.cursor,
        "reset_without_force_refused_with": halted_code,
        "refused_reset_changed_nothing": true,
        "reset_with_force_cu": forced_cu,
        "halt_after_forced_reset": forced_view.halt,
        "cursor_after_forced_reset": forced_view.cursor,
        "resets": forced_view.resets,
    });

    // --- 4. receipts -----------------------------------------------------------
    let final_state = session.data(state_signer.pubkey()).await?;
    let final_mirror = session.data(mirror_signer.pubkey()).await?;
    let final_view = session.view().await?;
    fs::write(receipt.join("final-state.st1"), &final_state)?;
    fs::write(receipt.join("final-mirror.mp1"), &final_mirror)?;
    fs::write(
        receipt.join("final-meta.bin"),
        &session.data(meta_key).await?,
    )?;
    let mut sorted = advance_units.clone();
    sorted.sort_unstable();
    let report = json!({
        "status": "measured",
        "gate": "DOOM-SIM/2 (d) live input past the captured DEMO1 end, SBF ProgramTest prefer_bpf(true)",
        "network_calls": 0,
        "image_sha256": base::sha(&image_bytes),
        "manifest_sha256": base::sha(&manifest_bytes),
        "manifest_source_commit": manifest_json["compile_source_commit"],
        "manifest_source_tree": manifest_json["compile_source_tree"],
        "context_account_bytes": layout.context_bytes,
        "static_resource_account_bytes": layout.static_bytes,
        "diagnostic_profile_image": layout.diagnostic,
        "driver_source_sha256": base::sha(DRIVER_SOURCE),
        "capture": capture_dir.display().to_string(),
        "resource_sha256": base::sha(&capture_data.resource),
        "controller_sha256": base::sha(&controller),
        "input_profile": profile,
        "tics": tics,
        "advance_tics": advance_tics,
        "sweeps_sent": sent,
        "sweeps_equal_to_host_ingress": equal_sweeps,
        "captured_demo_tics": demo_tics,
        "captured_demo_cursor_at_capture": capture_cursor,
        "first_cursor_past_demo_end": first_past_demo,
        "tics_past_demo_end": tics - demo_tics,
        "final_cursor": final_view.cursor,
        "final_state_root": common::hex(&final_view.state_root),
        "final_input_chain": common::hex(&final_view.input_chain),
        "final_st1_sha256": base::sha(&final_state),
        "final_mp1_sha256": base::sha(&final_mirror),
        "initial_state_root": common::hex(&initial_root),
        "e2r5_phase_cu": phases,
        "sim2_init_cu": init_cu,
        "deposit_transactions": deposit_units.len(),
        "deposit_cu_max": deposit_units.iter().max(),
        "advance_cu_min": sorted.first(),
        "advance_cu_median": sorted.get(sorted.len() / 2),
        "advance_cu_max": sorted.last(),
        "sweep_wall_seconds": sweep_seconds,
        "sweeps_per_second": sent as f64 / sweep_seconds,
        "tics_per_second_including_renders": tics as f64 / sweep_seconds,
        "rsr2_build_transactions": build_units.len(),
        "rsr2_build_cu_max": build_units.iter().max(),
        "render": render_rows,
        "buffered_strip_binding": buffered_rows,
        "render_buffer_depth": {
            "buffer_count": rsn3::BUFFER_COUNT,
            "provisioned_buffer": PROVISIONED_BUFFER,
            "provisioned_by_the_program": "INIT-BUFFERED/3 creates the account and the program pays minimum_balance(SNAPSHOT_BYTES) for it, GROW-BUFFERED/3 brings it to full size in commutative 10,240-byte steps, and eight INIT-STRIP/3 calls create the strips that hang off it.  Rent is per buffer and is not recovered, so this is the recurring cost of the depth and the only part of it that is not free",
            "provisioned_rent_lamports": provisioned_rent,
            "provisioned_grow_transactions": provision_grows,
            "provisioning_cu_max": provision_units.iter().max(),
            "buffers": depth_rows,
            "past_the_end": depth_refusals,
            "purity_per_buffer": "for every buffer inside the count, re-rendering that buffer's last strip with nothing in between reproduces the whole strip account byte for byte -- header, cursor, root, snapshot name and all 8,000 pixel bytes -- so the no-accumulation property the gate establishes for buffer 0 holds for a buffer number the two-buffer program would have refused",
            "byte_exactness_across_buffers": "buffer 1's strips, re-rendered after buffers 2 and 3 have been drawn into their own accounts, are byte-identical to the eight the offline RENDER produced at the same committed cursor, so the picture is a function of the cursor and not of which buffer it was drawn into",
            "byte_exact_across_the_depth": depth_across_frames,
            "working_context_not_byte_stable_across_transactions": depth_working_moved,
            "why_the_whole_account_digest_is_not_the_claim": "the strip render allocates its framebuffer and leaves viewimage, ylookup and screens[] pointing into that block inside the working context, so the static account's bytes are a function of where in the transaction sequence the read was taken as well as of the picture, and no cross-transaction digest of it means anything.  working_context_not_byte_stable_across_transactions records what a re-render actually moves there: on this run two bytes at one fixed offset, on the first rendered frame only, and nothing on the other nine of the twelve replays.  Two bytes at one offset on one frame is an allocation address, not accumulation -- accumulation would be systematic and would show up in the strips.  The byte-level claims are on the strip accounts and on the committed ST1 and MP1 accounts, and those are asserted",
            "why_four": "the depth note measures no gain from four buffers today: the buffer the loop came back to was free on 100.0% of 7,433 reconstructed frame iterations with a knee at 1.72 buffers, and the cap is the serial submit loop's 0.244 s a frame.  Four is in the program because it is the next binding constraint once the loop is fixed, not because it is faster now: depth binds below occupancy / cadence buffers, which is 3.1 at the 0.136 s cadence the loop work should reach, and four holds until the cadence drops below 0.105 s",
        },
        "render_purity": {
            "committed_state": "the ST1 state account is the committed game state and is not an account of RSR/2 RENDER or RENDER-BUFFERED; its digest is taken either side of every frame's strips and is unchanged",
            "no_accumulation": "re-rendering the last strip of a frame reproduces the static resource account byte for byte, so no earlier strip's frame counter, per-sector mark, linedef flag or subsector identity survives into a later one.  This is what the image's ri2_render_begin/ri2_render_end restore is for, and it is checked rather than assumed",
            "working_context": "the static account's first CTX_STRUCT_BYTES are a working copy of the engine context, and importing the RSN/3 snapshot into it is how a strip gets anything to render, so a frame does leave thousands of bytes different there.  That is a working area, not a record: the bytes are a function of the snapshot just rendered, which is what the replay check above establishes",
            "checked_in": "stunts/doom-sbf/driver/src/bin/live_input_gate.rs, once per rendered frame",
        },
        "strips_written": strips_written,
        "new_game_start": new_game_start,
        "reset9": {
            "start_account_bytes": resource_sbf::STATE_MAX_BYTES,
            "selector": sim2::RESET_START_STATE,
            "start_state_root": common::hex(&start_root),
            "refusals": reset_refusals,
            "cu": reset_cu,
            "cursor_after_reset": 0,
            "state_after_reset_equals_start": true,
            "mirror_after_reset_equals_derived": true,
            "staged_slots_cleared": true,
            "stale_snapshot_refused_with": stale_code,
            "post_reset_snapshot_accepted": true,
            "replay_batches": replay_equal,
            "replay_equal_to_first_run": true,
            "replay_cu_min": replay_units.iter().min(),
            "replay_cu_max": replay_units.iter().max(),
            "replay_note": "same staged commands from cursor 0: same ST1, MP1, state root and input chain at every cursor the first run committed, and equal to a fresh host session locked to those commands",
        },
        "restart_request": restart,
        "halted_slot": halted,
        "position": position_rows,
        "controls": controls,
        "host_ingress": "resource::NativeSimSession::step, the chain per-tic contract verbatim (pre-tic guard, DoomControllerArmV2, G_Ticker, gametic++, DOOM-CI3 receipt), locked to the same staged commands",
        "frame_oracle": "host session.frame(): the same R_RenderPlayerView the chain strips call",
        "abi": {
            "sim2": sim2::ABI_DESCRIPTOR,
            "advance_starved": sim2::ADVANCE_STARVED,
            "refuse_unconsumed_input": sim2::REFUSE_UNCONSUMED_INPUT,
        },
        "command_line": argv,
        "sources": common::driver_identity(&[
            ("driver/src/bin/live_input_gate.rs", DRIVER_SOURCE),
            ("driver/src/sweep_common.rs", SWEEP_COMMON_SOURCE),
        ]),
    });
    common::write_json(&receipt.join("summary.json"), &report)?;
    common::write_json(
        &receipt.join("identity.json"),
        &json!({
            "image": image.display().to_string(),
            "image_sha256": base::sha(&image_bytes),
            "manifest": manifest.display().to_string(),
            "manifest_sha256": base::sha(&manifest_bytes),
            "commands_sha256": base::sha(&commands),
            "starved_st1_sha256": base::sha(&starved_state),
            "source_tree": manifest_json["compile_source_tree"],
        }),
    )?;
    let _ = buffer_signer;
    println!(
        "DOOM-SIM/2 gate (d) measured: {tics} tics in {sent} ADVANCE/5 sweeps of {advance_tics}, \
         all {equal_sweeps} equal to the host ingress, 0 refusals; first cursor past the \
         {demo_tics}-tic demo end {first_past_demo:?}; {} frames 8/8 strips byte exact; a state \
         with no console player in the game is refused at E2R/5 (Custom {prepare_code:?}) and by \
         the host import, so a 288 cannot be staged; {depth_frames} of {} buffer frames byte \
         exact ({depth_frames} of {depth_buffer_frames} buffer-frames) and buffer {} refused on all \
         four wires; RESET/9 {reset_cu} CU then {} replay \
         batches land on the first run's bytes; a dead player's use press is refused Custom {:?}; \
         receipt={}",
        strips_written / 8,
        depth_buffer_frames,
        rsn3::BUFFER_COUNT,
        replay_equal,
        restart_code,
        receipt.display()
    );
    Ok(())
}
