//! DOOM-E2R/1: the additive resource-account successor to DOOM-E2S/1.
//!
//! E2S/1 remains the retained WAD-copy/state-only ABI.  E2R/1 adds one
//! read-only, sealed DOOM-RI1 account containing the pointer-free E1M5 static
//! graph.  Every invocation still creates a fresh context and rebinds native
//! pointers from section indices; no account address, C pointer, or mutable
//! global is a persisted input.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program_error::ProgramError,
    program_memory::sol_memcpy, pubkey::Pubkey,
};

pub const MAGIC: &[u8; 8] = b"DOOM-E2R";
pub const VERSION: u32 = 1;
pub const TIC: u8 = 2;
pub const INSTRUCTION_BYTES: usize = 32;
pub const ABI_ID: [u8; 32] = super::ex2::RESOURCE_SBF_ABI_ID;
pub const RESOURCE_SBF_ABI_DESCRIPTOR: &str = super::ex2::RESOURCE_SBF_ABI_DESCRIPTOR;

pub const RESOURCE_ACCOUNT: usize = 1;
pub const RESOURCE_ACCOUNT_BYTES: usize = 197_664;
pub const RESOURCE_HEADER_BYTES: usize = 256;
pub const RESOURCE_SECTION_COUNT: usize = 10;
pub const RESOURCE_IMAGE_BYTES: usize = RESOURCE_ACCOUNT_BYTES;
pub const WAD_ACCOUNT_COUNT: usize = 5;
pub const WAD_ACCOUNT_BYTES: usize = 1_048_576;
pub const WAD_HEADER_BYTES: usize = 32;
pub const WAD_IMAGE_BYTES: usize = 4_196_020;
pub const BOOTSTRAP_BYTES: usize = 2 * 1024 * 1024;
pub const ALLOC_ALIGNMENT: usize = 16;

pub const CONTEXT_ACCOUNT: usize = 0;
pub const WAD_FIRST_ACCOUNT: usize = 2;
pub const STATE_ACCOUNT: usize = 7;
pub const MIRROR_ACCOUNT: usize = 8;
pub const CONTROLLER_ACCOUNT: usize = 9;
pub const ACCOUNT_COUNT: usize = 10;

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
    pub resource_bytes: usize,
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
            || data[28..32].iter().any(|&byte| byte != 0)
        {
            return Err(ProgramError::InvalidInstructionData);
        }
        let request = Self {
            resource_bytes: u32_at(data, 16) as usize,
            state_len: u32_at(data, 20) as usize,
            wad_bytes: u32_at(data, 24) as usize,
        };
        if request.resource_bytes != RESOURCE_IMAGE_BYTES
            || !(64..=STATE_MAX_BYTES).contains(&request.state_len)
            || request.wad_bytes != WAD_IMAGE_BYTES
        {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(request)
    }

    pub fn encode(self) -> [u8; INSTRUCTION_BYTES] {
        let mut out = [0u8; INSTRUCTION_BYTES];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12] = TIC;
        out[16..20].copy_from_slice(&(self.resource_bytes as u32).to_le_bytes());
        out[20..24].copy_from_slice(&(self.state_len as u32).to_le_bytes());
        out[24..28].copy_from_slice(&(self.wad_bytes as u32).to_le_bytes());
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
        RESOURCE_ACCOUNT => RESOURCE_ACCOUNT_BYTES,
        WAD_FIRST_ACCOUNT..=6 => WAD_ACCOUNT_BYTES,
        STATE_ACCOUNT => STATE_MAX_BYTES,
        MIRROR_ACCOUNT => MIRROR_BYTES,
        CONTROLLER_ACCOUNT => CONTROLLER_BYTES,
        _ => 0,
    }
}

fn has_nonzero_byte(bytes: &[u8]) -> bool {
    let mut chunks = bytes.chunks_exact(8);
    if chunks
        .by_ref()
        .any(|chunk| u64::from_le_bytes(chunk.try_into().unwrap()) != 0)
    {
        return true;
    }
    chunks.remainder().iter().any(|&byte| byte != 0)
}

#[inline(always)]
fn copy_nonoverlapping(dst: &mut [u8], src: &[u8]) {
    debug_assert_eq!(dst.len(), src.len());
    sol_memcpy(dst, src, dst.len());
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
        || has_nonzero_byte(&data[WAD_HEADER_BYTES + payload_bytes..])
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn validate_resource_header(account: &AccountInfo, expected_bytes: usize) -> ProgramResult {
    let data = account.try_borrow_data()?;
    if data.len() != expected_bytes
        || &data[..8] != b"DOOM-RI1"
        || u32_at(&data, 8) != 1
        || u32_at(&data, 12) as usize != RESOURCE_HEADER_BYTES
        || u32_at(&data, 16) as usize != expected_bytes
        || u32_at(&data, 20) != 1
        || u32_at(&data, 24) != 1
        || u32_at(&data, 28) != 5
        || u32_at(&data, 72) as usize != RESOURCE_SECTION_COUNT
        || u32_at(&data, 80) as usize != RESOURCE_HEADER_BYTES
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
    validate_state(&accounts[STATE_ACCOUNT], request.state_len)?;
    validate_mirror(&accounts[MIRROR_ACCOUNT])?;
    let state = accounts[STATE_ACCOUNT].try_borrow_data()?;
    let gametic = u32_at(&state, 20);
    drop(state);
    // This is intentionally before the RI1 and five-WAD scans.  It preserves
    // the bounded malformed-CI2 refusal under the normal qualification limit.
    validate_controller(&accounts[CONTROLLER_ACCOUNT], gametic)?;
    validate_resource_header(&accounts[RESOURCE_ACCOUNT], request.resource_bytes)?;
    for ordinal in 0..WAD_ACCOUNT_COUNT {
        validate_wad_account(&accounts[WAD_FIRST_ACCOUNT + ordinal], ordinal)?;
    }
    Ok(())
}

#[cfg(all(feature = "ctx-engine", target_os = "solana"))]
unsafe extern "C" {
    fn doom_engine_ex2_resource_sbf_tic_entry(
        context: *const u8,
        resource: *const u8,
        resource_bytes: usize,
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
        copy_nonoverlapping(
            &mut context[cursor..cursor + payload_len],
            &source[WAD_HEADER_BYTES..WAD_HEADER_BYTES + payload_len],
        );
        cursor += payload_len;
    }
    if cursor != wad_end {
        return Err(ProgramError::InvalidAccountData);
    }
    let (_, context_tail) = context.split_at_mut(wad_start);
    let (wad_image, after_wad) = context_tail.split_at_mut(request.wad_bytes);
    let (_gap, bootstrap_tail) = after_wad.split_at_mut(bootstrap_start - wad_end);
    let bootstrap = &mut bootstrap_tail[..BOOTSTRAP_BYTES];
    if (bootstrap.as_mut_ptr() as usize) % ALLOC_ALIGNMENT != 0 {
        return Err(ProgramError::InvalidAccountData);
    }

    let resource = accounts[RESOURCE_ACCOUNT].try_borrow_data()?;
    let mut state = accounts[STATE_ACCOUNT].try_borrow_mut_data()?;
    let mut mirror = accounts[MIRROR_ACCOUNT].try_borrow_mut_data()?;
    let controller = accounts[CONTROLLER_ACCOUNT].try_borrow_data()?;
    let mut state_out_len = 0usize;
    let mut mirror_out_len = 0usize;
    let mut arena_high_water = 0usize;
    let rc = unsafe {
        doom_engine_ex2_resource_sbf_tic_entry(
            context_ptr,
            resource.as_ptr(),
            resource.len(),
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
            "DOOM-E2R rc={} arena_high_water={} state_len={} mirror_len={}",
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
    Ok(())
}

#[cfg(all(feature = "ctx-engine", not(target_os = "solana")))]
pub fn process(
    _program: &Pubkey,
    _accounts: &[AccountInfo],
    _instruction_data: &[u8],
) -> ProgramResult {
    // The successor is intentionally loader-only.  A host ProgramTest path
    // must not silently substitute the Rust/native model for the SBF image.
    Err(ProgramError::Custom(243))
}
