//! Versioned, account-ordered interface for the pending headless Doom path.
//!
//! This module deliberately stops before calling the linked C engine.  The
//! patch-0007 image currently contains C function pointers in the generated
//! `states` initializer, and their account-persistent representation has not
//! been made pointer-free or exercised on SBF.  The entry gate therefore
//! validates the wire/account shape and refuses with
//! `ENGINE_ACTION_POINTERS_UNRESOLVED` until that ABI gate is closed.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

pub const MAGIC: &[u8; 8] = b"DOOM-EX1";
pub const VERSION: u32 = 1;
pub const REQUEST_BYTES: usize = 32;

pub const INIT: u8 = 0;
pub const LOAD_LEVEL: u8 = 1;
pub const TIC: u8 = 2;
pub const RENDER: u8 = 3;

/// Deliberate fail-closed result from the source-only step-6 entry gate.
/// This is a design ABI value, not a measured SBF execution result.
pub const ENGINE_ACTION_POINTERS_UNRESOLVED: u32 = 230;

pub const CONTEXT_ACCOUNT: usize = 0;
pub const CONFIG_ACCOUNT: usize = 1;
pub const WAD_FIRST_ACCOUNT: usize = 2;
pub const WAD_ACCOUNT_COUNT: usize = 5;
pub const LEVEL_ACCOUNT: usize = 7;
pub const MOBJ_ACCOUNT: usize = 8;
pub const CONTROLLER_ACCOUNT: usize = 9;
pub const SCRATCH_ACCOUNT: usize = 10;
pub const FRAME_ACCOUNT: usize = 11;
pub const ACCOUNT_COUNT: usize = 12;

pub const CONTEXT_BYTES: usize = 3_440_952 + 24;
pub const CONFIG_BYTES: usize = 4_096;
pub const WAD_ACCOUNT_BYTES: usize = 1_048_576;
pub const LEVEL_BYTES: usize = 131_072;
pub const MOBJ_BYTES: usize = 65_536;
pub const CONTROLLER_BYTES: usize = 64;
pub const SCRATCH_BYTES: usize = 131_072;
pub const FRAME_BYTES: usize = 8_192;

pub const CONTROLLER_MAGIC: &[u8; 8] = b"DOOM-CI1";
pub const CONTROLLER_HEADER_BYTES: usize = 28;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub operation: u8,
    pub arg0: u32,
    pub arg1: u32,
    pub arg2: u32,
}

impl Request {
    pub const fn init() -> Self {
        Self {
            operation: INIT,
            arg0: 0,
            arg1: 0,
            arg2: 0,
        }
    }

    pub const fn load_level(episode: u32, map: u32, skill: u32) -> Self {
        Self {
            operation: LOAD_LEVEL,
            arg0: episode,
            arg1: map,
            arg2: skill,
        }
    }

    pub const fn tic(gametic: u32) -> Self {
        Self {
            operation: TIC,
            arg0: gametic,
            arg1: 0,
            arg2: 0,
        }
    }

    pub const fn render(gametic: u32, strip_id: u32) -> Self {
        Self {
            operation: RENDER,
            arg0: gametic,
            arg1: strip_id,
            arg2: 0,
        }
    }

    pub fn encode(self) -> [u8; REQUEST_BYTES] {
        let mut out = [0u8; REQUEST_BYTES];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12] = self.operation;
        out[16..20].copy_from_slice(&self.arg0.to_le_bytes());
        out[20..24].copy_from_slice(&self.arg1.to_le_bytes());
        out[24..28].copy_from_slice(&self.arg2.to_le_bytes());
        out
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != REQUEST_BYTES || &data[..8] != MAGIC || u32_at(data, 8) != VERSION {
            return Err(ProgramError::InvalidInstructionData);
        }
        if data[13..16].iter().any(|&byte| byte != 0) || data[28..].iter().any(|&byte| byte != 0) {
            return Err(ProgramError::InvalidInstructionData);
        }
        if !matches!(data[12], INIT | LOAD_LEVEL | TIC | RENDER) {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(Self {
            operation: data[12],
            arg0: u32_at(data, 16),
            arg1: u32_at(data, 20),
            arg2: u32_at(data, 24),
        })
    }
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn writable(operation: u8, account: usize) -> bool {
    if account == CONTEXT_ACCOUNT {
        return true;
    }
    match operation {
        INIT => matches!(account, CONFIG_ACCOUNT | LEVEL_ACCOUNT | MOBJ_ACCOUNT),
        LOAD_LEVEL | TIC => matches!(account, LEVEL_ACCOUNT | MOBJ_ACCOUNT),
        RENDER => matches!(account, SCRATCH_ACCOUNT | FRAME_ACCOUNT),
        _ => false,
    }
}

fn signer(operation: u8, account: usize) -> bool {
    operation == INIT && account == CONFIG_ACCOUNT
}

fn expected_bytes(account: usize) -> usize {
    if (WAD_FIRST_ACCOUNT..WAD_FIRST_ACCOUNT + WAD_ACCOUNT_COUNT).contains(&account) {
        return WAD_ACCOUNT_BYTES;
    }
    match account {
        CONTEXT_ACCOUNT => CONTEXT_BYTES,
        CONFIG_ACCOUNT => CONFIG_BYTES,
        LEVEL_ACCOUNT => LEVEL_BYTES,
        MOBJ_ACCOUNT => MOBJ_BYTES,
        CONTROLLER_ACCOUNT => CONTROLLER_BYTES,
        SCRATCH_ACCOUNT => SCRATCH_BYTES,
        FRAME_ACCOUNT => FRAME_BYTES,
        _ => 0,
    }
}

fn validate_controller(account: &AccountInfo, gametic: u32) -> ProgramResult {
    if account.data_len() != CONTROLLER_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = account.try_borrow_data()?;
    if &data[..8] != CONTROLLER_MAGIC
        || u32_at(&data, 8) != VERSION
        || u32_at(&data, 12) as usize != CONTROLLER_HEADER_BYTES
        || u32_at(&data, 16) != CONTROLLER_BYTES as u32
        || u32_at(&data, 20) != gametic
        || data[28..].iter().any(|&byte| byte != 0)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

/// Validate the fixed account vector before any future C call.  In
/// particular, the context account is always ordinal zero: the existing
/// `DOOM_CTX` constant only has a measured mapping at that position.
pub fn validate_accounts(
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
            || account.is_writable != writable(request.operation, index)
            || account.is_signer != signer(request.operation, index)
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
    if request.operation == LOAD_LEVEL
        && (request.arg0 != 1 || !(1..=9).contains(&request.arg1) || request.arg2 > 4)
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    if request.operation == RENDER && request.arg1 >= 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if request.operation == TIC {
        validate_controller(&accounts[CONTROLLER_ACCOUNT], request.arg0)?;
    }
    Ok(())
}

#[cfg(feature = "ctx-engine")]
unsafe extern "C" {
    fn doom_engine_entry_gate() -> i32;
}

/// Entry glue for the future account-backed engine operations.  The account
/// and wire checks are real interface behavior; the C engine call is not.
/// Keeping the refusal here prevents a reviewable driver from becoming an
/// accidental claim that init/load/tic/render ran on SBF.
#[cfg(feature = "ctx-engine")]
pub fn process(
    program: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let request = Request::decode(instruction_data)?;
    validate_accounts(program, accounts, request)?;
    let rc = unsafe { doom_engine_entry_gate() };
    Err(ProgramError::Custom(rc as u32))
}
