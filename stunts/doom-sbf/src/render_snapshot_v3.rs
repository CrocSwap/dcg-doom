//! DOOM-RENDER-SNAPSHOT/3: render-only dynamic state bound to a SIM2 tic.
//!
//! RSN/3 contains no ST1 or MP1 bytes. SNAPSHOT validates the committed SIM2
//! state, projects the renderer's dynamic inputs, and pins the source resource
//! image. SNAPSHOT-LIVE reads the same projection from an already-resident
//! E2R/5 and ADVANCE context. RENDER validates the RSN/3 digest and resource
//! pin before applying the compact payload.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, hash::hashv, program::invoke_signed,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, system_instruction, system_program,
    sysvar::Sysvar,
};

use crate::{ex2, resource_sbf, sim2, static_resource};

pub const MAGIC: &[u8; 8] = b"DOOM-RSN";
pub const VERSION: u32 = 3;
pub const OP_INIT: u8 = 0;
pub const OP_SNAPSHOT: u8 = 1;
pub const OP_RENDER: u8 = 2;
pub const OP_SNAPSHOT_LIVE: u8 = 3;
pub const OP_GROW: u8 = 4;
pub const OP_INIT_STRIP: u8 = 5;
pub const OP_SNAPSHOT_LIVE_LATEST: u8 = 6;
pub const OP_INIT_BUFFERED: u8 = 7;
pub const OP_SNAPSHOT_LIVE_BUFFERED: u8 = 8;
pub const OP_GROW_BUFFERED: u8 = 9;
pub const BUFFER_COUNT: u8 = 2;
pub const INIT_BYTES: usize = 16;
pub const REQUEST_BYTES: usize = 52;
pub const HEADER_BYTES: usize = 192;
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const SNAPSHOT_BYTES: usize = HEADER_BYTES + MAX_PAYLOAD_BYTES;
pub const SNAPSHOT_SEED: &[u8] = b"DOOM-RENDER-SNAPSHOT/3";
pub const BUFFERED_SNAPSHOT_SEED: &[u8] = b"DOOM-RENDER-SNAPSHOT-BUFFER/1";
pub const SCRATCH_SEED: &[u8] = b"DOOM-RENDER-SCRATCH/1";
pub const STRIP_SEED: &[u8] = b"DOOM-RENDER-STRIP/1";
pub const HASH_DOMAIN: &[u8] = b"DOOM-RENDER-SNAPSHOT/3\0";
pub const STRIP_HASH_DOMAIN: &[u8] = b"DOOM-RENDER-STRIP/1\0";
pub const STRIP_HEADER_BYTES: usize = 128;
pub const STRIP_PIXELS_BYTES: usize = 40 * 200;
pub const STRIP_ACCOUNT_BYTES: usize = STRIP_HEADER_BYTES + STRIP_PIXELS_BYTES;
const MAX_ACCOUNT_GROWTH: usize = 10_240;
pub const ABI_ID: [u8; 32] = ex2::RENDER_SNAPSHOT_V3_ABI_ID;
pub const ABI_DESCRIPTOR: &str = ex2::RENDER_SNAPSHOT_V3_ABI_DESCRIPTOR;
pub const BUFFERED_ABI_ID: [u8; 32] = [
    0x43, 0xb4, 0xbe, 0x16, 0xe6, 0xe5, 0xb4, 0x17, 0x2e, 0x43, 0x3a, 0x95, 0x5a, 0x0b, 0x8f, 0xc3,
    0x1f, 0xe9, 0x82, 0xd7, 0x7f, 0x7e, 0xa4, 0x56, 0x2b, 0xd5, 0xe3, 0xe2, 0x67, 0x42, 0x31, 0xf4,
];

const H_VERSION: usize = 8;
const H_HEADER_BYTES: usize = 12;
const H_TOTAL_BYTES: usize = 16;
const H_CURSOR: usize = 20;
const H_GAMETIC: usize = 24;
const H_PAYLOAD_BYTES: usize = 28;
const H_STATE_ROOT: usize = 32;
const H_WAD_HASH: usize = 64;
const H_RESOURCE_HASH: usize = 96;
const H_SNAPSHOT_HASH: usize = 128;
const H_ABI_ID: usize = 160;
const PAYLOAD_HEADER_BYTES: usize = 128;
const SECTOR_RECORD_BYTES: usize = 16;
const SIDE_RECORD_BYTES: usize = 16;
const MOB_RECORD_BYTES: usize = 52;
const PLAYER_SPRITE_RECORD_BYTES: usize = 16;
const RENDER_DATA_MAGIC: &[u8; 8] = b"R3-DATA\0";
const RENDER_DATA_VERSION: u32 = 1;

pub const REFUSE_MALFORMED: u32 = 297;
pub const REFUSE_ACCOUNTS: u32 = 298;
pub const REFUSE_BINDING: u32 = 299;
pub const REFUSE_SNAPSHOT: u32 = 300;

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
pub struct SnapshotView<'a> {
    pub cursor: u32,
    pub gametic: u32,
    pub payload: &'a [u8],
    pub state_root: [u8; 32],
    pub wad_hash: [u8; 32],
    pub resource_hash: [u8; 32],
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

impl SnapshotRequest {
    pub fn encode(self) -> [u8; REQUEST_BYTES] {
        encode_request(OP_SNAPSHOT, 0, self.cursor, &self.state_root)
    }

    pub fn encode_live(self) -> [u8; REQUEST_BYTES] {
        encode_request(OP_SNAPSHOT_LIVE, 0, self.cursor, &self.state_root)
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        validate_request(data, OP_SNAPSHOT)?;
        if data[13..16] != [0, 0, 0] {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(Self {
            cursor: u32_at(data, 16),
            state_root: data[20..52].try_into().unwrap(),
        })
    }

    pub fn decode_live(data: &[u8]) -> Result<Self, ProgramError> {
        validate_request(data, OP_SNAPSHOT_LIVE)?;
        if data[13..16] != [0, 0, 0] {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(Self {
            cursor: u32_at(data, 16),
            state_root: data[20..52].try_into().unwrap(),
        })
    }

    pub fn encode_live_latest() -> [u8; REQUEST_BYTES] {
        encode_request(OP_SNAPSHOT_LIVE_LATEST, 0, 0, &[0; 32])
    }

    pub fn decode_live_latest(data: &[u8]) -> Result<Self, ProgramError> {
        validate_request(data, OP_SNAPSHOT_LIVE_LATEST)?;
        if data[13..16] != [0, 0, 0] || u32_at(data, 16) != 0 || data[20..52] != [0; 32] {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(Self {
            cursor: 0,
            state_root: [0; 32],
        })
    }
}

impl RenderRequest {
    pub fn encode(self) -> [u8; REQUEST_BYTES] {
        encode_request(OP_RENDER, self.strip_id, self.cursor, &self.snapshot_hash)
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        validate_request(data, OP_RENDER)?;
        if data[14..16] != [0, 0] {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(Self {
            strip_id: data[13],
            cursor: u32_at(data, 16),
            snapshot_hash: data[20..52].try_into().unwrap(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferedSnapshotRequest {
    pub buffer: u8,
}

impl BufferedSnapshotRequest {
    pub fn encode(self) -> [u8; REQUEST_BYTES] {
        encode_request(OP_SNAPSHOT_LIVE_BUFFERED, self.buffer, 0, &[0; 32])
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        validate_request(data, OP_SNAPSHOT_LIVE_BUFFERED)?;
        if data[14..16] != [0, 0]
            || data[16..20] != [0; 4]
            || data[20..52] != [0; 32]
            || data[13] >= BUFFER_COUNT
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(Self { buffer: data[13] })
    }
}

fn encode_request(operation: u8, arg: u8, cursor: u32, digest: &[u8; 32]) -> [u8; REQUEST_BYTES] {
    let mut out = [0; REQUEST_BYTES];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12] = operation;
    out[13] = arg;
    out[16..20].copy_from_slice(&cursor.to_le_bytes());
    out[20..52].copy_from_slice(digest);
    out
}

fn validate_request(data: &[u8], operation: u8) -> ProgramResult {
    if data.len() != REQUEST_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, 8) != VERSION
        || data[12] != operation
    {
        return Err(refuse(REFUSE_MALFORMED));
    }
    Ok(())
}

pub fn encode_init() -> [u8; INIT_BYTES] {
    let mut out = [0; INIT_BYTES];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12] = OP_INIT;
    out
}

pub fn encode_grow() -> [u8; INIT_BYTES] {
    let mut out = [0; INIT_BYTES];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12] = OP_GROW;
    out
}

pub fn encode_init_buffered(buffer: u8) -> [u8; INIT_BYTES] {
    let mut out = [0; INIT_BYTES];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12] = OP_INIT_BUFFERED;
    out[13] = buffer;
    out
}

pub fn encode_grow_buffered(buffer: u8) -> [u8; INIT_BYTES] {
    let mut out = [0; INIT_BYTES];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12] = OP_GROW_BUFFERED;
    out[13] = buffer;
    out
}

pub fn encode_init_strip(strip_id: u8) -> [u8; INIT_BYTES] {
    let mut out = [0; INIT_BYTES];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12] = OP_INIT_STRIP;
    out[13] = strip_id;
    out
}

pub fn snapshot_address(program: &Pubkey, context: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SNAPSHOT_SEED, context.as_ref()], program)
}

pub fn buffered_snapshot_address(
    program: &Pubkey,
    context: &Pubkey,
    buffer: u8,
) -> Option<(Pubkey, u8)> {
    match buffer {
        0 => Some(snapshot_address(program, context)),
        1 => Some(Pubkey::find_program_address(
            &[BUFFERED_SNAPSHOT_SEED, context.as_ref(), &[buffer]],
            program,
        )),
        _ => None,
    }
}

pub fn scratch_address(program: &Pubkey, snapshot: &Pubkey, strip_id: u8) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SCRATCH_SEED, snapshot.as_ref(), &[strip_id]], program)
}

pub fn strip_address(program: &Pubkey, snapshot: &Pubkey, strip_id: u8) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[STRIP_SEED, snapshot.as_ref(), &[strip_id]], program)
}

fn image_hash(data: &[u8], payload_bytes: usize) -> Result<[u8; 32], ProgramError> {
    let end = HEADER_BYTES
        .checked_add(payload_bytes)
        .ok_or(refuse(REFUSE_SNAPSHOT))?;
    if data.len() != SNAPSHOT_BYTES || end > data.len() {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let zero_hash = [0; 32];
    Ok(hashv(&[
        HASH_DOMAIN,
        &data[..H_SNAPSHOT_HASH],
        &zero_hash,
        &data[H_ABI_ID..HEADER_BYTES],
        &data[HEADER_BYTES..end],
    ])
    .to_bytes())
}

fn payload_shape(payload: &[u8]) -> bool {
    if payload.len() < PAYLOAD_HEADER_BYTES
        || &payload[..8] != RENDER_DATA_MAGIC
        || u32_at(payload, 8) != RENDER_DATA_VERSION
    {
        return false;
    }
    let sectors = u32_at(payload, 12) as usize;
    let sides = u32_at(payload, 16) as usize;
    let textures = u32_at(payload, 20) as usize;
    let flats = u32_at(payload, 24) as usize;
    let mobjs = u32_at(payload, 28) as usize;
    let psprites = u32_at(payload, 32) as usize;
    if sectors > 8192
        || sides > 32768
        || textures > 8192
        || flats > 8192
        || mobjs > 1024
        || psprites > 16
    {
        return false;
    }
    let expected = PAYLOAD_HEADER_BYTES
        .checked_add(
            psprites
                .checked_mul(PLAYER_SPRITE_RECORD_BYTES)
                .unwrap_or(usize::MAX),
        )
        .and_then(|n| n.checked_add(flats.checked_mul(4)?))
        .and_then(|n| n.checked_add(textures.checked_mul(4)?))
        .and_then(|n| n.checked_add(sectors.checked_mul(SECTOR_RECORD_BYTES)?))
        .and_then(|n| n.checked_add(sides.checked_mul(SIDE_RECORD_BYTES)?))
        .and_then(|n| n.checked_add(mobjs.checked_mul(MOB_RECORD_BYTES)?));
    expected == Some(payload.len())
}

fn validate_snapshot_with_abi(
    data: &[u8],
    expected_abi: [u8; 32],
) -> Result<SnapshotView<'_>, ProgramError> {
    if data.len() != SNAPSHOT_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, H_VERSION) != VERSION
        || u32_at(data, H_HEADER_BYTES) as usize != HEADER_BYTES
        || u32_at(data, H_TOTAL_BYTES) as usize != SNAPSHOT_BYTES
        || data[H_ABI_ID..HEADER_BYTES] != expected_abi
    {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let payload_bytes = u32_at(data, H_PAYLOAD_BYTES) as usize;
    if payload_bytes > MAX_PAYLOAD_BYTES {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let payload_end = HEADER_BYTES + payload_bytes;
    let payload = &data[HEADER_BYTES..payload_end];
    if !payload_shape(payload) {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    let expected_hash = image_hash(data, payload_bytes)?;
    if data[H_SNAPSHOT_HASH..H_ABI_ID] != expected_hash {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    Ok(SnapshotView {
        cursor: u32_at(data, H_CURSOR),
        gametic: u32_at(data, H_GAMETIC),
        payload,
        state_root: data[H_STATE_ROOT..H_WAD_HASH].try_into().unwrap(),
        wad_hash: data[H_WAD_HASH..H_RESOURCE_HASH].try_into().unwrap(),
        resource_hash: data[H_RESOURCE_HASH..H_SNAPSHOT_HASH].try_into().unwrap(),
        snapshot_hash: expected_hash,
    })
}

pub fn validate_snapshot(data: &[u8]) -> Result<SnapshotView<'_>, ProgramError> {
    validate_snapshot_with_abi(data, ABI_ID)
}

pub fn validate_buffered_snapshot(data: &[u8]) -> Result<SnapshotView<'_>, ProgramError> {
    validate_snapshot_with_abi(data, BUFFERED_ABI_ID)
}

pub fn validate_buffered_snapshot_for(
    data: &[u8],
    buffer: u8,
) -> Result<SnapshotView<'_>, ProgramError> {
    validate_snapshot_with_abi(data, if buffer == 0 { ABI_ID } else { BUFFERED_ABI_ID })
}

pub fn write_snapshot_header(
    out: &mut [u8],
    cursor: u32,
    state_root: &[u8; 32],
    wad_hash: &[u8; 32],
    resource_hash: &[u8; 32],
    gametic: u32,
    payload_bytes: usize,
) -> ProgramResult {
    write_snapshot_header_with_abi(
        out,
        cursor,
        state_root,
        wad_hash,
        resource_hash,
        gametic,
        payload_bytes,
        ABI_ID,
    )
}

pub fn write_buffered_snapshot_header(
    out: &mut [u8],
    cursor: u32,
    state_root: &[u8; 32],
    wad_hash: &[u8; 32],
    resource_hash: &[u8; 32],
    gametic: u32,
    payload_bytes: usize,
) -> ProgramResult {
    write_buffered_snapshot_header_for(
        out,
        cursor,
        state_root,
        wad_hash,
        resource_hash,
        gametic,
        payload_bytes,
        1,
    )
}

pub fn write_buffered_snapshot_header_for(
    out: &mut [u8],
    cursor: u32,
    state_root: &[u8; 32],
    wad_hash: &[u8; 32],
    resource_hash: &[u8; 32],
    gametic: u32,
    payload_bytes: usize,
    buffer: u8,
) -> ProgramResult {
    write_snapshot_header_with_abi(
        out,
        cursor,
        state_root,
        wad_hash,
        resource_hash,
        gametic,
        payload_bytes,
        if buffer == 0 { ABI_ID } else { BUFFERED_ABI_ID },
    )
}

fn write_snapshot_header_with_abi(
    out: &mut [u8],
    cursor: u32,
    state_root: &[u8; 32],
    wad_hash: &[u8; 32],
    resource_hash: &[u8; 32],
    gametic: u32,
    payload_bytes: usize,
    abi_id: [u8; 32],
) -> ProgramResult {
    if out.len() != SNAPSHOT_BYTES
        || payload_bytes > MAX_PAYLOAD_BYTES
        || !payload_shape(&out[HEADER_BYTES..HEADER_BYTES + payload_bytes])
    {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    out[..HEADER_BYTES].fill(0);
    out[..8].copy_from_slice(MAGIC);
    put_u32(out, H_VERSION, VERSION);
    put_u32(out, H_HEADER_BYTES, HEADER_BYTES as u32);
    put_u32(out, H_TOTAL_BYTES, SNAPSHOT_BYTES as u32);
    put_u32(out, H_CURSOR, cursor);
    put_u32(out, H_GAMETIC, gametic);
    put_u32(out, H_PAYLOAD_BYTES, payload_bytes as u32);
    out[H_STATE_ROOT..H_WAD_HASH].copy_from_slice(state_root);
    out[H_WAD_HASH..H_RESOURCE_HASH].copy_from_slice(wad_hash);
    out[H_RESOURCE_HASH..H_SNAPSHOT_HASH].copy_from_slice(resource_hash);
    out[H_ABI_ID..HEADER_BYTES].copy_from_slice(&abi_id);
    let digest = image_hash(out, payload_bytes)?;
    out[H_SNAPSHOT_HASH..H_ABI_ID].copy_from_slice(&digest);
    Ok(())
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
    (0..accounts.len())
        .all(|a| ((a + 1)..accounts.len()).all(|b| accounts[a].key != accounts[b].key))
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
    let (expected, bump) = sim2::meta_address(program, &context);
    if meta.key != &expected || data[sim2::M_BUMP] != bump {
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
    let (expected, bump) = snapshot_address(program, context);
    if snapshot.key != &expected || !snapshot.is_writable {
        return Err(refuse(REFUSE_BINDING));
    }
    if !payer.is_signer || !payer.is_writable || system.key != &system_program::id() {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if snapshot.owner == program && snapshot.data_len() == 0 {
        return Ok(());
    }
    if snapshot.owner != &system_program::id() || snapshot.data_len() != 0 {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let rent_lamports = Rent::get()?.minimum_balance(SNAPSHOT_BYTES);
    let bump_seed = [bump];
    let signer_seeds: &[&[u8]] = &[SNAPSHOT_SEED, context.as_ref(), &bump_seed];
    invoke_signed(
        &system_instruction::create_account(payer.key, snapshot.key, rent_lamports, 0, program),
        &[payer.clone(), snapshot.clone(), system.clone()],
        &[signer_seeds],
    )
}

fn process_grow(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() != INIT_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, 8) != VERSION
        || data[12] != OP_GROW
        || data[13..16] != [0; 3]
    {
        return Err(refuse(REFUSE_MALFORMED));
    }
    let [snapshot, meta] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    let context = context_from_meta(program, meta)?;
    let (expected, _) = snapshot_address(program, &context);
    let current = snapshot.data_len();
    if snapshot.key != &expected
        || snapshot.owner != program
        || !snapshot.is_writable
        || current > SNAPSHOT_BYTES
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let next = current
        .saturating_add(MAX_ACCOUNT_GROWTH)
        .min(SNAPSHOT_BYTES);
    if next != current {
        snapshot.realloc(next, false)?;
    }
    Ok(())
}

fn initialize_buffered_account<'a>(
    program: &Pubkey,
    payer: &AccountInfo<'a>,
    snapshot: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    context: &Pubkey,
    buffer: u8,
) -> ProgramResult {
    let Some((expected, bump)) = buffered_snapshot_address(program, context, buffer) else {
        return Err(refuse(REFUSE_MALFORMED));
    };
    if snapshot.key != &expected || !snapshot.is_writable {
        return Err(refuse(REFUSE_BINDING));
    }
    if !payer.is_signer || !payer.is_writable || system.key != &system_program::id() {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if snapshot.owner == program && snapshot.data_len() == 0 {
        return Ok(());
    }
    if snapshot.owner != &system_program::id() || snapshot.data_len() != 0 {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let rent_lamports = Rent::get()?.minimum_balance(SNAPSHOT_BYTES);
    let bump_seed = [bump];
    let buffer_seed = [buffer];
    let signer_seeds: Vec<&[u8]> = if buffer == 0 {
        vec![SNAPSHOT_SEED, context.as_ref(), &bump_seed]
    } else {
        vec![BUFFERED_SNAPSHOT_SEED, context.as_ref(), &buffer_seed, &bump_seed]
    };
    invoke_signed(
        &system_instruction::create_account(payer.key, snapshot.key, rent_lamports, 0, program),
        &[payer.clone(), snapshot.clone(), system.clone()],
        &[signer_seeds.as_slice()],
    )
}

fn process_grow_buffered(
    program: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.len() != INIT_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, 8) != VERSION
        || data[12] != OP_GROW_BUFFERED
        || data[13] >= BUFFER_COUNT
        || data[14..16] != [0; 2]
    {
        return Err(refuse(REFUSE_MALFORMED));
    }
    let [snapshot, meta] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    let context = context_from_meta(program, meta)?;
    let Some((expected, _)) = buffered_snapshot_address(program, &context, data[13]) else {
        return Err(refuse(REFUSE_MALFORMED));
    };
    let current = snapshot.data_len();
    if snapshot.key != &expected
        || snapshot.owner != program
        || !snapshot.is_writable
        || current > SNAPSHOT_BYTES
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let next = current
        .saturating_add(MAX_ACCOUNT_GROWTH)
        .min(SNAPSHOT_BYTES);
    if next != current {
        snapshot.realloc(next, false)?;
    }
    Ok(())
}

fn process_init_buffered(
    program: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.len() != INIT_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, 8) != VERSION
        || data[12] != OP_INIT_BUFFERED
        || data[13] >= BUFFER_COUNT
        || data[14..16] != [0; 2]
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
    initialize_buffered_account(program, payer, snapshot, system, &context, data[13])
}

fn process_init_strip(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() != INIT_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, 8) != VERSION
        || data[12] != OP_INIT_STRIP
        || data[13] >= 8
        || data[14..16] != [0; 2]
    {
        return Err(refuse(REFUSE_MALFORMED));
    }
    let [strip, payer, snapshot, system] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if !distinct(accounts)
        || snapshot.owner != program
        || snapshot.data_len() != SNAPSHOT_BYTES
        || !payer.is_signer
        || !payer.is_writable
        || !strip.is_writable
        || system.key != &system_program::id()
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let strip_id = data[13];
    let (expected, bump) = strip_address(program, snapshot.key, strip_id);
    if strip.key != &expected {
        return Err(refuse(REFUSE_BINDING));
    }
    if strip.owner == program && strip.data_len() == STRIP_ACCOUNT_BYTES {
        return Ok(());
    }
    if strip.owner != &system_program::id() || strip.data_len() != 0 {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let rent = Rent::get()?.minimum_balance(STRIP_ACCOUNT_BYTES);
    let bump_seed = [bump];
    let seeds: &[&[u8]] = &[STRIP_SEED, snapshot.key.as_ref(), &[strip_id], &bump_seed];
    invoke_signed(
        &system_instruction::create_account(
            payer.key,
            strip.key,
            rent,
            STRIP_ACCOUNT_BYTES as u64,
            program,
        ),
        &[payer.clone(), strip.clone(), system.clone()],
        &[seeds],
    )
}

fn process_init(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() != INIT_BYTES
        || &data[..8] != MAGIC
        || u32_at(data, 8) != VERSION
        || data[12] != OP_INIT
        || data[13..16] != [0; 3]
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

#[cfg(all(feature = "ctx-engine", target_os = "solana"))]
unsafe extern "C" {
    fn doom_engine_ex2_resource_sbf_ri2_snapshot_rsn3(
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
        payload_out: *mut u8,
        payload_cap: usize,
        payload_len_out: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_snapshot_rsn3_live(
        context: *mut u8,
        payload_out: *mut u8,
        payload_cap: usize,
        payload_len_out: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_render_rsn3(
        context: *mut u8,
        resource: *const u8,
        resource_bytes: usize,
        wad_image: *const u8,
        wad_bytes: usize,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        payload: *const u8,
        payload_len: usize,
        strip_id: u32,
        strip_out: *mut u8,
        strip_cap: usize,
        arena_high_water: *mut usize,
    ) -> i32;
}

fn process_snapshot(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let request = SnapshotRequest::decode(data)?;
    const SCRATCH: usize = 0;
    const META: usize = 1;
    const STATE: usize = 2;
    const MIRROR: usize = 3;
    const RESOURCE: usize = 4;
    const WAD_FIRST: usize = 5;
    const SNAPSHOT: usize = 10;
    if accounts.len() != 11 || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if !owned(program, &accounts[META], sim2::META_BYTES, false)
        || !owned(
            program,
            &accounts[STATE],
            resource_sbf::STATE_MAX_BYTES,
            false,
        )
        || !owned(
            program,
            &accounts[MIRROR],
            resource_sbf::MIRROR_BYTES,
            false,
        )
        || !owned(
            program,
            &accounts[RESOURCE],
            resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES,
            false,
        )
        || !owned(
            program,
            &accounts[SCRATCH],
            resource_sbf::CONTEXT_BYTES,
            true,
        )
        || !owned(program, &accounts[SNAPSHOT], SNAPSHOT_BYTES, true)
        || (0..resource_sbf::WAD_ACCOUNT_COUNT).any(|i| {
            !owned(
                program,
                &accounts[WAD_FIRST + i],
                resource_sbf::WAD_ACCOUNT_BYTES,
                false,
            )
        })
        || accounts.iter().any(|a| a.is_signer)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let context = context_from_meta(program, &accounts[META])?;
    let (expected_snapshot, _) = snapshot_address(program, &context);
    let (expected_scratch, _) = scratch_address(program, accounts[SNAPSHOT].key, 0);
    if accounts[SNAPSHOT].key != &expected_snapshot || accounts[SCRATCH].key != &expected_scratch {
        return Err(refuse(REFUSE_BINDING));
    }
    resource_sbf::validate_resource_header_ri2(
        &accounts[RESOURCE],
        resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES,
    )?;
    let zeroes = [0u8; 256];
    for i in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        resource_sbf::validate_wad_account(&accounts[WAD_FIRST + i], i, &zeroes)?;
    }
    let meta_data = accounts[META].try_borrow_data()?;
    let state_data = accounts[STATE].try_borrow_data()?;
    let mirror_data = accounts[MIRROR].try_borrow_data()?;
    let state_len = sim2::state_len_checked(&state_data)?;
    if state_len != u32_at(&meta_data, sim2::M_STATE_LEN) as usize {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    sim2::snapshot_check(
        &meta_data,
        &state_data,
        &mirror_data,
        accounts[STATE].key,
        accounts[MIRROR].key,
        request.cursor,
        &request.state_root,
    )?;

    let mut scratch_data = accounts[SCRATCH].try_borrow_mut_data()?;
    let wad_end = resource_sbf::WAD_IMAGE_OFFSET + resource_sbf::WAD_IMAGE_BYTES;
    let bootstrap_start = resource_sbf::BOOTSTRAP_OFFSET;
    let bootstrap_end = bootstrap_start + resource_sbf::BOOTSTRAP_BYTES;
    if bootstrap_end > scratch_data.len() || wad_end > bootstrap_start {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let mut wad_cursor = resource_sbf::WAD_IMAGE_OFFSET;
    for i in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let wad_data = accounts[WAD_FIRST + i].try_borrow_data()?;
        let n = u32_at(&wad_data, 24) as usize;
        let end = resource_sbf::WAD_HEADER_BYTES
            .checked_add(n)
            .ok_or(refuse(REFUSE_ACCOUNTS))?;
        scratch_data[wad_cursor..wad_cursor + n]
            .copy_from_slice(&wad_data[resource_sbf::WAD_HEADER_BYTES..end]);
        wad_cursor += n;
    }
    if wad_cursor != wad_end {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let expected_wad_hash: [u8; 32] = mirror_data[36..68].try_into().unwrap();
    if hashv(&[&scratch_data[resource_sbf::WAD_IMAGE_OFFSET..wad_end]]).to_bytes()
        != expected_wad_hash
    {
        return Err(refuse(REFUSE_BINDING));
    }
    let resource_data = accounts[RESOURCE].try_borrow_data()?;
    let mut snapshot_data = accounts[SNAPSHOT].try_borrow_mut_data()?;
    let old_payload_bytes =
        if &snapshot_data[..8] == MAGIC && u32_at(&snapshot_data, H_VERSION) == VERSION {
            let prior = u32_at(&snapshot_data, H_PAYLOAD_BYTES) as usize;
            if prior > MAX_PAYLOAD_BYTES {
                return Err(refuse(REFUSE_SNAPSHOT));
            }
            prior
        } else if snapshot_data.iter().all(|b| *b == 0) {
            0
        } else {
            return Err(refuse(REFUSE_SNAPSHOT));
        };
    snapshot_data[..HEADER_BYTES].fill(0);
    snapshot_data[HEADER_BYTES..HEADER_BYTES + old_payload_bytes].fill(0);
    let (context_mem, tail) = scratch_data.split_at_mut(resource_sbf::WAD_IMAGE_OFFSET);
    let (wad_image, tail) = tail.split_at_mut(resource_sbf::WAD_IMAGE_BYTES);
    let (_, bootstrap_tail) = tail.split_at_mut(bootstrap_start - wad_end);
    let bootstrap = &mut bootstrap_tail[..resource_sbf::BOOTSTRAP_BYTES];
    let payload = &mut snapshot_data[HEADER_BYTES..];
    let mut payload_len = 0usize;
    #[cfg(all(feature = "ctx-engine", target_os = "solana"))]
    let rc = unsafe {
        doom_engine_ex2_resource_sbf_ri2_snapshot_rsn3(
            context_mem.as_mut_ptr(),
            resource_data.as_ptr(),
            resource_data.len(),
            wad_image.as_ptr(),
            wad_image.len(),
            bootstrap.as_mut_ptr(),
            bootstrap.len(),
            state_data.as_ptr(),
            state_len,
            mirror_data.as_ptr(),
            mirror_data.len(),
            payload.as_mut_ptr(),
            payload.len(),
            &mut payload_len,
        )
    };
    #[cfg(not(all(feature = "ctx-engine", target_os = "solana")))]
    let rc: i32 = -1;
    if rc != 0 {
        return Err(ProgramError::Custom(rc.unsigned_abs()));
    }
    let resource_hash = hashv(&[&resource_data]).to_bytes();
    let wad_hash: [u8; 32] = mirror_data[36..68].try_into().unwrap();
    write_snapshot_header(
        &mut snapshot_data,
        request.cursor,
        &request.state_root,
        &wad_hash,
        &resource_hash,
        u32_at(&state_data, 20),
        payload_len,
    )?;
    Ok(())
}

fn process_snapshot_live(
    program: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    latest: bool,
    buffer: Option<u8>,
) -> ProgramResult {
    let buffered_request = if let Some(buffer) = buffer {
        let request = BufferedSnapshotRequest::decode(data)?;
        if request.buffer != buffer {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Some(request)
    } else if latest {
        let _request = SnapshotRequest::decode_live_latest(data)?;
        None
    } else {
        let _request = SnapshotRequest::decode_live(data)?;
        None
    };
    const CONTEXT: usize = 0;
    const META: usize = 1;
    const STATE: usize = 2;
    const MIRROR: usize = 3;
    const STATIC: usize = 4;
    const SNAPSHOT: usize = 5;
    if accounts.len() != 6 || !distinct(accounts) || accounts.iter().any(|a| a.is_signer) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if !owned(
        program,
        &accounts[CONTEXT],
        resource_sbf::CONTEXT_BYTES,
        false,
    ) || !owned(program, &accounts[META], sim2::META_BYTES, false)
        || !owned(
            program,
            &accounts[STATE],
            resource_sbf::STATE_MAX_BYTES,
            false,
        )
        || !owned(
            program,
            &accounts[MIRROR],
            resource_sbf::MIRROR_BYTES,
            false,
        )
        || !owned(
            program,
            &accounts[STATIC],
            static_resource::ACCOUNT_BYTES,
            false,
        )
        || !owned(program, &accounts[SNAPSHOT], SNAPSHOT_BYTES, true)
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let (cursor, state_root) = if buffered_request.is_some() || latest {
        let meta_data = accounts[META].try_borrow_data()?;
        (
            u32_at(&meta_data, sim2::M_CURSOR),
            meta_data[sim2::M_STATE_ROOT..sim2::M_STATE_ROOT + 32]
                .try_into()
                .unwrap(),
        )
    } else {
        let request = SnapshotRequest::decode_live(data)?;
        (request.cursor, request.state_root)
    };
    sim2::validate_live_snapshot_state(
        program,
        &accounts[CONTEXT],
        &accounts[META],
        &accounts[STATE],
        &accounts[MIRROR],
        cursor,
        &state_root,
    )?;
    let context_key = accounts[CONTEXT].key;
    let expected_snapshot = if let Some(request) = buffered_request {
        let Some((expected, _)) = buffered_snapshot_address(program, context_key, request.buffer) else {
            return Err(refuse(REFUSE_MALFORMED));
        };
        expected
    } else {
        snapshot_address(program, context_key).0
    };
    if accounts[SNAPSHOT].key != &expected_snapshot {
        return Err(refuse(REFUSE_BINDING));
    }
    let (wad_hash, resource_hash) =
        static_resource::snapshot_binding_hashes(program, &accounts[STATIC])?;

    let state_data = accounts[STATE].try_borrow_data()?;
    let gametic = u32_at(&state_data, 20);
    let context_data = accounts[CONTEXT].try_borrow_data()?;
    let mut snapshot_data = accounts[SNAPSHOT].try_borrow_mut_data()?;
    if snapshot_data.len() != SNAPSHOT_BYTES {
        return Err(refuse(REFUSE_SNAPSHOT));
    }
    if snapshot_data[..HEADER_BYTES].iter().any(|byte| *byte != 0) {
        if let Some(request) = buffered_request {
            validate_buffered_snapshot_for(&snapshot_data, request.buffer)?;
        } else {
            validate_snapshot(&snapshot_data)?;
        }
    }
    let mut payload_len = 0usize;
    #[cfg(all(feature = "ctx-engine", target_os = "solana"))]
    let rc = unsafe {
        doom_engine_ex2_resource_sbf_ri2_snapshot_rsn3_live(
            context_data.as_ptr() as *mut u8,
            snapshot_data[HEADER_BYTES..].as_mut_ptr(),
            MAX_PAYLOAD_BYTES,
            &mut payload_len,
        )
    };
    #[cfg(not(all(feature = "ctx-engine", target_os = "solana")))]
    let rc: i32 = -1;
    if rc != 0 {
        return Err(ProgramError::Custom(rc.unsigned_abs()));
    }
    if let Some(request) = buffered_request {
        write_buffered_snapshot_header_for(
            &mut snapshot_data,
            cursor,
            &state_root,
            &wad_hash,
            &resource_hash,
            gametic,
            payload_len,
            request.buffer,
        )
    } else {
        write_snapshot_header(
            &mut snapshot_data,
            cursor,
            &state_root,
            &wad_hash,
            &resource_hash,
            gametic,
            payload_len,
        )
    }
}

fn process_render(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let request = RenderRequest::decode(data)?;
    const SCRATCH: usize = 0;
    const RESOURCE: usize = 1;
    const WAD_FIRST: usize = 2;
    const SNAPSHOT: usize = 7;
    const STRIP: usize = 8;
    if request.strip_id >= 8 || accounts.len() != 9 || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if accounts.iter().any(|a| a.is_signer)
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
        || (0..resource_sbf::WAD_ACCOUNT_COUNT).any(|i| {
            !owned(
                program,
                &accounts[WAD_FIRST + i],
                resource_sbf::WAD_ACCOUNT_BYTES,
                false,
            )
        })
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let (expected_scratch, _) = scratch_address(program, accounts[SNAPSHOT].key, request.strip_id);
    let (expected_strip, _) = strip_address(program, accounts[SNAPSHOT].key, request.strip_id);
    if accounts[SCRATCH].key != &expected_scratch || accounts[STRIP].key != &expected_strip {
        return Err(refuse(REFUSE_BINDING));
    }
    resource_sbf::validate_resource_header_ri2(
        &accounts[RESOURCE],
        resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES,
    )?;
    let zeroes = [0u8; 256];
    for i in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        resource_sbf::validate_wad_account(&accounts[WAD_FIRST + i], i, &zeroes)?;
    }
    let snapshot_data = accounts[SNAPSHOT].try_borrow_data()?;
    let snapshot = validate_snapshot(&snapshot_data)?;
    if snapshot.cursor != request.cursor || snapshot.snapshot_hash != request.snapshot_hash {
        return Err(refuse(REFUSE_BINDING));
    }
    let resource_data = accounts[RESOURCE].try_borrow_data()?;
    if hashv(&[&resource_data]).to_bytes() != snapshot.resource_hash {
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
    for i in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let wad_data = accounts[WAD_FIRST + i].try_borrow_data()?;
        let n = u32_at(&wad_data, 24) as usize;
        let end = resource_sbf::WAD_HEADER_BYTES
            .checked_add(n)
            .ok_or(refuse(REFUSE_ACCOUNTS))?;
        scratch_data[wad_cursor..wad_cursor + n]
            .copy_from_slice(&wad_data[resource_sbf::WAD_HEADER_BYTES..end]);
        wad_cursor += n;
    }
    if wad_cursor != wad_end {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if hashv(&[&scratch_data[resource_sbf::WAD_IMAGE_OFFSET..wad_end]]).to_bytes()
        != snapshot.wad_hash
    {
        return Err(refuse(REFUSE_BINDING));
    }
    let (context_mem, tail) = scratch_data.split_at_mut(resource_sbf::WAD_IMAGE_OFFSET);
    let (wad_image, tail) = tail.split_at_mut(resource_sbf::WAD_IMAGE_BYTES);
    let (_, bootstrap_tail) = tail.split_at_mut(bootstrap_start - wad_end);
    let bootstrap = &mut bootstrap_tail[..resource_sbf::BOOTSTRAP_BYTES];
    let mut strip_data = accounts[STRIP].try_borrow_mut_data()?;
    let mut arena_high_water = 0usize;
    #[cfg(all(feature = "ctx-engine", target_os = "solana"))]
    let rc = unsafe {
        doom_engine_ex2_resource_sbf_ri2_render_rsn3(
            context_mem.as_mut_ptr(),
            resource_data.as_ptr(),
            resource_data.len(),
            wad_image.as_ptr(),
            wad_image.len(),
            bootstrap.as_mut_ptr(),
            bootstrap.len(),
            snapshot.payload.as_ptr(),
            snapshot.payload.len(),
            u32::from(request.strip_id),
            strip_data[STRIP_HEADER_BYTES..].as_mut_ptr(),
            STRIP_PIXELS_BYTES,
            &mut arena_high_water,
        )
    };
    #[cfg(not(all(feature = "ctx-engine", target_os = "solana")))]
    let rc: i32 = -1;
    if rc != 0 {
        return Err(ProgramError::Custom(rc.unsigned_abs()));
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
        &strip_data[STRIP_HEADER_BYTES..],
    ])
    .to_bytes();
    strip_data[96..128].copy_from_slice(&strip_hash);
    let _ = arena_high_water;
    Ok(())
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
        OP_SNAPSHOT_LIVE => process_snapshot_live(program, accounts, instruction_data, false, None),
        OP_SNAPSHOT_LIVE_LATEST => process_snapshot_live(program, accounts, instruction_data, true, None),
        OP_SNAPSHOT_LIVE_BUFFERED => {
            process_snapshot_live(program, accounts, instruction_data, false, Some(instruction_data[13]))
        }
        OP_GROW => process_grow(program, accounts, instruction_data),
        OP_GROW_BUFFERED => process_grow_buffered(program, accounts, instruction_data),
        OP_INIT_STRIP => process_init_strip(program, accounts, instruction_data),
        OP_INIT_BUFFERED => process_init_buffered(program, accounts, instruction_data),
        _ => Err(refuse(REFUSE_MALFORMED)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let snap = SnapshotRequest {
            cursor: 42,
            state_root: [0x11; 32],
        };
        assert_eq!(SnapshotRequest::decode(&snap.encode()).unwrap(), snap);
        let render = RenderRequest {
            strip_id: 7,
            cursor: 42,
            snapshot_hash: [0x22; 32],
        };
        assert_eq!(RenderRequest::decode(&render.encode()).unwrap(), render);
    }

    #[test]
    fn live_snapshot_request_uses_a_distinct_operation_byte() {
        let request = SnapshotRequest {
            cursor: 42,
            state_root: [0x33; 32],
        };
        let encoded = request.encode_live();
        assert_eq!(encoded[12], OP_SNAPSHOT_LIVE);
        assert_eq!(SnapshotRequest::decode_live(&encoded).unwrap(), request);
        assert!(SnapshotRequest::decode(&encoded).is_err());
        assert!(SnapshotRequest::decode_live(&request.encode()).is_err());
    }

    #[test]
    fn latest_snapshot_request_has_zero_cursor_and_root() {
        let encoded = SnapshotRequest::encode_live_latest();
        assert_eq!(encoded[12], OP_SNAPSHOT_LIVE_LATEST);
        assert_eq!(u32_at(&encoded, 16), 0);
        assert_eq!(encoded[20..52], [0; 32]);
        assert_eq!(
            SnapshotRequest::decode_live_latest(&encoded)
                .unwrap()
                .cursor,
            0
        );
        assert!(SnapshotRequest::decode_live(&encoded).is_err());
    }

    #[test]
    fn buffered_snapshot_request_round_trips() {
        let encoded = BufferedSnapshotRequest { buffer: 1 }.encode();
        assert_eq!(encoded[12], OP_SNAPSHOT_LIVE_BUFFERED);
        assert_eq!(BufferedSnapshotRequest::decode(&encoded).unwrap().buffer, 1);
        let mut bad = encoded;
        bad[14] = 1;
        assert!(BufferedSnapshotRequest::decode(&bad).is_err());
    }

    #[test]
    fn buffered_snapshot_addresses_are_distinct() {
        let program = Pubkey::new_from_array([1; 32]);
        let context = Pubkey::new_from_array([2; 32]);
        let old = snapshot_address(&program, &context).0;
        let first = buffered_snapshot_address(&program, &context, 0).unwrap().0;
        let second = buffered_snapshot_address(&program, &context, 1).unwrap().0;
        assert_eq!(old, first);
        assert_ne!(old, second);
        assert_ne!(first, second);
        assert!(buffered_snapshot_address(&program, &context, 2).is_none());
    }

    #[test]
    fn account_provision_instructions_have_distinct_wires() {
        let grow = encode_grow();
        let strip = encode_init_strip(7);
        assert_eq!(&grow[..8], MAGIC);
        assert_eq!(u32_at(&grow, 8), VERSION);
        assert_eq!(grow[12], OP_GROW);
        assert_eq!(grow[13..16], [0; 3]);
        assert_eq!(strip[12], OP_INIT_STRIP);
        assert_eq!(strip[13], 7);
        assert_eq!(strip[14..16], [0; 2]);
        assert_eq!(encode_init().len(), INIT_BYTES);
    }

    #[test]
    fn snapshot_hash_and_bounds_round_trip_crafted_fixture() {
        let mut payload = vec![0; PAYLOAD_HEADER_BYTES];
        payload[..8].copy_from_slice(RENDER_DATA_MAGIC);
        put_u32(&mut payload, 8, RENDER_DATA_VERSION);
        let mut image = vec![0; SNAPSHOT_BYTES];
        image[HEADER_BYTES..HEADER_BYTES + payload.len()].copy_from_slice(&payload);
        write_snapshot_header(
            &mut image,
            1,
            &[3; 32],
            &[4; 32],
            &[5; 32],
            6,
            payload.len(),
        )
        .unwrap();
        let view = validate_snapshot(&image).unwrap();
        assert_eq!(view.cursor, 1);
        assert_eq!(view.gametic, 6);
        let mut bad = image;
        bad[HEADER_BYTES + 127] = 1;
        assert!(validate_snapshot(&bad).is_err());
    }
}
