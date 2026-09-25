//! DOOM-SIM/2: staged write-once input slots plus one ordered sweep.
//!
//! SIM/1 carried the expected tic, sequence and state root in every TICK, so
//! tic N+1 landing before tic N was refused and a sender could advance at most
//! one tic per confirmation round trip.  SIM/2 decouples landing order from
//! execution order:
//!
//! * DEPOSIT writes tic `first_tic + seq` into slot `seq` of a slotted input
//!   buffer.  Slots are write-once, indexed by the payload's sequence number,
//!   bound to the authority recorded in the buffer header, and independent of
//!   simulation state: deposits may land in any order and any distance ahead
//!   of the cursor.
//! * ADVANCE/3 reads the committed cursor from the meta.  ADVANCE/4 first
//!   checks its signed expected cursor, then both forms execute consecutive
//!   written slots through the resident BPF `G_Ticker`, stopping at the first
//!   empty slot, a halt, or the tic cap (and, in `metered-sweep` builds only,
//!   when the remaining compute would not cover another tic plus
//!   publication), then derive ST1/MP1 once and commit the new cursor, state
//!   root and input chain.  A future ADVANCE/4 that arrives out of order
//!   refuses atomically.  ADVANCE/5 adds the DOOM-CI3 receipt rule: a staged
//!   tic whose command the engine did not consume is refused (288) before any
//!   cursor, chain or engine byte is committed, so a cursor is never advanced
//!   over a tic the player did not receive.  A sweep that finds nothing staged
//!   is a cheap no-op.
//!   After INIT, ADVANCE is the only instruction that changes CONTEXT, STATE
//!   or MIRROR bytes. EXTEND-CAPACITY may change only the bound input buffer's
//!   capacity and the corresponding capacity field in META.
//! * INIT binds exactly one meta (a PDA of the context key) to the context,
//!   the published E2R/5 reconstruction and its authority.  It refuses any
//!   signer other than the reconstruction authority and any context that
//!   already has a meta.
//! * RESET/9 returns the context to the start state its meta was initialised
//!   with.  The image comes from a supplied start account (a staged account is
//!   sealed once, so the bound state account cannot be re-staged) and RESET/9
//!   accepts it only if the recomputed state root equals the root recorded at
//!   INIT.  It then rewinds the cursor, the input chain, the halt and sweep
//!   counters, clears the staged slots and derives the mirror from the decoded
//!   engine.  It is authority-signed, it does not realloc, and it is refused on
//!   a halted slot unless the request sets the force flag.  One provisioned
//!   context, many sessions; no rent recovered.
//! * SNAPSHOT is the read-side check a renderer must pass: it refuses unless
//!   the caller's cursor and state root equal the committed meta.
//!
//! The pure cores (`deposit`, `sweep`, `reset`, `init_layout`,
//! `snapshot_check`) take
//! byte slices and an [`Engine`], so the host gate drives exactly the code the
//! program runs, against the host session oracle.

use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    hash::{hash, hashv},
    program_error::ProgramError,
    program_memory::{sol_memcmp, sol_memcpy, sol_memset},
    pubkey::Pubkey,
};

use crate::{continuation_split_sbf, resource_sbf};

pub const MAGIC: &[u8; 8] = b"DOOM-SIM";
pub const VERSION: u32 = 2;
pub const INIT: u8 = 1;
pub const DEPOSIT: u8 = 2;
pub const ADVANCE: u8 = 3;
pub const SNAPSHOT: u8 = 4;
pub const GROW_BUFFER: u8 = 5;
/// Cursor-bound ADVANCE wire operation.  ADVANCE/3 remains unchanged for
/// historical SIM/2 clients; new pipelined senders use this explicit form.
pub const ADVANCE_EXPECTED_CURSOR: u8 = 6;
/// Extend a live input buffer and its bound meta by one bounded realloc.
pub const EXTEND_CAPACITY: u8 = 7;
/// ADVANCE/5: ADVANCE/4 plus the DOOM-CI3 receipt rule.  A staged tic whose
/// command the engine did not consume is a refusal, not a tick, so a cursor is
/// never committed over a tic the player did not receive.  ADVANCE/3 and /4
/// keep their old meaning so a historical client stays reproducible.
pub const ADVANCE_STARVED: u8 = 8;
/// RESET/9: return the context to the start state its meta was initialised
/// with, so one provisioned slot serves many sessions.  The staged start
/// image is re-staged into the bound state account by the ordinary STG/1
/// path; RESET/9 accepts it only if its recomputed state root equals the root
/// this meta was initialised with, rewinds the cursor, the input chain, the
/// halt and sweep counters, clears the staged slots, and derives the mirror
/// from the decoded engine.  Authority-signed, and a no realloc: it makes a
/// slot reusable, it does not recover rent.
pub const RESET: u8 = 9;
#[cfg(all(not(feature = "metered-sweep"), not(feature = "legacy-5120-buffer"), not(feature = "capacity-extension")))]
pub const ABI_DESCRIPTOR: &str = "DOOM-SIM/2|sweep=count-capped:default8:max64|header=16|operations=GROW-BUFFER/5+INIT/1+DEPOSIT/2+ADVANCE/3+ADVANCE/4+ADVANCE/5+SNAPSHOT/4+RESET/9|grow-buffer=24:step<=10240|init=408:accounts=7|deposit=24+4n:n<=64:accounts=2|advance=24:accounts=6|advance-expected=24:accounts=6:expected-cursor|advance-starved=24:accounts=6:expected-cursor:unconsumed-tic-refused|reset=24:accounts=7:authority-signed:start-root-committed:start-account-or-state|halt-needs-force|no-realloc|snapshot=56:accounts=3|meta=PDA(DOOM-SIM2-META,context):384|buffer=SIM2INPT/3:128+capacity*8:default65536:max65536|root=DOOM-SIM/2-STATE(ST1,MP1-used)|chain=DOOM-SIM/2-INPUT-CHAIN|special-buttons=refused|render=none";
#[cfg(all(not(feature = "metered-sweep"), not(feature = "legacy-5120-buffer"), feature = "capacity-extension"))]
pub const ABI_DESCRIPTOR: &str = "DOOM-SIM/2|sweep=count-capped:default8:max64|header=16|operations=GROW-BUFFER/5+INIT/1+DEPOSIT/2+ADVANCE/3+ADVANCE/4+ADVANCE/5+SNAPSHOT/4+RESET/9+EXTEND-CAPACITY/7|grow-buffer=24:step<=10240|extend-capacity=24:accounts=meta+buffer+payer+authority+system:max-realloc=10240|init=408:accounts=7|deposit=24+4n:n<=64:accounts=2|advance=24:accounts=6|advance-expected=24:accounts=6:expected-cursor|advance-starved=24:accounts=6:expected-cursor:unconsumed-tic-refused|reset=24:accounts=7:authority-signed:start-root-committed:start-account-or-state|halt-needs-force|no-realloc|snapshot=56:accounts=3|meta=PDA(DOOM-SIM2-META,context):384|buffer=SIM2INPT/3:128+capacity*8:default65536:max65536|root=DOOM-SIM/2-STATE(ST1,MP1-used)|chain=DOOM-SIM/2-INPUT-CHAIN|special-buttons=refused|render=none";
#[cfg(all(feature = "metered-sweep", not(feature = "legacy-5120-buffer"), not(feature = "capacity-extension")))]
pub const ABI_DESCRIPTOR: &str = "DOOM-SIM/2|sweep=metered:max64|header=16|operations=GROW-BUFFER/5+INIT/1+DEPOSIT/2+ADVANCE/3+ADVANCE/4+ADVANCE/5+SNAPSHOT/4+RESET/9|grow-buffer=24:step<=10240|init=408:accounts=7|deposit=24+4n:n<=64:accounts=2|advance=24:accounts=6|advance-expected=24:accounts=6:expected-cursor|advance-starved=24:accounts=6:expected-cursor:unconsumed-tic-refused|reset=24:accounts=7:authority-signed:start-root-committed:start-account-or-state|halt-needs-force|no-realloc|snapshot=56:accounts=3|meta=PDA(DOOM-SIM2-META,context):384|buffer=SIM2INPT/3:128+capacity*8:default65536:max65536|root=DOOM-SIM/2-STATE(ST1,MP1-used)|chain=DOOM-SIM/2-INPUT-CHAIN|special-buttons=refused|render=none";
#[cfg(all(feature = "metered-sweep", not(feature = "legacy-5120-buffer"), feature = "capacity-extension"))]
pub const ABI_DESCRIPTOR: &str = "DOOM-SIM/2|sweep=metered:max64|header=16|operations=GROW-BUFFER/5+INIT/1+DEPOSIT/2+ADVANCE/3+ADVANCE/4+ADVANCE/5+SNAPSHOT/4+RESET/9+EXTEND-CAPACITY/7|grow-buffer=24:step<=10240|extend-capacity=24:accounts=meta+buffer+payer+authority+system:max-realloc=10240|init=408:accounts=7|deposit=24+4n:n<=64:accounts=2|advance=24:accounts=6|advance-expected=24:accounts=6:expected-cursor|advance-starved=24:accounts=6:expected-cursor:unconsumed-tic-refused|reset=24:accounts=7:authority-signed:start-root-committed:start-account-or-state|halt-needs-force|no-realloc|snapshot=56:accounts=3|meta=PDA(DOOM-SIM2-META,context):384|buffer=SIM2INPT/3:128+capacity*8:default65536:max65536|root=DOOM-SIM/2-STATE(ST1,MP1-used)|chain=DOOM-SIM/2-INPUT-CHAIN|special-buttons=refused|render=none";
#[cfg(feature = "legacy-5120-buffer")]
pub const ABI_DESCRIPTOR: &str = "DOOM-SIM/2|legacy-buffer-layout=SIM2INPT/2:41088:5120x8|sweep=count-capped:default8:max64|header=16|operations=INIT/1+DEPOSIT/2+ADVANCE/3+ADVANCE/4+ADVANCE/5+SNAPSHOT/4+RESET/9|init=404:accounts=7|deposit=24+4n:n<=64:accounts=2|advance=24:accounts=6|advance-expected=24:accounts=6:expected-cursor|advance-starved=24:accounts=6:expected-cursor:unconsumed-tic-refused|reset=24:accounts=7:authority-signed:start-root-committed:start-account-or-state|halt-needs-force|no-realloc|snapshot=56:accounts=3|meta=PDA(DOOM-SIM2-META,context):384|root=DOOM-SIM/2-STATE(ST1,MP1-used)|chain=DOOM-SIM/2-INPUT-CHAIN|special-buttons=refused|render=none";


pub const REQUEST_HEADER_BYTES: usize = 16;
#[cfg(not(feature = "legacy-5120-buffer"))]
pub const INIT_BYTES: usize = 408;
#[cfg(feature = "legacy-5120-buffer")]
pub const INIT_BYTES: usize = 404;
pub const INIT_KEY_COUNT: usize = 9;
pub const DEPOSIT_HEADER_BYTES: usize = 24;
pub const MAX_DEPOSIT_SLOTS: usize = 64;
pub const GROW_BUFFER_BYTES: usize = 24;
pub const MAX_BUFFER_GROWTH: usize = 10_240;
pub const EXTEND_CAPACITY_BYTES: usize = 24;
pub const ADVANCE_BYTES: usize = 24;
pub const RESET_BYTES: usize = 24;
/// RESET/9 flags word (instruction bytes 16..20): bit 0 permits a reset of a
/// halted slot.  Every other bit must be zero.
pub const RESET_FORCE: u32 = 1;
/// RESET/9 start-state selector (instruction bytes 20..24): the start image is
/// the bound state account's own, which is where it is before the first
/// advance.
pub const RESET_START_META: u32 = 0;
/// RESET/9 start-state selector: the start image is in the supplied start
/// account.  This is the reusable-slot case, because a staged account is sealed
/// once and DOOM-STG/1 refuses to write it again.
pub const RESET_START_STATE: u32 = 1;
pub const SNAPSHOT_BYTES: usize = 56;

pub const META_SEED: &[u8] = b"DOOM-SIM2-META";
pub const META_BYTES: usize = 384;
pub const BUFFER_HEADER_BYTES: usize = 128;
pub const SLOT_BYTES: usize = 8;
#[cfg(feature = "legacy-5120-buffer")]
pub const BUFFER_CAPACITY: usize = 5_120;
#[cfg(not(feature = "legacy-5120-buffer"))]
pub const BUFFER_CAPACITY: usize = 65_536;
#[cfg(feature = "legacy-5120-buffer")]
pub const DEFAULT_BUFFER_CAPACITY: usize = 5_120;
#[cfg(not(feature = "legacy-5120-buffer"))]
pub const DEFAULT_BUFFER_CAPACITY: usize = 65_536;
#[cfg(feature = "legacy-5120-buffer")]
pub const MAX_BUFFER_CAPACITY: usize = 5_120;
#[cfg(not(feature = "legacy-5120-buffer"))]
pub const MAX_BUFFER_CAPACITY: usize = 65_536;
#[cfg(feature = "legacy-5120-buffer")]
pub const BUFFER_FORMAT_VERSION: u32 = 2;
#[cfg(not(feature = "legacy-5120-buffer"))]
pub const BUFFER_FORMAT_VERSION: u32 = 3;
pub const BUFFER_BYTES: usize = BUFFER_HEADER_BYTES + BUFFER_CAPACITY * SLOT_BYTES;
/// The pinned C Doom engine stores `gametic` and `leveltime` as signed `int`.
/// Leave the one-past-session value representable when INIT binds a capacity.
pub const MAX_GAME_TIC: u32 = i32::MAX as u32;
pub const SLOT_WRITTEN: u8 = 1;
pub const BT_SPECIAL: u8 = 0x80;

pub fn buffer_bytes(capacity: usize) -> Option<usize> {
    if capacity == 0 || capacity > MAX_BUFFER_CAPACITY {
        return None;
    }
    capacity
        .checked_mul(SLOT_BYTES)
        .and_then(|slots| BUFFER_HEADER_BYTES.checked_add(slots))
}

pub fn initial_buffer_bytes(capacity: usize) -> Option<usize> {
    buffer_bytes(capacity).map(|total| total.min(MAX_BUFFER_GROWTH))
}

/// Return the only valid next account length for a staged grow operation.
/// The caller binds this to the observed current length in the instruction.
pub fn next_buffer_len(current: usize, capacity: usize) -> Option<usize> {
    let target = buffer_bytes(capacity)?;
    if current == 0 || current >= target {
        return None;
    }
    current
        .checked_add(MAX_BUFFER_GROWTH)
        .map(|next| next.min(target))
}

/// Tic cap of an ADVANCE that passes `max_tics = 0` on the count-capped
/// (Fogo) profile.  Fogo does not activate the `remaining_compute_units`
/// syscall (feature 5TuppMutoyzhUSfuYdhgzD47F92GL1g89KpCZQKqedxP inactive on
/// testnet and mainnet, measured 2026-09-22), so a deployable sweep cannot
/// read its own budget.  Measured on SBF over the 1,000 DEMO1 tics: the worst
/// 8-tic window plus the worst pre-sweep, publication and post-publication
/// costs is 1,136,519 CU, leaving 263,481 CU under the 1,400,000 limit. The
/// recomputed 11-tic window is 1,374,193 CU and fits; the worst 12-tic window
/// exceeds the limit. Across the retained 5,026-tic DEMO1 lifetime, the
/// measured maximum sweep was 1,177,656 CU, so the margin is input-dependent
/// and smaller than the worst single-tic cost.
/// A sweep that does exceed its budget fails atomically and changes nothing;
/// the sender retries with a smaller explicit `max_tics`.
pub const DEFAULT_SWEEP_TICS: u32 = 8;
/// Upper bound on tics per ADVANCE in every profile.
pub const MAX_SWEEP_TICS: u32 = 64;

/// The tic cap ADVANCE applies: `max_tics = 0` selects the profile default
/// (count-capped: `DEFAULT_SWEEP_TICS`; metered: `MAX_SWEEP_TICS`, with the
/// budget stop below it), and any request is capped at `MAX_SWEEP_TICS`.
pub fn effective_max_tics(requested: u32) -> u32 {
    let default = if cfg!(feature = "metered-sweep") {
        MAX_SWEEP_TICS
    } else {
        DEFAULT_SWEEP_TICS
    };
    if requested == 0 {
        default
    } else {
        requested.min(MAX_SWEEP_TICS)
    }
}

/// `metered-sweep` only: compute reserved for one more tic before a sweep
/// starts it.  Measured on SBF over the 1,000 DEMO1 tics of the gate
/// (per-tic step = slot checks, ArmV2, G_Ticker, input chain): median
/// 46,202, p99 127,303, max 223,333; the reserve is the max with a 1.34x
/// margin.  See docs/experiments/doom-staged-sweep-2026-09-22.md.
pub const TIC_RESERVE_CU: u64 = 300_000;
/// `metered-sweep` only: compute reserved for everything after the last tic:
/// MP1 derive, ST1 encode, copy, post-shape checks, the state-root hash, the
/// meta write and the receipt log.  Measured on SBF: max 354,398 (publish) + 50,991 (post)
/// with 212-216 mobjs; the reserve covers the thinker- and MP1-proportional
/// parts scaled to the 384-slot MP1 cap (~530,000) plus margin.
pub const PUBLISH_RESERVE_CU: u64 = 560_000;

pub const REFUSE_ACCOUNTS: u32 = 270;
pub const REFUSE_AUTHORITY: u32 = 271;
pub const REFUSE_BINDING: u32 = 272;
pub const REFUSE_CONTEXT: u32 = 273;
pub const REFUSE_E2R_BINDING: u32 = 274;
pub const REFUSE_ALREADY_INIT: u32 = 275;
pub const REFUSE_MALFORMED: u32 = 276;
pub const REFUSE_DUPLICATE_SLOT: u32 = 277;
pub const REFUSE_SLOT_RANGE: u32 = 278;
pub const REFUSE_COMMAND: u32 = 279;
pub const REFUSE_STATE_ROOT: u32 = 280;
pub const REFUSE_STATE_SHAPE: u32 = 281;
pub const REFUSE_SNAPSHOT: u32 = 283;
pub const REFUSE_TIC: u32 = 284;
pub const REFUSE_BUFFER_GROWTH: u32 = 285;
pub const REFUSE_CURSOR_MISMATCH: u32 = 286;
pub const REFUSE_CAPACITY_GROWTH: u32 = 287;
/// ADVANCE/5: a staged tic ran without the engine consuming its command.
pub const REFUSE_UNCONSUMED_INPUT: u32 = 288;
/// RESET/9 on a halted slot with no force flag.  A halt is latched, so a reset
/// that ignored it would let anyone with the authority erase a session that is
/// still being audited; the operator discards the session deliberately instead.
pub const REFUSE_HALTED: u32 = 289;
/// A dead player pressed use: the pre-tic guard's restart request (shared with
/// the host oracle in `doom_sim2_guard.h`).  Nothing is committed and the meta
/// is not halted, so the answer is RESET/9 rather than a lost slot.  290 to 294
/// belong to DOOM-STG/1, so the SIM/2 range continues at 295.
pub const REFUSE_RESTART_REQUEST: u32 = 295;

pub const HALT_GAMESTATE: u8 = 1;
pub const HALT_GAMEACTION: u8 = 2;
pub const HALT_REBORN: u8 = 3;
pub const HALT_PAUSED: u8 = 4;
pub const HALT_DEMO_END: u8 = 5;

const META_MAGIC: &[u8; 8] = b"SIM2META";
const BUFFER_MAGIC: &[u8; 8] = b"SIM2INPT";
const BUFFER_GROW_MAGIC: &[u8; 8] = b"SIM2GROW";
const DOMAIN_STATE: &[u8] = b"DOOM-SIM/2-STATE\0";
const DOMAIN_CHAIN: &[u8] = b"DOOM-SIM/2-INPUT-CHAIN\0";
const DOMAIN_CHECKPOINT: &[u8] = b"DOOM-SIM/2-CHECKPOINT\0";
const RECEIPT_TAG: &[u8] = b"SIM2-ADV";
const RESET_RECEIPT_TAG: &[u8] = b"SIM2-RST";

const CHECKPOINT_BYTES: usize = continuation_split_sbf::CONTEXT_CHECKPOINT_BYTES;
const CHECKPOINT_OFFSET: usize = continuation_split_sbf::CONTEXT_CHECKPOINT_OFFSET;
const CHECKPOINT_PHASE_PUBLISHED: u32 = 8;
const CHECKPOINT_CURSOR_PUBLISHED: u32 = 8;
const MIRROR_HEADER_BYTES: usize = 4096;
const MIRROR_SLOT_BYTES: usize = 160;
const MIRROR_SLOTS: usize = 384;
const WAD_HASH: [u8; 32] = [
    0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9, 0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3, 0xe2, 0x9c,
    0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18, 0x16, 0xf6, 0x52, 0xa5, 0x26, 0xca, 0xc7, 0x71,
];

pub const M_BUMP: usize = 20;
pub const M_HALT: usize = 21;
pub const M_FIRST_TIC: usize = 24;
pub const M_CURSOR: usize = 28;
pub const M_STATE_LEN: usize = 32;
pub const M_SWEEPS: usize = 36;
pub const M_LAST_SWEEP_TICS: usize = 40;
pub const M_CAPACITY: usize = 44;
pub const M_CONTEXT: usize = 48;
pub const M_STATE: usize = 80;
pub const M_MIRROR: usize = 112;
pub const M_BUFFER: usize = 144;
pub const M_AUTHORITY: usize = 176;
pub const M_STATE_ROOT: usize = 208;
pub const M_INPUT_CHAIN: usize = 240;
pub const M_CHECKPOINT: usize = 272;
/// Root of the start state this meta was initialised with, written once by
/// INIT and never changed.  RESET/9 recomputes the state root of the image it
/// is given and refuses unless it equals this, which is what makes a reset
/// land on *the* provisioned start state rather than one supplied at reset
/// time.  It occupies what was the meta's reserved word.
pub const M_START_ROOT: usize = 304;
/// Successful RESET/9 count, so a slot's reuse history is visible on chain.
pub const M_RESETS: usize = 336;
const M_RESERVED: usize = 340;

const B_CAPACITY: usize = 20;
const B_FIRST_TIC: usize = 24;
const B_SLOT_BYTES: usize = 28;
pub const B_AUTHORITY: usize = 32;
pub const B_CONTEXT: usize = 64;
pub const B_META: usize = 96;

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn refuse(code: u32) -> ProgramError {
    ProgramError::Custom(code)
}

/// A non-empty slice is all zero iff its first byte is zero and every byte
/// equals its predecessor (one overlapping, read-only `sol_memcmp`).
fn exact_zero(bytes: &[u8]) -> bool {
    bytes.is_empty()
        || (bytes[0] == 0
            && sol_memcmp(&bytes[1..], &bytes[..bytes.len() - 1], bytes.len() - 1) == 0)
}

// ---------------------------------------------------------------------------
// State commitment
// ---------------------------------------------------------------------------

/// Validates the ST1 account image and returns the encoded length.  The
/// account tail after the encoded image must be zero, so the root over the
/// encoded prefix binds all 126,976 bytes.
pub fn state_len_checked(state: &[u8]) -> Result<usize, ProgramError> {
    if state.len() != resource_sbf::STATE_MAX_BYTES
        || &state[..8] != b"DOOM-ST1"
        || u32_at(state, 8) != 0
        || u32_at(state, 12) != 64
    {
        return Err(refuse(REFUSE_STATE_SHAPE));
    }
    let len = u32_at(state, 16) as usize;
    if !(64..=resource_sbf::STATE_MAX_BYTES).contains(&len) || !exact_zero(&state[len..]) {
        return Err(refuse(REFUSE_STATE_SHAPE));
    }
    Ok(len)
}

/// Validates the MP1 account image and returns the used prefix
/// (header plus `count` slots).  Every byte after it must be zero.
pub fn mirror_used_checked(mirror: &[u8]) -> Result<usize, ProgramError> {
    if mirror.len() != resource_sbf::MIRROR_BYTES
        || &mirror[..8] != b"DOOM-MP1"
        || u32_at(mirror, 8) != 1
        || u32_at(mirror, 12) as usize != MIRROR_HEADER_BYTES
        || u32_at(mirror, 16) as usize != resource_sbf::MIRROR_BYTES
        || u32_at(mirror, 20) as usize != MIRROR_SLOT_BYTES
        || u32_at(mirror, 24) as usize != MIRROR_SLOTS
        || mirror[36..68] != WAD_HASH
    {
        return Err(refuse(REFUSE_STATE_SHAPE));
    }
    let count = u32_at(mirror, 28) as usize;
    let capped = if count < MIRROR_SLOTS {
        count as u32
    } else {
        u32::MAX
    };
    let used = MIRROR_HEADER_BYTES + count * MIRROR_SLOT_BYTES;
    if count > MIRROR_SLOTS
        || u32_at(mirror, 32) != capped
        || !exact_zero(&mirror[68..MIRROR_HEADER_BYTES])
        || !exact_zero(&mirror[used..])
    {
        return Err(refuse(REFUSE_STATE_SHAPE));
    }
    Ok(used)
}

/// `H(DOMAIN, len(ST1), ST1, used(MP1), MP1[..used])`, a full binding of both
/// account images because both tails are checked to be zero.
pub fn state_root(state: &[u8], state_len: usize, mirror: &[u8], mirror_used: usize) -> [u8; 32] {
    hashv(&[
        DOMAIN_STATE,
        &(state_len as u32).to_le_bytes(),
        &state[..state_len],
        &(mirror_used as u32).to_le_bytes(),
        &mirror[..mirror_used],
    ])
    .to_bytes()
}

pub fn verified_state_root(state: &[u8], mirror: &[u8]) -> Result<(usize, [u8; 32]), ProgramError> {
    let state_len = state_len_checked(state)?;
    let mirror_used = mirror_used_checked(mirror)?;
    Ok((state_len, state_root(state, state_len, mirror, mirror_used)))
}

pub fn chain_init(context: &Pubkey, first_tic: u32) -> [u8; 32] {
    hashv(&[DOMAIN_CHAIN, context.as_ref(), &first_tic.to_le_bytes()]).to_bytes()
}

pub fn chain_step(chain: &[u8; 32], seq: u32, command: &[u8; 4]) -> [u8; 32] {
    hashv(&[DOMAIN_CHAIN, chain, &seq.to_le_bytes(), command]).to_bytes()
}

pub fn checkpoint_hash(context: &Pubkey, checkpoint: &[u8]) -> [u8; 32] {
    hashv(&[DOMAIN_CHECKPOINT, context.as_ref(), checkpoint]).to_bytes()
}

pub fn meta_address(program: &Pubkey, context: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[META_SEED, context.as_ref()], program)
}

fn checkpoint_published(checkpoint: &[u8]) -> bool {
    checkpoint.len() == CHECKPOINT_BYTES
        && &checkpoint[..8] == continuation_split_sbf::CHECKPOINT_MAGIC
        && u32_at(checkpoint, 8) == continuation_split_sbf::CHECKPOINT_VERSION
        && u32_at(checkpoint, 12) == CHECKPOINT_PHASE_PUBLISHED
        && u32_at(checkpoint, 16) == CHECKPOINT_CURSOR_PUBLISHED
        && u32_at(checkpoint, 84) == 0
        && u32_at(checkpoint, 88) as usize == CHECKPOINT_OFFSET
}

// ---------------------------------------------------------------------------
// Instruction data
// ---------------------------------------------------------------------------

fn header_ok(data: &[u8], operation: u8) -> bool {
    data.len() >= REQUEST_HEADER_BYTES
        && &data[..8] == MAGIC
        && u32_at(data, 8) == VERSION
        && data[12] == operation
        && data[13..16] == [0, 0, 0]
}

pub fn request_header(operation: u8) -> [u8; REQUEST_HEADER_BYTES] {
    let mut out = [0u8; REQUEST_HEADER_BYTES];
    out[..8].copy_from_slice(MAGIC);
    put_u32(&mut out, 8, VERSION);
    out[12] = operation;
    out
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitRequest {
    pub bump: u8,
    /// Slot count fixed by INIT.  SIM2INPT/2 legacy requests omit this and
    /// imply the historical fixed 5,120-slot layout.
    pub capacity: u32,
    /// E2R/5 account keys not present in the SIM/2 account list, in E2R/5
    /// order: resource, wad0..wad4, controller, frame, continuation.
    pub e2r_keys: [Pubkey; INIT_KEY_COUNT],
    pub resource_hash: [u8; 32],
    pub controller_hash: [u8; 32],
    pub frame_hash: [u8; 32],
}

impl InitRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = request_header(INIT).to_vec();
        out.push(self.bump);
        out.extend_from_slice(&[0, 0, 0]);
        #[cfg(not(feature = "legacy-5120-buffer"))]
        out.extend_from_slice(&self.capacity.to_le_bytes());
        for key in &self.e2r_keys {
            out.extend_from_slice(key.as_ref());
        }
        out.extend_from_slice(&self.resource_hash);
        out.extend_from_slice(&self.controller_hash);
        out.extend_from_slice(&self.frame_hash);
        out
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != INIT_BYTES || !header_ok(data, INIT) || data[17..20] != [0, 0, 0] {
            return Err(refuse(REFUSE_MALFORMED));
        }
        #[cfg(feature = "legacy-5120-buffer")]
        let (capacity, keys_at) = (BUFFER_CAPACITY as u32, 20);
        #[cfg(not(feature = "legacy-5120-buffer"))]
        let (capacity, keys_at) = (u32_at(data, 20), 24);
        if capacity == 0 || capacity as usize > MAX_BUFFER_CAPACITY {
            return Err(refuse(REFUSE_MALFORMED));
        }
        let mut e2r_keys = [Pubkey::default(); INIT_KEY_COUNT];
        for (index, key) in e2r_keys.iter_mut().enumerate() {
            let at = keys_at + index * 32;
            *key = Pubkey::new_from_array(data[at..at + 32].try_into().unwrap());
        }
        let hashes = keys_at + INIT_KEY_COUNT * 32;
        Ok(Self {
            bump: data[16],
            capacity,
            e2r_keys,
            resource_hash: data[hashes..hashes + 32].try_into().unwrap(),
            controller_hash: data[hashes + 32..hashes + 64].try_into().unwrap(),
            frame_hash: data[hashes + 64..hashes + 96].try_into().unwrap(),
        })
    }
}

pub fn encode_grow_buffer(capacity: u32, expected_current_len: u32) -> Vec<u8> {
    let mut out = request_header(GROW_BUFFER).to_vec();
    out.extend_from_slice(&capacity.to_le_bytes());
    out.extend_from_slice(&expected_current_len.to_le_bytes());
    out
}

pub fn decode_grow_buffer(data: &[u8]) -> Result<(u32, u32), ProgramError> {
    if data.len() != GROW_BUFFER_BYTES || !header_ok(data, GROW_BUFFER) {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok((u32_at(data, 16), u32_at(data, 20)))
}

pub fn encode_extend_capacity(capacity: u32, expected_current_len: u32) -> Vec<u8> {
    let mut out = request_header(EXTEND_CAPACITY).to_vec();
    out.extend_from_slice(&capacity.to_le_bytes());
    out.extend_from_slice(&expected_current_len.to_le_bytes());
    out
}

pub fn decode_extend_capacity(data: &[u8]) -> Result<(u32, u32), ProgramError> {
    if data.len() != EXTEND_CAPACITY_BYTES || !header_ok(data, EXTEND_CAPACITY) {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok((u32_at(data, 16), u32_at(data, 20)))
}

/// Return the exact new byte length for one post-INIT capacity extension.
/// Solana limits account-data growth to 10,240 bytes in one instruction.
pub fn capacity_extension_bytes(current_capacity: usize, new_capacity: usize) -> Option<usize> {
    let current = buffer_bytes(current_capacity)?;
    let target = buffer_bytes(new_capacity)?;
    (new_capacity > current_capacity && target - current <= MAX_BUFFER_GROWTH).then_some(target)
}

pub fn encode_deposit(first_seq: u32, commands: &[u8]) -> Vec<u8> {
    let mut out = request_header(DEPOSIT).to_vec();
    out.extend_from_slice(&first_seq.to_le_bytes());
    out.extend_from_slice(&((commands.len() / 4) as u16).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(commands);
    out
}

pub fn decode_deposit(data: &[u8]) -> Result<(u32, &[u8]), ProgramError> {
    if !header_ok(data, DEPOSIT) || data.len() < DEPOSIT_HEADER_BYTES || data[22..24] != [0, 0] {
        return Err(refuse(REFUSE_MALFORMED));
    }
    let count = u16::from_le_bytes([data[20], data[21]]) as usize;
    if count == 0 || count > MAX_DEPOSIT_SLOTS || data.len() != DEPOSIT_HEADER_BYTES + count * 4 {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok((u32_at(data, 16), &data[DEPOSIT_HEADER_BYTES..]))
}

pub fn encode_advance(max_tics: u32) -> Vec<u8> {
    let mut out = request_header(ADVANCE).to_vec();
    out.extend_from_slice(&max_tics.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out
}

/// Encode cursor-bound ADVANCE/4.  This separate operation preserves the
/// historical ADVANCE/3 wire contract while making an expected cursor part
/// of each pipelined packet's signed message.
pub fn encode_advance_expected(expected_cursor: u32, max_tics: u32) -> Vec<u8> {
    let mut out = request_header(ADVANCE_EXPECTED_CURSOR).to_vec();
    out.extend_from_slice(&expected_cursor.to_le_bytes());
    out.extend_from_slice(&max_tics.to_le_bytes());
    out
}

/// Encode cursor-bound ADVANCE/5: ADVANCE/4 with the DOOM-CI3 receipt rule
/// (a staged tic the engine did not consume is a refusal).
pub fn encode_advance_starved(expected_cursor: u32, max_tics: u32) -> Vec<u8> {
    let mut out = request_header(ADVANCE_STARVED).to_vec();
    out.extend_from_slice(&expected_cursor.to_le_bytes());
    out.extend_from_slice(&max_tics.to_le_bytes());
    out
}

pub fn decode_advance_starved(data: &[u8]) -> Result<(u32, u32), ProgramError> {
    if data.len() != ADVANCE_BYTES || !header_ok(data, ADVANCE_STARVED) {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok((u32_at(data, 16), u32_at(data, 20)))
}

pub fn decode_advance_expected(data: &[u8]) -> Result<(u32, u32), ProgramError> {
    if data.len() != ADVANCE_BYTES || !header_ok(data, ADVANCE_EXPECTED_CURSOR) {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok((u32_at(data, 16), u32_at(data, 20)))
}

/// Encode RESET/9.  `force` permits resetting a halted slot; `selector` names
/// the start state, and this version accepts only the meta's own.
pub fn encode_reset(selector: u32, force: bool) -> Vec<u8> {
    let mut out = request_header(RESET).to_vec();
    out.extend_from_slice(&(if force { RESET_FORCE } else { 0 }).to_le_bytes());
    out.extend_from_slice(&selector.to_le_bytes());
    out
}

pub fn decode_reset(data: &[u8]) -> Result<(u32, bool), ProgramError> {
    if data.len() != RESET_BYTES || !header_ok(data, RESET) {
        return Err(refuse(REFUSE_MALFORMED));
    }
    let flags = u32_at(data, 16);
    let selector = u32_at(data, 20);
    if flags & !RESET_FORCE != 0 || selector > RESET_START_STATE {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok((selector, flags & RESET_FORCE != 0))
}

pub fn decode_advance(data: &[u8]) -> Result<u32, ProgramError> {
    if data.len() != ADVANCE_BYTES || !header_ok(data, ADVANCE) || data[20..24] != [0; 4] {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok(u32_at(data, 16))
}

pub fn encode_snapshot(cursor: u32, root: &[u8; 32]) -> Vec<u8> {
    let mut out = request_header(SNAPSHOT).to_vec();
    out.extend_from_slice(&cursor.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(root);
    out
}

pub fn decode_snapshot(data: &[u8]) -> Result<(u32, [u8; 32]), ProgramError> {
    if data.len() != SNAPSHOT_BYTES || !header_ok(data, SNAPSHOT) || data[20..24] != [0; 4] {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok((u32_at(data, 16), data[24..56].try_into().unwrap()))
}

// ---------------------------------------------------------------------------
// Pure cores
// ---------------------------------------------------------------------------

pub struct InitParams {
    pub context: Pubkey,
    pub state: Pubkey,
    pub mirror: Pubkey,
    pub buffer: Pubkey,
    pub meta: Pubkey,
    pub authority: Pubkey,
    pub bump: u8,
    pub capacity: u32,
    pub first_tic: u32,
    pub state_len: u32,
    pub state_root: [u8; 32],
    pub checkpoint_hash: [u8; 32],
}

/// Writes the buffer header and the initial meta.  Both must be all zero:
/// a context has exactly one meta and a buffer is bound exactly once.
pub fn init_layout(meta: &mut [u8], buffer: &mut [u8], params: &InitParams) -> ProgramResult {
    let capacity = params.capacity as usize;
    let Some(total_bytes) = buffer_bytes(capacity) else {
        return Err(refuse(REFUSE_MALFORMED));
    };
    if meta.len() != META_BYTES || buffer.len() != total_bytes {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if params
        .first_tic
        .checked_add(params.capacity)
        .is_none_or(|end| end > MAX_GAME_TIC)
    {
        return Err(refuse(REFUSE_TIC));
    }
    if !exact_zero(meta) || !preinit_buffer_ok(buffer, capacity) {
        return Err(refuse(REFUSE_ALREADY_INIT));
    }
    buffer[..8].copy_from_slice(BUFFER_MAGIC);
    put_u32(buffer, 8, BUFFER_FORMAT_VERSION);
    put_u32(buffer, 12, BUFFER_HEADER_BYTES as u32);
    put_u32(buffer, 16, total_bytes as u32);
    put_u32(buffer, B_CAPACITY, params.capacity);
    put_u32(buffer, B_FIRST_TIC, params.first_tic);
    put_u32(buffer, B_SLOT_BYTES, SLOT_BYTES as u32);
    buffer[B_AUTHORITY..B_AUTHORITY + 32].copy_from_slice(params.authority.as_ref());
    buffer[B_CONTEXT..B_CONTEXT + 32].copy_from_slice(params.context.as_ref());
    buffer[B_META..B_META + 32].copy_from_slice(params.meta.as_ref());

    meta[..8].copy_from_slice(META_MAGIC);
    put_u32(meta, 8, VERSION);
    put_u32(meta, 12, META_BYTES as u32);
    put_u32(meta, 16, META_BYTES as u32);
    meta[M_BUMP] = params.bump;
    put_u32(meta, M_FIRST_TIC, params.first_tic);
    put_u32(meta, M_STATE_LEN, params.state_len);
    put_u32(meta, M_CAPACITY, params.capacity);
    meta[M_CONTEXT..M_CONTEXT + 32].copy_from_slice(params.context.as_ref());
    meta[M_STATE..M_STATE + 32].copy_from_slice(params.state.as_ref());
    meta[M_MIRROR..M_MIRROR + 32].copy_from_slice(params.mirror.as_ref());
    meta[M_BUFFER..M_BUFFER + 32].copy_from_slice(params.buffer.as_ref());
    meta[M_AUTHORITY..M_AUTHORITY + 32].copy_from_slice(params.authority.as_ref());
    meta[M_STATE_ROOT..M_STATE_ROOT + 32].copy_from_slice(&params.state_root);
    // The start state is the state this meta is initialised with.  RESET/9
    // refuses any other image, so the root is recorded here, once.
    meta[M_START_ROOT..M_START_ROOT + 32].copy_from_slice(&params.state_root);
    meta[M_INPUT_CHAIN..M_INPUT_CHAIN + 32]
        .copy_from_slice(&chain_init(&params.context, params.first_tic));
    meta[M_CHECKPOINT..M_CHECKPOINT + 32].copy_from_slice(&params.checkpoint_hash);
    Ok(())
}

fn buffer_header_ok(buffer: &[u8]) -> bool {
    if buffer.len() < BUFFER_HEADER_BYTES || &buffer[..8] != BUFFER_MAGIC {
        return false;
    }
    let capacity = u32_at(buffer, B_CAPACITY) as usize;
    let Some(expected_bytes) = buffer_bytes(capacity) else {
        return false;
    };
    buffer.len() == expected_bytes
        && u32_at(buffer, 8) == BUFFER_FORMAT_VERSION
        && u32_at(buffer, 12) as usize == BUFFER_HEADER_BYTES
        && u32_at(buffer, 16) as usize == expected_bytes
        && u32_at(buffer, B_SLOT_BYTES) as usize == SLOT_BYTES
}

/// Before INIT, only GROW-BUFFER may have touched a large input account. Its
/// fixed header marker proves the intended capacity and completed length;
/// account creation and realloc zero-fill every slot byte. This avoids an
/// O(524 KiB) zero scan inside INIT while keeping staged, pre-INIT writes out.
fn preinit_buffer_ok(buffer: &[u8], capacity: usize) -> bool {
    let Some(total) = buffer_bytes(capacity) else {
        return false;
    };
    if buffer.len() != total || buffer.len() < BUFFER_HEADER_BYTES {
        return false;
    }
    if total <= MAX_BUFFER_GROWTH {
        return exact_zero(buffer);
    }
    &buffer[..8] == BUFFER_GROW_MAGIC
        && u32_at(buffer, 8) == BUFFER_FORMAT_VERSION
        && u32_at(buffer, 12) as usize == capacity
        && u32_at(buffer, 16) as usize == total
        && buffer[20..BUFFER_HEADER_BYTES]
            .iter()
            .all(|byte| *byte == 0)
}

fn slot(buffer: &[u8], seq: usize) -> &[u8] {
    let at = BUFFER_HEADER_BYTES + seq * SLOT_BYTES;
    &buffer[at..at + SLOT_BYTES]
}

/// DEPOSIT: write `commands` (4 bytes per tic) into slots
/// `first_seq..first_seq + n`.  All-or-nothing; never reads the cursor.
pub fn deposit(
    buffer: &mut [u8],
    signer: &Pubkey,
    first_seq: u32,
    commands: &[u8],
) -> ProgramResult {
    if !buffer_header_ok(buffer) {
        return Err(refuse(REFUSE_BINDING));
    }
    if buffer[B_AUTHORITY..B_AUTHORITY + 32] != signer.as_ref()[..] {
        return Err(refuse(REFUSE_AUTHORITY));
    }
    let count = commands.len() / 4;
    if commands.len() % 4 != 0 || count == 0 || count > MAX_DEPOSIT_SLOTS {
        return Err(refuse(REFUSE_MALFORMED));
    }
    let first = first_seq as usize;
    if first
        .checked_add(count)
        .is_none_or(|end| end > u32_at(buffer, B_CAPACITY) as usize)
    {
        return Err(refuse(REFUSE_SLOT_RANGE));
    }
    if commands
        .chunks_exact(4)
        .any(|command| command[3] & BT_SPECIAL != 0)
    {
        return Err(refuse(REFUSE_COMMAND));
    }
    if (first..first + count).any(|seq| slot(buffer, seq)[4] != 0) {
        return Err(refuse(REFUSE_DUPLICATE_SLOT));
    }
    for (index, command) in commands.chunks_exact(4).enumerate() {
        let at = BUFFER_HEADER_BYTES + (first + index) * SLOT_BYTES;
        buffer[at..at + 4].copy_from_slice(command);
        buffer[at + 4] = SLOT_WRITTEN;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepResult {
    Ran,
    Halt(u8),
    /// The tic ran, but the engine did not consume the command the caller
    /// armed.  Only ADVANCE/5 refuses on this; ADVANCE/3 and /4 keep counting
    /// it as a ran tic.
    Unconsumed,
}

/// One resident engine: `step` runs one tic (or halts before it), `publish`
/// derives ST1/MP1 from the live engine.
pub trait Engine {
    fn step(&mut self, command: &[u8; 4]) -> Result<StepResult, u32>;
    fn publish(&mut self, state: &mut [u8], mirror: &mut [u8]) -> Result<usize, u32>;
}

pub trait Budget {
    fn remaining(&self) -> u64;
}

/// The reset half of a resident engine: re-seed it from a committed start
/// image and derive the mirror from the decoded engine.  The image is read,
/// never written, and the mirror is derived rather than read from an account,
/// so a reset cannot install a pool that disagrees with the state it claims to
/// describe.
pub trait ResetEngine {
    fn reset(&mut self, state: &[u8], mirror: &mut [u8]) -> Result<(), u32>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Stop {
    /// The slot at the cursor is empty: nothing to do.
    NoneStaged = 0,
    /// At least one tic ran, then an empty slot.
    Gap = 1,
    Budget = 2,
    MaxTics = 3,
    Capacity = 4,
    Halt = 5,
    /// The meta was already halted.
    Halted = 6,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SweepReport {
    pub cursor_before: u32,
    pub cursor_after: u32,
    pub ran: u32,
    pub stop: Stop,
    pub halt: u8,
    pub state_root: [u8; 32],
    pub input_chain: [u8; 32],
}

impl SweepReport {
    pub const BYTES: usize = 78;

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        out[..4].copy_from_slice(&self.cursor_before.to_le_bytes());
        out[4..8].copy_from_slice(&self.cursor_after.to_le_bytes());
        out[8..12].copy_from_slice(&self.ran.to_le_bytes());
        out[12] = self.stop as u8;
        out[13] = self.halt;
        out[14..46].copy_from_slice(&self.state_root);
        out[46..78].copy_from_slice(&self.input_chain);
        out
    }
}

pub struct SweepKeys<'a> {
    pub context: &'a Pubkey,
    pub state: &'a Pubkey,
    pub mirror: &'a Pubkey,
    pub buffer: &'a Pubkey,
    pub meta: &'a Pubkey,
    pub signer: &'a Pubkey,
}

/// Checks the meta is a live SIM/2 meta bound to exactly these accounts and
/// this signer, and that the published checkpoint is unchanged.
pub fn validate_meta(meta: &[u8], keys: &SweepKeys, checkpoint: &[u8]) -> ProgramResult {
    if meta.len() != META_BYTES
        || &meta[..8] != META_MAGIC
        || u32_at(meta, 8) != VERSION
        || u32_at(meta, 12) as usize != META_BYTES
        || u32_at(meta, 16) as usize != META_BYTES
        || meta[22..24] != [0, 0]
        || u32_at(meta, M_CAPACITY) == 0
        || u32_at(meta, M_CAPACITY) as usize > MAX_BUFFER_CAPACITY
        || meta[M_CONTEXT..M_CONTEXT + 32] != keys.context.as_ref()[..]
        || meta[M_STATE..M_STATE + 32] != keys.state.as_ref()[..]
        || meta[M_MIRROR..M_MIRROR + 32] != keys.mirror.as_ref()[..]
        || meta[M_BUFFER..M_BUFFER + 32] != keys.buffer.as_ref()[..]
        || !exact_zero(&meta[M_RESERVED..])
    {
        return Err(refuse(REFUSE_BINDING));
    }
    if meta[M_AUTHORITY..M_AUTHORITY + 32] != keys.signer.as_ref()[..] {
        return Err(refuse(REFUSE_AUTHORITY));
    }
    if !checkpoint_published(checkpoint)
        || meta[M_CHECKPOINT..M_CHECKPOINT + 32] != checkpoint_hash(keys.context, checkpoint)
    {
        return Err(refuse(REFUSE_CONTEXT));
    }
    Ok(())
}

/// ADVANCE core.  Executes consecutive written slots from the committed
/// cursor, then publishes ST1/MP1 once and commits the new cursor.
///
/// `require_consumed` is ADVANCE/5.  When it is set, a tic the engine did not
/// consume is `REFUSE_UNCONSUMED_INPUT` rather than a committed tic; when it
/// is clear (ADVANCE/3, ADVANCE/4) the step is counted, and the tic check then
/// refuses 284 instead, because DOOM-CI3 stops the real engine's `gametic++`
/// before a consumed tic exists.  Either way a starved tic is not committed.
#[allow(clippy::too_many_arguments)]
pub fn sweep(
    meta: &mut [u8],
    buffer: &[u8],
    state: &mut [u8],
    mirror: &mut [u8],
    keys: &SweepKeys,
    checkpoint: &[u8],
    max_tics: u32,
    require_consumed: bool,
    engine: &mut dyn Engine,
    budget: &dyn Budget,
) -> Result<SweepReport, ProgramError> {
    validate_meta(meta, keys, checkpoint)?;
    if !buffer_header_ok(buffer)
        || u32_at(buffer, B_CAPACITY) != u32_at(meta, M_CAPACITY)
        || buffer[B_AUTHORITY..B_AUTHORITY + 32] != meta[M_AUTHORITY..M_AUTHORITY + 32]
        || buffer[B_CONTEXT..B_CONTEXT + 32] != keys.context.as_ref()[..]
        || buffer[B_META..B_META + 32] != keys.meta.as_ref()[..]
        || u32_at(buffer, B_FIRST_TIC) != u32_at(meta, M_FIRST_TIC)
    {
        return Err(refuse(REFUSE_BINDING));
    }
    let first_tic = u32_at(meta, M_FIRST_TIC);
    let cursor = u32_at(meta, M_CURSOR);
    let mut report = SweepReport {
        cursor_before: cursor,
        cursor_after: cursor,
        ran: 0,
        stop: Stop::NoneStaged,
        halt: meta[M_HALT],
        state_root: meta[M_STATE_ROOT..M_STATE_ROOT + 32].try_into().unwrap(),
        input_chain: meta[M_INPUT_CHAIN..M_INPUT_CHAIN + 32].try_into().unwrap(),
    };
    if report.halt != 0 {
        report.stop = Stop::Halted;
        return Ok(report);
    }
    let capacity = u32_at(meta, M_CAPACITY);
    if cursor >= capacity {
        report.stop = Stop::Capacity;
        return Ok(report);
    }
    if slot(buffer, cursor as usize)[4] == 0 {
        return Ok(report);
    }
    // At least one staged tic: authenticate the live ST1/MP1 against the
    // committed root before the engine is touched.
    let (state_len, live_root) = verified_state_root(state, mirror)?;
    if state_len != u32_at(meta, M_STATE_LEN) as usize || live_root != report.state_root {
        return Err(refuse(REFUSE_STATE_ROOT));
    }
    crate::diagnostic_phase(922);
    let mut chain = report.input_chain;
    let mut next = cursor;
    let mut ran = 0u32;
    let mut halt = 0u8;
    let stop = loop {
        if max_tics != 0 && ran == max_tics {
            break Stop::MaxTics;
        }
        if next >= capacity {
            break Stop::Capacity;
        }
        let staged = slot(buffer, next as usize);
        if staged[4] == 0 {
            break Stop::Gap;
        }
        if staged[4] != SLOT_WRITTEN || staged[5..8] != [0, 0, 0] {
            return Err(refuse(REFUSE_MALFORMED));
        }
        let command: [u8; 4] = staged[..4].try_into().unwrap();
        if command[3] & BT_SPECIAL != 0 {
            return Err(refuse(REFUSE_COMMAND));
        }
        if budget.remaining() < TIC_RESERVE_CU + PUBLISH_RESERVE_CU {
            break Stop::Budget;
        }
        match engine.step(&command).map_err(refuse)? {
            StepResult::Ran => {
                chain = chain_step(&chain, next, &command);
                next += 1;
                ran += 1;
            }
            // ADVANCE/5: refuse before the cursor, the chain, the state root
            // or any engine byte is committed.  The instruction fails, so the
            // tic the engine already ran is rolled back with it.
            StepResult::Unconsumed if require_consumed => {
                return Err(refuse(REFUSE_UNCONSUMED_INPUT));
            }
            StepResult::Unconsumed => {
                chain = chain_step(&chain, next, &command);
                next += 1;
                ran += 1;
            }
            StepResult::Halt(code) => {
                halt = code;
                break Stop::Halt;
            }
        }
    };
    crate::diagnostic_phase(923);
    report.stop = stop;
    report.halt = halt;
    if ran == 0 {
        // Nothing executed (budget exhausted, or a halt that fires before
        // G_Ticker): the engine, ST1 and MP1 are untouched.
        if halt != 0 {
            meta[M_HALT] = halt;
            put_u32(meta, M_SWEEPS, u32_at(meta, M_SWEEPS).wrapping_add(1));
            put_u32(meta, M_LAST_SWEEP_TICS, 0);
        }
        return Ok(report);
    }
    let published_len = engine.publish(state, mirror).map_err(refuse)?;
    crate::diagnostic_phase(924);
    let (new_len, new_root) = verified_state_root(state, mirror)?;
    if new_len != published_len || Some(u32_at(state, 20)) != first_tic.checked_add(next) {
        return Err(refuse(REFUSE_TIC));
    }
    put_u32(meta, M_CURSOR, next);
    put_u32(meta, M_STATE_LEN, new_len as u32);
    put_u32(meta, M_SWEEPS, u32_at(meta, M_SWEEPS).wrapping_add(1));
    put_u32(meta, M_LAST_SWEEP_TICS, ran);
    meta[M_HALT] = halt;
    meta[M_STATE_ROOT..M_STATE_ROOT + 32].copy_from_slice(&new_root);
    meta[M_INPUT_CHAIN..M_INPUT_CHAIN + 32].copy_from_slice(&chain);
    report.cursor_after = next;
    report.ran = ran;
    report.state_root = new_root;
    report.input_chain = chain;
    crate::diagnostic_phase(925);
    Ok(report)
}

/// RESET/9 core.  Returns the state root the slot was reset to.
///
/// Every refusal happens before the engine is touched or the meta is written,
/// except the root check, which needs the derived mirror; an error from this
/// function fails the whole instruction, so the engine change, the derived
/// mirror and the cleared buffer are rolled back with it.  The checks, in
/// order: the meta is a live meta bound to exactly these accounts and this
/// signer (271, 272, 273); the buffer is the bound input buffer; a halted slot
/// needs the force flag (289); the staged image is shape-valid (281) and sits
/// at the meta's first tic (284); the decoded pair's root is the root this
/// meta was initialised with (280).
pub fn reset(
    meta: &mut [u8],
    buffer: &mut [u8],
    state: &mut [u8],
    start: &[u8],
    mirror: &mut [u8],
    keys: &SweepKeys,
    checkpoint: &[u8],
    force: bool,
    engine: &mut dyn ResetEngine,
) -> Result<[u8; 32], ProgramError> {
    validate_meta(meta, keys, checkpoint)?;
    if !buffer_header_ok(buffer)
        || u32_at(buffer, B_CAPACITY) != u32_at(meta, M_CAPACITY)
        || buffer[B_AUTHORITY..B_AUTHORITY + 32] != meta[M_AUTHORITY..M_AUTHORITY + 32]
        || buffer[B_CONTEXT..B_CONTEXT + 32] != keys.context.as_ref()[..]
        || buffer[B_META..B_META + 32] != keys.meta.as_ref()[..]
        || u32_at(buffer, B_FIRST_TIC) != u32_at(meta, M_FIRST_TIC)
    {
        return Err(refuse(REFUSE_BINDING));
    }
    if meta[M_HALT] != 0 && !force {
        return Err(refuse(REFUSE_HALTED));
    }
    // The start image is checked before the state account is touched, so a
    // malformed or misplaced image cannot leave a half-reset slot behind even
    // before the transaction rollback is considered.
    let state_len = state_len_checked(start)?;
    let first_tic = u32_at(meta, M_FIRST_TIC);
    if u32_at(start, 20) != first_tic {
        return Err(refuse(REFUSE_TIC));
    }
    if start.as_ptr() as usize != state.as_ptr() as usize {
        // A supplied start account: install the image and its zero tail into
        // the bound state account, which is the account every later read, the
        // render path included, is bound to.
        sol_memcpy(state, start, state_len);
        sol_memset(&mut state[state_len..], 0, state.len() - state_len);
    }
    crate::diagnostic_phase(926);
    engine.reset(state, mirror).map_err(refuse)?;
    crate::diagnostic_phase(927);
    let (checked, root) = verified_state_root(state, mirror)?;
    if checked != state_len || meta[M_START_ROOT..M_START_ROOT + 32] != root[..] {
        return Err(refuse(REFUSE_STATE_ROOT));
    }
    // The staged slots are write-once and indexed by tic, so a slot that still
    // holds a command from the finished session would be executed again after
    // the rewind.  Zeroing the body drops both the command bytes and every
    // SLOT_WRITTEN byte; the header, and with it the capacity, the first tic,
    // the authority and the account bindings, is left alone so the checks
    // above keep holding.
    let body = buffer.len() - BUFFER_HEADER_BYTES;
    sol_memset(&mut buffer[BUFFER_HEADER_BYTES..], 0, body);
    put_u32(meta, M_CURSOR, 0);
    put_u32(meta, M_STATE_LEN, state_len as u32);
    put_u32(meta, M_SWEEPS, 0);
    put_u32(meta, M_LAST_SWEEP_TICS, 0);
    meta[M_HALT] = 0;
    meta[M_STATE_ROOT..M_STATE_ROOT + 32].copy_from_slice(&root);
    meta[M_INPUT_CHAIN..M_INPUT_CHAIN + 32].copy_from_slice(&chain_init(keys.context, first_tic));
    put_u32(meta, M_RESETS, u32_at(meta, M_RESETS).wrapping_add(1));
    Ok(root)
}

/// SNAPSHOT core: a reader may use ST1/MP1 only at the committed cursor and
/// root.
pub fn snapshot_check(
    meta: &[u8],
    state: &[u8],
    mirror: &[u8],
    state_key: &Pubkey,
    mirror_key: &Pubkey,
    cursor: u32,
    root: &[u8; 32],
) -> ProgramResult {
    if meta.len() != META_BYTES
        || &meta[..8] != META_MAGIC
        || u32_at(meta, 8) != VERSION
        || meta[M_STATE..M_STATE + 32] != state_key.as_ref()[..]
        || meta[M_MIRROR..M_MIRROR + 32] != mirror_key.as_ref()[..]
    {
        return Err(refuse(REFUSE_BINDING));
    }
    let (_, live) = verified_state_root(state, mirror)?;
    if u32_at(meta, M_CURSOR) != cursor
        || meta[M_STATE_ROOT..M_STATE_ROOT + 32] != root[..]
        || live != *root
    {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    Ok(())
}

/// Validate the committed SIM2 state and the already-resident Doom context
/// for a render snapshot. This reads the engine context produced by E2R/5 and
/// ADVANCE; it deliberately does not reconstruct it from ST1 or WAD accounts.
pub fn validate_live_snapshot_state(
    program: &Pubkey,
    context: &AccountInfo,
    meta: &AccountInfo,
    state: &AccountInfo,
    mirror: &AccountInfo,
    cursor: u32,
    root: &[u8; 32],
) -> ProgramResult {
    if !owned(program, context, resource_sbf::CONTEXT_BYTES, false)
        || !owned(program, meta, META_BYTES, false)
        || !owned(program, state, resource_sbf::STATE_MAX_BYTES, false)
        || !owned(program, mirror, resource_sbf::MIRROR_BYTES, false)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let meta_data = meta.try_borrow_data()?;
    if meta_data.len() != META_BYTES
        || &meta_data[..8] != META_MAGIC
        || u32_at(&meta_data, 8) != VERSION
        || u32_at(&meta_data, 12) as usize != META_BYTES
        || u32_at(&meta_data, 16) as usize != META_BYTES
        || meta_data[22..24] != [0, 0]
        || !exact_zero(&meta_data[M_RESERVED..])
        || meta_data[M_CONTEXT..M_CONTEXT + 32] != context.key.as_ref()[..]
        || meta_data[M_STATE..M_STATE + 32] != state.key.as_ref()[..]
        || meta_data[M_MIRROR..M_MIRROR + 32] != mirror.key.as_ref()[..]
    {
        return Err(refuse(REFUSE_BINDING));
    }
    check_meta_address(program, context.key, meta.key, meta_data[M_BUMP])?;
    let checkpoint = read_checkpoint(context)?;
    if !checkpoint_published(&checkpoint)
        || meta_data[M_CHECKPOINT..M_CHECKPOINT + 32] != checkpoint_hash(context.key, &checkpoint)
    {
        return Err(refuse(REFUSE_CONTEXT));
    }
    if u32_at(&meta_data, M_CURSOR) != cursor
        || meta_data[M_STATE_ROOT..M_STATE_ROOT + 32] != root[..]
    {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let state_data = state.try_borrow_data()?;
    let state_len = state_len_checked(&state_data)?;
    if state_len != u32_at(&meta_data, M_STATE_LEN) as usize
        || u32_at(&state_data, 20)
            != u32_at(&meta_data, M_FIRST_TIC)
                .checked_add(cursor)
                .ok_or(refuse(REFUSE_TIC))?
    {
        return Err(refuse(REFUSE_TIC));
    }
    snapshot_check(
        &meta_data,
        &state_data,
        &mirror.try_borrow_data()?,
        state.key,
        mirror.key,
        cursor,
        root,
    )
}

/// INIT binding: the context is a published E2R/5 reconstruction whose
/// account set (all 13 keys, including STATE, MIRROR and the authority) and
/// component commitment (including the live ST1/MP1 bytes) are exactly the
/// ones the checkpoint recorded.
#[allow(clippy::too_many_arguments)]
pub fn verify_publication(
    program: &Pubkey,
    checkpoint: &[u8],
    context: &Pubkey,
    state_key: &Pubkey,
    mirror_key: &Pubkey,
    authority: &Pubkey,
    state: &[u8],
    mirror: &[u8],
    request: &InitRequest,
) -> ProgramResult {
    if !checkpoint_published(checkpoint) {
        return Err(refuse(REFUSE_CONTEXT));
    }
    let k = &request.e2r_keys;
    let keys: [&Pubkey; continuation_split_sbf::ACCOUNT_COUNT] = [
        context, &k[0], &k[1], &k[2], &k[3], &k[4], &k[5], state_key, mirror_key, &k[6], &k[7],
        &k[8], authority,
    ];
    if continuation_split_sbf::account_set_root(program, &keys) != checkpoint[52..84] {
        return Err(refuse(REFUSE_E2R_BINDING));
    }
    let components = continuation_split_sbf::published_components(
        &request.resource_hash,
        &hash(state).to_bytes(),
        &hash(mirror).to_bytes(),
        &request.controller_hash,
        &request.frame_hash,
    );
    if components != checkpoint[96..128] {
        return Err(refuse(REFUSE_E2R_BINDING));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Program handlers
// ---------------------------------------------------------------------------

#[cfg(target_os = "solana")]
unsafe extern "C" {
    fn doom_sbf_sim2_step(context: *mut u8, command: *const u8, command_len: usize) -> i32;
    fn doom_sbf_sim2_publish(
        context: *mut u8,
        state_out: *mut u8,
        state_cap: usize,
        state_out_len: *mut usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
    ) -> i32;
    fn doom_sbf_sim2_reset(
        context: *mut u8,
        state: *const u8,
        state_len: usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
    ) -> i32;
}

/// The engine resident in the pinned context account (account 0).
struct ResidentEngine {
    #[cfg_attr(not(target_os = "solana"), allow(dead_code))]
    context: *mut u8,
}

impl Engine for ResidentEngine {
    #[cfg(target_os = "solana")]
    fn step(&mut self, command: &[u8; 4]) -> Result<StepResult, u32> {
        match unsafe { doom_sbf_sim2_step(self.context, command.as_ptr(), 4) } {
            0 => Ok(StepResult::Ran),
            code @ 1..=5 => Ok(StepResult::Halt(code as u8)),
            238 => Ok(StepResult::Unconsumed),
            code => Err(code as u32),
        }
    }

    #[cfg(target_os = "solana")]
    fn publish(&mut self, state: &mut [u8], mirror: &mut [u8]) -> Result<usize, u32> {
        let mut len = 0usize;
        match unsafe {
            doom_sbf_sim2_publish(
                self.context,
                state.as_mut_ptr(),
                state.len(),
                &mut len,
                mirror.as_mut_ptr(),
                mirror.len(),
            )
        } {
            0 => Ok(len),
            code => Err(code as u32),
        }
    }

    #[cfg(not(target_os = "solana"))]
    fn step(&mut self, _command: &[u8; 4]) -> Result<StepResult, u32> {
        Err(244)
    }

    #[cfg(not(target_os = "solana"))]
    fn publish(&mut self, _state: &mut [u8], _mirror: &mut [u8]) -> Result<usize, u32> {
        Err(244)
    }
}

impl ResetEngine for ResidentEngine {
    #[cfg(target_os = "solana")]
    fn reset(&mut self, state: &[u8], mirror: &mut [u8]) -> Result<(), u32> {
        match unsafe {
            doom_sbf_sim2_reset(
                self.context,
                state.as_ptr(),
                state.len(),
                mirror.as_mut_ptr(),
                mirror.len(),
            )
        } {
            0 => Ok(()),
            code => Err(code as u32),
        }
    }

    #[cfg(not(target_os = "solana"))]
    fn reset(&mut self, _state: &[u8], _mirror: &mut [u8]) -> Result<(), u32> {
        Err(244)
    }
}

/// The deployable budget: the compute meter where the runtime exposes it
/// (`metered-sweep`), otherwise no budget stop (the tic cap bounds the sweep).
/// The count-capped image references no `sol_remaining_compute_units`.
struct ComputeMeter;

impl Budget for ComputeMeter {
    fn remaining(&self) -> u64 {
        #[cfg(all(target_os = "solana", feature = "metered-sweep"))]
        {
            solana_program::compute_units::sol_remaining_compute_units()
        }
        #[cfg(not(all(target_os = "solana", feature = "metered-sweep")))]
        {
            u64::MAX
        }
    }
}

fn distinct(accounts: &[AccountInfo]) -> bool {
    accounts.iter().enumerate().all(|(index, account)| {
        accounts[..index]
            .iter()
            .all(|other| other.key != account.key)
    })
}

/// Program-owned, exact size, not a signer, and writable when the handler
/// writes it.  Read-only use does not require a read-only flag: a
/// transaction may legitimately mark the account writable for another
/// instruction.
fn owned(program: &Pubkey, account: &AccountInfo, len: usize, written: bool) -> bool {
    account.owner == program
        && account.data_len() == len
        && (account.is_writable || !written)
        && !account.is_signer
}

fn check_meta_address(
    program: &Pubkey,
    context: &Pubkey,
    meta: &Pubkey,
    bump: u8,
) -> ProgramResult {
    let (canonical, canonical_bump) =
        Pubkey::find_program_address(&[META_SEED, context.as_ref()], program);
    if bump != canonical_bump || *meta != canonical {
        return Err(refuse(REFUSE_BINDING));
    }
    Ok(())
}

fn read_checkpoint(context: &AccountInfo) -> Result<[u8; CHECKPOINT_BYTES], ProgramError> {
    let data = context.try_borrow_data()?;
    Ok(
        data[CHECKPOINT_OFFSET..CHECKPOINT_OFFSET + CHECKPOINT_BYTES]
            .try_into()
            .unwrap(),
    )
}

fn create_meta<'a>(
    program: &Pubkey,
    meta: &AccountInfo<'a>,
    authority: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    context: &Pubkey,
    bump: u8,
) -> ProgramResult {
    #[allow(deprecated)]
    use solana_program::system_instruction;
    use solana_program::{
        program::{invoke, invoke_signed},
        rent::Rent,
        sysvar::Sysvar,
    };
    let seeds: &[&[u8]] = &[META_SEED, context.as_ref(), &[bump]];
    let rent = Rent::get()?.minimum_balance(META_BYTES);
    if meta.lamports() == 0 {
        invoke_signed(
            &system_instruction::create_account(
                authority.key,
                meta.key,
                rent,
                META_BYTES as u64,
                program,
            ),
            &[authority.clone(), meta.clone(), system.clone()],
            &[seeds],
        )
    } else {
        // A pre-funded PDA cannot be created with create_account; top it up,
        // then allocate and assign it under the program's signature.
        let shortfall = rent.saturating_sub(meta.lamports());
        if shortfall > 0 {
            invoke(
                &system_instruction::transfer(authority.key, meta.key, shortfall),
                &[authority.clone(), meta.clone(), system.clone()],
            )?;
        }
        invoke_signed(
            &system_instruction::allocate(meta.key, META_BYTES as u64),
            &[meta.clone(), system.clone()],
            &[seeds],
        )?;
        invoke_signed(
            &system_instruction::assign(meta.key, program),
            &[meta.clone(), system.clone()],
            &[seeds],
        )
    }
}

fn process_init(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [context, state, mirror, meta, buffer, authority, system] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    let request = InitRequest::decode(data)?;
    let buffer_len =
        buffer_bytes(request.capacity as usize).ok_or_else(|| refuse(REFUSE_MALFORMED))?;
    if meta.owner == program {
        return Err(refuse(REFUSE_ALREADY_INIT));
    }
    if !owned(program, context, resource_sbf::CONTEXT_BYTES, false)
        || !owned(program, state, resource_sbf::STATE_MAX_BYTES, false)
        || !owned(program, mirror, resource_sbf::MIRROR_BYTES, false)
        || !owned(program, buffer, buffer_len, true)
        || *meta.owner != solana_program::system_program::ID
        || meta.data_len() != 0
        || !meta.is_writable
        || meta.is_signer
        || *system.key != solana_program::system_program::ID
        || !distinct(&accounts[..6])
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if !authority.is_signer || !authority.is_writable {
        return Err(refuse(REFUSE_AUTHORITY));
    }
    check_meta_address(program, context.key, meta.key, request.bump)?;
    let checkpoint = read_checkpoint(context)?;
    let (state_len, root, first_tic) = {
        let state_data = state.try_borrow_data()?;
        let mirror_data = mirror.try_borrow_data()?;
        verify_publication(
            program,
            &checkpoint,
            context.key,
            state.key,
            mirror.key,
            authority.key,
            &state_data,
            &mirror_data,
            &request,
        )?;
        let (state_len, root) = verified_state_root(&state_data, &mirror_data)?;
        (state_len, root, u32_at(&state_data, 20))
    };
    create_meta(program, meta, authority, system, context.key, request.bump)?;
    let mut meta_data = meta.try_borrow_mut_data()?;
    let mut buffer_data = buffer.try_borrow_mut_data()?;
    init_layout(
        &mut meta_data,
        &mut buffer_data,
        &InitParams {
            context: *context.key,
            state: *state.key,
            mirror: *mirror.key,
            buffer: *buffer.key,
            meta: *meta.key,
            authority: *authority.key,
            bump: request.bump,
            capacity: request.capacity,
            first_tic,
            state_len: state_len as u32,
            state_root: root,
            checkpoint_hash: checkpoint_hash(context.key, &checkpoint),
        },
    )
}

fn process_deposit(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [buffer, authority] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if buffer.owner != program
        || buffer.data_len() < BUFFER_HEADER_BYTES
        || !buffer.is_writable
        || buffer.is_signer
        || buffer.key == authority.key
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if !authority.is_signer {
        return Err(refuse(REFUSE_AUTHORITY));
    }
    let (first_seq, commands) = decode_deposit(data)?;
    let mut buffer_data = buffer.try_borrow_mut_data()?;
    deposit(&mut buffer_data, authority.key, first_seq, commands)
}

fn process_grow_buffer(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [buffer] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if buffer.owner != program || buffer.data_len() == 0 || !buffer.is_writable || !buffer.is_signer
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let (capacity, expected_current_len) = decode_grow_buffer(data)?;
    let capacity = capacity as usize;
    let current = buffer.data_len();
    if current != expected_current_len as usize {
        return Err(refuse(REFUSE_BUFFER_GROWTH));
    }
    let initial = initial_buffer_bytes(capacity).ok_or_else(|| refuse(REFUSE_MALFORMED))?;
    if current < initial {
        return Err(refuse(REFUSE_BUFFER_GROWTH));
    }
    let mut reached = initial;
    while reached < current {
        reached = next_buffer_len(reached, capacity).ok_or_else(|| refuse(REFUSE_BUFFER_GROWTH))?;
    }
    if reached != current {
        return Err(refuse(REFUSE_BUFFER_GROWTH));
    }
    let next = next_buffer_len(current, capacity).ok_or_else(|| refuse(REFUSE_BUFFER_GROWTH))?;
    {
        let mut data = buffer.try_borrow_mut_data()?;
        if current == initial && data[..8] == [0; 8] {
            data[..8].copy_from_slice(BUFFER_GROW_MAGIC);
            put_u32(&mut data, 8, BUFFER_FORMAT_VERSION);
            put_u32(&mut data, 12, capacity as u32);
            put_u32(&mut data, 16, next as u32);
        } else if &data[..8] == BUFFER_GROW_MAGIC
            && u32_at(&data, 8) == BUFFER_FORMAT_VERSION
            && u32_at(&data, 12) as usize == capacity
            && u32_at(&data, 16) as usize == current
            && data[20..BUFFER_HEADER_BYTES].iter().all(|byte| *byte == 0)
        {
            put_u32(&mut data, 16, next as u32);
        } else {
            return Err(refuse(REFUSE_BUFFER_GROWTH));
        }
    }
    buffer.realloc(next, true)
}

/// Extend an initialized SIM2 input buffer without replacing its session.
/// The capacity and account length change atomically, and one instruction may
/// add at most the runtime's 10,240-byte account-growth limit.
#[cfg(feature = "capacity-extension")]
#[cold]
#[inline(never)]
fn process_extend_capacity(
    program: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    #[allow(deprecated)]
    use solana_program::system_instruction;
    use solana_program::{program::invoke, rent::Rent, sysvar::Sysvar};

    let [meta, buffer, payer, authority, system] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if meta.owner != program
        || meta.data_len() != META_BYTES
        || !meta.is_writable
        || meta.is_signer
        || buffer.owner != program
        || buffer.data_len() < BUFFER_HEADER_BYTES
        || !buffer.is_writable
        || !buffer.is_signer
        || !payer.is_writable
        || !payer.is_signer
        || !authority.is_signer
        || authority.is_writable
        || payer.key == buffer.key
        || *system.key != solana_program::system_program::ID
        || meta.key == buffer.key
        || !distinct(accounts)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let (new_capacity, expected_current_len) = decode_extend_capacity(data)?;
    let current_len = buffer.data_len();
    if current_len != expected_current_len as usize {
        return Err(refuse(REFUSE_CAPACITY_GROWTH));
    }

    let (context, old_capacity, first_tic, bound_authority, cursor) = {
        let meta_data = meta.try_borrow_data()?;
        if &meta_data[..8] != META_MAGIC
            || u32_at(&meta_data, 8) != VERSION
            || u32_at(&meta_data, 12) as usize != META_BYTES
            || u32_at(&meta_data, 16) as usize != META_BYTES
            || meta_data[22..24] != [0, 0]
            || !exact_zero(&meta_data[M_RESERVED..])
        {
            return Err(refuse(REFUSE_BINDING));
        }
        let context =
            Pubkey::new_from_array(meta_data[M_CONTEXT..M_CONTEXT + 32].try_into().unwrap());
        check_meta_address(program, &context, meta.key, meta_data[M_BUMP])?;
        if meta_data[M_BUFFER..M_BUFFER + 32] != buffer.key.as_ref()[..] {
            return Err(refuse(REFUSE_BINDING));
        }
        (
            context,
            u32_at(&meta_data, M_CAPACITY) as usize,
            u32_at(&meta_data, M_FIRST_TIC),
            Pubkey::new_from_array(meta_data[M_AUTHORITY..M_AUTHORITY + 32].try_into().unwrap()),
            u32_at(&meta_data, M_CURSOR),
        )
    };
    let Some(new_len) = capacity_extension_bytes(old_capacity, new_capacity as usize) else {
        return Err(refuse(REFUSE_CAPACITY_GROWTH));
    };
    if current_len != buffer_bytes(old_capacity).ok_or_else(|| refuse(REFUSE_CAPACITY_GROWTH))?
        || first_tic
            .checked_add(new_capacity)
            .is_none_or(|end| end > MAX_GAME_TIC)
        || cursor as usize > old_capacity
        || authority.key != &bound_authority
    {
        return Err(refuse(REFUSE_CAPACITY_GROWTH));
    }
    {
        let buffer_data = buffer.try_borrow_data()?;
        if !buffer_header_ok(&buffer_data)
            || u32_at(&buffer_data, B_CAPACITY) as usize != old_capacity
            || u32_at(&buffer_data, B_FIRST_TIC) != first_tic
            || buffer_data[B_AUTHORITY..B_AUTHORITY + 32] != bound_authority.as_ref()[..]
            || buffer_data[B_CONTEXT..B_CONTEXT + 32] != context.as_ref()[..]
            || buffer_data[B_META..B_META + 32] != meta.key.as_ref()[..]
        {
            return Err(refuse(REFUSE_BINDING));
        }
    }

    let rent = Rent::get()?.minimum_balance(new_len);
    let shortfall = rent.saturating_sub(buffer.lamports());
    if shortfall != 0 {
        invoke(
            &system_instruction::transfer(payer.key, buffer.key, shortfall),
            &[payer.clone(), buffer.clone(), system.clone()],
        )?;
    }
    buffer.realloc(new_len, true)?;
    {
        let mut buffer_data = buffer.try_borrow_mut_data()?;
        put_u32(&mut buffer_data, B_CAPACITY, new_capacity);
        put_u32(&mut buffer_data, 16, new_len as u32);
    }
    let mut meta_data = meta.try_borrow_mut_data()?;
    put_u32(&mut meta_data, M_CAPACITY, new_capacity);
    Ok(())
}

fn process_advance(
    program: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    cursor_bound: bool,
    require_consumed: bool,
) -> ProgramResult {
    let [context, state, mirror, meta, buffer, authority] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if !owned(program, context, resource_sbf::CONTEXT_BYTES, true)
        || !owned(program, state, resource_sbf::STATE_MAX_BYTES, true)
        || !owned(program, mirror, resource_sbf::MIRROR_BYTES, true)
        || !owned(program, meta, META_BYTES, true)
        || buffer.data_len() < BUFFER_HEADER_BYTES
        || !owned(program, buffer, buffer.data_len(), false)
        || !distinct(accounts)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if !authority.is_signer {
        return Err(refuse(REFUSE_AUTHORITY));
    }
    let (expected_cursor, requested_max_tics) = match (cursor_bound, require_consumed) {
        (true, true) => {
            let (expected, max_tics) = decode_advance_starved(data)?;
            (Some(expected), max_tics)
        }
        (true, false) => {
            let (expected, max_tics) = decode_advance_expected(data)?;
            (Some(expected), max_tics)
        }
        (false, _) => (None, decode_advance(data)?),
    };
    let max_tics = effective_max_tics(requested_max_tics);
    let checkpoint = read_checkpoint(context)?;
    let mut meta_data = meta.try_borrow_mut_data()?;
    check_meta_address(program, context.key, meta.key, meta_data[M_BUMP])?;
    if expected_cursor.is_some_and(|expected| u32_at(&meta_data, M_CURSOR) != expected) {
        // This check precedes every mutation.  A future packet that reaches
        // the account before its predecessor is an atomic, fee-only refusal;
        // the sender can re-sign that logical cursor with a fresh blockhash
        // once readback reaches it.
        return Err(refuse(REFUSE_CURSOR_MISMATCH));
    }
    crate::diagnostic_phase(921);
    let buffer_data = buffer.try_borrow_data()?;
    let mut state_data = state.try_borrow_mut_data()?;
    let mut mirror_data = mirror.try_borrow_mut_data()?;
    let mut context_data = context.try_borrow_mut_data()?;
    let mut engine = ResidentEngine {
        context: context_data.as_mut_ptr(),
    };
    let report = sweep(
        &mut meta_data,
        &buffer_data,
        &mut state_data,
        &mut mirror_data,
        &SweepKeys {
            context: context.key,
            state: state.key,
            mirror: mirror.key,
            buffer: buffer.key,
            meta: meta.key,
            signer: authority.key,
        },
        &checkpoint,
        max_tics,
        require_consumed,
        &mut engine,
        &ComputeMeter,
    )?;
    solana_program::log::sol_log_data(&[RECEIPT_TAG, &report.encode()]);
    Ok(())
}

/// RESET/9.  Six of the seven accounts are exactly the ones ADVANCE uses, so
/// the account-count constant and the C ABI do not change; the seventh is the
/// start image, needed because a staged account is sealed once and can never
/// be re-staged (DOOM-STG/1 refuses a sealed target).  `payer` is the fee payer
/// and the authority signer; the reset moves no lamports and does not realloc,
/// which is why it needs no system program.
///
/// The start account is not bound by address: any ST1 whose recomputed state
/// root equals the root this meta was initialised with is this slot's start
/// state, and any other image is refused with 280.
fn process_reset(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [context, state, mirror, meta, buffer, start, payer] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if !owned(program, context, resource_sbf::CONTEXT_BYTES, true)
        || !owned(program, state, resource_sbf::STATE_MAX_BYTES, true)
        || !owned(program, mirror, resource_sbf::MIRROR_BYTES, true)
        || !owned(program, meta, META_BYTES, true)
        || buffer.data_len() < BUFFER_HEADER_BYTES
        || !owned(program, buffer, buffer.data_len(), true)
        || !owned(program, start, resource_sbf::STATE_MAX_BYTES, false)
        || !payer.is_signer
        || !payer.is_writable
        || !distinct(accounts)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let (selector, force) = decode_reset(data)?;
    let checkpoint = read_checkpoint(context)?;
    let mut meta_data = meta.try_borrow_mut_data()?;
    check_meta_address(program, context.key, meta.key, meta_data[M_BUMP])?;
    let mut buffer_data = buffer.try_borrow_mut_data()?;
    let mut state_data = state.try_borrow_mut_data()?;
    let start_data = start.try_borrow_data()?;
    let mut mirror_data = mirror.try_borrow_mut_data()?;
    let mut context_data = context.try_borrow_mut_data()?;
    let mut engine = ResidentEngine {
        context: context_data.as_mut_ptr(),
    };
    // Selector 0 reads the start image from the state account itself, which is
    // where it is before the first advance; selector 1 reads the supplied
    // account and installs it.
    let start_slice: &[u8] = if selector == RESET_START_STATE {
        &start_data
    } else {
        &state_data
    };
    let root = reset(
        &mut meta_data,
        &mut buffer_data,
        &mut state_data,
        start_slice,
        &mut mirror_data,
        &SweepKeys {
            context: context.key,
            state: state.key,
            mirror: mirror.key,
            buffer: buffer.key,
            meta: meta.key,
            signer: payer.key,
        },
        &checkpoint,
        force,
        &mut engine,
    )?;
    let resets = u32_at(&meta_data, M_RESETS);
    let mut receipt = [0u8; 44];
    receipt[..8].copy_from_slice(RESET_RECEIPT_TAG);
    receipt[8..40].copy_from_slice(&root);
    receipt[40..44].copy_from_slice(&resets.to_le_bytes());
    solana_program::log::sol_log_data(&[RESET_RECEIPT_TAG, &receipt]);
    Ok(())
}

fn process_snapshot(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [meta, state, mirror] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if !owned(program, meta, META_BYTES, false)
        || !owned(program, state, resource_sbf::STATE_MAX_BYTES, false)
        || !owned(program, mirror, resource_sbf::MIRROR_BYTES, false)
        || !distinct(accounts)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let (cursor, root) = decode_snapshot(data)?;
    let meta_data = meta.try_borrow_data()?;
    let context = Pubkey::new_from_array(meta_data[M_CONTEXT..M_CONTEXT + 32].try_into().unwrap());
    check_meta_address(program, &context, meta.key, meta_data[M_BUMP])?;
    snapshot_check(
        &meta_data,
        &state.try_borrow_data()?,
        &mirror.try_borrow_data()?,
        state.key,
        mirror.key,
        cursor,
        &root,
    )
}

pub fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    crate::diagnostic_phase(920);
    if data.len() < REQUEST_HEADER_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, 8) != VERSION
        || data[13..16] != [0, 0, 0]
    {
        return Err(refuse(REFUSE_MALFORMED));
    }
    match data[12] {
        INIT => process_init(program, accounts, data),
        DEPOSIT => process_deposit(program, accounts, data),
        ADVANCE => process_advance(program, accounts, data, false, false),
        ADVANCE_EXPECTED_CURSOR => process_advance(program, accounts, data, true, false),
        ADVANCE_STARVED => process_advance(program, accounts, data, true, true),
        RESET => process_reset(program, accounts, data),
        SNAPSHOT => process_snapshot(program, accounts, data),
        #[cfg(not(feature = "legacy-5120-buffer"))]
        GROW_BUFFER => process_grow_buffer(program, accounts, data),
        #[cfg(all(feature = "capacity-extension", not(feature = "legacy-5120-buffer")))]
        EXTEND_CAPACITY => process_extend_capacity(program, accounts, data),
        _ => Err(refuse(REFUSE_MALFORMED)),
    }
}

/// Reads the fields a sender or viewer needs from a meta image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetaView {
    pub first_tic: u32,
    pub cursor: u32,
    pub capacity: u32,
    pub halt: u8,
    pub state_len: u32,
    pub sweeps: u32,
    pub last_sweep_tics: u32,
    pub state_root: [u8; 32],
    pub input_chain: [u8; 32],
    /// Root of the start state this meta was initialised with: the image
    /// RESET/9 requires.
    pub start_root: [u8; 32],
    /// Successful RESET/9 count.
    pub resets: u32,
}

impl MetaView {
    pub fn parse(meta: &[u8]) -> Option<Self> {
        if meta.len() != META_BYTES || &meta[..8] != META_MAGIC || u32_at(meta, 8) != VERSION {
            return None;
        }
        Some(Self {
            first_tic: u32_at(meta, M_FIRST_TIC),
            cursor: u32_at(meta, M_CURSOR),
            capacity: u32_at(meta, M_CAPACITY),
            halt: meta[M_HALT],
            state_len: u32_at(meta, M_STATE_LEN),
            sweeps: u32_at(meta, M_SWEEPS),
            last_sweep_tics: u32_at(meta, M_LAST_SWEEP_TICS),
            state_root: meta[M_STATE_ROOT..M_STATE_ROOT + 32].try_into().unwrap(),
            input_chain: meta[M_INPUT_CHAIN..M_INPUT_CHAIN + 32].try_into().unwrap(),
            start_root: meta[M_START_ROOT..M_START_ROOT + 32].try_into().unwrap(),
            resets: u32_at(meta, M_RESETS),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let init = InitRequest {
            bump: 254,
            capacity: DEFAULT_BUFFER_CAPACITY as u32,
            e2r_keys: [Pubkey::new_from_array([7; 32]); INIT_KEY_COUNT],
            resource_hash: [1; 32],
            controller_hash: [2; 32],
            frame_hash: [3; 32],
        };
        assert_eq!(init.encode().len(), INIT_BYTES);
        assert_eq!(InitRequest::decode(&init.encode()), Ok(init));
        let commands = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let deposit_data = encode_deposit(9, &commands);
        assert_eq!(decode_deposit(&deposit_data), Ok((9, &commands[..])));
        let grow_data = encode_grow_buffer(DEFAULT_BUFFER_CAPACITY as u32, 10_240);
        assert_eq!(
            decode_grow_buffer(&grow_data),
            Ok((DEFAULT_BUFFER_CAPACITY as u32, 10_240))
        );
        let extension = encode_extend_capacity(2_500, 10_288);
        assert_eq!(extension[12], EXTEND_CAPACITY);
        assert_eq!(decode_extend_capacity(&extension), Ok((2_500, 10_288)));
        let mut wrong_extension_op = extension.clone();
        wrong_extension_op[12] = SNAPSHOT;
        assert_eq!(
            decode_extend_capacity(&wrong_extension_op),
            Err(refuse(REFUSE_MALFORMED))
        );
        let mut trailing_extension_byte = extension.clone();
        trailing_extension_byte.push(0);
        assert_eq!(
            decode_extend_capacity(&trailing_extension_byte),
            Err(refuse(REFUSE_MALFORMED))
        );
        assert_eq!(decode_advance(&encode_advance(17)), Ok(17));
        let expected_advance = encode_advance_expected(23, 7);
        assert_eq!(expected_advance[12], ADVANCE_EXPECTED_CURSOR);
        assert_eq!(decode_advance_expected(&expected_advance), Ok((23, 7)));
        assert_eq!(
            decode_snapshot(&encode_snapshot(5, &[9; 32])),
            Ok((5, [9; 32]))
        );
    }

    // ---- ADVANCE/5 golden vectors and the unconsumed-tic control -------

    const GOLDEN_COMMANDS: [[u8; 4]; 6] = [
        [0x10, 0x00, 0x00, 0x00],
        [0x00, 0x20, 0x00, 0x00],
        [0x00, 0x00, 0x04, 0x01],
        [0x00, 0x00, 0xfc, 0x00],
        [0x08, 0x00, 0x00, 0x00],
        [0x00, 0x00, 0x00, 0x00],
    ];

    struct Golden {
        meta: Vec<u8>,
        buffer: Vec<u8>,
        state: Vec<u8>,
        mirror: Vec<u8>,
        checkpoint: Vec<u8>,
        keys: SweepKeys<'static>,
    }

    struct MockEngine {
        /// Steps taken so far, `Ran` and `Unconsumed` alike.
        steps: u32,
        /// Steps the engine reported as `Ran`.  Only these advance the ST1
        /// gametic, exactly as the real engine does: `doom_sbf_sim2_step`
        /// returns 238 *before* `gametic++` (doom_sbf_resource_ri2.c), so a
        /// tic that did not consume its command leaves the tic counter alone.
        advanced: u32,
        /// `Some(seq)`: from that step index on, every step reports
        /// `Unconsumed` (the DOOM-CI3 starved tic).  `None` runs every tic.
        starve_from: Option<u32>,
        published: u32,
    }

    impl MockEngine {
        fn new(starve_from: Option<u32>) -> Self {
            Self { steps: 0, advanced: 0, starve_from, published: 0 }
        }
    }

    impl Engine for MockEngine {
        fn step(&mut self, _command: &[u8; 4]) -> Result<StepResult, u32> {
            let seq = self.steps;
            self.steps += 1;
            if self.starve_from.is_some_and(|from| seq >= from) {
                return Ok(StepResult::Unconsumed);
            }
            self.advanced += 1;
            Ok(StepResult::Ran)
        }
        fn publish(&mut self, state: &mut [u8], _mirror: &mut [u8]) -> Result<usize, u32> {
            self.published += 1;
            // A publish must leave a shape-valid ST1 whose gametic advanced by
            // exactly the tics the engine actually ran.
            let len = u32_at(state, 16) as usize;
            put_u32(state, 20, u32_at(state, 20) + self.advanced);
            Ok(len)
        }
    }

    struct NoBudget;
    impl Budget for NoBudget {
        fn remaining(&self) -> u64 {
            u64::MAX
        }
    }

    fn golden() -> Golden {
        let context = Pubkey::new_from_array([0xc0; 32]);
        let state_key = Pubkey::new_from_array([0x51; 32]);
        let mirror_key = Pubkey::new_from_array([0x52; 32]);
        let buffer_key = Pubkey::new_from_array([0x53; 32]);
        let meta_key = Pubkey::new_from_array([0x54; 32]);
        let signer = Pubkey::new_from_array([0xa1; 32]);
        let capacity = 64usize;

        let mut state = vec![0u8; resource_sbf::STATE_MAX_BYTES];
        state[..8].copy_from_slice(b"DOOM-ST1");
        put_u32(&mut state, 8, 0);
        put_u32(&mut state, 12, 64);
        put_u32(&mut state, 16, 64);
        put_u32(&mut state, 20, 0);

        let mut mirror = vec![0u8; resource_sbf::MIRROR_BYTES];
        mirror[..8].copy_from_slice(b"DOOM-MP1");
        put_u32(&mut mirror, 8, 1);
        put_u32(&mut mirror, 12, MIRROR_HEADER_BYTES as u32);
        put_u32(&mut mirror, 16, resource_sbf::MIRROR_BYTES as u32);
        put_u32(&mut mirror, 20, MIRROR_SLOT_BYTES as u32);
        put_u32(&mut mirror, 24, MIRROR_SLOTS as u32);
        mirror[36..68].copy_from_slice(&WAD_HASH);
        put_u32(&mut mirror, 28, 0);
        put_u32(&mut mirror, 32, 0);

        let mut buffer = vec![0u8; buffer_bytes(capacity).unwrap()];
        buffer[..8].copy_from_slice(BUFFER_MAGIC);
        put_u32(&mut buffer, 8, BUFFER_FORMAT_VERSION);
        put_u32(&mut buffer, 12, BUFFER_HEADER_BYTES as u32);
        let buffer_len = buffer.len() as u32;
        put_u32(&mut buffer, 16, buffer_len);
        put_u32(&mut buffer, B_CAPACITY, capacity as u32);
        put_u32(&mut buffer, B_FIRST_TIC, 0);
        put_u32(&mut buffer, B_SLOT_BYTES, SLOT_BYTES as u32);
        buffer[B_AUTHORITY..B_AUTHORITY + 32].copy_from_slice(signer.as_ref());
        buffer[B_CONTEXT..B_CONTEXT + 32].copy_from_slice(context.as_ref());
        buffer[B_META..B_META + 32].copy_from_slice(meta_key.as_ref());
        deposit(&mut buffer, &signer, 0, &GOLDEN_COMMANDS[..4].concat()).unwrap();

        let (state_len, root) = verified_state_root(&state, &mirror).unwrap();
        let mut checkpoint = vec![0u8; CHECKPOINT_BYTES];
        checkpoint[..8].copy_from_slice(continuation_split_sbf::CHECKPOINT_MAGIC);
        put_u32(&mut checkpoint, 8, continuation_split_sbf::CHECKPOINT_VERSION);
        put_u32(&mut checkpoint, 12, CHECKPOINT_PHASE_PUBLISHED);
        put_u32(&mut checkpoint, 16, CHECKPOINT_CURSOR_PUBLISHED);
        put_u32(&mut checkpoint, 84, 0);
        put_u32(&mut checkpoint, 88, CHECKPOINT_OFFSET as u32);

        let mut meta = vec![0u8; META_BYTES];
        meta[..8].copy_from_slice(META_MAGIC);
        put_u32(&mut meta, 8, VERSION);
        put_u32(&mut meta, 12, META_BYTES as u32);
        put_u32(&mut meta, 16, META_BYTES as u32);
        put_u32(&mut meta, M_FIRST_TIC, 0);
        put_u32(&mut meta, M_CURSOR, 0);
        put_u32(&mut meta, M_STATE_LEN, state_len as u32);
        put_u32(&mut meta, M_CAPACITY, capacity as u32);
        meta[M_CONTEXT..M_CONTEXT + 32].copy_from_slice(context.as_ref());
        meta[M_STATE..M_STATE + 32].copy_from_slice(state_key.as_ref());
        meta[M_MIRROR..M_MIRROR + 32].copy_from_slice(mirror_key.as_ref());
        meta[M_BUFFER..M_BUFFER + 32].copy_from_slice(buffer_key.as_ref());
        meta[M_AUTHORITY..M_AUTHORITY + 32].copy_from_slice(signer.as_ref());
        meta[M_STATE_ROOT..M_STATE_ROOT + 32].copy_from_slice(&root);
        meta[M_INPUT_CHAIN..M_INPUT_CHAIN + 32]
            .copy_from_slice(&chain_init(&context, 0));
        meta[M_CHECKPOINT..M_CHECKPOINT + 32]
            .copy_from_slice(&checkpoint_hash(&context, &checkpoint));

        Golden {
            meta,
            buffer,
            state,
            mirror,
            checkpoint,
            keys: SweepKeys {
                context: Box::leak(Box::new(context)),
                state: Box::leak(Box::new(state_key)),
                mirror: Box::leak(Box::new(mirror_key)),
                buffer: Box::leak(Box::new(buffer_key)),
                meta: Box::leak(Box::new(meta_key)),
                signer: Box::leak(Box::new(signer)),
            },
        }
    }

    /// The input chain is a pure function of the committed chain, the slot
    /// index and the staged bytes, so it is a golden vector that survives any
    /// engine change: ADVANCE/5 must commit exactly this after four tics.
    /// Pinned as literal hex, not recomputed from `chain_step`, so a change to
    /// the domain, the context binding, or the step encoding fails here instead
    /// of quietly redefining what "the same chain" means.
    const GOLDEN_CHAIN_AFTER_FOUR: [u8; 32] = [
        0x1d, 0xed, 0x37, 0xcb, 0x9b, 0x5a, 0xda, 0x0f, 0x2c, 0x1a, 0x70, 0xb2, 0x27, 0x6e, 0x5d, 0x10,
        0xa6, 0x0f, 0xa1, 0x4d, 0x32, 0x40, 0x16, 0xd9, 0x5f, 0x6d, 0x43, 0xbb, 0xfc, 0x09, 0x35, 0x75,
    ];

    /// The pinned vector has to be the one the function actually produces, and
    /// this is the only place the two are tied together.
    #[test]
    fn golden_chain_vector_is_the_committed_function() {
        let mut chain = chain_init(&Pubkey::new_from_array([0xc0; 32]), 0);
        for (seq, command) in GOLDEN_COMMANDS[..4].iter().enumerate() {
            chain = chain_step(&chain, seq as u32, command);
        }
        assert_eq!(chain, GOLDEN_CHAIN_AFTER_FOUR);
    }

    #[test]
    fn advance5_commits_the_golden_chain_and_cursor() {
        let mut g = golden();
        let mut engine = MockEngine::new(None);
        let report = sweep(
            &mut g.meta,
            &g.buffer,
            &mut g.state,
            &mut g.mirror,
            &g.keys,
            &g.checkpoint,
            8,
            true,
            &mut engine,
            &NoBudget,
        )
        .unwrap();
        assert_eq!(engine.steps, 4);
        assert_eq!(engine.advanced, 4);
        assert_eq!(engine.published, 1);
        assert_eq!(report.ran, 4);
        assert_eq!(report.stop, Stop::Gap);
        assert_eq!(report.cursor_after, 4);
        assert_eq!(report.input_chain, GOLDEN_CHAIN_AFTER_FOUR);
        assert_eq!(u32_at(&g.meta, M_CURSOR), 4);
        assert_eq!(u32_at(&g.meta, M_LAST_SWEEP_TICS), 4);
    }

    /// The malformed-input control: a sweep whose staged command the engine
    /// never consumed is refused, and nothing is committed.  `starve_from` is 0
    /// (the very first staged tic starves) and 3 (three tics run, the fourth
    /// starves).  The second shape is the one that matters: by then the engine
    /// has already run three tics, so if the refusal ever committed a byte of
    /// ST1/MP1 it would show -- and the check is on the bytes, not on the meta.
    #[test]
    fn advance5_refuses_a_tic_with_no_consumed_command() {
        for (starve_from, steps) in [(Some(0), 1), (Some(3), 4)] {
            let mut g = golden();
            let meta_before = g.meta.clone();
            let state_before = g.state.clone();
            let mirror_before = g.mirror.clone();
            let mut engine = MockEngine::new(starve_from);
            let result = sweep(
                &mut g.meta,
                &g.buffer,
                &mut g.state,
                &mut g.mirror,
                &g.keys,
                &g.checkpoint,
                8,
                true,
                &mut engine,
                &NoBudget,
            );
            assert_eq!(
                result,
                Err(refuse(REFUSE_UNCONSUMED_INPUT)),
                "starve_from={starve_from:?}"
            );
            assert_eq!(engine.steps, steps, "starve_from={starve_from:?}");
            assert_eq!(engine.advanced, steps - 1, "starve_from={starve_from:?}");
            assert_eq!(engine.published, 0, "starve_from={starve_from:?}");
            assert_eq!(g.meta, meta_before, "starve_from={starve_from:?}");
            // A refused sweep commits nothing: not the cursor, not the chain,
            // and not one byte of the engine state or of its mirror.
            assert_eq!(g.state, state_before, "starve_from={starve_from:?}");
            assert_eq!(g.mirror, mirror_before, "starve_from={starve_from:?}");
        }
    }

    /// ADVANCE/3 and ADVANCE/4 do NOT keep a permissive meaning on the real
    /// engine.  DOOM-CI3 lives in `doom_sbf_sim2_step`, not in the ADVANCE
    /// opcode: an unconsumed tic returns 238 *before* `gametic++`
    /// (doom_sbf_resource_ri2.c), so the engine's tic counter does not move
    /// even though the sweep loop would have counted the step.  The
    /// post-publish tic check then sees a cursor that has run ahead of gametic
    /// and refuses 284.  A starved tic is uncommittable under every opcode;
    /// ADVANCE/5 only names the refusal earlier, with 288 instead of 284.
    ///
    /// The 284 refusal lands *after* the publish, so in this harness the ST1
    /// bytes are already changed.  That is exactly why the refusal has to be a
    /// whole-instruction ProgramError: on chain Solana rolls those writes back
    /// with the rest of the instruction, and a unit test has no such rollback.
    /// What is assertable here is that nothing was *committed* -- meta cursor,
    /// chain and state root are untouched -- and that the published gametic is
    /// the one that proves the mismatch.
    #[test]
    fn advance3_also_refuses_a_tic_with_no_consumed_command() {
        for (starve_from, ran_tics) in [(Some(0), 0), (Some(3), 3)] {
            let mut g = golden();
            let meta_before = g.meta.clone();
            let state_before = g.state.clone();
            let mut engine = MockEngine::new(starve_from);
            let result = sweep(
                &mut g.meta,
                &g.buffer,
                &mut g.state,
                &mut g.mirror,
                &g.keys,
                &g.checkpoint,
                8,
                false,
                &mut engine,
                &NoBudget,
            );
            assert_eq!(result, Err(refuse(REFUSE_TIC)), "starve_from={starve_from:?}");
            assert_eq!(engine.published, 1, "starve_from={starve_from:?}");
            assert_eq!(engine.steps, 4, "starve_from={starve_from:?}");
            assert_eq!(engine.advanced, ran_tics, "starve_from={starve_from:?}");
            // Nothing was committed: no cursor, no chain, no state root.
            assert_eq!(g.meta, meta_before, "starve_from={starve_from:?}");
            assert_eq!(u32_at(&g.meta, M_CURSOR), 0, "starve_from={starve_from:?}");
            // The engine published a gametic that is short of the cursor the
            // sweep counted, which is the whole cause of the 284.  When the
            // very first tic starved there was no tic to advance, so the
            // published image is the pre-sweep one.
            assert_eq!(u32_at(&g.state, 20), ran_tics, "starve_from={starve_from:?}");
            if ran_tics > 0 {
                assert_ne!(g.state, state_before, "starve_from={starve_from:?}");
            }
        }
    }

    /// The engine-driven control for the tic check itself: with every staged
    /// tic consumed, /3 and /5 agree, which is the only shape in which the
    /// permissive branch is still reachable.
    #[test]
    fn advance3_and_advance5_agree_when_every_tic_is_consumed() {
        let mut five = golden();
        let mut three = golden();
        let mut engine_five = MockEngine::new(None);
        let mut engine_three = MockEngine::new(None);
        let report_five = sweep(
            &mut five.meta,
            &five.buffer,
            &mut five.state,
            &mut five.mirror,
            &five.keys,
            &five.checkpoint,
            8,
            true,
            &mut engine_five,
            &NoBudget,
        )
        .unwrap();
        let report_three = sweep(
            &mut three.meta,
            &three.buffer,
            &mut three.state,
            &mut three.mirror,
            &three.keys,
            &three.checkpoint,
            8,
            false,
            &mut engine_three,
            &NoBudget,
        )
        .unwrap();
        assert_eq!(report_five.input_chain, report_three.input_chain);
        assert_eq!(report_five.cursor_after, report_three.cursor_after);
        assert_eq!(report_five.input_chain, GOLDEN_CHAIN_AFTER_FOUR);
        assert_eq!(five.meta, three.meta);
    }

    #[test]
    fn advance5_wire_is_separate_from_advance4() {
        let starved = encode_advance_starved(23, 7);
        assert_eq!(starved[12], ADVANCE_STARVED);
        assert_eq!(decode_advance_starved(&starved), Ok((23, 7)));
        // ADVANCE/4 must not accept an ADVANCE/5 packet and the reverse.
        assert_eq!(
            decode_advance_expected(&starved),
            Err(refuse(REFUSE_MALFORMED))
        );
        assert_eq!(
            decode_advance_starved(&encode_advance_expected(23, 7)),
            Err(refuse(REFUSE_MALFORMED))
        );
        let mut short = starved.clone();
        short.pop();
        assert_eq!(decode_advance_starved(&short), Err(refuse(REFUSE_MALFORMED)));
    }

    #[test]
    #[cfg(not(feature = "legacy-5120-buffer"))]
    fn capacity_sizes_and_incremental_growth_are_bounded() {
        assert_eq!(DEFAULT_BUFFER_CAPACITY, 65_536);
        assert_eq!(buffer_bytes(65_536), Some(524_416));
        assert_eq!(buffer_bytes(5_120), Some(41_088));
        assert_eq!(initial_buffer_bytes(65_536), Some(10_240));
        assert_eq!(buffer_bytes(MAX_BUFFER_CAPACITY + 1), None);
        assert_eq!(capacity_extension_bytes(1_270, 2_500), Some(20_128));
        assert_eq!(capacity_extension_bytes(1_270, 2_550), Some(20_528));
        assert_eq!(capacity_extension_bytes(1_270, 2_551), None);
        assert_eq!(capacity_extension_bytes(1_270, 1_270), None);
        assert_eq!(capacity_extension_bytes(2_500, 1_270), None);
        let mut current = initial_buffer_bytes(DEFAULT_BUFFER_CAPACITY).unwrap();
        let mut steps = 0;
        while current < BUFFER_BYTES {
            current = next_buffer_len(current, DEFAULT_BUFFER_CAPACITY).unwrap();
            steps += 1;
        }
        assert_eq!(current, BUFFER_BYTES);
        assert_eq!(steps, 51);
        assert_eq!(next_buffer_len(current, DEFAULT_BUFFER_CAPACITY), None);
        assert_eq!(next_buffer_len(0, DEFAULT_BUFFER_CAPACITY), None);
    }

    #[test]
    #[cfg(feature = "legacy-5120-buffer")]
    fn legacy_5120_buffer_layout_remains_versioned() {
        assert_eq!(BUFFER_FORMAT_VERSION, 2);
        assert_eq!(BUFFER_CAPACITY, 5_120);
        assert_eq!(BUFFER_BYTES, 41_088);
        assert_eq!(INIT_BYTES, 404);
        assert_eq!(ABI_DESCRIPTOR.contains("SIM2INPT/2:41088:5120x8"), true);
    }

    #[test]
    fn malformed_requests_refuse() {
        let mut advance = encode_advance(0);
        advance[20] = 1;
        assert_eq!(decode_advance(&advance), Err(refuse(REFUSE_MALFORMED)));
        let mut deposit_data = encode_deposit(0, &[0; 4]);
        deposit_data[20] = 2;
        assert_eq!(decode_deposit(&deposit_data), Err(refuse(REFUSE_MALFORMED)));
        assert_eq!(
            decode_deposit(&encode_deposit(0, &[0; 4 * (MAX_DEPOSIT_SLOTS + 1)])),
            Err(refuse(REFUSE_MALFORMED))
        );
        let mut extend = encode_extend_capacity(2_500, 10_288);
        extend[12] = GROW_BUFFER;
        assert_eq!(
            decode_extend_capacity(&extend),
            Err(refuse(REFUSE_MALFORMED))
        );
    }

    #[test]
    fn sweep_tic_cap_defaults_and_bounds() {
        let default = if cfg!(feature = "metered-sweep") {
            MAX_SWEEP_TICS
        } else {
            DEFAULT_SWEEP_TICS
        };
        assert_eq!(effective_max_tics(0), default);
        assert_eq!(effective_max_tics(3), 3);
        assert_eq!(effective_max_tics(u32::MAX), MAX_SWEEP_TICS);
    }

    #[test]
    fn meta_address_requires_the_canonical_bump() {
        let program = Pubkey::new_from_array([0x11; 32]);
        let context = Pubkey::new_from_array([0x22; 32]);
        let (canonical, bump) = meta_address(&program, &context);
        assert_eq!(
            check_meta_address(&program, &context, &canonical, bump),
            Ok(())
        );
        let (alternate, alternate_bump) = (0..bump)
            .find_map(|candidate| {
                Pubkey::create_program_address(
                    &[META_SEED, context.as_ref(), &[candidate]],
                    &program,
                )
                .ok()
                .map(|key| (key, candidate))
            })
            .expect("test PDA has an alternate valid bump");
        assert_ne!(alternate, canonical);
        assert_eq!(
            check_meta_address(&program, &context, &alternate, alternate_bump),
            Err(refuse(REFUSE_BINDING))
        );
    }

    // ---- RESET/9 ---------------------------------------------------------

    /// Re-seeds a resident engine from the committed start image and derives
    /// the mirror, exactly as `doom_sbf_sim2_reset` does: the state image is
    /// read, never written, and the mirror is written from the decoded engine
    /// rather than taken from the account.  `refuse` is the code a real decode
    /// would return, so the refusal paths are reachable.
    struct MockReset {
        start_mirror: Vec<u8>,
        resets: u32,
        refuse: Option<u32>,
    }

    impl ResetEngine for MockReset {
        fn reset(&mut self, _state: &[u8], mirror: &mut [u8]) -> Result<(), u32> {
            self.resets += 1;
            if let Some(code) = self.refuse {
                return Err(code);
            }
            mirror.copy_from_slice(&self.start_mirror);
            Ok(())
        }
    }

    /// The STG/1 step a caller performs before RESET/9: the start image is
    /// written into the bound state account with a zero tail.
    fn stage_start(g: &mut Golden, start_state: &[u8]) {
        let len = start_state.len();
        g.state[..len].copy_from_slice(start_state);
        g.state[len..].fill(0);
    }

    /// A golden slot that has been swept once: the cursor has moved, the input
    /// chain has moved with it, and the meta records the start root INIT wrote.
    /// The staged start state and mirror are the copies a reset restores.
    fn played(start_root: bool) -> (Golden, Vec<u8>, Vec<u8>) {
        let mut g = golden();
        let (start_state, start_mirror) = (g.state.clone(), g.mirror.clone());
        if start_root {
            let (_, start) = verified_state_root(&start_state, &start_mirror).unwrap();
            g.meta[M_START_ROOT..M_START_ROOT + 32].copy_from_slice(&start);
        }
        let mut engine = MockEngine::new(None);
        sweep(
            &mut g.meta,
            &g.buffer,
            &mut g.state,
            &mut g.mirror,
            &g.keys,
            &g.checkpoint,
            8,
            true,
            &mut engine,
            &NoBudget,
        )
        .unwrap();
        assert_eq!(u32_at(&g.meta, M_CURSOR), 4);
        (g, start_state, start_mirror)
    }

    fn mock_reset(start_mirror: Vec<u8>) -> MockReset {
        MockReset { start_mirror, resets: 0, refuse: None }
    }

    #[test]
    fn reset_wire_is_24_bytes_and_only_names_the_meta_start_state() {
        let data = encode_reset(RESET_START_META, false);
        assert_eq!(data.len(), RESET_BYTES);
        assert_eq!(data[12], RESET);
        assert_eq!(decode_reset(&data), Ok((RESET_START_META, false)));
        let forced = encode_reset(RESET_START_META, true);
        assert_eq!(decode_reset(&forced), Ok((RESET_START_META, true)));
        // A selector this version does not serve, an unknown flag bit, a
        // trailing byte and the wrong operation number are all malformed.
        assert_eq!(
            decode_reset(&encode_reset(1, false)),
            Err(refuse(REFUSE_MALFORMED))
        );
        let mut bad_flag = encode_reset(RESET_START_META, true);
        put_u32(&mut bad_flag, 16, RESET_FORCE | 2);
        assert_eq!(decode_reset(&bad_flag), Err(refuse(REFUSE_MALFORMED)));
        let mut trailing = encode_reset(RESET_START_META, false);
        trailing.push(0);
        assert_eq!(decode_reset(&trailing), Err(refuse(REFUSE_MALFORMED)));
        let mut wrong_op = encode_reset(RESET_START_META, false);
        wrong_op[12] = SNAPSHOT;
        assert_eq!(decode_reset(&wrong_op), Err(refuse(REFUSE_MALFORMED)));
    }

    /// INIT records the start root once, and the meta it writes is still
    /// shape-valid afterwards: the reserved tail starts where the new fields
    /// end, so the binding check's zero scan still covers every byte nothing
    /// claims.
    #[test]
    fn init_records_the_start_root_and_keeps_the_reserved_tail_zero() {
        let context = Pubkey::new_from_array([0xc0; 32]);
        let state = Pubkey::new_from_array([0x51; 32]);
        let mirror = Pubkey::new_from_array([0x52; 32]);
        let buffer = Pubkey::new_from_array([0x53; 32]);
        let meta_key = Pubkey::new_from_array([0x54; 32]);
        let authority = Pubkey::new_from_array([0xa1; 32]);
        let capacity = 64usize;
        let mut buffer_bytes = vec![0u8; buffer_bytes(capacity).unwrap()];
        let mut meta_bytes = vec![0u8; META_BYTES];
        let root = [0x5a; 32];
        init_layout(
            &mut meta_bytes,
            &mut buffer_bytes,
            &InitParams {
                context,
                state,
                mirror,
                buffer,
                meta: meta_key,
                authority,
                bump: 254,
                capacity: capacity as u32,
                first_tic: 0,
                state_len: 64,
                state_root: root,
                checkpoint_hash: [0x11; 32],
            },
        )
        .unwrap();
        assert_eq!(&meta_bytes[M_START_ROOT..M_START_ROOT + 32], &root);
        assert_eq!(u32_at(&meta_bytes, M_RESETS), 0);
        assert!(exact_zero(&meta_bytes[M_RESERVED..]));
        let view = MetaView::parse(&meta_bytes).unwrap();
        assert_eq!(view.start_root, root);
        assert_eq!(view.resets, 0);
        assert_eq!(view.state_root, root);
    }

    /// The whole point: a swept slot, its staged input cleared, its cursor,
    /// chain, halt and sweep counters rewound, and the committed state back to
    /// the start image.  A second reset of the same slot is byte-identical to
    /// the first apart from the reset counter, which is what makes a slot
    /// reusable rather than merely re-runnable.
    #[test]
    fn reset_returns_a_played_slot_to_its_start_state() {
        let (mut g, start_state, start_mirror) = played(true);
        let start_image = start_state.clone();
        stage_start(&mut g, &start_state);
        let mut engine = mock_reset(start_mirror.clone());
        let root = reset(
            &mut g.meta,
            &mut g.buffer,
            &mut g.state,
            &start_image,
            &mut g.mirror,
            &g.keys,
            &g.checkpoint,
            false,
            &mut engine,
        )
        .unwrap();
        assert_eq!(engine.resets, 1);
        assert_eq!(u32_at(&g.meta, M_CURSOR), 0);
        assert_eq!(g.meta[M_HALT], 0);
        assert_eq!(u32_at(&g.meta, M_SWEEPS), 0);
        assert_eq!(u32_at(&g.meta, M_LAST_SWEEP_TICS), 0);
        assert_eq!(u32_at(&g.meta, M_RESETS), 1);
        assert_eq!(&g.meta[M_STATE_ROOT..M_STATE_ROOT + 32], &root);
        assert_eq!(&g.meta[M_START_ROOT..M_START_ROOT + 32], &root);
        assert_eq!(
            &g.meta[M_INPUT_CHAIN..M_INPUT_CHAIN + 32],
            &chain_init(g.keys.context, 0)
        );
        assert_eq!(g.state, start_state, "state is the start image again");
        assert_eq!(g.mirror, start_mirror, "mirror is derived, not supplied");
        // Every staged slot is gone, the header is not.
        assert!(exact_zero(&g.buffer[BUFFER_HEADER_BYTES..]));
        assert_eq!(&g.buffer[..8], BUFFER_MAGIC);
        assert_eq!(u32_at(&g.buffer, B_CAPACITY), 64);
        assert_eq!(&g.buffer[B_AUTHORITY..B_AUTHORITY + 32], &[0xa1; 32]);
        // The rewritten meta is still a valid meta for this slot.
        validate_meta(&g.meta, &g.keys, &g.checkpoint).unwrap();
        // And a second reset lands on exactly the same bytes.
        let after_first = (g.meta.clone(), g.buffer.clone(), g.state.clone(), g.mirror.clone());
        let mut again = mock_reset(start_mirror.clone());
        let second = reset(
            &mut g.meta,
            &mut g.buffer,
            &mut g.state,
            &start_image,
            &mut g.mirror,
            &g.keys,
            &g.checkpoint,
            false,
            &mut again,
        )
        .unwrap();
        assert_eq!(second, root);
        assert_eq!(g.state, after_first.2);
        assert_eq!(g.mirror, after_first.3);
        assert_eq!(g.buffer, after_first.1);
        assert_eq!(u32_at(&g.meta, M_RESETS), 2);
        let mut first = after_first.0;
        put_u32(&mut first, M_RESETS, 2);
        assert_eq!(&g.meta[..M_RESETS], &first[..M_RESETS]);
    }

    /// The reusable-slot case: the start image arrives in its own sealed
    /// account, so the reset installs it into the bound state account with a
    /// zero tail before the decode.  Without this the state account would
    /// still hold the finished session and the root check could never pass.
    #[test]
    fn reset_installs_a_supplied_start_account_into_the_state_account() {
        let (mut g, start_state, start_mirror) = played(true);
        // The state account still holds the finished session here: no STG/1.
        assert_eq!(u32_at(&g.state, 20), 4);
        // A supplied start account is a whole sealed ST1 account: the encoded
        // image and a zero tail out to STATE_MAX_BYTES.
        let mut supplied = vec![0u8; resource_sbf::STATE_MAX_BYTES];
        supplied[..start_state.len()].copy_from_slice(&start_state);
        let start_image = supplied.clone();
        let mut engine = mock_reset(start_mirror);
        let root = reset(
            &mut g.meta,
            &mut g.buffer,
            &mut g.state,
            &start_image,
            &mut g.mirror,
            &g.keys,
            &g.checkpoint,
            false,
            &mut engine,
        )
        .unwrap();
        assert_eq!(g.state, supplied, "the state account is the start image");
        assert_eq!(u32_at(&g.state, 20), 0, "the start state's own tic");
        assert_eq!(&g.meta[M_STATE_ROOT..M_STATE_ROOT + 32], &root);
        assert_eq!(u32_at(&g.meta, M_CURSOR), 0);
        assert_eq!(u32_at(&g.meta, M_RESETS), 1);
        validate_meta(&g.meta, &g.keys, &g.checkpoint).unwrap();
    }

    /// A reset lands on *the* provisioned start state, not on one supplied at
    /// reset time.  The state is shape-valid and at the right tic; only the
    /// root differs, and the refusal commits nothing: the cursor, the chain and
    /// the staged slots are exactly as they were.
    #[test]
    fn reset_refuses_a_state_that_is_not_the_committed_start_state() {
        let (mut g, start_state, start_mirror) = played(true);
        let meta_before = g.meta.clone();
        let buffer_before = g.buffer.clone();
        let mut wrong = start_state.clone();
        // A canonical ST1 at the same tic with one different committed header
        // field: still shape-valid (the encoded length and the zero tail are
        // untouched), so only the root check can catch it.
        wrong[24..28].copy_from_slice(&29u32.to_le_bytes());
        stage_start(&mut g, &wrong);
        let start_image = wrong.clone();
        let mut engine = mock_reset(start_mirror);
        assert_eq!(
            reset(
                &mut g.meta,
                &mut g.buffer,
                &mut g.state,
                &start_image,
                &mut g.mirror,
                &g.keys,
                &g.checkpoint,
                false,
                &mut engine,
            ),
            Err(refuse(REFUSE_STATE_ROOT))
        );
        assert_eq!(engine.resets, 1, "the decode ran and the root did not match");
        assert_eq!(g.meta, meta_before);
        assert_eq!(g.buffer, buffer_before);
    }

    /// A state from another tic is refused before the engine is touched: the
    /// root would have caught it too, but the tic check is the one that says
    /// "this is not a start state", and it costs nothing.
    #[test]
    fn reset_refuses_a_state_from_another_tic_before_the_decode() {
        let (mut g, mut start_state, start_mirror) = played(true);
        put_u32(&mut start_state, 20, 8);
        let start_image = start_state.clone();
        stage_start(&mut g, &start_state);
        let mut engine = mock_reset(start_mirror);
        assert_eq!(
            reset(
                &mut g.meta,
                &mut g.buffer,
                &mut g.state,
                &start_image,
                &mut g.mirror,
                &g.keys,
                &g.checkpoint,
                false,
                &mut engine,
            ),
            Err(refuse(REFUSE_TIC))
        );
        assert_eq!(engine.resets, 0, "no decode ran");
    }

    /// A decode refusal is the decode's own code, and it too commits nothing.
    #[test]
    fn reset_refuses_a_state_the_engine_cannot_decode() {
        let (mut g, start_state, start_mirror) = played(true);
        let start_image = start_state.clone();
        stage_start(&mut g, &start_state);
        let mut engine = mock_reset(start_mirror);
        engine.refuse = Some(232);
        assert_eq!(
            reset(
                &mut g.meta,
                &mut g.buffer,
                &mut g.state,
                &start_image,
                &mut g.mirror,
                &g.keys,
                &g.checkpoint,
                false,
                &mut engine,
            ),
            Err(refuse(232))
        );
        assert_eq!(u32_at(&g.meta, M_CURSOR), 4);
        assert_eq!(u32_at(&g.meta, M_RESETS), 0);
    }

    /// A malformed ST1 account is refused by the shape check (281) before the
    /// engine sees it.
    #[test]
    fn reset_refuses_a_malformed_state_account() {
        let (mut g, start_state, start_mirror) = played(true);
        let start_image = start_state.clone();
        let mut engine = mock_reset(start_mirror);
        let mut state = g.state.clone();
        state[0] = b'X';
        let result = reset(
            &mut g.meta,
            &mut g.buffer,
            &mut state,
            &start_image,
            &mut g.mirror,
            &g.keys,
            &g.checkpoint,
            false,
            &mut engine,
        );
        assert_eq!(result, Err(refuse(REFUSE_STATE_SHAPE)));
        assert_eq!(engine.resets, 0);
    }

    /// A halt is latched and terminal, so a reset ignores one only when the
    /// request says so.  This is the griefing bound: without the authority and
    /// without the flag, a reset cannot erase a session someone is auditing.
    #[test]
    fn reset_refuses_a_halted_slot_unless_the_request_forces_it() {
        for (force, halted, expect) in [
            (false, 1u8, Err(REFUSE_HALTED)),
            (true, 1u8, Ok(())),
            (false, 0u8, Ok(())),
        ] {
            let (mut g, start_state, start_mirror) = played(true);
            let start_image = start_state.clone();
            stage_start(&mut g, &start_state);
            g.meta[M_HALT] = halted;
            let mut engine = mock_reset(start_mirror);
            let result = reset(
                &mut g.meta,
                &mut g.buffer,
                &mut g.state,
                &start_image,
                &mut g.mirror,
                &g.keys,
                &g.checkpoint,
                force,
                &mut engine,
            );
            match expect {
                Ok(()) => {
                    result.unwrap();
                    assert_eq!(g.meta[M_HALT], 0, "force={force} halted={halted}");
                    assert_eq!(u32_at(&g.meta, M_CURSOR), 0);
                }
                Err(code) => {
                    assert_eq!(result, Err(refuse(code)), "force={force}");
                    assert_eq!(engine.resets, 0, "force={force}");
                    assert_eq!(u32_at(&g.meta, M_CURSOR), 4, "force={force}");
                }
            }
        }
    }

    /// Authority, account binding and the published checkpoint, all through the
    /// one `validate_meta` the sweep already uses: RESET/9 grants no more
    /// authority over a slot than ADVANCE does.
    #[test]
    fn reset_checks_authority_and_binding_before_anything_else() {
        let (mut g, start_state, start_mirror) = played(true);
        let start_image = start_state.clone();
        stage_start(&mut g, &start_state);
        let stranger = Pubkey::new_from_array([0xa2; 32]);
        let other = SweepKeys {
            context: g.keys.context,
            state: g.keys.state,
            mirror: g.keys.mirror,
            buffer: g.keys.buffer,
            meta: g.keys.meta,
            signer: Box::leak(Box::new(stranger)),
        };
        let mut engine = mock_reset(start_mirror.clone());
        assert_eq!(
            reset(
                &mut g.meta,
                &mut g.buffer,
                &mut g.state,
                &start_image,
                &mut g.mirror,
                &other,
                &g.checkpoint,
                false,
                &mut engine,
            ),
            Err(refuse(REFUSE_AUTHORITY))
        );
        assert_eq!(engine.resets, 0);
        // A buffer that is not this meta's bound input buffer.
        let mut other_buffer = g.buffer.clone();
        other_buffer[B_META..B_META + 32].copy_from_slice(&[0x77; 32]);
        let mut engine = mock_reset(start_mirror.clone());
        assert_eq!(
            reset(
                &mut g.meta,
                &mut other_buffer,
                &mut g.state,
                &start_image,
                &mut g.mirror,
                &g.keys,
                &g.checkpoint,
                false,
                &mut engine,
            ),
            Err(refuse(REFUSE_BINDING))
        );
        assert_eq!(engine.resets, 0);
        // A context whose E2R/5 publication is no longer the one the meta
        // committed.
        let mut checkpoint = g.checkpoint.clone();
        checkpoint[12..16].copy_from_slice(&7u32.to_le_bytes());
        let mut engine = mock_reset(start_mirror);
        assert_eq!(
            reset(
                &mut g.meta,
                &mut g.buffer,
                &mut g.state,
                &start_image,
                &mut g.mirror,
                &g.keys,
                &checkpoint,
                false,
                &mut engine,
            ),
            Err(refuse(REFUSE_CONTEXT))
        );
        assert_eq!(engine.resets, 0);
    }

    /// The dead player's use press.  `doom_sim2_guard.h` returns 295 from the
    /// pre-tic guard, which reaches the sweep as an engine error: the tic does
    /// not run, nothing is committed, and the meta is not halted, so the answer
    /// is RESET/9 rather than a lost slot.
    #[test]
    fn a_restart_request_refuses_the_sweep_and_commits_nothing() {
        let mut g = golden();
        let meta_before = g.meta.clone();
        let state_before = g.state.clone();
        let mut engine = RestartRequest;
        assert_eq!(
            sweep(
                &mut g.meta,
                &g.buffer,
                &mut g.state,
                &mut g.mirror,
                &g.keys,
                &g.checkpoint,
                8,
                true,
                &mut engine,
                &NoBudget,
            ),
            Err(refuse(REFUSE_RESTART_REQUEST))
        );
        assert_eq!(g.meta, meta_before);
        assert_eq!(g.state, state_before);
        assert_eq!(g.meta[M_HALT], 0, "a restart request is not a halt");
    }

    /// A press on a live player is ordinary input: only a *dead* player's use
    /// becomes a restart request, so this control runs the same command and
    /// gets a committed tic.
    struct RestartRequest;
    impl Engine for RestartRequest {
        fn step(&mut self, _command: &[u8; 4]) -> Result<StepResult, u32> {
            Err(REFUSE_RESTART_REQUEST)
        }
        fn publish(&mut self, state: &mut [u8], _mirror: &mut [u8]) -> Result<usize, u32> {
            Ok(u32_at(state, 16) as usize)
        }
    }

    #[test]
    fn exact_zero_matches_bytewise_scan() {
        for len in [0usize, 1, 2, 7, 4096] {
            let mut bytes = vec![0u8; len];
            assert!(exact_zero(&bytes));
            if len > 0 {
                bytes[len - 1] = 1;
                assert!(!exact_zero(&bytes));
                bytes[len - 1] = 0;
                bytes[0] = 1;
                assert!(!exact_zero(&bytes));
            }
        }
    }
}
