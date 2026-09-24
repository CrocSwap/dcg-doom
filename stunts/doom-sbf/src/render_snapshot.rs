//! DOOM-RENDER-SNAPSHOT/2: bind one render snapshot to a committed SIM2 tic.
//!
//! The operation reads the existing SIM2 meta/ST1/MP1 accounts and writes a
//! separate context-derived snapshot PDA. SIM2 ADVANCE does not receive or
//! touch that PDA. The render path can therefore sample a stable frame without
//! adding a read or write dependency to the live tic instruction.

use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    hash::hashv,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction, system_program,
    sysvar::Sysvar,
};

use crate::{ex2, resource_sbf, sim2};

pub const MAGIC: &[u8; 8] = b"DOOM-RSN";
pub const VERSION: u32 = 2;
pub const OP_INIT: u8 = 0;
pub const OP_SNAPSHOT: u8 = 1;
pub const OP_RENDER: u8 = 2;
pub const INIT_BYTES: usize = 16;
pub const REQUEST_BYTES: usize = 52;
pub const STRIP_HEADER_BYTES: usize = 128;
pub const STRIP_PIXELS_BYTES: usize = 40 * 200;
pub const STRIP_ACCOUNT_BYTES: usize = STRIP_HEADER_BYTES + STRIP_PIXELS_BYTES;
pub const SNAPSHOT_SEED: &[u8] = b"DOOM-RENDER-SNAPSHOT/2";
pub const SCRATCH_SEED: &[u8] = b"DOOM-RENDER-SCRATCH/1";
pub const STRIP_SEED: &[u8] = b"DOOM-RENDER-STRIP/1";
pub const HEADER_BYTES: usize = 192;
pub const SNAPSHOT_BYTES: usize =
    HEADER_BYTES + resource_sbf::STATE_MAX_BYTES + resource_sbf::MIRROR_BYTES;
pub const HASH_DOMAIN: &[u8] = b"DOOM-RENDER-SNAPSHOT/2\0";
pub const STRIP_HASH_DOMAIN: &[u8] = b"DOOM-RENDER-STRIP/1\0";
pub const ABI_DESCRIPTOR: &str = ex2::RENDER_SNAPSHOT_ABI_DESCRIPTOR;
pub const ABI_ID: [u8; 32] = ex2::RENDER_SNAPSHOT_ABI_ID;

const H_VERSION: usize = 8;
const H_HEADER_BYTES: usize = 12;
const H_TOTAL_BYTES: usize = 16;
const H_CURSOR: usize = 20;
const H_GAMETIC: usize = 24;
const H_STATE_LEN: usize = 28;
const H_STATE_ROOT: usize = 32;
const H_WAD_HASH: usize = 64;
const H_RESOURCE_ABI_ID: usize = 96;
const H_SNAPSHOT_HASH: usize = 128;
const H_RESERVED: usize = 160;
const H_ST1: usize = HEADER_BYTES;
const H_MP1: usize = H_ST1 + resource_sbf::STATE_MAX_BYTES;

pub const REFUSE_MALFORMED: u32 = 287;
pub const REFUSE_ACCOUNTS: u32 = 288;
pub const REFUSE_BINDING: u32 = 289;
pub const REFUSE_SNAPSHOT: u32 = 290;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotRequest {
    pub cursor: u32,
    pub state_root: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderRequest {
    pub strip_id: u8,
    pub cursor: u32,
    pub snapshot_hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StripView<'a> {
    pub strip_id: u32,
    pub cursor: u32,
    pub gametic: u32,
    pub snapshot_hash: [u8; 32],
    pub state_root: [u8; 32],
    pub strip_hash: [u8; 32],
    pub pixels: &'a [u8],
}

impl RenderRequest {
    pub fn encode(self) -> [u8; REQUEST_BYTES] {
        let mut out = [0u8; REQUEST_BYTES];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12] = OP_RENDER;
        out[13] = self.strip_id;
        out[16..20].copy_from_slice(&self.cursor.to_le_bytes());
        out[20..52].copy_from_slice(&self.snapshot_hash);
        out
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != REQUEST_BYTES
            || &data[..8] != MAGIC
            || u32_at(data, 8) != VERSION
            || data[12] != OP_RENDER
            || data[14..16] != [0, 0]
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(Self {
            strip_id: data[13],
            cursor: u32_at(data, 16),
            snapshot_hash: data[20..52].try_into().unwrap(),
        })
    }
}

impl SnapshotRequest {
    pub fn encode(self) -> [u8; REQUEST_BYTES] {
        let mut out = [0u8; REQUEST_BYTES];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12] = OP_SNAPSHOT;
        out[16..20].copy_from_slice(&self.cursor.to_le_bytes());
        out[20..52].copy_from_slice(&self.state_root);
        out
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != REQUEST_BYTES
            || &data[..8] != MAGIC
            || u32_at(data, 8) != VERSION
            || data[12] != OP_SNAPSHOT
            || data[13..16] != [0, 0, 0]
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(Self {
            cursor: u32_at(data, 16),
            state_root: data[20..52].try_into().unwrap(),
        })
    }
}

pub fn encode_init() -> [u8; INIT_BYTES] {
    let mut out = [0u8; INIT_BYTES];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12] = OP_INIT;
    out
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotView<'a> {
    pub cursor: u32,
    pub gametic: u32,
    pub state_len: usize,
    pub state_root: [u8; 32],
    pub wad_hash: [u8; 32],
    pub resource_abi_id: [u8; 32],
    pub snapshot_hash: [u8; 32],
    pub state: &'a [u8],
    pub mirror: &'a [u8],
}

pub fn snapshot_address(program: &Pubkey, context: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SNAPSHOT_SEED, context.as_ref()], program)
}

pub fn scratch_address(program: &Pubkey, snapshot: &Pubkey, strip_id: u8) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SCRATCH_SEED, snapshot.as_ref(), &[strip_id]], program)
}

pub fn strip_address(program: &Pubkey, snapshot: &Pubkey, strip_id: u8) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[STRIP_SEED, snapshot.as_ref(), &[strip_id]], program)
}

pub fn validate_strip(data: &[u8]) -> Result<StripView<'_>, ProgramError> {
    if data.len() != STRIP_ACCOUNT_BYTES
        || &data[..8] != b"DOOM-RS1"
        || u32_at(data, 8) != 1
        || u32_at(data, 12) as usize != STRIP_HEADER_BYTES
        || u32_at(data, 16) >= 8
        || u32_at(data, 28) as usize != STRIP_PIXELS_BYTES
    {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let strip_id = u32_at(data, 16);
    let cursor = u32_at(data, 20);
    let gametic = u32_at(data, 24);
    let snapshot_hash: [u8; 32] = data[32..64].try_into().unwrap();
    let state_root: [u8; 32] = data[64..96].try_into().unwrap();
    let expected_hash = hashv(&[
        STRIP_HASH_DOMAIN,
        &strip_id.to_le_bytes(),
        &cursor.to_le_bytes(),
        &gametic.to_le_bytes(),
        &snapshot_hash,
        &data[STRIP_HEADER_BYTES..],
    ])
    .to_bytes();
    if data[96..128] != expected_hash {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    Ok(StripView {
        strip_id,
        cursor,
        gametic,
        snapshot_hash,
        state_root,
        strip_hash: expected_hash,
        pixels: &data[STRIP_HEADER_BYTES..],
    })
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn refuse(code: u32) -> ProgramError {
    ProgramError::Custom(code)
}

fn distinct(accounts: &[AccountInfo]) -> bool {
    (0..accounts.len()).all(|left| {
        ((left + 1)..accounts.len()).all(|right| accounts[left].key != accounts[right].key)
    })
}

fn owned(program: &Pubkey, account: &AccountInfo, bytes: usize, writable: bool) -> bool {
    account.owner == program && account.data_len() == bytes && account.is_writable == writable
}

fn context_from_meta(program: &Pubkey, meta: &AccountInfo) -> Result<Pubkey, ProgramError> {
    if !owned(program, meta, sim2::META_BYTES, false) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let data = meta.try_borrow_data()?;
    if &data[..8] != b"SIM2META" || u32_at(&data, 8) != sim2::VERSION {
        return Err(refuse(REFUSE_BINDING));
    }
    let context = Pubkey::new_from_array(
        data[sim2::M_CONTEXT..sim2::M_CONTEXT + 32]
            .try_into()
            .unwrap(),
    );
    let (expected_meta, bump) = sim2::meta_address(program, &context);
    if meta.key != &expected_meta || data[sim2::M_BUMP] != bump {
        return Err(refuse(REFUSE_BINDING));
    }
    Ok(context)
}

fn initialize_account<'a>(
    program: &Pubkey,
    payer: &AccountInfo<'a>,
    snapshot: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    context: &Pubkey,
) -> ProgramResult {
    let (expected_snapshot, bump) = snapshot_address(program, context);
    if snapshot.key != &expected_snapshot || !snapshot.is_writable {
        return Err(refuse(REFUSE_BINDING));
    }
    if !payer.is_signer
        || !payer.is_writable
        || system.key != &system_program::id()
        || snapshot.owner != &system_program::id()
        || snapshot.data_len() != 0
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let rent_lamports = Rent::get()?.minimum_balance(SNAPSHOT_BYTES);
    let shortfall = rent_lamports.saturating_sub(snapshot.lamports());
    if shortfall != 0 {
        let transfer = system_instruction::transfer(payer.key, snapshot.key, shortfall);
        invoke(
            &transfer,
            &[payer.clone(), snapshot.clone(), system.clone()],
        )?;
    }
    let bump_seed = [bump];
    let signer_seeds: &[&[u8]] = &[SNAPSHOT_SEED, context.as_ref(), &bump_seed];
    let allocate = system_instruction::allocate(snapshot.key, SNAPSHOT_BYTES as u64);
    invoke_signed(
        &allocate,
        &[snapshot.clone(), system.clone()],
        &[signer_seeds],
    )?;
    let assign = system_instruction::assign(snapshot.key, program);
    invoke_signed(
        &assign,
        &[snapshot.clone(), system.clone()],
        &[signer_seeds],
    )
}

fn process_init(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() != INIT_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, 8) != VERSION
        || data[12] != OP_INIT
        || data[13..16] != [0, 0, 0]
    {
        return Err(refuse(REFUSE_MALFORMED));
    }
    let [payer, meta, snapshot, system] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let context = context_from_meta(program, meta)?;
    initialize_account(program, payer, snapshot, system, &context)
}

/// Hash an entire fixed-size image with its hash field treated as zero.
fn image_hash(data: &[u8]) -> Result<[u8; 32], ProgramError> {
    if data.len() != SNAPSHOT_BYTES {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let zero_hash = [0u8; 32];
    Ok(hashv(&[
        HASH_DOMAIN,
        &data[..H_SNAPSHOT_HASH],
        &zero_hash,
        &data[H_RESERVED..],
    ])
    .to_bytes())
}

/// Validate the canonical snapshot account before a renderer reads it.
pub fn validate_snapshot(data: &[u8]) -> Result<SnapshotView<'_>, ProgramError> {
    if data.len() != SNAPSHOT_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, H_VERSION) != VERSION
        || u32_at(data, H_HEADER_BYTES) as usize != HEADER_BYTES
        || u32_at(data, H_TOTAL_BYTES) as usize != SNAPSHOT_BYTES
        || data[H_RESERVED..H_ST1].iter().any(|&byte| byte != 0)
    {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let state_len = u32_at(data, H_STATE_LEN) as usize;
    if !(64..=resource_sbf::STATE_MAX_BYTES).contains(&state_len) {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let padded_state = &data[H_ST1..H_MP1];
    let state = &padded_state[..state_len];
    let mirror = &data[H_MP1..];
    if &state[..8] != b"DOOM-ST1"
        || u32_at(state, 8) != 0
        || u32_at(state, 12) != 64
        || u32_at(state, 16) as usize != state_len
        || u32_at(state, 20) != u32_at(data, H_GAMETIC)
        || mirror[36..68] != data[H_WAD_HASH..H_WAD_HASH + 32]
        || data[H_RESOURCE_ABI_ID..H_SNAPSHOT_HASH] != resource_sbf::RI2_ABI_ID
    {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let (_, root) = sim2::verified_state_root(padded_state, mirror)?;
    if root != data[H_STATE_ROOT..H_WAD_HASH] {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let expected_hash = image_hash(data)?;
    if data[H_SNAPSHOT_HASH..H_RESERVED] != expected_hash {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    Ok(SnapshotView {
        cursor: u32_at(data, H_CURSOR),
        gametic: u32_at(data, H_GAMETIC),
        state_len,
        state_root: data[H_STATE_ROOT..H_WAD_HASH].try_into().unwrap(),
        wad_hash: data[H_WAD_HASH..H_RESOURCE_ABI_ID].try_into().unwrap(),
        resource_abi_id: data[H_RESOURCE_ABI_ID..H_SNAPSHOT_HASH].try_into().unwrap(),
        snapshot_hash: data[H_SNAPSHOT_HASH..H_RESERVED].try_into().unwrap(),
        state,
        mirror,
    })
}

/// Construct a snapshot only after the caller has checked its SIM2 binding.
pub fn write_snapshot(
    out: &mut [u8],
    cursor: u32,
    state_root: &[u8; 32],
    state: &[u8],
    mirror: &[u8],
) -> ProgramResult {
    if out.len() != SNAPSHOT_BYTES {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let state_len = sim2::state_len_checked(state)?;
    let _mirror_used = sim2::mirror_used_checked(mirror)?;
    if state.len() != resource_sbf::STATE_MAX_BYTES
        || state_len > resource_sbf::STATE_MAX_BYTES
    {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    out.fill(0);
    out[..8].copy_from_slice(MAGIC);
    put_u32(out, H_VERSION, VERSION);
    put_u32(out, H_HEADER_BYTES, HEADER_BYTES as u32);
    put_u32(out, H_TOTAL_BYTES, SNAPSHOT_BYTES as u32);
    put_u32(out, H_CURSOR, cursor);
    put_u32(out, H_GAMETIC, u32_at(state, 20));
    put_u32(out, H_STATE_LEN, state_len as u32);
    out[H_STATE_ROOT..H_WAD_HASH].copy_from_slice(state_root);
    out[H_WAD_HASH..H_RESOURCE_ABI_ID].copy_from_slice(&mirror[36..68]);
    out[H_RESOURCE_ABI_ID..H_SNAPSHOT_HASH].copy_from_slice(&resource_sbf::RI2_ABI_ID);
    out[H_ST1..H_MP1].copy_from_slice(state);
    out[H_MP1..].copy_from_slice(mirror);
    let digest = image_hash(out)?;
    out[H_SNAPSHOT_HASH..H_RESERVED].copy_from_slice(&digest);
    Ok(())
}

fn process_snapshot(
    program: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let request = SnapshotRequest::decode(instruction_data)?;
    let [meta, state, mirror, snapshot] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if !distinct(accounts)
        || !owned(program, meta, sim2::META_BYTES, false)
        || !owned(program, state, resource_sbf::STATE_MAX_BYTES, false)
        || !owned(program, mirror, resource_sbf::MIRROR_BYTES, false)
        || !owned(program, snapshot, SNAPSHOT_BYTES, true)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let context = context_from_meta(program, meta)?;
    let (expected_snapshot, _) = snapshot_address(program, &context);
    if snapshot.key != &expected_snapshot {
        return Err(refuse(REFUSE_BINDING));
    }
    let meta_data = meta.try_borrow_data()?;
    let state_data = state.try_borrow_data()?;
    let mirror_data = mirror.try_borrow_data()?;
    let state_len = sim2::state_len_checked(&state_data)?;
    if state_len != u32_at(&meta_data, sim2::M_STATE_LEN) as usize {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    sim2::snapshot_check(
        &meta_data,
        &state_data,
        &mirror_data,
        state.key,
        mirror.key,
        request.cursor,
        &request.state_root,
    )?;
    let mut snapshot_data = snapshot.try_borrow_mut_data()?;
    write_snapshot(
        &mut snapshot_data,
        request.cursor,
        &request.state_root,
        &state_data,
        &mirror_data,
    )
}

#[cfg(all(feature = "ctx-engine", target_os = "solana"))]
unsafe extern "C" {
    fn doom_engine_ex2_resource_sbf_ri2_render_strip(
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
        strip_id: u32,
        strip_out: *mut u8,
        strip_cap: usize,
        arena_high_water: *mut usize,
    ) -> i32;
}

#[cfg(all(feature = "ctx-engine", target_os = "solana"))]
fn process_render(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    const SCRATCH: usize = 0;
    const RESOURCE: usize = 1;
    const WAD_FIRST: usize = 2;
    const SNAPSHOT: usize = 7;
    const STRIP: usize = 8;
    const ACCOUNT_COUNT: usize = 9;

    let request = RenderRequest::decode(data)?;
    if request.strip_id >= 8 || accounts.len() != ACCOUNT_COUNT {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if !distinct(accounts)
        || accounts.iter().any(|account| account.is_signer)
        || !owned(
            program,
            &accounts[RESOURCE],
            resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES,
            false,
        )
        || !owned(program, &accounts[SNAPSHOT], SNAPSHOT_BYTES, false)
        || !owned(
            program,
            &accounts[SCRATCH],
            resource_sbf::CONTEXT_BYTES,
            true,
        )
        || !owned(program, &accounts[STRIP], STRIP_ACCOUNT_BYTES, true)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        if !owned(
            program,
            &accounts[WAD_FIRST + ordinal],
            resource_sbf::WAD_ACCOUNT_BYTES,
            false,
        ) {
            return Err(refuse(REFUSE_ACCOUNTS));
        }
    }
    let (expected_scratch, _) = scratch_address(program, accounts[SNAPSHOT].key, request.strip_id);
    let (expected_strip, _) = strip_address(program, accounts[SNAPSHOT].key, request.strip_id);
    if accounts[SCRATCH].key != &expected_scratch || accounts[STRIP].key != &expected_strip {
        return Err(refuse(REFUSE_BINDING));
    }

    let zeroes = [0u8; 256];
    resource_sbf::validate_resource_header_ri2(
        &accounts[RESOURCE],
        resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES,
    )?;
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        resource_sbf::validate_wad_account(&accounts[WAD_FIRST + ordinal], ordinal, &zeroes)?;
    }
    let snapshot_data = accounts[SNAPSHOT].try_borrow_data()?;
    let snapshot = validate_snapshot(&snapshot_data)?;
    if snapshot.cursor != request.cursor || snapshot.snapshot_hash != request.snapshot_hash {
        return Err(refuse(REFUSE_BINDING));
    }

    let mut scratch_data = accounts[SCRATCH].try_borrow_mut_data()?;
    let wad_end = resource_sbf::WAD_IMAGE_OFFSET + resource_sbf::WAD_IMAGE_BYTES;
    let bootstrap_start = resource_sbf::BOOTSTRAP_OFFSET;
    let bootstrap_end = bootstrap_start + resource_sbf::BOOTSTRAP_BYTES;
    if bootstrap_end > scratch_data.len() || wad_end > bootstrap_start {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let mut wad_cursor = resource_sbf::WAD_IMAGE_OFFSET;
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let wad_data = accounts[WAD_FIRST + ordinal].try_borrow_data()?;
        let payload_len = u32_at(&wad_data, 24) as usize;
        let payload =
            &wad_data[resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + payload_len];
        scratch_data[wad_cursor..wad_cursor + payload_len].copy_from_slice(payload);
        wad_cursor += payload_len;
    }
    if wad_cursor != wad_end {
        return Err(refuse(REFUSE_ACCOUNTS));
    }

    let resource_data = accounts[RESOURCE].try_borrow_data()?;
    let mut strip_data = accounts[STRIP].try_borrow_mut_data()?;
    let (context, tail) = scratch_data.split_at_mut(resource_sbf::WAD_IMAGE_OFFSET);
    let (wad_image, tail) = tail.split_at_mut(resource_sbf::WAD_IMAGE_BYTES);
    let (padding, bootstrap_tail) = tail.split_at_mut(bootstrap_start - wad_end);
    let _ = padding;
    let bootstrap = &mut bootstrap_tail[..resource_sbf::BOOTSTRAP_BYTES];
    let mut arena_high_water = 0usize;
    let pixel_start = STRIP_HEADER_BYTES;
    let rc = unsafe {
        doom_engine_ex2_resource_sbf_ri2_render_strip(
            context.as_mut_ptr(),
            resource_data.as_ptr(),
            resource_data.len(),
            wad_image.as_ptr(),
            wad_image.len(),
            bootstrap.as_mut_ptr(),
            bootstrap.len(),
            snapshot.state.as_ptr(),
            snapshot.state.len(),
            snapshot.mirror.as_ptr(),
            snapshot.mirror.len(),
            u32::from(request.strip_id),
            strip_data[pixel_start..].as_mut_ptr(),
            STRIP_PIXELS_BYTES,
            &mut arena_high_water,
        )
    };
    if rc != 0 {
        return Err(ProgramError::Custom(rc as u32));
    }

    strip_data[..STRIP_HEADER_BYTES].fill(0);
    strip_data[..8].copy_from_slice(b"DOOM-RS1");
    put_u32(&mut strip_data, 8, 1);
    put_u32(&mut strip_data, 12, STRIP_HEADER_BYTES as u32);
    put_u32(&mut strip_data, 16, u32::from(request.strip_id));
    put_u32(&mut strip_data, 20, snapshot.cursor);
    put_u32(&mut strip_data, 24, snapshot.gametic);
    put_u32(&mut strip_data, 28, STRIP_PIXELS_BYTES as u32);
    strip_data[32..64].copy_from_slice(&snapshot.snapshot_hash);
    strip_data[64..96].copy_from_slice(&snapshot.state_root);
    let strip_hash = hashv(&[
        STRIP_HASH_DOMAIN,
        &u32::from(request.strip_id).to_le_bytes(),
        &snapshot.cursor.to_le_bytes(),
        &snapshot.gametic.to_le_bytes(),
        &snapshot.snapshot_hash,
        &strip_data[pixel_start..],
    ])
    .to_bytes();
    strip_data[96..128].copy_from_slice(&strip_hash);
    let _ = arena_high_water;
    Ok(())
}

#[cfg(not(all(feature = "ctx-engine", target_os = "solana")))]
fn process_render(_: &Pubkey, _: &[AccountInfo], _: &[u8]) -> ProgramResult {
    Err(ProgramError::InvalidInstructionData)
}

pub fn process(
    program: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.len() < 13
        || !instruction_data.starts_with(MAGIC)
        || u32_at(instruction_data, 8) != VERSION
    {
        return Err(refuse(REFUSE_MALFORMED));
    }
    match instruction_data[12] {
        OP_INIT => process_init(program, accounts, instruction_data),
        OP_SNAPSHOT => process_snapshot(program, accounts, instruction_data),
        OP_RENDER => process_render(program, accounts, instruction_data),
        _ => Err(refuse(REFUSE_MALFORMED)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_request_round_trips_crafted_fixture() {
        let request = SnapshotRequest {
            cursor: 0x1234_5678,
            state_root: [0x5a; 32],
        };
        let encoded = request.encode();
        assert_eq!(SnapshotRequest::decode(&encoded).unwrap(), request);
        assert_eq!(encoded.len(), REQUEST_BYTES);
    }

    #[test]
    fn render_request_round_trips_crafted_fixture() {
        let request = RenderRequest {
            strip_id: 7,
            cursor: 0x1234_5678,
            snapshot_hash: [0x5a; 32],
        };
        let encoded = request.encode();
        assert_eq!(RenderRequest::decode(&encoded).unwrap(), request);
        assert_eq!(encoded[14..16], [0, 0]);
    }

    #[test]
    fn snapshot_account_layout_is_bounded_crafted_fixture() {
        assert_eq!(SNAPSHOT_BYTES, 192_704);
        assert_eq!(
            HEADER_BYTES + resource_sbf::STATE_MAX_BYTES + resource_sbf::MIRROR_BYTES,
            SNAPSHOT_BYTES
        );
    }

    #[test]
    fn snapshot_image_round_trips_crafted_fixture() {
        let wad_hash = [
            0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9, 0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3,
            0xe2, 0x9c, 0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18, 0x16, 0xf6, 0x52, 0xa5,
            0x26, 0xca, 0xc7, 0x71,
        ];
        let mut state = vec![0; resource_sbf::STATE_MAX_BYTES];
        state[..8].copy_from_slice(b"DOOM-ST1");
        state[12..16].copy_from_slice(&64u32.to_le_bytes());
        state[16..20].copy_from_slice(&64u32.to_le_bytes());
        state[20..24].copy_from_slice(&17u32.to_le_bytes());
        state[32..64].fill(0xa5);

        let mut mirror = vec![0; resource_sbf::MIRROR_BYTES];
        mirror[..8].copy_from_slice(b"DOOM-MP1");
        mirror[8..12].copy_from_slice(&1u32.to_le_bytes());
        mirror[12..16].copy_from_slice(&4096u32.to_le_bytes());
        mirror[16..20].copy_from_slice(&(resource_sbf::MIRROR_BYTES as u32).to_le_bytes());
        mirror[20..24].copy_from_slice(&160u32.to_le_bytes());
        mirror[24..28].copy_from_slice(&384u32.to_le_bytes());
        mirror[36..68].copy_from_slice(&wad_hash);

        let mut snapshot = vec![0; SNAPSHOT_BYTES];
        let (_, root) = sim2::verified_state_root(&state, &mirror).unwrap();
        write_snapshot(&mut snapshot, 91, &root, &state, &mirror).unwrap();
        let view = validate_snapshot(&snapshot).unwrap();
        assert_eq!(view.cursor, 91);
        assert_eq!(view.gametic, 17);
        assert_eq!(view.state_len, 64);
        assert_eq!(view.state_root, root);
        assert_eq!(view.wad_hash, wad_hash);
        assert_eq!(view.resource_abi_id, resource_sbf::RI2_ABI_ID);
        assert_eq!(view.state, &state[..64]);
        assert_eq!(view.mirror, mirror);

        let mut wrong_wad_binding = snapshot.clone();
        wrong_wad_binding[H_WAD_HASH] ^= 1;
        let digest = image_hash(&wrong_wad_binding).unwrap();
        wrong_wad_binding[H_SNAPSHOT_HASH..H_RESERVED].copy_from_slice(&digest);
        assert!(validate_snapshot(&wrong_wad_binding).is_err());

        let mut wrong_root = snapshot.clone();
        wrong_root[H_STATE_ROOT] ^= 1;
        assert!(validate_snapshot(&wrong_root).is_err());

        let mut changed_actor_mirror = snapshot;
        changed_actor_mirror[H_MP1 + 4_096] ^= 1;
        assert!(validate_snapshot(&changed_actor_mirror).is_err());
    }

    #[test]
    fn strip_image_hashes_crafted_fixture() {
        let mut strip = vec![0u8; STRIP_ACCOUNT_BYTES];
        strip[..8].copy_from_slice(b"DOOM-RS1");
        put_u32(&mut strip, 8, 1);
        put_u32(&mut strip, 12, STRIP_HEADER_BYTES as u32);
        put_u32(&mut strip, 16, 4);
        put_u32(&mut strip, 20, 91);
        put_u32(&mut strip, 24, 17);
        put_u32(&mut strip, 28, STRIP_PIXELS_BYTES as u32);
        strip[32..64].copy_from_slice(&[0x31; 32]);
        strip[64..96].copy_from_slice(&[0x42; 32]);
        strip[STRIP_HEADER_BYTES..].fill(0x53);
        let digest = hashv(&[
            STRIP_HASH_DOMAIN,
            &4u32.to_le_bytes(),
            &91u32.to_le_bytes(),
            &17u32.to_le_bytes(),
            &[0x31; 32],
            &strip[STRIP_HEADER_BYTES..],
        ])
        .to_bytes();
        strip[96..128].copy_from_slice(&digest);
        let view = validate_strip(&strip).unwrap();
        assert_eq!(view.strip_id, 4);
        assert_eq!(view.cursor, 91);
        assert_eq!(view.gametic, 17);
        assert_eq!(view.state_root, [0x42; 32]);
        assert_eq!(view.pixels, &[0x53; STRIP_PIXELS_BYTES]);
        strip[STRIP_HEADER_BYTES + 2] ^= 1;
        assert!(validate_strip(&strip).is_err());
    }
}
