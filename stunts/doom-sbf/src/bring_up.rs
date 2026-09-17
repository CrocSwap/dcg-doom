use sha2::digest::generic_array::GenericArray;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program_error::ProgramError,
    pubkey::Pubkey,
};

pub const INIT_MAGIC: &[u8; 8] = b"DOOM-BU1";
pub const CHUNK_BYTES: usize = 1_048_576;
pub const PAYLOAD_BYTES: usize = CHUNK_BYTES - 32;
pub const WAD_BYTES: usize = 4_196_020;
pub const CONFIG_BYTES: usize = 4_096;
pub const COMPRESS_WINDOW_BYTES: usize = 4_096;
pub const INIT_DATA_BYTES: usize = 48;
pub const WAD_HASH: [u8; 32] = [
    0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9, 0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3, 0xe2, 0x9c,
    0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18, 0x16, 0xf6, 0x52, 0xa5, 0x26, 0xca, 0xc7, 0x71,
];
const IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn compress(words: &mut [u32; 8], block: &[u8; 64]) {
    sha2::compress256(words, &[*GenericArray::from_slice(block)]);
}

fn advance(words: &mut [u32; 8], payloads: &[&[u8]; 5], consumed: usize) -> usize {
    let end = (consumed + COMPRESS_WINDOW_BYTES).min(WAD_BYTES);
    let mut cursor = consumed;
    while cursor < end {
        let mut block = [0u8; 64];
        let count = (end - cursor).min(64);
        let mut filled = 0;
        while filled < count {
            let index = cursor / PAYLOAD_BYTES;
            let offset = cursor % PAYLOAD_BYTES;
            let take = (count - filled).min(payloads[index].len() - offset);
            block[filled..filled + take].copy_from_slice(&payloads[index][offset..offset + take]);
            cursor += take;
            filled += take;
        }
        if count < 64 {
            block[count] = 0x80;
            if count >= 56 {
                compress(words, &block);
                block = [0; 64];
            }
            block[56..64].copy_from_slice(&((WAD_BYTES as u64) * 8).to_be_bytes());
        }
        compress(words, &block);
    }
    if end == WAD_BYTES && WAD_BYTES % 64 == 0 {
        let mut block = [0u8; 64];
        block[0] = 0x80;
        block[56..64].copy_from_slice(&((WAD_BYTES as u64) * 8).to_be_bytes());
        compress(words, &block);
    }
    end
}

pub fn process_init(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() != INIT_DATA_BYTES || &data[..8] != INIT_MAGIC || u32_at(data, 8) != 1 {
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
    let consumed = u32_at(data, 12) as usize;
    if consumed % COMPRESS_WINDOW_BYTES != 0 || consumed >= WAD_BYTES {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut output = config.try_borrow_mut_data()?;
    let fresh = output.iter().all(|&b| b == 0);
    let mut words = IV;
    if fresh {
        if consumed != 0 {
            return Err(ProgramError::Custom(2));
        }
    } else {
        if &output[..8] != INIT_MAGIC || u32_at(&output, 8) != 1 {
            return Err(ProgramError::InvalidAccountData);
        }
        if u32_at(&output, 12) as usize != consumed || output[48] != 0 {
            return Err(ProgramError::Custom(2));
        }
        if output[16..48] != WAD_HASH {
            return Err(ProgramError::Custom(1));
        }
        for (index, account) in accounts[1..].iter().enumerate() {
            if output[96 + index * 32..128 + index * 32] != account.key.to_bytes() {
                return Err(ProgramError::InvalidAccountData);
            }
        }
        for (index, word) in words.iter_mut().enumerate() {
            *word = u32_at(&output, 64 + index * 4);
        }
    }
    let chunks = [
        accounts[1].try_borrow_data()?, accounts[2].try_borrow_data()?,
        accounts[3].try_borrow_data()?, accounts[4].try_borrow_data()?,
        accounts[5].try_borrow_data()?,
    ];
    let mut payloads: [&[u8]; 5] = [&[]; 5];
    for (index, chunk) in chunks.iter().enumerate() {
        let length = (WAD_BYTES - index * PAYLOAD_BYTES).min(PAYLOAD_BYTES);
        if &chunk[..8] != b"DOOM-WC1"
            || u32_at(chunk, 8) != 1 || u32_at(chunk, 12) != 32
            || u32_at(chunk, 16) != CHUNK_BYTES as u32
            || u32_at(chunk, 20) != index as u32
            || u32_at(chunk, 24) != length as u32 || u32_at(chunk, 28) != 0
        {
            return Err(ProgramError::InvalidAccountData);
        }
        payloads[index] = &chunk[32..32 + length];
    }
    let cursor = advance(&mut words, &payloads, consumed);
    if cursor == WAD_BYTES {
        let mut digest = [0u8; 32];
        for (index, word) in words.iter().enumerate() {
            digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        if digest != WAD_HASH {
            return Err(ProgramError::Custom(1));
        }
        msg!("DOOM_INIT_WAD_SHA256_OK");
    }
    output[..8].copy_from_slice(INIT_MAGIC);
    output[8..12].copy_from_slice(&1u32.to_le_bytes());
    output[12..16].copy_from_slice(&(cursor as u32).to_le_bytes());
    output[16..48].copy_from_slice(&WAD_HASH);
    output[48] = u8::from(cursor == WAD_BYTES);
    for (index, word) in words.iter().enumerate() {
        output[64 + index * 4..68 + index * 4].copy_from_slice(&word.to_le_bytes());
    }
    for (index, account) in accounts[1..].iter().enumerate() {
        output[96 + index * 32..128 + index * 32].copy_from_slice(account.key.as_ref());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn midstate_matches_standard_hash_across_account_boundaries() {
        let wad: Vec<u8> = (0..WAD_BYTES).map(|i| (i.wrapping_mul(131) >> 5) as u8).collect();
        let payloads: Vec<&[u8]> = wad.chunks(PAYLOAD_BYTES).collect();
        let payloads: [&[u8]; 5] = payloads.try_into().unwrap();
        let mut words = IV;
        let mut cursor = 0;
        while cursor < WAD_BYTES {
            cursor = advance(&mut words, &payloads, cursor);
        }
        let actual: Vec<u8> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
        assert_eq!(actual, Sha256::digest(&wad).to_vec());
    }
}
