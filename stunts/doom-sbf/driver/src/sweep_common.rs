//! Shared pieces of the DOOM-SIM/2 gates: captured inputs, the DEMO1 command
//! stream, the host session engine, and the sequential no-render oracle.
//!
//! Everything here reads retained captures; nothing synthesises chain state.

use std::{cell::Cell, error::Error, fs, io::Write, path::Path, rc::Rc};

use doom_sbf::{resource, resource_sbf, sim2};
use sha2::{Digest, Sha256};

use crate::base;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct Capture {
    pub wad: Vec<Vec<u8>>,
    /// The captured ST1 image (encoded length, no padding).
    pub state: Vec<u8>,
    /// The captured MP1 image.
    pub mirror: Vec<u8>,
    pub resource: Vec<u8>,
    pub wad_payload_sha256: String,
}

impl Capture {
    pub fn load(capture: &Path, resource_path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut wad = Vec::new();
        let mut payload = Sha256::new();
        for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
            let bytes = base::read(&capture.join(format!("account/wad-{ordinal:03}.bin")))?;
            base::wad_account(&bytes, ordinal)?;
            let len = base::u32_at(&bytes, 24) as usize;
            payload.update(
                &bytes[resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + len],
            );
            wad.push(bytes);
        }
        let wad_payload_sha256 = hex(&payload.finalize());
        if wad_payload_sha256 != base::WAD_CONCAT_SHA256 {
            return Err("recomputed WAD payload hash mismatch".into());
        }
        let resource = base::read(resource_path)?;
        if resource.len() != resource_sbf::RI2_RESOURCE_IMAGE_BYTES
            || base::sha(&resource) != base::RESOURCE_IMAGE_SHA256
        {
            return Err("RI2 identity mismatch".into());
        }
        Ok(Self {
            wad,
            state: base::read(&capture.join("dumped-doom-st1.bin"))?,
            mirror: base::read(&capture.join("mobj_pool.bin"))?,
            resource,
            wad_payload_sha256,
        })
    }

    pub fn wad_refs(&self) -> [&[u8]; 5] {
        [
            &self.wad[0],
            &self.wad[1],
            &self.wad[2],
            &self.wad[3],
            &self.wad[4],
        ]
    }

    /// The published ST1 account image: the captured ST1 with a zero tail.
    pub fn state_account(&self) -> Vec<u8> {
        base::padded_state(&self.state)
    }

    /// DEMO1 ticcmds starting at the captured ST1 demo cursor.  The captured
    /// state is at gametic 0 with the cursor at the first command, so these
    /// are the demo's own recorded commands in order.
    pub fn demo_commands(&self, count: usize) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut payload = Vec::new();
        for account in &self.wad {
            let length = base::u32_at(account, 24) as usize;
            payload.extend_from_slice(
                &account[resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + length],
            );
        }
        let lump_count = base::u32_at(&payload, 4) as usize;
        let directory = base::u32_at(&payload, 8) as usize;
        let cursor = base::u32_at(&self.state, 24) as usize;
        for index in 0..lump_count {
            let at = directory + index * 16;
            if &payload[at + 8..at + 13] != b"DEMO1" {
                continue;
            }
            let offset = base::u32_at(&payload, at) as usize;
            let length = base::u32_at(&payload, at + 4) as usize;
            if cursor < 13 || cursor + count * 4 > length {
                return Err("DEMO1 shorter than the requested command count".into());
            }
            let commands = payload[offset + cursor..offset + cursor + count * 4].to_vec();
            if commands.chunks_exact(4).any(|c| c[0] == 0x80) {
                return Err("DEMO1 marker inside requested command range".into());
            }
            return Ok(commands);
        }
        Err("DEMO1 lump not found".into())
    }
}

/// `sim2::Engine` over the resident host session.  The budget cell models
/// the compute meter: each step spends `tic_cost`.
pub struct NativeEngine {
    pub session: resource::NativeSimSession,
    pub budget: Rc<Cell<u64>>,
    pub tic_cost: u64,
    pub steps: u64,
    pub publishes: u64,
}

impl sim2::Engine for NativeEngine {
    fn step(&mut self, command: &[u8; 4]) -> Result<sim2::StepResult, u32> {
        let rc = self.session.step(command);
        self.budget
            .set(self.budget.get().saturating_sub(self.tic_cost));
        match rc {
            0 => {
                self.steps += 1;
                Ok(sim2::StepResult::Ran)
            }
            1..=5 => Ok(sim2::StepResult::Halt(rc as u8)),
            code => Err(code as u32),
        }
    }

    fn publish(&mut self, state: &mut [u8], mirror: &mut [u8]) -> Result<usize, u32> {
        self.publishes += 1;
        self.session.publish(state, mirror).map_err(|rc| rc as u32)
    }
}

pub struct CellBudget(pub Rc<Cell<u64>>);

impl sim2::Budget for CellBudget {
    fn remaining(&self) -> u64 {
        self.0.get()
    }
}

/// One tic of the sequential oracle: ST1/MP1 published after every tic.
pub struct OracleTic {
    pub state: Vec<u8>,
    pub mirror: Vec<u8>,
    pub state_len: usize,
    pub state_root: [u8; 32],
    pub chain: [u8; 32],
    pub probe: resource::SessionProbe,
}

pub struct Oracle {
    pub initial_root: [u8; 32],
    pub tics: Vec<OracleTic>,
}

/// Sequential no-render oracle: one host session, every command stepped in
/// order, ST1/MP1 published after every tic.  `context` seeds the input
/// chain exactly as SIM/2 INIT does.
pub fn sequential_oracle(
    capture: &Capture,
    commands: &[u8],
    context: &solana_pubkey::Pubkey,
) -> Result<Oracle, Box<dyn Error>> {
    let mut session =
        resource::NativeSimSession::open(capture.wad_refs(), &capture.resource, &capture.state)
            .map_err(|rc| format!("oracle session open rc={rc}"))?;
    let initial_state = capture.state_account();
    let (_, initial_root) = sim2::verified_state_root(&initial_state, &capture.mirror)
        .map_err(|e| format!("initial state root: {e:?}"))?;
    let first_tic = base::u32_at(&capture.state, 20);
    let mut chain = sim2::chain_init(context, first_tic);
    let mut tics = Vec::with_capacity(commands.len() / 4);
    for (seq, command) in commands.chunks_exact(4).enumerate() {
        let command: [u8; 4] = command.try_into().unwrap();
        let rc = session.step(&command);
        if rc != 0 {
            return Err(format!("oracle step {seq} rc={rc}").into());
        }
        chain = sim2::chain_step(&chain, seq as u32, &command);
        let mut state = vec![0u8; resource_sbf::STATE_MAX_BYTES];
        let mut mirror = vec![0u8; resource_sbf::MIRROR_BYTES];
        let state_len = session
            .publish(&mut state, &mut mirror)
            .map_err(|rc| format!("oracle publish {seq} rc={rc}"))?;
        let (checked, state_root) = sim2::verified_state_root(&state, &mirror)
            .map_err(|e| format!("oracle root {seq}: {e:?}"))?;
        if checked != state_len || base::u32_at(&state, 20) != first_tic + seq as u32 + 1 {
            return Err(format!("oracle tic {seq} shape").into());
        }
        tics.push(OracleTic {
            state,
            mirror,
            state_len,
            state_root,
            chain,
            probe: session.probe(),
        });
    }
    Ok(Oracle { initial_root, tics })
}

pub fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), Box<dyn Error>> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

/// Retains the oracle: per-tic hashes/roots/probes as JSONL plus the full
/// ST1 and MP1 bytes of every tic (fixed strides), gzip-compressed.
pub fn retain_oracle(receipt: &Path, oracle: &Oracle) -> Result<(), Box<dyn Error>> {
    let mut lines = fs::File::create(receipt.join("oracle-per-tic.jsonl"))?;
    let mut states = fs::File::create(receipt.join("oracle-st1.bin"))?;
    let mut mirrors = fs::File::create(receipt.join("oracle-mp1.bin"))?;
    for (seq, tic) in oracle.tics.iter().enumerate() {
        writeln!(
            lines,
            "{}",
            serde_json::json!({
                "seq": seq,
                "cursor": seq + 1,
                "st1_len": tic.state_len,
                "st1_account_sha256": base::sha(&tic.state),
                "mp1_sha256": base::sha(&tic.mirror),
                "state_root": hex(&tic.state_root),
                "input_chain": hex(&tic.chain),
                "gametic": tic.probe.gametic,
                "gamestate": tic.probe.gamestate,
                "gameaction": tic.probe.gameaction,
                "playerstate": tic.probe.playerstate,
                "health": tic.probe.health,
                "demo_offset": tic.probe.demo_offset,
            })
        )?;
        states.write_all(&tic.state)?;
        mirrors.write_all(&tic.mirror)?;
    }
    drop(states);
    drop(mirrors);
    for name in ["oracle-st1.bin", "oracle-mp1.bin"] {
        let status = std::process::Command::new("gzip")
            .arg("-9")
            .arg(receipt.join(name))
            .status()?;
        if !status.success() {
            return Err(format!("gzip {name} failed").into());
        }
    }
    Ok(())
}

pub fn driver_identity(sources: &[(&str, &[u8])]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (name, bytes) in sources {
        map.insert((*name).into(), serde_json::json!(base::sha(bytes)));
    }
    serde_json::Value::Object(map)
}

/// Deterministic xorshift64* for the scrambled deposit order.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}

/// The deposit schedule both gates use: every seq in `0..count` exactly once
/// in a scrambled order, as runs of 1..=max_run contiguous slots.  `hold`
/// seqs are removed from the scramble and deposited late by the caller.
pub fn scrambled_runs(rng: &mut Rng, count: u32, max_run: u32, hold: &[u32]) -> Vec<(u32, u32)> {
    let mut runs = Vec::new();
    let mut seq = 0u32;
    while seq < count {
        if hold.contains(&seq) {
            seq += 1;
            continue;
        }
        let mut len = 1 + rng.below(max_run as u64) as u32;
        len = len.min(count - seq);
        if let Some(stop) = hold.iter().filter(|&&h| h > seq).min() {
            len = len.min(stop - seq);
        }
        runs.push((seq, len));
        seq += len;
    }
    rng.shuffle(&mut runs);
    runs
}
