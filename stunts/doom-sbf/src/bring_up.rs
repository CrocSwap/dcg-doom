use sha2::Digest;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program_error::ProgramError,
    pubkey::Pubkey,
};
use generic_array::GenericArray;

pub const INIT_MAGIC: &[u8; 8] = b"DOOM-BU1";
pub const CHUNK_BYTES: usize = 1_048_576;
pub const PAYLOAD_BYTES: usize = CHUNK_BYTES - 32;
pub const WAD_BYTES: usize = 4_196_020;
pub const CONFIG_BYTES: usize = 4_096;
pub const COMPRESS_WINDOW_BYTES: usize = 32_768;
pub const SHA256_BLOCK_BYTES: usize = 64;
pub const MIDSTATE_BYTES: usize = 40;
pub const WAD_HASH: [u8; 32] = [
    0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9, 0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3, 0xe2, 0x9c,
    0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18, 0x16, 0xf6, 0x52, 0xa5, 0x26, 0xca, 0xc7, 0x71,
];

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// `DOOM-BU1` v0: 8-byte magic, `u32le` version 0, `u32le` consumed WAD bytes,
/// 32-byte expected WAD root, then a 40-byte SHA-256 midstate (eight
/// little-endian state words plus the `u32le` buffered-byte count).
pub const INIT_DATA_BYTES: usize = 8 + 4 + 4 + 32 + MIDSTATE_BYTES;

pub fn sha256_midstate(consumed_bytes: u64) -> ([u8; 32], u32) {
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut buffer = [0u8; SHA256_BLOCK_BYTES];
    (
        state
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<u8>>()
            .try_into()
            .unwrap(),
        0,
    )
}

pub fn process_init(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() != INIT_DATA_BYTES || &data[..8] != INIT_MAGIC || u32_at(data, 8) != 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if data[16..48] != WAD_HASH {
        return Err(ProgramError::Custom(1));
    }
    if accounts.len() != 6 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let config = &accounts[0];
    if config.owner != program || !config.is_writable || !config.is_signer {
        return Err(ProgramError::InvalidAccountOwner);
    }
    if config.data_len() != CONFIG_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    for (index, chunk) in accounts[1..].iter().enumerate() {
        if chunk.owner != program || chunk.is_writable || chunk.data_len() != CHUNK_BYTES {
            return Err(ProgramError::InvalidAccountData);
        }
        if accounts[..index + 1].iter().any(|a| a.key == chunk.key) {
            return Err(ProgramError::InvalidAccountData);
        }
    }
    let chunks = [
        accounts[1].try_borrow_data()?,
        accounts[2].try_borrow_data()?,
        accounts[3].try_borrow_data()?,
        accounts[4].try_borrow_data()?,
        accounts[5].try_borrow_data()?,
    ];
    let consumed = u32_at(data, 12) as usize;
    if consumed % COMPRESS_WINDOW_BYTES != 0 || consumed > WAD_BYTES {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut payloads: [&[u8]; 5] = [&[]; 5];
    for (index, chunk) in chunks.iter().enumerate() {
        let length = (WAD_BYTES - index * PAYLOAD_BYTES).min(PAYLOAD_BYTES);
        if &chunk[..8] != b"DOOM-WC1"
            || u32_at(chunk, 8) != 1
            || u32_at(chunk, 12) != 32
            || u32_at(chunk, 16) != CHUNK_BYTES as u32
            || u32_at(chunk, 20) != index as u32
            || u32_at(chunk, 24) != length as u32
            || u32_at(chunk, 28) != 0
        {
            return Err(ProgramError::InvalidAccountData);
        }
        payloads[index] = &chunk[32..32 + length];
    }
    let mut flat = [0u8; COMPRESS_WINDOW_BYTES];
    let mut filled = 0;
    let mut cursor = consumed;
    while filled < COMPRESS_WINDOW_BYTES && cursor < WAD_BYTES {
        let chunk = cursor / PAYLOAD_BYTES;
        let offset = 32 + cursor % PAYLOAD_BYTES;
        let room = PAYLOAD_BYTES - cursor % PAYLOAD_BYTES;
        let take = (COMPRESS_WINDOW_BYTES - filled).min(room).min(WAD_BYTES - cursor);
        flat[filled..filled + take].copy_from_slice(&payloads[chunk][offset..offset + take]);
        filled += take;
        cursor += take;
    }
    let mut words = [0u32; 8];
    for (index, word) in words.iter_mut().enumerate() {
        *word = u32::from_le_bytes(data[48 + index * 4..48 + index * 4 + 4].try_into().unwrap());
    }
    let mut blocks: Vec<_> = flat[..filled - filled % SHA256_BLOCK_BYTES]
        .chunks_exact(SHA256_BLOCK_BYTES)
        .map(|block| *generic_array_from(block))
        .collect();
    sha2::compress256(&mut words, &blocks);
    blocks.clear();
    let mut hasher = Sha256::new();
    for (index, word) in words.iter().enumerate() {
        hasher.update(word.to_be_bytes());
    }
    let digest: [u8; 32] = hasher.finalize().into();
    let mut output = config.try_borrow_mut_data()?;
    for (index, word) in words.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    output[32..40].copy_from_slice(&0u32.to_le_bytes());
    output[40..48].copy_from_slice(&((consumed + filled) as u64).to_le_bytes());
    output[48..80].copy_from_slice(&digest);
    if consumed + filled == WAD_BYTES {
        if digest != WAD_HASH {
            return Err(ProgramError::Custom(1));
        }
        msg!("DOOM_INIT_WAD_SHA256_OK");
    }
    Ok(())
}

fn generic_array_from(block: &[u8; 64]) -> &[u8; 64] {
    block
}
