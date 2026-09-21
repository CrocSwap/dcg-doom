//! DOOM-E2R/4: bounded, authenticated reconstruction continuation.
//!
//! E2R/3 remains the two-instruction reconstruction experiment.  This module
//! is a separate wire version because it commits the live source-matched
//! engine context after setup and after decode.  The continuation account
//! carries captured bytes and typed offsets only; the pinned context account
//! carries the live C pointers and arena between transactions.

use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    hash::{hash, hashv},
    program_error::ProgramError,
    program_memory::sol_memcpy,
    pubkey::Pubkey,
};

use crate::resource_sbf;

pub const MAGIC: &[u8; 8] = b"DOOM-E2R";
pub const VERSION: u32 = 4;
pub const PREPARE: u8 = 5;
pub const STEP: u8 = 6;
pub const INSTRUCTION_BYTES: usize = 32;
pub const ABI_DESCRIPTOR: &str = "DOOM-E2R/4|instruction=32|operations=PREPARE/5+STEP/6|accounts=13|continuation=262144|payload=ST1+MP1+CI2+FB1|relocations=typed-offsets|context=DOOM-CT4-pinned-live-engine|phases=setup+decode+finalize";

pub const CONTINUATION_ACCOUNT: usize = 11;
pub const AUTHORITY_ACCOUNT: usize = 12;
pub const ACCOUNT_COUNT: usize = 13;

pub const HEADER_BYTES: usize = 1024;
pub const STATE_BYTES: usize = resource_sbf::STATE_MAX_BYTES;
pub const MIRROR_BYTES: usize = resource_sbf::MIRROR_BYTES;
pub const CONTROLLER_BYTES: usize = resource_sbf::CONTROLLER_BYTES;
pub const FRAME_BYTES: usize = resource_sbf::RI2_FRAME_BYTES;
pub const STATE_OFFSET: usize = HEADER_BYTES;
pub const MIRROR_OFFSET: usize = STATE_OFFSET + STATE_BYTES;
pub const CONTROLLER_OFFSET: usize = MIRROR_OFFSET + MIRROR_BYTES;
pub const FRAME_OFFSET: usize = CONTROLLER_OFFSET + CONTROLLER_BYTES;
pub const RELOC_OFFSET: usize = FRAME_OFFSET + FRAME_BYTES;
pub const RELOC_COUNT: usize = 3;
pub const CONTINUATION_BYTES: usize = 262_144;
pub const CONTEXT_CHECKPOINT_OFFSET: usize = resource_sbf::CONTEXT_BYTES - 128;
pub const CONTEXT_CHECKPOINT_BYTES: usize = 128;

const PHASE_PREPARED: u32 = 1;
const PHASE_PREFLIGHT: u32 = 2;
const PHASE_SETUP: u32 = 3;
const PHASE_DECODE_PREFLIGHT: u32 = 4;
const PHASE_DECODE_PREPARED: u32 = 5;
const PHASE_DECODED: u32 = 6;
const PHASE_PUBLISHED: u32 = 7;
const DOMAIN_ROOT: &[u8] = b"DOOM-E2R/4-ROOT\0";
const DOMAIN_ACCOUNTS: &[u8] = b"DOOM-E2R/4-ACCOUNTS\0";
const DOMAIN_PROTOCOL: &[u8] = b"DOOM-E2R/4-PROTOCOL\0";
const DOMAIN_PROGRAM: &[u8] = b"DOOM-E2R/4-PROGRAM\0";
const WAD_HASH: [u8; 32] = [
    0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9, 0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3, 0xe2, 0x9c,
    0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18, 0x16, 0xf6, 0x52, 0xa5, 0x26, 0xca, 0xc7, 0x71,
];

const H_VERSION: usize = 8;
const H_HEADER: usize = 12;
const H_TOTAL: usize = 16;
const H_OPERATION: usize = 20;
const H_PHASE: usize = 24;
const H_CURSOR: usize = 28;
const H_STATE_LEN: usize = 32;
const H_STATE_BYTES: usize = 36;
const H_MIRROR_BYTES: usize = 40;
const H_CONTROLLER_BYTES: usize = 44;
const H_FRAME_BYTES: usize = 48;
const H_RESOURCE_BYTES: usize = 52;
const H_WAD_BYTES: usize = 56;
const H_OUTPUT_STATE_LEN: usize = 60;
const H_RELOC_OFFSET: usize = 64;
const H_RELOC_COUNT: usize = 68;
const H_PROTOCOL: usize = 72;
const H_PROGRAM: usize = 104;
const H_SOURCE: usize = 136;
const H_RESOURCE_HASH: usize = 168;
const H_WAD_HASH: usize = 200;
const H_STATE_HASH: usize = 232;
const H_MIRROR_HASH: usize = 264;
const H_CONTROLLER_HASH: usize = 296;
const H_FRAME_HASH: usize = 328;
const H_PREVIOUS_ROOT: usize = 360;
const H_OUTPUT_ROOT: usize = 392;
const H_ACCOUNT_SET: usize = 424;
const H_AUTHORITY: usize = 456;
const H_CONTEXT_ROOT: usize = 488;
const H_SOURCE_ABI: usize = 520;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub operation: u8,
    pub state_len: usize,
    pub cursor: u32,
}

impl Request {
    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != INSTRUCTION_BYTES
            || &data[..8] != MAGIC
            || u32_at(data, 8) != VERSION
            || !matches!(data[12], PREPARE | STEP)
            || data[13..16].iter().any(|&byte| byte != 0)
            || data[24..32].iter().any(|&byte| byte != 0)
        {
            return Err(ProgramError::InvalidInstructionData);
        }
        let state_len = u32_at(data, 16) as usize;
        if !(64..=STATE_BYTES).contains(&state_len) {
            return Err(ProgramError::InvalidInstructionData);
        }
        let cursor = u32_at(data, 20);
        if (data[12] == PREPARE && cursor != 0) || (data[12] == STEP && !(1..=6).contains(&cursor))
        {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(Self {
            operation: data[12],
            state_len,
            cursor,
        })
    }

    pub fn encode(self) -> [u8; INSTRUCTION_BYTES] {
        let mut out = [0u8; INSTRUCTION_BYTES];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12] = self.operation;
        out[16..20].copy_from_slice(&(self.state_len as u32).to_le_bytes());
        out[20..24].copy_from_slice(&self.cursor.to_le_bytes());
        out
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    hash(bytes).to_bytes()
}

fn account_set_hash(program: &Pubkey, accounts: &[AccountInfo]) -> [u8; 32] {
    let keys: Vec<&[u8]> = accounts
        .iter()
        .map(|account| account.key.as_ref())
        .collect();
    hashv(&[DOMAIN_ACCOUNTS, program.as_ref(), &keys.concat()]).to_bytes()
}

fn protocol_hash() -> [u8; 32] {
    hash(DOMAIN_PROTOCOL).to_bytes()
}

fn program_hash(program: &Pubkey) -> [u8; 32] {
    hashv(&[DOMAIN_PROGRAM, program.as_ref()]).to_bytes()
}

fn source_hash() -> [u8; 32] {
    hashv(&[
        b"DOOM-E2R/4-SOURCE\0",
        ABI_DESCRIPTOR.as_bytes(),
        &resource_sbf::RI2_ABI_ID,
    ])
    .to_bytes()
}

fn abi_hash() -> [u8; 32] {
    hash_bytes(ABI_DESCRIPTOR.as_bytes())
}

fn root_hash(state: &[u8], mirror: &[u8], controller: &[u8], frame: &[u8]) -> [u8; 32] {
    hashv(&[DOMAIN_ROOT, state, mirror, controller, frame]).to_bytes()
}

fn context_root(context: &[u8]) -> [u8; 32] {
    hashv(&[
        b"DOOM-E2R/4-CONTEXT-GUARD\0",
        &context[..64],
        &context[CONTEXT_CHECKPOINT_OFFSET - 64..CONTEXT_CHECKPOINT_OFFSET],
    ])
    .to_bytes()
}

fn expected_writable(index: usize) -> bool {
    matches!(index, 0 | 7 | 8 | 10 | CONTINUATION_ACCOUNT)
}

fn expected_bytes(index: usize) -> usize {
    match index {
        0 => resource_sbf::CONTEXT_BYTES,
        1 => resource_sbf::RI2_RESOURCE_IMAGE_BYTES,
        2..=6 => resource_sbf::WAD_ACCOUNT_BYTES,
        7 => STATE_BYTES,
        8 => MIRROR_BYTES,
        9 => CONTROLLER_BYTES,
        10 => FRAME_BYTES,
        CONTINUATION_ACCOUNT => CONTINUATION_BYTES,
        _ => 0,
    }
}

fn validate_accounts(program: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    if accounts.len() != ACCOUNT_COUNT {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    for (index, account) in accounts.iter().enumerate().take(CONTINUATION_ACCOUNT + 1) {
        if account.owner != program
            || account.data_len() != expected_bytes(index)
            || account.is_writable != expected_writable(index)
            || account.is_signer
            || accounts[..index]
                .iter()
                .any(|previous| previous.key == account.key)
        {
            return Err(ProgramError::InvalidAccountData);
        }
    }
    let authority = &accounts[AUTHORITY_ACCOUNT];
    if !authority.is_signer
        || authority.is_writable
        || accounts[..AUTHORITY_ACCOUNT]
            .iter()
            .any(|previous| previous.key == authority.key)
    {
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}

fn validate_state(state: &[u8], state_len: usize) -> ProgramResult {
    if state_len > state.len()
        || &state[..8] != b"DOOM-ST1"
        || u32_at(state, 8) != 0
        || u32_at(state, 12) != 64
        || u32_at(state, 16) as usize != state_len
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn validate_mirror(mirror: &[u8]) -> ProgramResult {
    if &mirror[..8] != b"DOOM-MP1"
        || u32_at(mirror, 8) != 1
        || u32_at(mirror, 12) != 4096
        || u32_at(mirror, 16) as usize != MIRROR_BYTES
        || u32_at(mirror, 20) != 160
        || u32_at(mirror, 24) != 384
        || u32_at(mirror, 28) > 384
        || mirror[68..4096].iter().any(|&byte| byte != 0)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn validate_controller(controller: &[u8], tic: u32) -> ProgramResult {
    if &controller[..8] != b"DOOM-CI2"
        || u32_at(controller, 8) != 1
        || u32_at(controller, 12) != 28
        || u32_at(controller, 16) as usize != CONTROLLER_BYTES
        || u32_at(controller, 20) != tic
        || controller[28..].iter().any(|&byte| byte != 0)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn validate_resource(resource: &[u8]) -> ProgramResult {
    if resource.len() != resource_sbf::RI2_RESOURCE_IMAGE_BYTES
        || &resource[..8] != b"DOOM-RI2"
        || u32_at(resource, 8) != resource_sbf::RI2_VERSION
        || u32_at(resource, 12) != 256
        || u32_at(resource, 16) as usize != resource.len()
        || u32_at(resource, 20) != 2
        || u32_at(resource, 24) != 1
        || u32_at(resource, 28) != 5
        || u32_at(resource, 32) != 5
        || u32_at(resource, 36) != 256
        || resource[72..104] != WAD_HASH
    {
        return Err(ProgramError::Custom(240));
    }
    Ok(())
}

fn validate_wad(account: &[u8], ordinal: usize) -> ProgramResult {
    if &account[..8] != b"DOOM-WC1"
        || u32_at(account, 8) != 1
        || u32_at(account, 12) != 32
        || u32_at(account, 16) as usize != resource_sbf::WAD_ACCOUNT_BYTES
        || u32_at(account, 20) as usize != ordinal
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let payload = u32_at(account, 24) as usize;
    if payload > resource_sbf::WAD_ACCOUNT_BYTES - resource_sbf::WAD_HEADER_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn write_relocs(data: &mut [u8]) {
    let rows = [
        (
            0u32,
            CONTEXT_CHECKPOINT_OFFSET as u32,
            CONTEXT_CHECKPOINT_BYTES as u32,
            1u32,
        ),
        (1u32, 0u32, STATE_BYTES as u32, 2u32),
        (2u32, 0u32, FRAME_BYTES as u32, 3u32),
    ];
    for (index, row) in rows.into_iter().enumerate() {
        let at = RELOC_OFFSET + index * 16;
        put_u32(data, at, row.0);
        put_u32(data, at + 4, row.1);
        put_u32(data, at + 8, row.2);
        put_u32(data, at + 12, row.3);
    }
}

fn validate_relocs(data: &[u8]) -> ProgramResult {
    let expected = [
        (
            0u32,
            CONTEXT_CHECKPOINT_OFFSET as u32,
            CONTEXT_CHECKPOINT_BYTES as u32,
            1u32,
        ),
        (1u32, 0u32, STATE_BYTES as u32, 2u32),
        (2u32, 0u32, FRAME_BYTES as u32, 3u32),
    ];
    for (index, row) in expected.into_iter().enumerate() {
        let at = RELOC_OFFSET + index * 16;
        if [
            u32_at(data, at),
            u32_at(data, at + 4),
            u32_at(data, at + 8),
            u32_at(data, at + 12),
        ] != [row.0, row.1, row.2, row.3]
        {
            return Err(ProgramError::InvalidAccountData);
        }
    }
    Ok(())
}

fn set_hash(data: &mut [u8], offset: usize, value: &[u8; 32]) {
    data[offset..offset + 32].copy_from_slice(value);
}

fn fill_checkpoint(
    context: &mut [u8],
    phase: u32,
    cursor: u32,
    state_root: &[u8; 32],
    accounts_root: &[u8; 32],
) {
    let checkpoint = &mut context
        [CONTEXT_CHECKPOINT_OFFSET..CONTEXT_CHECKPOINT_OFFSET + CONTEXT_CHECKPOINT_BYTES];
    checkpoint.fill(0);
    checkpoint[..8].copy_from_slice(b"DOOM-CT4");
    put_u32(checkpoint, 8, VERSION);
    put_u32(checkpoint, 12, phase);
    put_u32(checkpoint, 16, cursor);
    set_hash(checkpoint, 20, state_root);
    set_hash(checkpoint, 52, accounts_root);
    put_u32(checkpoint, 84, 0);
    put_u32(checkpoint, 88, CONTEXT_CHECKPOINT_OFFSET as u32);
}

fn check_checkpoint(
    context: &[u8],
    phase: u32,
    cursor: u32,
    state_root: &[u8; 32],
    accounts_root: &[u8; 32],
) -> ProgramResult {
    let checkpoint =
        &context[CONTEXT_CHECKPOINT_OFFSET..CONTEXT_CHECKPOINT_OFFSET + CONTEXT_CHECKPOINT_BYTES];
    if &checkpoint[..8] != b"DOOM-CT4"
        || u32_at(checkpoint, 8) != VERSION
        || u32_at(checkpoint, 12) != phase
        || u32_at(checkpoint, 16) != cursor
        || u32_at(checkpoint, 84) != 0
        || u32_at(checkpoint, 88) as usize != CONTEXT_CHECKPOINT_OFFSET
        || &checkpoint[20..52] != state_root
        || &checkpoint[52..84] != accounts_root
    {
        return Err(ProgramError::Custom(241));
    }
    Ok(())
}

fn fill_header(
    continuation: &mut [u8],
    program: &Pubkey,
    accounts: &[AccountInfo],
    request: Request,
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    frame: &[u8],
    context: &[u8],
) {
    continuation[..8].copy_from_slice(MAGIC);
    put_u32(continuation, H_VERSION, VERSION);
    put_u32(continuation, H_HEADER, HEADER_BYTES as u32);
    put_u32(continuation, H_TOTAL, CONTINUATION_BYTES as u32);
    put_u32(continuation, H_OPERATION, PREPARE as u32);
    put_u32(continuation, H_PHASE, PHASE_PREPARED);
    put_u32(continuation, H_CURSOR, 1);
    put_u32(continuation, H_STATE_LEN, request.state_len as u32);
    put_u32(continuation, H_STATE_BYTES, STATE_BYTES as u32);
    put_u32(continuation, H_MIRROR_BYTES, MIRROR_BYTES as u32);
    put_u32(continuation, H_CONTROLLER_BYTES, CONTROLLER_BYTES as u32);
    put_u32(continuation, H_FRAME_BYTES, FRAME_BYTES as u32);
    put_u32(
        continuation,
        H_RESOURCE_BYTES,
        resource_sbf::RI2_RESOURCE_IMAGE_BYTES as u32,
    );
    put_u32(
        continuation,
        H_WAD_BYTES,
        resource_sbf::WAD_IMAGE_BYTES as u32,
    );
    put_u32(continuation, H_OUTPUT_STATE_LEN, request.state_len as u32);
    put_u32(continuation, H_RELOC_OFFSET, RELOC_OFFSET as u32);
    put_u32(continuation, H_RELOC_COUNT, RELOC_COUNT as u32);
    set_hash(continuation, H_PROTOCOL, &protocol_hash());
    set_hash(continuation, H_PROGRAM, &program_hash(program));
    set_hash(continuation, H_SOURCE, &source_hash());
    set_hash(
        continuation,
        H_RESOURCE_HASH,
        &hash_bytes(&accounts[1].try_borrow_data().unwrap()),
    );
    set_hash(continuation, H_WAD_HASH, &WAD_HASH);
    set_hash(continuation, H_STATE_HASH, &hash_bytes(state));
    set_hash(continuation, H_MIRROR_HASH, &hash_bytes(mirror));
    set_hash(continuation, H_CONTROLLER_HASH, &hash_bytes(controller));
    set_hash(continuation, H_FRAME_HASH, &hash_bytes(frame));
    let root = root_hash(state, mirror, controller, frame);
    set_hash(continuation, H_PREVIOUS_ROOT, &root);
    set_hash(continuation, H_OUTPUT_ROOT, &root);
    set_hash(
        continuation,
        H_ACCOUNT_SET,
        &account_set_hash(program, accounts),
    );
    set_hash(
        continuation,
        H_AUTHORITY,
        accounts[AUTHORITY_ACCOUNT].key.as_ref().try_into().unwrap(),
    );
    set_hash(continuation, H_CONTEXT_ROOT, &context_root(context));
    set_hash(continuation, H_SOURCE_ABI, &abi_hash());
    sol_memcpy(
        &mut continuation[STATE_OFFSET..STATE_OFFSET + STATE_BYTES],
        state,
        STATE_BYTES,
    );
    sol_memcpy(
        &mut continuation[MIRROR_OFFSET..MIRROR_OFFSET + MIRROR_BYTES],
        mirror,
        MIRROR_BYTES,
    );
    sol_memcpy(
        &mut continuation[CONTROLLER_OFFSET..CONTROLLER_OFFSET + CONTROLLER_BYTES],
        controller,
        CONTROLLER_BYTES,
    );
    sol_memcpy(
        &mut continuation[FRAME_OFFSET..FRAME_OFFSET + FRAME_BYTES],
        frame,
        FRAME_BYTES,
    );
    write_relocs(continuation);
}

fn validate_header(
    continuation: &[u8],
    program: &Pubkey,
    accounts: &[AccountInfo],
    request: Request,
    phase: u32,
    context: &[u8],
    resource: &[u8],
    state: &[u8],
    mirror: &[u8],
    controller: &[u8],
    frame: &[u8],
) -> ProgramResult {
    if &continuation[..8] != MAGIC
        || u32_at(continuation, H_VERSION) != VERSION
        || u32_at(continuation, H_HEADER) as usize != HEADER_BYTES
        || u32_at(continuation, H_TOTAL) as usize != CONTINUATION_BYTES
        || u32_at(continuation, H_OPERATION)
            != if phase == PHASE_PREPARED {
                PREPARE as u32
            } else {
                STEP as u32
            }
        || u32_at(continuation, H_PHASE) != phase
        || u32_at(continuation, H_CURSOR) != request.cursor
        || u32_at(continuation, H_STATE_LEN) as usize != request.state_len
        || u32_at(continuation, H_STATE_BYTES) as usize != STATE_BYTES
        || u32_at(continuation, H_MIRROR_BYTES) as usize != MIRROR_BYTES
        || u32_at(continuation, H_CONTROLLER_BYTES) as usize != CONTROLLER_BYTES
        || u32_at(continuation, H_FRAME_BYTES) as usize != FRAME_BYTES
        || u32_at(continuation, H_RESOURCE_BYTES) as usize != resource_sbf::RI2_RESOURCE_IMAGE_BYTES
        || u32_at(continuation, H_WAD_BYTES) as usize != resource_sbf::WAD_IMAGE_BYTES
        || u32_at(continuation, H_OUTPUT_STATE_LEN) as usize != request.state_len
        || u32_at(continuation, H_RELOC_OFFSET) as usize != RELOC_OFFSET
        || u32_at(continuation, H_RELOC_COUNT) as usize != RELOC_COUNT
        || &continuation[H_PROTOCOL..H_PROTOCOL + 32] != protocol_hash()
        || &continuation[H_PROGRAM..H_PROGRAM + 32] != program_hash(program)
        || &continuation[H_SOURCE..H_SOURCE + 32] != source_hash()
        || &continuation[H_RESOURCE_HASH..H_RESOURCE_HASH + 32] != hash_bytes(resource)
        || &continuation[H_WAD_HASH..H_WAD_HASH + 32] != WAD_HASH
        || &continuation[H_STATE_HASH..H_STATE_HASH + 32] != hash_bytes(state)
        || &continuation[H_MIRROR_HASH..H_MIRROR_HASH + 32] != hash_bytes(mirror)
        || &continuation[H_CONTROLLER_HASH..H_CONTROLLER_HASH + 32] != hash_bytes(controller)
        || &continuation[H_FRAME_HASH..H_FRAME_HASH + 32] != hash_bytes(frame)
        || &continuation[H_PREVIOUS_ROOT..H_PREVIOUS_ROOT + 32]
            != root_hash(state, mirror, controller, frame)
        || &continuation[H_OUTPUT_ROOT..H_OUTPUT_ROOT + 32]
            != root_hash(state, mirror, controller, frame)
        || &continuation[H_ACCOUNT_SET..H_ACCOUNT_SET + 32] != account_set_hash(program, accounts)
        || &continuation[H_AUTHORITY..H_AUTHORITY + 32] != accounts[AUTHORITY_ACCOUNT].key.as_ref()
        || &continuation[H_CONTEXT_ROOT..H_CONTEXT_ROOT + 32] != context_root(context)
        || &continuation[H_SOURCE_ABI..H_SOURCE_ABI + 32] != abi_hash()
        || continuation[H_SOURCE_ABI + 32..HEADER_BYTES]
            .iter()
            .any(|&byte| byte != 0)
        || continuation[STATE_OFFSET..STATE_OFFSET + STATE_BYTES] != *state
        || continuation[MIRROR_OFFSET..MIRROR_OFFSET + MIRROR_BYTES] != *mirror
        || continuation[CONTROLLER_OFFSET..CONTROLLER_OFFSET + CONTROLLER_BYTES] != *controller
        || continuation[FRAME_OFFSET..FRAME_OFFSET + FRAME_BYTES] != *frame
    {
        return Err(ProgramError::Custom(242));
    }
    validate_relocs(continuation)
}

fn process_prepare(program: &Pubkey, accounts: &[AccountInfo], request: Request) -> ProgramResult {
    let mut context = accounts[0].try_borrow_mut_data()?;
    let resource = accounts[1].try_borrow_data()?;
    validate_resource(&resource)?;
    let mut wad_bytes = 0usize;
    for ordinal in 0..5 {
        let wad = accounts[2 + ordinal].try_borrow_data()?;
        validate_wad(&wad, ordinal)?;
        wad_bytes = wad_bytes
            .checked_add(u32_at(&wad, 24) as usize)
            .ok_or(ProgramError::InvalidAccountData)?;
    }
    if wad_bytes != resource_sbf::WAD_IMAGE_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    let state = accounts[7].try_borrow_data()?;
    let mirror = accounts[8].try_borrow_data()?;
    let controller = accounts[9].try_borrow_data()?;
    let frame = accounts[10].try_borrow_data()?;
    validate_state(&state, request.state_len)?;
    validate_mirror(&mirror)?;
    validate_controller(&controller, u32_at(&state, 20))?;
    if frame.iter().any(|&byte| byte != 0) {
        return Err(ProgramError::InvalidAccountData);
    }
    drop(resource);
    let mut continuation = accounts[CONTINUATION_ACCOUNT].try_borrow_mut_data()?;
    if continuation[..HEADER_BYTES].iter().any(|&byte| byte != 0) {
        return Err(ProgramError::Custom(241));
    }
    let root = root_hash(&state, &mirror, &controller, &frame);
    let accounts_root = account_set_hash(program, accounts);
    fill_checkpoint(&mut context, PHASE_PREPARED, 1, &root, &accounts_root);
    fill_header(
        &mut continuation,
        program,
        accounts,
        request,
        &state,
        &mirror,
        &controller,
        &frame,
        &context,
    );
    Ok(())
}

#[cfg(target_os = "solana")]
unsafe extern "C" {
    fn doom_engine_ex2_resource_sbf_ri2_split_preflight(
        context: *mut u8,
        resource: *const u8,
        resource_bytes: usize,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        arena_high_water: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_split_setup(
        context: *mut u8,
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
        arena_high_water: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_split_decode_preflight(
        context: *mut u8,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        state: *const u8,
        state_len: usize,
        controller: *const u8,
        controller_len: usize,
        arena_high_water: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_split_decode_prepare(
        context: *mut u8,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        state: *const u8,
        state_len: usize,
        controller: *const u8,
        controller_len: usize,
        arena_high_water: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_split_decode_parse(
        context: *mut u8,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        state: *const u8,
        state_len: usize,
        controller: *const u8,
        controller_len: usize,
        arena_high_water: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_split_finalize(
        context: *mut u8,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        state_out: *mut u8,
        state_cap: usize,
        state_out_len: *mut usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
        mirror_out_len: *mut usize,
        frame_out: *mut u8,
        frame_cap: usize,
        frame_out_len: *mut usize,
        arena_high_water: *mut usize,
    ) -> i32;
}

#[cfg(target_os = "solana")]
fn context_parts(context: &mut [u8]) -> Result<(*mut u8, *mut u8, usize), ProgramError> {
    let wad_start = resource_sbf::WAD_IMAGE_OFFSET;
    let wad_end = wad_start + resource_sbf::WAD_IMAGE_BYTES;
    let bootstrap_start = resource_sbf::BOOTSTRAP_OFFSET;
    let bootstrap_end = bootstrap_start + resource_sbf::BOOTSTRAP_BYTES;
    if wad_end > bootstrap_start || bootstrap_end > context.len() {
        return Err(ProgramError::InvalidAccountData);
    }
    let context_ptr = context.as_mut_ptr();
    let (_, context_tail) = context.split_at_mut(wad_start);
    let (_, after_wad) = context_tail.split_at_mut(resource_sbf::WAD_IMAGE_BYTES);
    let (_, bootstrap_tail) = after_wad.split_at_mut(bootstrap_start - wad_end);
    let bootstrap = &mut bootstrap_tail[..resource_sbf::BOOTSTRAP_BYTES];
    if (context_ptr as usize) % resource_sbf::ALLOC_ALIGNMENT != 0
        || (bootstrap.as_mut_ptr() as usize) % resource_sbf::ALLOC_ALIGNMENT != 0
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok((context_ptr, bootstrap.as_mut_ptr(), bootstrap.len()))
}

#[cfg(target_os = "solana")]
fn run_phase(
    accounts: &[AccountInfo],
    cursor: u32,
    state_len: usize,
    saved: &[u8],
) -> ProgramResult {
    let mut context = accounts[0].try_borrow_mut_data()?;
    let resource = accounts[1].try_borrow_data()?;
    let state = accounts[7].try_borrow_data()?;
    let mirror = accounts[8].try_borrow_data()?;
    let controller = accounts[9].try_borrow_data()?;
    let mut arena_high_water = 0usize;
    let (context_ptr, bootstrap_ptr, bootstrap_len) = context_parts(&mut context)?;
    let rc = if cursor == 1 {
        unsafe {
            doom_engine_ex2_resource_sbf_ri2_split_preflight(
                context_ptr,
                resource.as_ptr(),
                resource.len(),
                bootstrap_ptr,
                bootstrap_len,
                &mut arena_high_water,
            )
        }
    } else if cursor == 2 {
        let wad_start = resource_sbf::WAD_IMAGE_OFFSET;
        let wad_end = wad_start + resource_sbf::WAD_IMAGE_BYTES;
        let mut at = wad_start;
        for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
            let source = accounts[2 + ordinal].try_borrow_data()?;
            let payload_len = u32_at(&source, 24) as usize;
            sol_memcpy(
                &mut context[at..at + payload_len],
                &source
                    [resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + payload_len],
                payload_len,
            );
            at += payload_len;
        }
        if at != wad_end {
            return Err(ProgramError::InvalidAccountData);
        }
        unsafe {
            doom_engine_ex2_resource_sbf_ri2_split_setup(
                context_ptr,
                resource.as_ptr(),
                resource.len(),
                context[wad_start..wad_end].as_ptr(),
                resource_sbf::WAD_IMAGE_BYTES,
                bootstrap_ptr,
                bootstrap_len,
                state.as_ptr(),
                state_len,
                mirror.as_ptr(),
                mirror.len(),
                controller.as_ptr(),
                controller.len(),
                &mut arena_high_water,
            )
        }
    } else if cursor == 3 {
        unsafe {
            doom_engine_ex2_resource_sbf_ri2_split_decode_preflight(
                context_ptr,
                bootstrap_ptr,
                bootstrap_len,
                state.as_ptr(),
                state_len,
                controller.as_ptr(),
                controller.len(),
                &mut arena_high_water,
            )
        }
    } else if cursor == 4 {
        unsafe {
            doom_engine_ex2_resource_sbf_ri2_split_decode_prepare(
                context_ptr,
                bootstrap_ptr,
                bootstrap_len,
                state.as_ptr(),
                state_len,
                controller.as_ptr(),
                controller.len(),
                &mut arena_high_water,
            )
        }
    } else if cursor == 5 {
        unsafe {
            doom_engine_ex2_resource_sbf_ri2_split_decode_parse(
                context_ptr,
                bootstrap_ptr,
                bootstrap_len,
                state.as_ptr(),
                state_len,
                controller.as_ptr(),
                controller.len(),
                &mut arena_high_water,
            )
        }
    } else {
        drop(state);
        drop(mirror);
        drop(controller);
        let mut state_out = accounts[7].try_borrow_mut_data()?;
        let mut mirror_out = accounts[8].try_borrow_mut_data()?;
        let mut frame_out = accounts[10].try_borrow_mut_data()?;
        let mut state_out_len = 0usize;
        let mut mirror_out_len = 0usize;
        let mut frame_out_len = 0usize;
        let rc = unsafe {
            doom_engine_ex2_resource_sbf_ri2_split_finalize(
                context_ptr,
                bootstrap_ptr,
                bootstrap_len,
                state_out.as_mut_ptr(),
                state_out.len(),
                &mut state_out_len,
                mirror_out.as_mut_ptr(),
                mirror_out.len(),
                &mut mirror_out_len,
                frame_out.as_mut_ptr(),
                frame_out.len(),
                &mut frame_out_len,
                &mut arena_high_water,
            )
        };
        if rc == 0
            && (state_out_len != u32_at(saved, H_STATE_LEN) as usize
                || mirror_out_len != MIRROR_BYTES
                || frame_out_len != FRAME_BYTES
                || state_out[..STATE_BYTES] != saved[STATE_OFFSET..STATE_OFFSET + STATE_BYTES]
                || mirror_out[..MIRROR_BYTES] != saved[MIRROR_OFFSET..MIRROR_OFFSET + MIRROR_BYTES]
                || frame_out[..FRAME_BYTES] != saved[FRAME_OFFSET..FRAME_OFFSET + FRAME_BYTES])
        {
            return Err(ProgramError::Custom(243));
        }
        rc
    };
    if rc != 0 {
        return Err(ProgramError::Custom(rc as u32));
    }
    let _ = arena_high_water;
    Ok(())
}

#[cfg(not(target_os = "solana"))]
fn run_phase(
    _accounts: &[AccountInfo],
    _cursor: u32,
    _state_len: usize,
    _saved: &[u8],
) -> ProgramResult {
    Err(ProgramError::Custom(244))
}

fn process_step(program: &Pubkey, accounts: &[AccountInfo], request: Request) -> ProgramResult {
    let context = accounts[0].try_borrow_data()?;
    let resource = accounts[1].try_borrow_data()?;
    validate_resource(&resource)?;
    let mut wad_bytes = 0usize;
    for ordinal in 0..5 {
        let wad = accounts[2 + ordinal].try_borrow_data()?;
        validate_wad(&wad, ordinal)?;
        wad_bytes = wad_bytes
            .checked_add(u32_at(&wad, 24) as usize)
            .ok_or(ProgramError::InvalidAccountData)?;
    }
    if wad_bytes != resource_sbf::WAD_IMAGE_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    let state = accounts[7].try_borrow_data()?;
    let mirror = accounts[8].try_borrow_data()?;
    let controller = accounts[9].try_borrow_data()?;
    let frame = accounts[10].try_borrow_data()?;
    validate_state(&state, request.state_len)?;
    validate_mirror(&mirror)?;
    validate_controller(&controller, u32_at(&state, 20))?;
    if frame.iter().any(|&byte| byte != 0) {
        return Err(ProgramError::InvalidAccountData);
    }
    let expected_root = root_hash(&state, &mirror, &controller, &frame);
    let accounts_root = account_set_hash(program, accounts);
    let expected_phase = request.cursor;
    let phase = match expected_phase {
        1 => PHASE_PREPARED,
        2 => PHASE_PREFLIGHT,
        3 => PHASE_SETUP,
        4 => PHASE_DECODE_PREFLIGHT,
        5 => PHASE_DECODE_PREPARED,
        6 => PHASE_DECODED,
        _ => unreachable!(),
    };
    check_checkpoint(
        &context,
        phase,
        expected_phase,
        &expected_root,
        &accounts_root,
    )?;
    let continuation = accounts[CONTINUATION_ACCOUNT].try_borrow_data()?;
    validate_header(
        &continuation,
        program,
        accounts,
        request,
        phase,
        &context,
        &resource,
        &state,
        &mirror,
        &controller,
        &frame,
    )?;
    let saved = continuation.to_vec();
    drop(continuation);
    drop(frame);
    drop(controller);
    drop(mirror);
    drop(state);
    drop(resource);
    drop(context);
    run_phase(accounts, request.cursor, request.state_len, &saved)?;

    let next_phase = match request.cursor {
        1 => PHASE_PREFLIGHT,
        2 => PHASE_SETUP,
        3 => PHASE_DECODE_PREFLIGHT,
        4 => PHASE_DECODE_PREPARED,
        5 => PHASE_DECODED,
        6 => PHASE_PUBLISHED,
        _ => unreachable!(),
    };
    let next_cursor = request.cursor + 1;
    let mut context = accounts[0].try_borrow_mut_data()?;
    fill_checkpoint(
        &mut context,
        next_phase,
        next_cursor,
        &expected_root,
        &accounts_root,
    );
    let mut continuation = accounts[CONTINUATION_ACCOUNT].try_borrow_mut_data()?;
    put_u32(&mut continuation, H_OPERATION, STEP as u32);
    put_u32(&mut continuation, H_PHASE, next_phase);
    put_u32(&mut continuation, H_CURSOR, next_cursor);
    set_hash(&mut continuation, H_CONTEXT_ROOT, &context_root(&context));
    Ok(())
}

pub fn process(
    program: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let request = Request::decode(instruction_data)?;
    validate_accounts(program, accounts)?;
    match request.operation {
        PREPARE => process_prepare(program, accounts, request),
        STEP => process_step(program, accounts, request),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_preserves_cursor() {
        let request = Request {
            operation: STEP,
            state_len: 12_345,
            cursor: 6,
        };
        assert_eq!(Request::decode(&request.encode()), Ok(request));
    }

    #[test]
    fn request_rejects_out_of_range_cursor() {
        let request = Request {
            operation: STEP,
            state_len: 64,
            cursor: 7,
        };
        assert_eq!(
            Request::decode(&request.encode()),
            Err(ProgramError::InvalidInstructionData)
        );
    }
}
