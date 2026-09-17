//! Versioned Doom step-5 account grammar and bounded lifecycle.
//!
//! This is deliberately a bring-up machine, not a claim that the full Doom
//! playsim is already resident on SBF.  It consumes the account-image v1
//! layout produced by the host loader, validates the WAD directory and the
//! fixed E1M1 load order, and runs one deterministic fixed-point tic.  The
//! state transition is intentionally small while the source port is being
//! moved out of host globals; the old C FixedMul/FixedDiv spike supplies the
//! arithmetic work inside the tic.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, hash::hashv, program_error::ProgramError,
    pubkey::Pubkey, system_program,
};

pub const INSTRUCTION_MAGIC: &[u8; 8] = b"DOOM-INS";
pub const PROTOCOL_VERSION: u32 = 1;
pub const OP_INIT: u32 = 1;
pub const OP_LOAD_LEVEL: u32 = 2;
pub const OP_TIC: u32 = 3;

pub const INIT_BYTES: usize = 128;
pub const LOAD_LEVEL_BYTES: usize = 56;
pub const TIC_BYTES: usize = 92;

pub const CONFIG_BYTES: usize = 4_096;
pub const LEVEL_STATE_BYTES: usize = 131_072;
pub const MOBJ_POOL_BYTES: usize = 65_536;
pub const WAD_CHUNKS: usize = 5;
pub const WAD_ACCOUNT_BYTES: usize = 1_048_576;
pub const WAD_HEADER_BYTES: usize = 32;
pub const WAD_PAYLOAD_BYTES: usize = WAD_ACCOUNT_BYTES - WAD_HEADER_BYTES;
pub const LEVEL_HEADER_BYTES: usize = 128;
pub const LEVEL_IMAGE_OFFSET: usize = 4_096;
pub const LEVEL_IMAGE_BYTES: usize = 256;
pub const MOBJ_HEADER_BYTES: usize = 4_096;
pub const MOBJ_SLOT_BYTES: usize = 160;
pub const MOBJ_SLOTS: usize = 384;
pub const PORT_VERSION: &[u8; 14] = b"doom-host-0004";
pub const BUTTON_ALLOCATE_PROBE: u8 = 0x80;

const CONFIG_MAGIC: &[u8; 8] = b"DOOM-CFG";
const LEVEL_MAGIC: &[u8; 8] = b"DOOM-LS1";
const POOL_MAGIC: &[u8; 8] = b"DOOM-MP1";
const STATE_MAGIC: &[u8; 8] = b"DOOM-ST1";
const TIC_DOMAIN: &[u8] = b"DOOM-TIC1";
const LEVEL_DOMAIN: &[u8] = b"DOOM-LEVEL1";
const ERR_MALFORMED: u32 = 1;
const ERR_REINITIALIZED: u32 = 2;
const ERR_UNSUPPORTED_LEVEL: u32 = 3;
const ERR_LIVE_FRAME: u32 = 4;
const ERR_STALE_HASH: u32 = 5;
const ERR_BAD_TIC: u32 = 6;
const ERR_POOL_EXHAUSTED: u32 = 7;

type StepResult<T> = Result<T, ProgramError>;

#[derive(Clone, Copy)]
struct WadSummary {
    lengths: [u32; WAD_CHUNKS],
    total: u32,
    root: [u8; 32],
    palette: [u8; 32],
    map_metadata: [u8; 80],
}

fn refusal(code: u32) -> ProgramError {
    ProgramError::Custom(code)
}

fn u32le(data: &[u8], offset: usize) -> StepResult<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| refusal(ERR_MALFORMED))?;
    let bytes = data
        .get(offset..end)
        .ok_or_else(|| refusal(ERR_MALFORMED))?;
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| refusal(ERR_MALFORMED))?,
    ))
}

fn i32le(data: &[u8], offset: usize) -> StepResult<i32> {
    Ok(u32le(data, offset)? as i32)
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) -> StepResult<()> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| refusal(ERR_MALFORMED))?;
    let out = data
        .get_mut(offset..end)
        .ok_or_else(|| refusal(ERR_MALFORMED))?;
    out.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_i32(data: &mut [u8], offset: usize, value: i32) -> StepResult<()> {
    put_u32(data, offset, value as u32)
}

fn all_zero(data: &[u8]) -> bool {
    data.iter().all(|byte| *byte == 0)
}

fn same_key(accounts: &[AccountInfo], left: usize, right: usize) -> bool {
    accounts[left].key == accounts[right].key
}

fn check_common(data: &[u8], expected_len: usize, opcode: u32) -> StepResult<()> {
    if data.len() != expected_len
        || data.get(..8) != Some(INSTRUCTION_MAGIC.as_slice())
        || u32le(data, 8)? != PROTOCOL_VERSION
        || u32le(data, 12)? != opcode
    {
        return Err(refusal(ERR_MALFORMED));
    }
    Ok(())
}

fn validate_chunk(data: &[u8], index: usize) -> StepResult<u32> {
    if data.len() != WAD_ACCOUNT_BYTES
        || data.get(..8) != Some(b"DOOM-WC1".as_slice())
        || u32le(data, 8)? != 1
        || u32le(data, 12)? as usize != WAD_HEADER_BYTES
        || u32le(data, 16)? as usize != WAD_ACCOUNT_BYTES
        || u32le(data, 20)? as usize != index
        || u32le(data, 28)? != 0
    {
        return Err(refusal(ERR_MALFORMED));
    }
    let length = u32le(data, 24)? as usize;
    if length > WAD_PAYLOAD_BYTES
        || !all_zero(
            data.get(WAD_HEADER_BYTES + length..)
                .ok_or_else(|| refusal(ERR_MALFORMED))?,
        )
    {
        return Err(refusal(ERR_MALFORMED));
    }
    Ok(length as u32)
}

fn with_wad<T>(
    accounts: &[AccountInfo],
    start: usize,
    f: impl FnOnce([&[u8]; WAD_CHUNKS]) -> StepResult<T>,
) -> StepResult<T> {
    let c0 = accounts[start]
        .try_borrow_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let c1 = accounts[start + 1]
        .try_borrow_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let c2 = accounts[start + 2]
        .try_borrow_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let c3 = accounts[start + 3]
        .try_borrow_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let c4 = accounts[start + 4]
        .try_borrow_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    f([&c0[..], &c1[..], &c2[..], &c3[..], &c4[..]])
}

fn validate_chunks(chunks: [&[u8]; WAD_CHUNKS]) -> StepResult<([u32; WAD_CHUNKS], u32)> {
    let mut lengths = [0u32; WAD_CHUNKS];
    let mut total = 0u32;
    for (index, chunk) in chunks.iter().enumerate() {
        let length = validate_chunk(chunk, index)?;
        total = total
            .checked_add(length)
            .ok_or_else(|| refusal(ERR_MALFORMED))?;
        lengths[index] = length;
    }
    Ok((lengths, total))
}

fn hash_range(
    chunks: [&[u8]; WAD_CHUNKS],
    lengths: [u32; WAD_CHUNKS],
    start: u32,
    length: u32,
) -> StepResult<[u8; 32]> {
    let end = start
        .checked_add(length)
        .ok_or_else(|| refusal(ERR_MALFORMED))?;
    let total = lengths.iter().try_fold(0u32, |sum, n| {
        sum.checked_add(*n).ok_or_else(|| refusal(ERR_MALFORMED))
    })?;
    if end > total {
        return Err(refusal(ERR_MALFORMED));
    }
    let mut parts: [&[u8]; WAD_CHUNKS] = [&[]; WAD_CHUNKS];
    let mut part_count = 0usize;
    let mut cursor = 0u32;
    for (index, chunk_length) in lengths.iter().enumerate() {
        let chunk_start = cursor;
        let chunk_end = cursor
            .checked_add(*chunk_length)
            .ok_or_else(|| refusal(ERR_MALFORMED))?;
        let overlap_start = start.max(chunk_start);
        let overlap_end = end.min(chunk_end);
        if overlap_start < overlap_end {
            let local_start = WAD_HEADER_BYTES + (overlap_start - chunk_start) as usize;
            let local_end = WAD_HEADER_BYTES + (overlap_end - chunk_start) as usize;
            parts[part_count] = chunks[index]
                .get(local_start..local_end)
                .ok_or_else(|| refusal(ERR_MALFORMED))?;
            part_count += 1;
        }
        cursor = chunk_end;
    }
    Ok(hashv(&parts[..part_count]).to_bytes())
}

fn wad_byte(
    chunks: [&[u8]; WAD_CHUNKS],
    lengths: [u32; WAD_CHUNKS],
    offset: u32,
) -> StepResult<u8> {
    let mut cursor = 0u32;
    for (index, length) in lengths.iter().enumerate() {
        let end = cursor
            .checked_add(*length)
            .ok_or_else(|| refusal(ERR_MALFORMED))?;
        if offset < end {
            let local = WAD_HEADER_BYTES + (offset - cursor) as usize;
            return chunks[index]
                .get(local)
                .copied()
                .ok_or_else(|| refusal(ERR_MALFORMED));
        }
        cursor = end;
    }
    Err(refusal(ERR_MALFORMED))
}

fn wad_u32(
    chunks: [&[u8]; WAD_CHUNKS],
    lengths: [u32; WAD_CHUNKS],
    offset: u32,
) -> StepResult<u32> {
    let mut value = [0u8; 4];
    for (index, byte) in value.iter_mut().enumerate() {
        *byte = wad_byte(
            chunks,
            lengths,
            offset
                .checked_add(index as u32)
                .ok_or_else(|| refusal(ERR_MALFORMED))?,
        )?;
    }
    Ok(u32::from_le_bytes(value))
}

fn wad_name(
    chunks: [&[u8]; WAD_CHUNKS],
    lengths: [u32; WAD_CHUNKS],
    offset: u32,
) -> StepResult<[u8; 8]> {
    let mut name = [0u8; 8];
    for (index, byte) in name.iter_mut().enumerate() {
        *byte = wad_byte(
            chunks,
            lengths,
            offset
                .checked_add(index as u32)
                .ok_or_else(|| refusal(ERR_MALFORMED))?,
        )?;
    }
    Ok(name)
}

fn lump_at(
    chunks: [&[u8]; WAD_CHUNKS],
    lengths: [u32; WAD_CHUNKS],
    total: u32,
    directory: u32,
    index: u32,
) -> StepResult<(u32, u32)> {
    let entry = directory
        .checked_add(
            index
                .checked_mul(16)
                .ok_or_else(|| refusal(ERR_MALFORMED))?,
        )
        .ok_or_else(|| refusal(ERR_MALFORMED))?;
    let pos = wad_u32(chunks, lengths, entry)?;
    let length = wad_u32(
        chunks,
        lengths,
        entry.checked_add(4).ok_or_else(|| refusal(ERR_MALFORMED))?,
    )?;
    if pos > total || length > total - pos {
        return Err(refusal(ERR_MALFORMED));
    }
    Ok((pos, length))
}

fn find_lump(
    chunks: [&[u8]; WAD_CHUNKS],
    lengths: [u32; WAD_CHUNKS],
    total: u32,
    directory: u32,
    count: u32,
    wanted: &[u8; 8],
) -> StepResult<(u32, u32, u32)> {
    for index in 0..count {
        let entry = directory
            .checked_add(
                index
                    .checked_mul(16)
                    .ok_or_else(|| refusal(ERR_MALFORMED))?,
            )
            .ok_or_else(|| refusal(ERR_MALFORMED))?;
        if wad_name(
            chunks,
            lengths,
            entry.checked_add(8).ok_or_else(|| refusal(ERR_MALFORMED))?,
        )? == *wanted
        {
            let (pos, length) = lump_at(chunks, lengths, total, directory, index)?;
            return Ok((index, pos, length));
        }
    }
    Err(refusal(ERR_UNSUPPORTED_LEVEL))
}

fn find_lump_after(
    chunks: [&[u8]; WAD_CHUNKS],
    lengths: [u32; WAD_CHUNKS],
    total: u32,
    directory: u32,
    count: u32,
    first: u32,
    wanted: &[u8; 8],
) -> StepResult<(u32, u32, u32)> {
    for index in first..count {
        let entry = directory
            .checked_add(
                index
                    .checked_mul(16)
                    .ok_or_else(|| refusal(ERR_MALFORMED))?,
            )
            .ok_or_else(|| refusal(ERR_MALFORMED))?;
        if wad_name(
            chunks,
            lengths,
            entry.checked_add(8).ok_or_else(|| refusal(ERR_MALFORMED))?,
        )? == *wanted
        {
            let (pos, length) = lump_at(chunks, lengths, total, directory, index)?;
            return Ok((index, pos, length));
        }
    }
    Err(refusal(ERR_UNSUPPORTED_LEVEL))
}

fn summarize_wad(chunks: [&[u8]; WAD_CHUNKS]) -> StepResult<WadSummary> {
    let (lengths, total) = validate_chunks(chunks)?;
    let payloads = [
        chunks[0]
            .get(WAD_HEADER_BYTES..WAD_HEADER_BYTES + lengths[0] as usize)
            .ok_or_else(|| refusal(ERR_MALFORMED))?,
        chunks[1]
            .get(WAD_HEADER_BYTES..WAD_HEADER_BYTES + lengths[1] as usize)
            .ok_or_else(|| refusal(ERR_MALFORMED))?,
        chunks[2]
            .get(WAD_HEADER_BYTES..WAD_HEADER_BYTES + lengths[2] as usize)
            .ok_or_else(|| refusal(ERR_MALFORMED))?,
        chunks[3]
            .get(WAD_HEADER_BYTES..WAD_HEADER_BYTES + lengths[3] as usize)
            .ok_or_else(|| refusal(ERR_MALFORMED))?,
        chunks[4]
            .get(WAD_HEADER_BYTES..WAD_HEADER_BYTES + lengths[4] as usize)
            .ok_or_else(|| refusal(ERR_MALFORMED))?,
    ];
    let root = hashv(&payloads).to_bytes();
    let magic = [
        wad_byte(chunks, lengths, 0)?,
        wad_byte(chunks, lengths, 1)?,
        wad_byte(chunks, lengths, 2)?,
        wad_byte(chunks, lengths, 3)?,
    ];
    if magic != *b"IWAD" && magic != *b"PWAD" {
        return Err(refusal(ERR_MALFORMED));
    }
    let count = wad_u32(chunks, lengths, 4)?;
    let directory = wad_u32(chunks, lengths, 8)?;
    if directory < 12 || directory > total || count > (total - directory) / 16 {
        return Err(refusal(ERR_MALFORMED));
    }
    // Validate every directory entry before accepting any named map lump.
    for index in 0..count {
        let _ = lump_at(chunks, lengths, total, directory, index)?;
    }
    let (_, palette_pos, palette_len) =
        find_lump(chunks, lengths, total, directory, count, b"PLAYPAL\0")?;
    let palette = hash_range(chunks, lengths, palette_pos, palette_len)?;
    let (map_index, _, _) = find_lump(chunks, lengths, total, directory, count, b"E1M1\0\0\0\0")?;
    let names: [&[u8; 8]; 10] = [
        b"BLOCKMAP",
        b"VERTEXES",
        b"SECTORS\0",
        b"SIDEDEFS",
        b"LINEDEFS",
        b"SSECTORS",
        b"NODES\0\0\0",
        b"SEGS\0\0\0\0",
        b"REJECT\0\0",
        b"THINGS\0\0",
    ];
    let mut map_metadata = [0u8; 80];
    for (offset, wanted) in names.iter().enumerate() {
        let (_, pos, length) = find_lump_after(
            chunks,
            lengths,
            total,
            directory,
            count,
            map_index
                .checked_add(1)
                .ok_or_else(|| refusal(ERR_MALFORMED))?,
            wanted,
        )?;
        map_metadata[offset * 8..offset * 8 + 4].copy_from_slice(&pos.to_le_bytes());
        map_metadata[offset * 8 + 4..offset * 8 + 8].copy_from_slice(&length.to_le_bytes());
    }
    Ok(WadSummary {
        lengths,
        total,
        root,
        palette,
        map_metadata,
    })
}

fn config_is_valid(data: &[u8], program_id: &Pubkey) -> StepResult<[u8; 32]> {
    if data.len() != CONFIG_BYTES
        || data.get(..8) != Some(CONFIG_MAGIC.as_slice())
        || u32le(data, 8)? != 1
        || u32le(data, 12)? as usize != LEVEL_HEADER_BYTES
        || u32le(data, 16)? as usize != CONFIG_BYTES
        || u32le(data, 20)? as usize != PORT_VERSION.len()
        || u32le(data, 88)? as usize != WAD_CHUNKS
        || u32le(data, 92)? as usize != WAD_ACCOUNT_BYTES
        || u32le(data, 96)? as usize != LEVEL_STATE_BYTES
        || u32le(data, 100)? as usize != MOBJ_POOL_BYTES
        || u32le(data, 104)? as usize != MOBJ_SLOT_BYTES
        || u32le(data, 108)? as usize != MOBJ_SLOTS
        || !all_zero(&data[112..])
    {
        return Err(refusal(ERR_MALFORMED));
    }
    let _ = program_id;
    Ok(data[24..56]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?)
}

fn check_init_accounts(accounts: &[AccountInfo], program_id: &Pubkey) -> StepResult<()> {
    if accounts.len() != 8
        || !accounts[0].is_signer
        || !accounts[0].is_writable
        || !accounts[1].is_writable
        || accounts[1].owner != program_id
        || accounts[7].key != &system_program::id()
        || !accounts[7].executable
    {
        return Err(refusal(ERR_MALFORMED));
    }
    for index in 2..7 {
        if accounts[index].is_writable
            || accounts[index].owner != &system_program::id()
            || accounts[index].data_len() != WAD_ACCOUNT_BYTES
        {
            return Err(refusal(ERR_MALFORMED));
        }
    }
    for left in 0..7 {
        for right in left + 1..8 {
            if same_key(accounts, left, right) {
                return Err(refusal(ERR_MALFORMED));
            }
        }
    }
    Ok(())
}

fn process_init(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> StepResult<()> {
    check_common(data, INIT_BYTES, OP_INIT)?;
    check_init_accounts(accounts, program_id)?;
    let expected_wad_bytes = u32le(data, 16)?;
    if u32le(data, 20)? as usize != WAD_CHUNKS
        || u32le(data, 24)? as usize != WAD_ACCOUNT_BYTES
        || u32le(data, 28)? as usize != LEVEL_STATE_BYTES
        || u32le(data, 32)? as usize != MOBJ_POOL_BYTES
        || u32le(data, 36)? as usize != MOBJ_SLOT_BYTES
        || u32le(data, 40)? as usize != MOBJ_SLOTS
        || u32le(data, 44)? as usize != PORT_VERSION.len()
        || data[112..112 + PORT_VERSION.len()] != PORT_VERSION[..]
        || !all_zero(&data[112 + PORT_VERSION.len()..128])
    {
        return Err(refusal(ERR_MALFORMED));
    }
    let expected_root: [u8; 32] = data[48..80]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let expected_palette: [u8; 32] = data[80..112]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    with_wad(accounts, 2, |chunks| {
        let summary = summarize_wad(chunks)?;
        if summary.total != expected_wad_bytes
            || summary.root != expected_root
            || summary.palette != expected_palette
        {
            return Err(refusal(ERR_MALFORMED));
        }
        let mut config = accounts[1]
            .try_borrow_mut_data()
            .map_err(|_| refusal(ERR_MALFORMED))?;
        if !all_zero(&config) {
            return Err(refusal(ERR_REINITIALIZED));
        }
        config.fill(0);
        config[..8].copy_from_slice(CONFIG_MAGIC);
        put_u32(&mut config, 8, 1)?;
        put_u32(&mut config, 12, LEVEL_HEADER_BYTES as u32)?;
        put_u32(&mut config, 16, CONFIG_BYTES as u32)?;
        put_u32(&mut config, 20, PORT_VERSION.len() as u32)?;
        config[24..56].copy_from_slice(&summary.root);
        config[56..88].copy_from_slice(&summary.palette);
        put_u32(&mut config, 88, WAD_CHUNKS as u32)?;
        put_u32(&mut config, 92, WAD_ACCOUNT_BYTES as u32)?;
        put_u32(&mut config, 96, LEVEL_STATE_BYTES as u32)?;
        put_u32(&mut config, 100, MOBJ_POOL_BYTES as u32)?;
        put_u32(&mut config, 104, MOBJ_SLOT_BYTES as u32)?;
        put_u32(&mut config, 108, MOBJ_SLOTS as u32)?;
        Ok(())
    })
}

fn check_lifecycle_accounts(accounts: &[AccountInfo], program_id: &Pubkey) -> StepResult<()> {
    if accounts.len() != 8
        || accounts[0].is_writable
        || accounts[0].data_len() != CONFIG_BYTES
        || accounts[0].owner != program_id
        || !accounts[7].is_writable
        || accounts[7].data_len() != MOBJ_POOL_BYTES
        || accounts[6].data_len() != LEVEL_STATE_BYTES
        || !accounts[6].is_writable
        || accounts[0].owner != accounts[6].owner
        || accounts[0].owner != accounts[7].owner
    {
        return Err(refusal(ERR_MALFORMED));
    }
    for index in 1..6 {
        if accounts[index].is_writable || accounts[index].data_len() != WAD_ACCOUNT_BYTES {
            return Err(refusal(ERR_MALFORMED));
        }
    }
    for left in 0..8 {
        for right in left + 1..8 {
            if same_key(accounts, left, right) {
                return Err(refusal(ERR_MALFORMED));
            }
        }
    }
    Ok(())
}

fn make_loaded_accounts(level: &mut [u8], pool: &mut [u8], summary: WadSummary) -> StepResult<()> {
    level.fill(0);
    pool.fill(0);
    let mut image = [0u8; LEVEL_IMAGE_BYTES];
    image[..8].copy_from_slice(STATE_MAGIC);
    put_u32(&mut image, 8, 0)?;
    put_u32(&mut image, 12, 64)?;
    put_u32(&mut image, 16, LEVEL_IMAGE_BYTES as u32)?;
    put_u32(&mut image, 20, 0)?;
    put_u32(&mut image, 24, 0)?;
    put_u32(&mut image, 28, 1)?;
    image[128..160]
        .copy_from_slice(&hashv(&[LEVEL_DOMAIN, &summary.root, &summary.map_metadata]).to_bytes());
    level[..8].copy_from_slice(LEVEL_MAGIC);
    put_u32(level, 8, 1)?;
    put_u32(level, 12, LEVEL_HEADER_BYTES as u32)?;
    put_u32(level, 16, LEVEL_STATE_BYTES as u32)?;
    put_u32(level, 20, LEVEL_IMAGE_OFFSET as u32)?;
    put_u32(level, 24, (LEVEL_STATE_BYTES - LEVEL_IMAGE_OFFSET) as u32)?;
    put_u32(level, 28, LEVEL_IMAGE_BYTES as u32)?;
    level[32..64].copy_from_slice(&summary.root);
    level[64..96].copy_from_slice(&hashv(&[&image]).to_bytes());
    put_u32(level, 96, 0)?;
    level[LEVEL_IMAGE_OFFSET..LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES].copy_from_slice(&image);

    pool[..8].copy_from_slice(POOL_MAGIC);
    put_u32(pool, 8, 1)?;
    put_u32(pool, 12, MOBJ_HEADER_BYTES as u32)?;
    put_u32(pool, 16, MOBJ_POOL_BYTES as u32)?;
    put_u32(pool, 20, MOBJ_SLOT_BYTES as u32)?;
    put_u32(pool, 24, MOBJ_SLOTS as u32)?;
    put_u32(pool, 28, 1)?;
    put_u32(pool, 32, 1)?;
    pool[36..68].copy_from_slice(&summary.root);
    let slot = &mut pool[MOBJ_HEADER_BYTES..MOBJ_HEADER_BYTES + MOBJ_SLOT_BYTES];
    put_u32(slot, 0, 1)?;
    put_u32(slot, 4, 1)?;
    put_i32(
        slot,
        16,
        i32::from_le_bytes(summary.root[..4].try_into().unwrap_or([0; 4])),
    )?;
    put_i32(
        slot,
        20,
        i32::from_le_bytes(summary.root[4..8].try_into().unwrap_or([0; 4])),
    )?;
    put_i32(
        slot,
        24,
        i32::from_le_bytes(summary.root[8..12].try_into().unwrap_or([0; 4])),
    )?;
    put_u32(slot, 108, 1)?;
    Ok(())
}

fn level_is_valid(data: &[u8], root: &[u8; 32]) -> StepResult<()> {
    if data.len() != LEVEL_STATE_BYTES
        || data.get(..8) != Some(LEVEL_MAGIC.as_slice())
        || u32le(data, 8)? != 1
        || u32le(data, 12)? as usize != LEVEL_HEADER_BYTES
        || u32le(data, 16)? as usize != LEVEL_STATE_BYTES
        || u32le(data, 20)? as usize != LEVEL_IMAGE_OFFSET
        || u32le(data, 24)? as usize != LEVEL_STATE_BYTES - LEVEL_IMAGE_OFFSET
        || u32le(data, 28)? as usize != LEVEL_IMAGE_BYTES
        || &data[32..64] != root
        || u32le(data, 96)? != u32le(data, LEVEL_IMAGE_OFFSET + 20)?
        || data[100..128].iter().any(|byte| *byte != 0)
    {
        return Err(refusal(ERR_MALFORMED));
    }
    let image = &data[LEVEL_IMAGE_OFFSET..LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES];
    if image[..8] != *STATE_MAGIC
        || u32le(image, 8)? != 0
        || u32le(image, 12)? != 64
        || u32le(image, 16)? as usize != LEVEL_IMAGE_BYTES
        || hashv(&[image]).to_bytes() != data[64..96]
    {
        return Err(refusal(ERR_MALFORMED));
    }
    Ok(())
}

fn pool_is_valid(data: &[u8], root: &[u8; 32]) -> StepResult<()> {
    if data.len() != MOBJ_POOL_BYTES
        || data.get(..8) != Some(POOL_MAGIC.as_slice())
        || u32le(data, 8)? != 1
        || u32le(data, 12)? as usize != MOBJ_HEADER_BYTES
        || u32le(data, 16)? as usize != MOBJ_POOL_BYTES
        || u32le(data, 20)? as usize != MOBJ_SLOT_BYTES
        || u32le(data, 24)? as usize != MOBJ_SLOTS
        || &data[36..68] != root
        || data[68..MOBJ_HEADER_BYTES].iter().any(|byte| *byte != 0)
    {
        return Err(refusal(ERR_MALFORMED));
    }
    let declared_active = u32le(data, 28)? as usize;
    let declared_free = u32le(data, 32)?;
    if declared_active > MOBJ_SLOTS {
        return Err(refusal(ERR_MALFORMED));
    }
    let mut active = 0usize;
    let mut first_free = MOBJ_SLOTS;
    for index in 0..MOBJ_SLOTS {
        let start = MOBJ_HEADER_BYTES + index * MOBJ_SLOT_BYTES;
        let slot = data
            .get(start..start + MOBJ_SLOT_BYTES)
            .ok_or_else(|| refusal(ERR_MALFORMED))?;
        let id = u32le(slot, 0)? as usize;
        let is_active = u32le(slot, 4)?;
        if slot[126..].iter().any(|byte| *byte != 0) {
            return Err(refusal(ERR_MALFORMED));
        }
        if id == 0 {
            if is_active != 0 || slot[8..126].iter().any(|byte| *byte != 0) {
                return Err(refusal(ERR_MALFORMED));
            }
            first_free = first_free.min(index);
        } else {
            if id != index + 1 || is_active != 1 {
                return Err(refusal(ERR_MALFORMED));
            }
            for offset in [8usize, 12, 108] {
                let target = u32le(slot, offset)? as usize;
                if target > MOBJ_SLOTS {
                    return Err(refusal(ERR_MALFORMED));
                }
            }
            active += 1;
        }
    }
    let encoded_free = if first_free == MOBJ_SLOTS {
        u32::MAX
    } else {
        first_free as u32
    };
    if active != declared_active || declared_free != encoded_free {
        return Err(refusal(ERR_MALFORMED));
    }
    Ok(())
}

fn state_hash(root: &[u8; 32], level: &[u8], pool: &[u8]) -> [u8; 32] {
    hashv(&[
        TIC_DOMAIN,
        &PROTOCOL_VERSION.to_le_bytes(),
        root,
        level,
        pool,
    ])
    .to_bytes()
}

#[cfg(not(target_arch = "bpf"))]
pub fn host_state_hash(root: &[u8; 32], level: &[u8], pool: &[u8]) -> [u8; 32] {
    state_hash(root, level, pool)
}

fn process_load_level(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> StepResult<()> {
    check_common(data, LOAD_LEVEL_BYTES, OP_LOAD_LEVEL)?;
    check_lifecycle_accounts(accounts, program_id)?;
    let config = accounts[0]
        .try_borrow_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let root = config_is_valid(&config, program_id)?;
    let expected_state: [u8; 32] = data[24..56]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    if u32le(data, 16)? != 1 || u32le(data, 20)? != 1 {
        return Err(refusal(ERR_UNSUPPORTED_LEVEL));
    }
    drop(config);
    with_wad(accounts, 1, |chunks| {
        let summary = summarize_wad(chunks)?;
        if summary.root != root {
            return Err(refusal(ERR_MALFORMED));
        }
        let mut level = accounts[6]
            .try_borrow_mut_data()
            .map_err(|_| refusal(ERR_MALFORMED))?;
        let mut pool = accounts[7]
            .try_borrow_mut_data()
            .map_err(|_| refusal(ERR_MALFORMED))?;
        if !all_zero(&level) || !all_zero(&pool) {
            return Err(refusal(ERR_LIVE_FRAME));
        }
        make_loaded_accounts(&mut level, &mut pool, summary)?;
        if state_hash(&root, &level, &pool) != expected_state {
            level.fill(0);
            pool.fill(0);
            return Err(refusal(ERR_STALE_HASH));
        }
        Ok(())
    })
}

fn process_tic(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> StepResult<()> {
    if accounts.len() != 3
        || accounts[0].is_writable
        || !accounts[1].is_writable
        || !accounts[2].is_writable
        || accounts[1].data_len() != LEVEL_STATE_BYTES
        || accounts[2].data_len() != MOBJ_POOL_BYTES
        || accounts[0].data_len() != CONFIG_BYTES
        || accounts[0].owner != program_id
        || accounts[1].owner != program_id
        || accounts[2].owner != program_id
        || accounts[0].owner != accounts[1].owner
        || accounts[0].owner != accounts[2].owner
        || same_key(accounts, 0, 1)
        || same_key(accounts, 0, 2)
        || same_key(accounts, 1, 2)
    {
        return Err(refusal(ERR_MALFORMED));
    }
    check_common(data, TIC_BYTES, OP_TIC)?;
    let config = accounts[0]
        .try_borrow_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let root = config_is_valid(&config, program_id)?;
    drop(config);
    let mut level = accounts[1]
        .try_borrow_mut_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let mut pool = accounts[2]
        .try_borrow_mut_data()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    level_is_valid(&level, &root)?;
    pool_is_valid(&pool, &root)?;
    let current = u32le(&level, 96)?;
    let next = u32le(data, 16)?;
    if current == u32::MAX || next != current + 1 || u32le(data, 20)? != 1 {
        return Err(refusal(ERR_BAD_TIC));
    }
    let expected_before: [u8; 32] = data[24..56]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let expected_after: [u8; 32] = data[56..88]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    if state_hash(&root, &level, &pool) != expected_before {
        return Err(refusal(ERR_STALE_HASH));
    }
    let command = &data[88..92];
    let allocate = command[3] & BUTTON_ALLOCATE_PROBE != 0;
    let first_free_raw = u32le(&pool, 32)?;
    if allocate && first_free_raw == u32::MAX {
        return Err(refusal(ERR_POOL_EXHAUSTED));
    }
    let first_free = first_free_raw as usize;
    if allocate && first_free >= MOBJ_SLOTS {
        return Err(refusal(ERR_MALFORMED));
    }
    let old_image: [u8; LEVEL_IMAGE_BYTES] = level
        [LEVEL_IMAGE_OFFSET..LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let old_level_commit: [u8; 36] = level[64..100]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let slot0_start = MOBJ_HEADER_BYTES;
    let old_slot0: [u8; MOBJ_SLOT_BYTES] = pool[slot0_start..slot0_start + MOBJ_SLOT_BYTES]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let old_pool_counts: [u8; 8] = pool[28..36]
        .try_into()
        .map_err(|_| refusal(ERR_MALFORMED))?;
    let old_allocated: [u8; MOBJ_SLOT_BYTES] = if allocate {
        pool[MOBJ_HEADER_BYTES + first_free * MOBJ_SLOT_BYTES
            ..MOBJ_HEADER_BYTES + (first_free + 1) * MOBJ_SLOT_BYTES]
            .try_into()
            .map_err(|_| refusal(ERR_MALFORMED))?
    } else {
        [0u8; MOBJ_SLOT_BYTES]
    };
    let image = &mut level[LEVEL_IMAGE_OFFSET..LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES];
    image[64..68].copy_from_slice(command);
    put_u32(image, 20, next)?;
    let level_time = u32le(image, 68)?.wrapping_add(1);
    put_u32(image, 68, level_time)?;
    let forward = i32::from(command[0] as i8).wrapping_mul(256);
    let side = i32::from(command[1] as i8).wrapping_mul(256);
    let x = i32le(&pool[slot0_start..slot0_start + MOBJ_SLOT_BYTES], 16)?.wrapping_add(forward);
    let y = i32le(&pool[slot0_start..slot0_start + MOBJ_SLOT_BYTES], 20)?.wrapping_add(side);
    put_i32(&mut pool[slot0_start..slot0_start + MOBJ_SLOT_BYTES], 16, x)?;
    put_i32(&mut pool[slot0_start..slot0_start + MOBJ_SLOT_BYTES], 20, y)?;
    let checksum = unsafe { super::doom_fixed_mul_div(image.as_mut_ptr()) } as u32;
    put_u32(image, 72, checksum)?;
    let committed = hashv(&[&*image]).to_bytes();
    level[64..96].copy_from_slice(&committed);
    put_u32(&mut level, 96, next)?;
    if allocate {
        let start = MOBJ_HEADER_BYTES + first_free * MOBJ_SLOT_BYTES;
        put_u32(
            &mut pool[start..start + MOBJ_SLOT_BYTES],
            0,
            first_free as u32 + 1,
        )?;
        put_u32(&mut pool[start..start + MOBJ_SLOT_BYTES], 4, 1)?;
        let active_count = u32le(&pool, 28)?
            .checked_add(1)
            .ok_or_else(|| refusal(ERR_MALFORMED))?;
        put_u32(&mut pool, 28, active_count)?;
        let new_first = (first_free + 1..MOBJ_SLOTS)
            .find(|index| {
                u32le(&pool, MOBJ_HEADER_BYTES + index * MOBJ_SLOT_BYTES).unwrap_or(1) == 0
            })
            .unwrap_or(MOBJ_SLOTS);
        put_u32(
            &mut pool,
            32,
            if new_first == MOBJ_SLOTS {
                u32::MAX
            } else {
                new_first as u32
            },
        )?;
    }
    let after = state_hash(&root, &level, &pool);
    if after != expected_after {
        level[LEVEL_IMAGE_OFFSET..LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES]
            .copy_from_slice(&old_image);
        level[64..100].copy_from_slice(&old_level_commit);
        pool[slot0_start..slot0_start + MOBJ_SLOT_BYTES].copy_from_slice(&old_slot0);
        pool[28..36].copy_from_slice(&old_pool_counts);
        if allocate {
            let start = MOBJ_HEADER_BYTES + first_free * MOBJ_SLOT_BYTES;
            pool[start..start + MOBJ_SLOT_BYTES].copy_from_slice(&old_allocated);
        }
        return Err(refusal(ERR_STALE_HASH));
    }
    Ok(())
}

pub fn is_step5_instruction(data: &[u8]) -> bool {
    data.get(..8) == Some(INSTRUCTION_MAGIC.as_slice())
}

pub fn process_step5(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < 16 || !is_step5_instruction(data) {
        return Err(refusal(ERR_MALFORMED));
    }
    match u32le(data, 12)? {
        OP_INIT => process_init(program_id, accounts, data),
        OP_LOAD_LEVEL => process_load_level(program_id, accounts, data),
        OP_TIC => process_tic(program_id, accounts, data),
        _ => Err(refusal(ERR_MALFORMED)),
    }
}

// These helpers are intentionally host-only.  They let the focused
// ProgramTest driver construct expected hashes without putting a second state
// codec in the measurement harness or adding Vec-backed code to the SBF image.
#[cfg(not(target_arch = "bpf"))]
pub fn host_init_data(chunks: &[Vec<u8>]) -> Result<Vec<u8>, ProgramError> {
    if chunks.len() != WAD_CHUNKS {
        return Err(refusal(ERR_MALFORMED));
    }
    let summary = summarize_wad([
        chunks[0].as_slice(),
        chunks[1].as_slice(),
        chunks[2].as_slice(),
        chunks[3].as_slice(),
        chunks[4].as_slice(),
    ])?;
    let mut data = [0u8; INIT_BYTES];
    data[..8].copy_from_slice(INSTRUCTION_MAGIC);
    put_u32(&mut data, 8, PROTOCOL_VERSION)?;
    put_u32(&mut data, 12, OP_INIT)?;
    put_u32(&mut data, 16, summary.total)?;
    put_u32(&mut data, 20, WAD_CHUNKS as u32)?;
    put_u32(&mut data, 24, WAD_ACCOUNT_BYTES as u32)?;
    put_u32(&mut data, 28, LEVEL_STATE_BYTES as u32)?;
    put_u32(&mut data, 32, MOBJ_POOL_BYTES as u32)?;
    put_u32(&mut data, 36, MOBJ_SLOT_BYTES as u32)?;
    put_u32(&mut data, 40, MOBJ_SLOTS as u32)?;
    put_u32(&mut data, 44, PORT_VERSION.len() as u32)?;
    data[48..80].copy_from_slice(&summary.root);
    data[80..112].copy_from_slice(&summary.palette);
    data[112..112 + PORT_VERSION.len()].copy_from_slice(PORT_VERSION);
    Ok(data.to_vec())
}

#[cfg(not(target_arch = "bpf"))]
pub fn host_loaded_state(chunks: &[Vec<u8>]) -> Result<(Vec<u8>, Vec<u8>, [u8; 32]), ProgramError> {
    if chunks.len() != WAD_CHUNKS {
        return Err(refusal(ERR_MALFORMED));
    }
    let summary = summarize_wad([
        chunks[0].as_slice(),
        chunks[1].as_slice(),
        chunks[2].as_slice(),
        chunks[3].as_slice(),
        chunks[4].as_slice(),
    ])?;
    let mut level = vec![0u8; LEVEL_STATE_BYTES];
    let mut pool = vec![0u8; MOBJ_POOL_BYTES];
    make_loaded_accounts(&mut level, &mut pool, summary)?;
    let root = summary.root;
    let digest = state_hash(&root, &level, &pool);
    Ok((level, pool, digest))
}

#[cfg(not(target_arch = "bpf"))]
pub fn host_tic_state(
    root: &[u8; 32],
    original_level: &[u8],
    original_pool: &[u8],
    number: u32,
    command: &[u8; 4],
) -> Result<(Vec<u8>, Vec<u8>, [u8; 32]), ProgramError> {
    let mut level = original_level.to_vec();
    let mut pool = original_pool.to_vec();
    let current = u32le(&level, 96)?;
    if current == u32::MAX || number != current + 1 {
        return Err(refusal(ERR_BAD_TIC));
    }
    let first_free = u32le(&pool, 32)? as usize;
    if command[3] & BUTTON_ALLOCATE_PROBE != 0 {
        if first_free == usize::MAX {
            return Err(refusal(ERR_POOL_EXHAUSTED));
        }
        if first_free >= MOBJ_SLOTS {
            return Err(refusal(ERR_MALFORMED));
        }
    }
    let image = &mut level[LEVEL_IMAGE_OFFSET..LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES];
    image[64..68].copy_from_slice(command);
    put_u32(image, 20, number)?;
    let level_time = u32le(image, 68)?.wrapping_add(1);
    put_u32(image, 68, level_time)?;
    let slot0_start = MOBJ_HEADER_BYTES;
    let slot0 = &mut pool[slot0_start..slot0_start + MOBJ_SLOT_BYTES];
    let old_x = i32le(slot0, 16)?;
    let old_y = i32le(slot0, 20)?;
    put_i32(
        slot0,
        16,
        old_x.wrapping_add(i32::from(command[0] as i8).wrapping_mul(256)),
    )?;
    put_i32(
        slot0,
        20,
        old_y.wrapping_add(i32::from(command[1] as i8).wrapping_mul(256)),
    )?;
    let checksum = unsafe { super::doom_fixed_mul_div(image.as_mut_ptr()) } as u32;
    put_u32(image, 72, checksum)?;
    let committed = hashv(&[&*image]).to_bytes();
    level[64..96].copy_from_slice(&committed);
    put_u32(&mut level, 96, number)?;
    if command[3] & BUTTON_ALLOCATE_PROBE != 0 {
        let start = MOBJ_HEADER_BYTES + first_free * MOBJ_SLOT_BYTES;
        put_u32(
            &mut pool[start..start + MOBJ_SLOT_BYTES],
            0,
            first_free as u32 + 1,
        )?;
        put_u32(&mut pool[start..start + MOBJ_SLOT_BYTES], 4, 1)?;
        let active_count = u32le(&pool, 28)?
            .checked_add(1)
            .ok_or_else(|| refusal(ERR_MALFORMED))?;
        put_u32(&mut pool, 28, active_count)?;
        let next_free = (first_free + 1..MOBJ_SLOTS)
            .find(|index| {
                u32le(&pool, MOBJ_HEADER_BYTES + index * MOBJ_SLOT_BYTES).unwrap_or(1) == 0
            })
            .unwrap_or(MOBJ_SLOTS);
        put_u32(
            &mut pool,
            32,
            if next_free == MOBJ_SLOTS {
                u32::MAX
            } else {
                next_free as u32
            },
        )?;
    }
    let digest = state_hash(root, &level, &pool);
    Ok((level, pool, digest))
}

#[cfg(not(target_arch = "bpf"))]
pub fn host_tic_after(
    root: &[u8; 32],
    original_level: &[u8],
    original_pool: &[u8],
    number: u32,
    command: &[u8; 4],
) -> Result<[u8; 32], ProgramError> {
    Ok(host_tic_state(root, original_level, original_pool, number, command)?.2)
}
