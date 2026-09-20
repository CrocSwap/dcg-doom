//! DOOM-E2S/1: the first account-backed SBF EX2 boundary.
//!
//! This is intentionally not the host DOOM-EX2/v2 envelope.  The host
//! envelope retains a native account vector and an optional framebuffer; this
//! instruction has an explicit account layout, fixed bounded WAD image bridge,
//! and a state-only continuation.  It calls the retained engine itself and
//! returns the pointer-free ST1/MP1 bytes in the same state and mirror
//! accounts.  Renderer setup is a later versioned boundary.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program_error::ProgramError,
    pubkey::Pubkey,
};

pub const MAGIC: &[u8; 8] = b"DOOM-E2S";
pub const VERSION: u32 = 1;
pub const TIC: u8 = 2;
pub const INSTRUCTION_BYTES: usize = 24;
pub const ABI_ID: [u8; 32] = super::ex2::SBF_ABI_ID;

pub const WAD_ACCOUNT_COUNT: usize = 5;
pub const WAD_ACCOUNT_BYTES: usize = 1_048_576;
pub const WAD_HEADER_BYTES: usize = 32;
/// Measured from the retained account images under the native acceptance
/// capture.  The entry does not rehash this image in the first tic; MP1's
/// captured WAD binding is passed to the engine as the already-committed
/// input, while each account header and padding is checked here.
pub const WAD_IMAGE_BYTES: usize = 4_196_020;
pub const BOOTSTRAP_BYTES: usize = 2 * 1024 * 1024;
/// The C carrier serves malloc/calloc/realloc and requires a sixteen-byte
/// aligned base for this SBF engine cut. The runtime checks below also reject
/// an account mapping whose actual base does not satisfy this contract.
pub const ALLOC_ALIGNMENT: usize = 16;

pub const CONTEXT_ACCOUNT: usize = 0;
pub const WAD_FIRST_ACCOUNT: usize = 1;
pub const STATE_ACCOUNT: usize = 6;
pub const MIRROR_ACCOUNT: usize = 7;
pub const CONTROLLER_ACCOUNT: usize = 8;
pub const ACCOUNT_COUNT: usize = 9;

pub const STATE_MAX_BYTES: usize = super::ex2::STATE_MAX_BYTES;
pub const MIRROR_BYTES: usize = super::ex2::MIRROR_BYTES;
pub const CONTROLLER_BYTES: usize = super::ex2::CONTROLLER_BYTES;
pub const CTX_STRUCT_BYTES: usize = super::CTX_MIN_BYTES - 24;
pub const WAD_IMAGE_OFFSET: usize = super::CTX_MIN_BYTES;
pub const BOOTSTRAP_OFFSET: usize = align16_const(WAD_IMAGE_OFFSET + WAD_IMAGE_BYTES);
pub const CONTEXT_BYTES: usize = BOOTSTRAP_OFFSET + BOOTSTRAP_BYTES;

const CONTROLLER_HEADER_BYTES: usize = 28;

const fn align16_const(value: usize) -> usize {
    (value + (ALLOC_ALIGNMENT - 1)) & !(ALLOC_ALIGNMENT - 1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub state_len: usize,
    pub wad_bytes: usize,
}

impl Request {
    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != INSTRUCTION_BYTES
            || &data[..8] != MAGIC
            || u32_at(data, 8) != VERSION
            || data[12] != TIC
            || data[13..16].iter().any(|&byte| byte != 0)
        {
            return Err(ProgramError::InvalidInstructionData);
        }
        let state_len = u32_at(data, 16) as usize;
        let wad_bytes = u32_at(data, 20) as usize;
        if !(64..=STATE_MAX_BYTES).contains(&state_len) || wad_bytes != WAD_IMAGE_BYTES {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(Self {
            state_len,
            wad_bytes,
        })
    }

    pub fn encode(self) -> [u8; INSTRUCTION_BYTES] {
        let mut out = [0u8; INSTRUCTION_BYTES];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12] = TIC;
        out[16..20].copy_from_slice(&(self.state_len as u32).to_le_bytes());
        out[20..24].copy_from_slice(&(self.wad_bytes as u32).to_le_bytes());
        out
    }
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn expected_writable(index: usize) -> bool {
    matches!(index, CONTEXT_ACCOUNT | STATE_ACCOUNT | MIRROR_ACCOUNT)
}

fn expected_bytes(index: usize) -> usize {
    match index {
        CONTEXT_ACCOUNT => CONTEXT_BYTES,
        WAD_FIRST_ACCOUNT..=5 => WAD_ACCOUNT_BYTES,
        STATE_ACCOUNT => STATE_MAX_BYTES,
        MIRROR_ACCOUNT => MIRROR_BYTES,
        CONTROLLER_ACCOUNT => CONTROLLER_BYTES,
        _ => 0,
    }
}

fn validate_wad_account(account: &AccountInfo, ordinal: usize) -> ProgramResult {
    let data = account.try_borrow_data()?;
    if &data[..8] != b"DOOM-WC1"
        || u32_at(&data, 8) != 1
        || u32_at(&data, 12) as usize != WAD_HEADER_BYTES
        || u32_at(&data, 16) as usize != WAD_ACCOUNT_BYTES
        || u32_at(&data, 20) as usize != ordinal
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let payload_bytes = u32_at(&data, 24) as usize;
    if payload_bytes > WAD_ACCOUNT_BYTES - WAD_HEADER_BYTES
        || data[WAD_HEADER_BYTES + payload_bytes..]
            .iter()
            .any(|&byte| byte != 0)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn validate_state(account: &AccountInfo, expected_len: usize) -> ProgramResult {
    let data = account.try_borrow_data()?;
    if &data[..8] != b"DOOM-ST1"
        || u32_at(&data, 8) != 0
        || u32_at(&data, 12) as usize != 64
        || u32_at(&data, 16) as usize != expected_len
        || expected_len > data.len()
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn validate_mirror(account: &AccountInfo) -> ProgramResult {
    let data = account.try_borrow_data()?;
    if &data[..8] != b"DOOM-MP1"
        || u32_at(&data, 8) != 1
        || u32_at(&data, 12) as usize != 4096
        || u32_at(&data, 16) as usize != MIRROR_BYTES
        || u32_at(&data, 20) as usize != 160
        || u32_at(&data, 24) as usize != 384
        || u32_at(&data, 28) as usize > 384
        || data[68..4096].iter().any(|&byte| byte != 0)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn validate_controller(account: &AccountInfo, expected_tic: u32) -> ProgramResult {
    let data = account.try_borrow_data()?;
    if &data[..8] != b"DOOM-CI2"
        || u32_at(&data, 8) != 1
        || u32_at(&data, 12) as usize != CONTROLLER_HEADER_BYTES
        || u32_at(&data, 16) as usize != CONTROLLER_BYTES
        || u32_at(&data, 20) != expected_tic
        || data[28..].iter().any(|&byte| byte != 0)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn validate_accounts(
    program: &Pubkey,
    accounts: &[AccountInfo],
    request: Request,
) -> ProgramResult {
    if accounts.len() != ACCOUNT_COUNT {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    for (index, account) in accounts.iter().enumerate() {
        if account.owner != program
            || account.data_len() != expected_bytes(index)
            || account.is_writable != expected_writable(index)
            || account.is_signer
        {
            return Err(ProgramError::InvalidAccountData);
        }
        if accounts[..index]
            .iter()
            .any(|previous| previous.key == account.key)
        {
            return Err(ProgramError::InvalidAccountData);
        }
    }
    for ordinal in 0..WAD_ACCOUNT_COUNT {
        validate_wad_account(&accounts[WAD_FIRST_ACCOUNT + ordinal], ordinal)?;
    }
    validate_state(&accounts[STATE_ACCOUNT], request.state_len)?;
    validate_mirror(&accounts[MIRROR_ACCOUNT])?;
    let state = accounts[STATE_ACCOUNT].try_borrow_data()?;
    let gametic = u32_at(&state, 20);
    drop(state);
    validate_controller(&accounts[CONTROLLER_ACCOUNT], gametic)
}

#[cfg(all(feature = "ctx-engine", target_os = "solana"))]
unsafe extern "C" {
    fn doom_engine_ex2_sbf_tic_entry(
        context: *const u8,
        wad_image: *const u8,
        wad_bytes: usize,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        state: *const u8,
        state_len: usize,
        mirror: *const u8,
        mirror_len: usize,
        controller: *const u8,
        controller_len: usize,
        state_out: *mut u8,
        state_cap: usize,
        state_out_len: *mut usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
        mirror_out_len: *mut usize,
        arena_high_water: *mut usize,
    ) -> i32;
}

#[cfg(all(feature = "ctx-engine", target_os = "solana"))]
pub fn process(
    program: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let request = Request::decode(instruction_data)?;
    validate_accounts(program, accounts, request)?;

    let mut context = accounts[CONTEXT_ACCOUNT].try_borrow_mut_data()?;
    let wad_start = WAD_IMAGE_OFFSET;
    let wad_end = wad_start + request.wad_bytes;
    let bootstrap_start = BOOTSTRAP_OFFSET;
    let bootstrap_end = bootstrap_start + BOOTSTRAP_BYTES;
    if wad_end > bootstrap_start || bootstrap_end > context.len() {
        return Err(ProgramError::InvalidAccountData);
    }
    let context_ptr = context.as_mut_ptr();
    if (context_ptr as usize) % ALLOC_ALIGNMENT != 0 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut cursor = wad_start;
    for ordinal in 0..WAD_ACCOUNT_COUNT {
        let source = accounts[WAD_FIRST_ACCOUNT + ordinal].try_borrow_data()?;
        let payload_len = u32_at(&source, 24) as usize;
        context[cursor..cursor + payload_len]
            .copy_from_slice(&source[WAD_HEADER_BYTES..WAD_HEADER_BYTES + payload_len]);
        cursor += payload_len;
    }
    if cursor != wad_end {
        return Err(ProgramError::InvalidAccountData);
    }
    let (_, context_tail) = context.split_at_mut(wad_start);
    let (wad_image, after_wad) = context_tail.split_at_mut(request.wad_bytes);
    let (_alignment_gap, bootstrap_tail) = after_wad.split_at_mut(bootstrap_start - wad_end);
    let bootstrap = &mut bootstrap_tail[..BOOTSTRAP_BYTES];
    if (bootstrap.as_mut_ptr() as usize) % ALLOC_ALIGNMENT != 0 {
        return Err(ProgramError::InvalidAccountData);
    }

    let mut state = accounts[STATE_ACCOUNT].try_borrow_mut_data()?;
    let mut mirror = accounts[MIRROR_ACCOUNT].try_borrow_mut_data()?;
    let controller = accounts[CONTROLLER_ACCOUNT].try_borrow_data()?;
    let mut state_out_len = 0usize;
    let mut mirror_out_len = 0usize;
    let mut arena_high_water = 0usize;
    let rc = unsafe {
        doom_engine_ex2_sbf_tic_entry(
            context_ptr,
            wad_image.as_ptr(),
            wad_image.len(),
            bootstrap.as_mut_ptr(),
            bootstrap.len(),
            state.as_ptr(),
            request.state_len,
            mirror.as_ptr(),
            mirror.len(),
            controller.as_ptr(),
            controller.len(),
            state.as_mut_ptr(),
            state.len(),
            &mut state_out_len,
            mirror.as_mut_ptr(),
            mirror.len(),
            &mut mirror_out_len,
            &mut arena_high_water,
        )
    };
    if rc != 0 {
        msg!(
            "DOOM-E2S rc={} arena_high_water={} state_len={} mirror_len={}",
            rc,
            arena_high_water,
            state_out_len,
            mirror_out_len
        );
        return Err(ProgramError::Custom(rc as u32));
    }
    if state_out_len < 64
        || state_out_len > state.len()
        || mirror_out_len != MIRROR_BYTES
        || u32_at(&state, 16) as usize != state_out_len
    {
        return Err(ProgramError::InvalidAccountData);
    }
    // The fixed probe scratch is retained as a bounded diagnostic counter for
    // the next experiment.  It is outside the generated context struct and
    // does not participate in ST1/MP1 consensus bytes.
    context[CTX_STRUCT_BYTES..CTX_STRUCT_BYTES + 8]
        .copy_from_slice(&(arena_high_water as u64).to_le_bytes());
    Ok(())
}

/// The ProgramTest driver is a host binary and must never execute this seam
/// through a native processor.  Keep the host library linkable while making
/// that accidental path an explicit legacy refusal; the real implementation
/// above is compiled only for the SBF target and is reached by loader
/// dispatch.  `prefer_bpf(true)` in `driver/src/bin/ex2_sbf.rs` is therefore a
/// required part of the positive gate, not an optimization hint.
#[cfg(all(feature = "ctx-engine", not(target_os = "solana")))]
pub fn process(
    _program: &Pubkey,
    _accounts: &[AccountInfo],
    _instruction_data: &[u8],
) -> ProgramResult {
    Err(ProgramError::Custom(231))
}
