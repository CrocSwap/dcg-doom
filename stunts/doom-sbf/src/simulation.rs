//! DOOM-SIM/1: persistent no-render simulation ticks.
//!
//! This is an additive successor to DOOM-E2R/4.  E2R/4 is still responsible
//! for constructing the real initialized context.  SIM1 only consumes that
//! published context: it validates a sealed CI3 command sequence, runs one
//! `G_Ticker`, derives ST1/MP1, and advances a small authenticated cursor.
//! There is deliberately no resource, WAD, framebuffer, or render account in
//! this account set.
//!
//! LEGACY: superseded by DOOM-SIM/2 (`sim2.rs`).  SIM/1 INIT is permissionless
//! (review F1) and ROLLBACK_TEST is a test-only opcode (F6), so SIM/1 is not
//! dispatched by the deployable `sim-transport` profile; it stays in the
//! legacy ctx-engine profile only to reproduce M1201.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, hash::hashv, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::resource_sbf;

pub const MAGIC: &[u8; 8] = b"DOOM-SIM";
pub const VERSION: u32 = 1;
pub const INIT: u8 = 1;
pub const TICK: u8 = 2;
/// Test-only transaction shape: execute one real tick, then return an error so
/// ProgramTest can prove transaction rollback of context, ST1, MP1, and SIM1.
pub const ROLLBACK_TEST: u8 = 3;
pub const INSTRUCTION_BYTES: usize = 112;
pub const ABI_DESCRIPTOR: &str =
    "DOOM-SIM/1|instruction=112|operations=INIT/1+TICK/2+ROLLBACK_TEST/3|accounts=6|context=DOOM-CT5-published|state=ST1+MP1|input=CI3-sealed|render=none";

pub const CONTEXT_ACCOUNT: usize = 0;
pub const STATE_ACCOUNT: usize = 1;
pub const MIRROR_ACCOUNT: usize = 2;
pub const INPUT_ACCOUNT: usize = 3;
pub const META_ACCOUNT: usize = 4;
pub const AUTHORITY_ACCOUNT: usize = 5;
pub const ACCOUNT_COUNT: usize = 6;

pub const META_BYTES: usize = 256;
pub const INPUT_HEADER_BYTES: usize = 64;
pub const MAX_TICS: usize = 128;
pub const INPUT_MAX_BYTES: usize = INPUT_HEADER_BYTES + MAX_TICS * 4;

const SIM_META_MAGIC: &[u8; 8] = b"SIM1META";
const CI3_MAGIC: &[u8; 8] = b"DOOM-CI3";
const DOMAIN_STATE: &[u8] = b"DOOM-SIM/1-STATE\0";
const DOMAIN_INPUT: &[u8] = b"DOOM-SIM/1-INPUT\0";
const DOMAIN_ACCOUNTS: &[u8] = b"DOOM-SIM/1-ACCOUNTS\0";
const DOMAIN_CONTEXT: &[u8] = b"DOOM-SIM/1-CONTEXT\0";

const SIM_PHASE_PUBLISHED: u32 = 8;
const SIM_CURSOR_PUBLISHED: u32 = 8;
const CHECKPOINT_BYTES: usize = 128;
const CHECKPOINT_OFFSET: usize = resource_sbf::CONTEXT_BYTES - CHECKPOINT_BYTES;

const I_VERSION: usize = 8;
const I_HEADER: usize = 12;
const I_TOTAL: usize = 16;
const I_FIRST_TIC: usize = 20;
const I_COUNT: usize = 24;
const I_COMMAND_BYTES: usize = 28;
const I_ROOT: usize = 32;

const M_VERSION: usize = 8;
const M_HEADER: usize = 12;
const M_TOTAL: usize = 16;
const M_LAST_TIC: usize = 20;
const M_SEQUENCE: usize = 24;
const M_STATE_LEN: usize = 32;
const M_INPUT_INDEX: usize = 36;
const M_INPUT_ROOT: usize = 40;
const M_STATE_ROOT: usize = 72;
const M_CONTEXT_ROOT: usize = 104;
const M_ACCOUNT_SET: usize = 136;
const M_AUTHORITY: usize = 168;
const M_RESERVED: usize = 200;

const E_EXPECTED_TIC: usize = 16;
const E_SEQUENCE: usize = 20;
const E_STATE_ROOT: usize = 28;
const E_INPUT_ROOT: usize = 60;
const E_INPUT_INDEX: usize = 92;

pub const REFUSE_SEQUENCE: u32 = 250;
pub const REFUSE_ROOT: u32 = 251;
pub const REFUSE_INPUT: u32 = 252;
pub const REFUSE_AUTHORITY: u32 = 253;
pub const REFUSE_ROLLBACK: u32 = 254;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub operation: u8,
    pub expected_tic: u32,
    pub sequence: u64,
    pub state_root: [u8; 32],
    pub input_root: [u8; 32],
    pub input_index: u32,
}

impl Request {
    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != INSTRUCTION_BYTES
            || &data[..8] != MAGIC
            || u32_at(data, 8) != VERSION
            || !matches!(data[12], INIT | TICK | ROLLBACK_TEST)
            || data[13..16].iter().any(|&byte| byte != 0)
            || data[96..].iter().any(|&byte| byte != 0)
        {
            return Err(ProgramError::InvalidInstructionData);
        }
        let mut state_root = [0; 32];
        state_root.copy_from_slice(&data[E_STATE_ROOT..E_STATE_ROOT + 32]);
        let mut input_root = [0; 32];
        input_root.copy_from_slice(&data[E_INPUT_ROOT..E_INPUT_ROOT + 32]);
        Ok(Self {
            operation: data[12],
            expected_tic: u32_at(data, E_EXPECTED_TIC),
            sequence: u64_at(data, E_SEQUENCE),
            state_root,
            input_root,
            input_index: u32_at(data, E_INPUT_INDEX),
        })
    }

    pub fn encode(self) -> [u8; INSTRUCTION_BYTES] {
        let mut out = [0; INSTRUCTION_BYTES];
        out[..8].copy_from_slice(MAGIC);
        put_u32(&mut out, 8, VERSION);
        out[12] = self.operation;
        put_u32(&mut out, E_EXPECTED_TIC, self.expected_tic);
        out[E_SEQUENCE..E_SEQUENCE + 8].copy_from_slice(&self.sequence.to_le_bytes());
        out[E_STATE_ROOT..E_STATE_ROOT + 32].copy_from_slice(&self.state_root);
        out[E_INPUT_ROOT..E_INPUT_ROOT + 32].copy_from_slice(&self.input_root);
        put_u32(&mut out, E_INPUT_INDEX, self.input_index);
        out
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputInfo {
    first_tic: u32,
    count: usize,
    root: [u8; 32],
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn state_root(state: &[u8], state_len: usize, mirror: &[u8]) -> [u8; 32] {
    hashv(&[DOMAIN_STATE, &state[..state_len], mirror]).to_bytes()
}

fn input_root(input: &[u8], total: usize) -> [u8; 32] {
    hashv(&[
        DOMAIN_INPUT,
        &input[..32],
        &input[INPUT_HEADER_BYTES..total],
    ])
    .to_bytes()
}

fn account_set_hash(program: &Pubkey, accounts: &[AccountInfo]) -> [u8; 32] {
    let mut keys = Vec::with_capacity(accounts.len() * 32);
    for account in accounts {
        keys.extend_from_slice(account.key.as_ref());
    }
    hashv(&[DOMAIN_ACCOUNTS, program.as_ref(), &keys]).to_bytes()
}

fn context_root(context: &[u8]) -> [u8; 32] {
    hashv(&[
        DOMAIN_CONTEXT,
        &context[..64],
        &context[CHECKPOINT_OFFSET..CHECKPOINT_OFFSET + CHECKPOINT_BYTES],
    ])
    .to_bytes()
}

fn validate_context_ready(context: &[u8]) -> ProgramResult {
    let checkpoint = &context[CHECKPOINT_OFFSET..CHECKPOINT_OFFSET + CHECKPOINT_BYTES];
    if &checkpoint[..8] != b"DOOM-CT5"
        || u32_at(checkpoint, 8) != 5
        || u32_at(checkpoint, 12) != SIM_PHASE_PUBLISHED
        || u32_at(checkpoint, 16) != SIM_CURSOR_PUBLISHED
        || u32_at(checkpoint, 84) != 0
        || u32_at(checkpoint, 88) as usize != CHECKPOINT_OFFSET
    {
        return Err(ProgramError::Custom(REFUSE_SEQUENCE));
    }
    Ok(())
}

fn validate_accounts(program: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    if accounts.len() != ACCOUNT_COUNT {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let expected = [
        resource_sbf::CONTEXT_BYTES,
        resource_sbf::STATE_MAX_BYTES,
        resource_sbf::MIRROR_BYTES,
        0,
        META_BYTES,
        0,
    ];
    for (index, account) in accounts.iter().enumerate() {
        if (index != AUTHORITY_ACCOUNT && account.is_signer)
            || account.is_writable
                != matches!(
                    index,
                    CONTEXT_ACCOUNT | STATE_ACCOUNT | MIRROR_ACCOUNT | META_ACCOUNT
                )
            || accounts[..index]
                .iter()
                .any(|previous| previous.key == account.key)
        {
            return Err(ProgramError::InvalidAccountData);
        }
        if index != AUTHORITY_ACCOUNT && account.owner != program {
            return Err(ProgramError::InvalidAccountData);
        }
        if index != INPUT_ACCOUNT
            && index != AUTHORITY_ACCOUNT
            && account.data_len() != expected[index]
        {
            return Err(ProgramError::InvalidAccountData);
        }
    }
    let input = &accounts[INPUT_ACCOUNT];
    if input.owner != program
        || input.data_len() < INPUT_HEADER_BYTES
        || input.data_len() > INPUT_MAX_BYTES
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let authority = &accounts[AUTHORITY_ACCOUNT];
    if !authority.is_signer || authority.is_writable {
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}

fn validate_input(input: &[u8]) -> Result<InputInfo, ProgramError> {
    if input.len() < INPUT_HEADER_BYTES
        || &input[..8] != CI3_MAGIC
        || u32_at(input, I_VERSION) != 1
        || u32_at(input, I_HEADER) as usize != INPUT_HEADER_BYTES
        || u32_at(input, I_TOTAL) as usize != input.len()
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let count = u32_at(input, I_COUNT) as usize;
    let command_bytes = u32_at(input, I_COMMAND_BYTES) as usize;
    if !(1..=MAX_TICS).contains(&count)
        || command_bytes != count * 4
        || INPUT_HEADER_BYTES + command_bytes != input.len()
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let root = input_root(input, input.len());
    if input[I_ROOT..I_ROOT + 32] != root {
        return Err(ProgramError::Custom(REFUSE_INPUT));
    }
    Ok(InputInfo {
        first_tic: u32_at(input, I_FIRST_TIC),
        count,
        root,
    })
}

fn validate_state(state: &[u8], state_len: usize) -> ProgramResult {
    if !(64..=resource_sbf::STATE_MAX_BYTES).contains(&state_len)
        || state_len > state.len()
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
    if mirror.len() != resource_sbf::MIRROR_BYTES
        || &mirror[..8] != b"DOOM-MP1"
        || u32_at(mirror, 8) != 1
        || u32_at(mirror, 12) != 4096
        || u32_at(mirror, 16) as usize != resource_sbf::MIRROR_BYTES
        || u32_at(mirror, 20) != 160
        || u32_at(mirror, 24) != 384
        || u32_at(mirror, 28) > 384
        || mirror[68..4096].iter().any(|&byte| byte != 0)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn meta_zero(meta: &[u8]) -> bool {
    meta.iter().all(|&byte| byte == 0)
}

fn validate_meta(
    meta: &[u8],
    input: &InputInfo,
    state_len: usize,
    state_root_expected: &[u8; 32],
    context_root_expected: &[u8; 32],
    account_set: &[u8; 32],
    authority: &Pubkey,
) -> ProgramResult {
    if &meta[..8] != SIM_META_MAGIC
        || u32_at(meta, M_VERSION) != VERSION
        || u32_at(meta, M_HEADER) != 64
        || u32_at(meta, M_TOTAL) as usize != META_BYTES
        || u32_at(meta, M_STATE_LEN) as usize != state_len
        || &meta[M_INPUT_ROOT..M_INPUT_ROOT + 32] != input.root
        || &meta[M_STATE_ROOT..M_STATE_ROOT + 32] != state_root_expected
        || &meta[M_CONTEXT_ROOT..M_CONTEXT_ROOT + 32] != context_root_expected
        || &meta[M_ACCOUNT_SET..M_ACCOUNT_SET + 32] != account_set
        || &meta[M_AUTHORITY..M_AUTHORITY + 32] != authority.as_ref()
        || meta[M_RESERVED..].iter().any(|&byte| byte != 0)
    {
        return Err(ProgramError::Custom(REFUSE_ROOT));
    }
    Ok(())
}

fn fill_meta(
    meta: &mut [u8],
    last_tic: u32,
    sequence: u64,
    state_len: usize,
    input_index: usize,
    input_root_value: &[u8; 32],
    state_root_value: &[u8; 32],
    context_root_value: &[u8; 32],
    account_set: &[u8; 32],
    authority: &Pubkey,
) {
    meta.fill(0);
    meta[..8].copy_from_slice(SIM_META_MAGIC);
    put_u32(meta, M_VERSION, VERSION);
    put_u32(meta, M_HEADER, 64);
    put_u32(meta, M_TOTAL, META_BYTES as u32);
    put_u32(meta, M_LAST_TIC, last_tic);
    put_u64(meta, M_SEQUENCE, sequence);
    put_u32(meta, M_STATE_LEN, state_len as u32);
    put_u32(meta, M_INPUT_INDEX, input_index as u32);
    meta[M_INPUT_ROOT..M_INPUT_ROOT + 32].copy_from_slice(input_root_value);
    meta[M_STATE_ROOT..M_STATE_ROOT + 32].copy_from_slice(state_root_value);
    meta[M_CONTEXT_ROOT..M_CONTEXT_ROOT + 32].copy_from_slice(context_root_value);
    meta[M_ACCOUNT_SET..M_ACCOUNT_SET + 32].copy_from_slice(account_set);
    meta[M_AUTHORITY..M_AUTHORITY + 32].copy_from_slice(authority.as_ref());
}

#[cfg(target_os = "solana")]
unsafe extern "C" {
    fn doom_sbf_sim_tick(
        context: *mut u8,
        command: *const u8,
        command_len: usize,
        state_out: *mut u8,
        state_cap: usize,
        state_out_len: *mut usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
        mirror_out_len: *mut usize,
    ) -> i32;
}

fn process_init(program: &Pubkey, accounts: &[AccountInfo], request: Request) -> ProgramResult {
    let context = accounts[CONTEXT_ACCOUNT].try_borrow_data()?;
    validate_context_ready(&context)?;
    let input = accounts[INPUT_ACCOUNT].try_borrow_data()?;
    let input_info = validate_input(&input)?;
    let state = accounts[STATE_ACCOUNT].try_borrow_data()?;
    let state_len = u32_at(&state, 16) as usize;
    validate_state(&state, state_len)?;
    let mirror = accounts[MIRROR_ACCOUNT].try_borrow_data()?;
    validate_mirror(&mirror)?;
    if meta_zero(&accounts[META_ACCOUNT].try_borrow_data()?)
        && request.expected_tic == u32_at(&state, 20)
        && input_info.first_tic == request.expected_tic
        && request.input_index == 0
        && request.sequence == 0
        && request.input_root == input_info.root
        && request.state_root == state_root(&state, state_len, &mirror)
    {
        let account_set = account_set_hash(program, accounts);
        let context_commitment = context_root(&context);
        drop(mirror);
        drop(state);
        drop(input);
        drop(context);
        let mut meta = accounts[META_ACCOUNT].try_borrow_mut_data()?;
        fill_meta(
            &mut meta,
            request.expected_tic,
            0,
            state_len,
            0,
            &input_info.root,
            &request.state_root,
            &context_commitment,
            &account_set,
            accounts[AUTHORITY_ACCOUNT].key,
        );
        return Ok(());
    }
    Err(ProgramError::Custom(REFUSE_SEQUENCE))
}

#[cfg(target_os = "solana")]
fn run_c_tick(
    context: &mut [u8],
    command: &[u8],
    state: &mut [u8],
    mirror: &mut [u8],
) -> Result<(usize, usize), ProgramError> {
    let mut state_len = 0usize;
    let mut mirror_len = 0usize;
    let rc = unsafe {
        doom_sbf_sim_tick(
            context.as_mut_ptr(),
            command.as_ptr(),
            command.len(),
            state.as_mut_ptr(),
            state.len(),
            &mut state_len,
            mirror.as_mut_ptr(),
            mirror.len(),
            &mut mirror_len,
        )
    };
    if rc != 0 {
        return Err(ProgramError::Custom(rc as u32));
    }
    Ok((state_len, mirror_len))
}

#[cfg(not(target_os = "solana"))]
fn run_c_tick(
    _context: &mut [u8],
    _command: &[u8],
    _state: &mut [u8],
    _mirror: &mut [u8],
) -> Result<(usize, usize), ProgramError> {
    Err(ProgramError::Custom(244))
}

fn process_tick(
    program: &Pubkey,
    accounts: &[AccountInfo],
    request: Request,
    rollback: bool,
) -> ProgramResult {
    crate::diagnostic_phase(800);
    let context = accounts[CONTEXT_ACCOUNT].try_borrow_data()?;
    validate_context_ready(&context)?;
    crate::diagnostic_phase(801);
    let input = accounts[INPUT_ACCOUNT].try_borrow_data()?;
    let input_info = validate_input(&input)?;
    crate::diagnostic_phase(802);
    let state = accounts[STATE_ACCOUNT].try_borrow_data()?;
    let mirror = accounts[MIRROR_ACCOUNT].try_borrow_data()?;
    validate_mirror(&mirror)?;
    crate::diagnostic_phase(803);
    let state_len = u32_at(&state, 16) as usize;
    validate_state(&state, state_len)?;
    crate::diagnostic_phase(804);
    let actual_state_root = state_root(&state, state_len, &mirror);
    crate::diagnostic_phase(805);
    let account_set = account_set_hash(program, accounts);
    crate::diagnostic_phase(806);
    let context_commitment = context_root(&context);
    crate::diagnostic_phase(807);
    let meta = accounts[META_ACCOUNT].try_borrow_data()?;
    validate_meta(
        &meta,
        &input_info,
        state_len,
        &actual_state_root,
        &context_commitment,
        &account_set,
        accounts[AUTHORITY_ACCOUNT].key,
    )?;
    if request.expected_tic != u32_at(&meta, M_LAST_TIC)
        || request.sequence != u64_at(&meta, M_SEQUENCE)
        || request.input_index != u32_at(&meta, M_INPUT_INDEX)
    {
        return Err(ProgramError::Custom(REFUSE_SEQUENCE));
    }
    if request.state_root != actual_state_root
        || request.state_root != meta[M_STATE_ROOT..M_STATE_ROOT + 32]
        || request.input_root != input_info.root
        || request.input_root != meta[M_INPUT_ROOT..M_INPUT_ROOT + 32]
        || request.input_index as usize >= input_info.count
        || input_info.first_tic.checked_add(request.input_index) != Some(request.expected_tic)
    {
        return Err(ProgramError::Custom(REFUSE_ROOT));
    }
    crate::diagnostic_phase(808);
    let command_start = INPUT_HEADER_BYTES + request.input_index as usize * 4;
    let mut command = [0u8; 4];
    command.copy_from_slice(&input[command_start..command_start + 4]);
    drop(meta);
    drop(mirror);
    drop(state);
    drop(input);
    drop(context);

    let mut context = accounts[CONTEXT_ACCOUNT].try_borrow_mut_data()?;
    let mut state = accounts[STATE_ACCOUNT].try_borrow_mut_data()?;
    let mut mirror = accounts[MIRROR_ACCOUNT].try_borrow_mut_data()?;
    crate::diagnostic_phase(809);
    let (new_state_len, new_mirror_len) =
        run_c_tick(&mut context, &command, &mut state, &mut mirror)?;
    crate::diagnostic_phase(820);
    if rollback {
        return Err(ProgramError::Custom(REFUSE_ROLLBACK));
    }
    validate_state(&state, new_state_len)?;
    crate::diagnostic_phase(821);
    validate_mirror(&mirror)?;
    crate::diagnostic_phase(822);
    if new_mirror_len != resource_sbf::MIRROR_BYTES
        || u32_at(&state, 20) != request.expected_tic.wrapping_add(1)
    {
        return Err(ProgramError::Custom(REFUSE_ROOT));
    }
    let new_state_root = state_root(&state, new_state_len, &mirror);
    crate::diagnostic_phase(824);
    let new_context_root = context_root(&context);
    crate::diagnostic_phase(825);
    let next_tic = request
        .expected_tic
        .checked_add(1)
        .ok_or(ProgramError::Custom(REFUSE_SEQUENCE))?;
    let next_sequence = request
        .sequence
        .checked_add(1)
        .ok_or(ProgramError::Custom(REFUSE_SEQUENCE))?;
    let next_index = (request.input_index as usize)
        .checked_add(1)
        .ok_or(ProgramError::Custom(REFUSE_SEQUENCE))?;
    drop(mirror);
    drop(state);
    drop(context);
    let mut meta = accounts[META_ACCOUNT].try_borrow_mut_data()?;
    fill_meta(
        &mut meta,
        next_tic,
        next_sequence,
        new_state_len,
        next_index,
        &request.input_root,
        &new_state_root,
        &new_context_root,
        &account_set,
        accounts[AUTHORITY_ACCOUNT].key,
    );
    crate::diagnostic_phase(826);
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
        INIT => process_init(program, accounts, request),
        TICK => process_tick(program, accounts, request, false),
        ROLLBACK_TEST => process_tick(program, accounts, request, true),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_preserves_roots_and_sequence() {
        let request = Request {
            operation: TICK,
            expected_tic: 17,
            sequence: 18,
            state_root: [1; 32],
            input_root: [2; 32],
            input_index: 18,
        };
        assert_eq!(Request::decode(&request.encode()), Ok(request));
    }

    #[test]
    fn input_root_does_not_include_its_own_stored_field() {
        let mut input = vec![0; INPUT_HEADER_BYTES + 4];
        input[..8].copy_from_slice(CI3_MAGIC);
        put_u32(&mut input, I_VERSION, 1);
        put_u32(&mut input, I_HEADER, INPUT_HEADER_BYTES as u32);
        let input_len = input.len() as u32;
        put_u32(&mut input, I_TOTAL, input_len);
        put_u32(&mut input, I_COUNT, 1);
        put_u32(&mut input, I_COMMAND_BYTES, 4);
        input[INPUT_HEADER_BYTES..].copy_from_slice(&[0, 1, 2, 3]);
        let root = input_root(&input, input.len());
        input[I_ROOT..I_ROOT + 32].copy_from_slice(&root);
        assert_eq!(validate_input(&input).unwrap().root, root);
    }
}
