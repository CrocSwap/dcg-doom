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
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
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
        accounts[1].try_borrow_data()?,
        accounts[2].try_borrow_data()?,
        accounts[3].try_borrow_data()?,
        accounts[4].try_borrow_data()?,
        accounts[5].try_borrow_data()?,
    ];
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

pub const DIRECTORY_MAGIC: &[u8; 8] = b"DOOM-DI1";
pub const DIRECTORY_WINDOW: usize = 64;

fn wad_read(payloads: &[&[u8]; 5], offset: usize, output: &mut [u8]) -> ProgramResult {
    if offset > WAD_BYTES || output.len() > WAD_BYTES - offset {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut cursor = offset;
    let mut filled = 0;
    while filled < output.len() {
        let index = cursor / PAYLOAD_BYTES;
        let start = cursor % PAYLOAD_BYTES;
        let source = payloads[index]
            .get(start..)
            .ok_or(ProgramError::InvalidAccountData)?;
        let take = source.len().min(output.len() - filled);
        if take == 0 {
            return Err(ProgramError::InvalidAccountData);
        }
        output[filled..filled + take].copy_from_slice(&source[..take]);
        cursor += take;
        filled += take;
    }
    Ok(())
}

fn directory_step(config: &mut [u8], payloads: &[&[u8]; 5], cursor: usize) -> ProgramResult {
    if config.len() != CONFIG_BYTES || &config[..8] != INIT_MAGIC || u32_at(config, 8) != 1 {
        return Err(ProgramError::InvalidAccountData);
    }
    if config[48] != 1 || u32_at(config, 12) as usize != WAD_BYTES {
        return Err(ProgramError::Custom(3));
    }
    if config[16..48] != WAD_HASH {
        return Err(ProgramError::Custom(1));
    }
    for (index, word) in WAD_HASH.chunks_exact(4).enumerate() {
        if u32_at(config, 64 + index * 4) != u32::from_be_bytes(word.try_into().unwrap()) {
            return Err(ProgramError::Custom(1));
        }
    }
    let fresh = config[256..288].iter().all(|&b| b == 0);
    if fresh {
        if cursor != 0 {
            return Err(ProgramError::Custom(2));
        }
    } else if &config[256..264] != DIRECTORY_MAGIC || u32_at(config, 264) != 1 {
        return Err(ProgramError::InvalidAccountData);
    } else if u32_at(config, 268) as usize != cursor || config[280] != 0 {
        return Err(ProgramError::Custom(2));
    }
    let mut header = [0; 12];
    wad_read(payloads, 0, &mut header)?;
    let count = u32_at(&header, 4) as usize;
    let directory = u32_at(&header, 8) as usize;
    if &header[..4] != b"IWAD"
        || count == 0
        || directory < 12
        || directory > WAD_BYTES
        || count > (WAD_BYTES - directory) / 16
        || cursor >= count
        || cursor % DIRECTORY_WINDOW != 0
    {
        return Err(ProgramError::InvalidAccountData);
    }
    if !fresh
        && (u32_at(config, 272) as usize != count || u32_at(config, 276) as usize != directory)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let end = (cursor + DIRECTORY_WINDOW).min(count);
    for entry in cursor..end {
        let mut lump = [0; 16];
        wad_read(payloads, directory + entry * 16, &mut lump)?;
        let offset = u32_at(&lump, 0) as usize;
        let length = u32_at(&lump, 4) as usize;
        if offset > WAD_BYTES || length > WAD_BYTES - offset {
            return Err(ProgramError::InvalidAccountData);
        }
    }
    config[256..264].copy_from_slice(DIRECTORY_MAGIC);
    config[264..268].copy_from_slice(&1u32.to_le_bytes());
    config[268..272].copy_from_slice(&(end as u32).to_le_bytes());
    config[272..276].copy_from_slice(&(count as u32).to_le_bytes());
    config[276..280].copy_from_slice(&(directory as u32).to_le_bytes());
    config[280] = u8::from(end == count);
    Ok(())
}

pub fn process_directory(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() != 16 || &data[..8] != DIRECTORY_MAGIC || u32_at(data, 8) != 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if accounts.len() != 6 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let config = &accounts[0];
    if config.owner != program || !config.is_signer || !config.is_writable {
        return Err(ProgramError::InvalidAccountOwner);
    }
    if config.data_len() != CONFIG_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut output = config.try_borrow_mut_data()?;
    for (index, chunk) in accounts[1..].iter().enumerate() {
        if chunk.owner != program
            || chunk.is_writable
            || chunk.data_len() != CHUNK_BYTES
            || output[96 + index * 32..128 + index * 32] != chunk.key.to_bytes()
            || accounts[..index + 1].iter().any(|a| a.key == chunk.key)
        {
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
    directory_step(&mut output, &payloads, u32_at(data, 12) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn directory_continuation_refuses_invalid_lifecycle_and_bounds_without_writes() {
        let mut wad = vec![0; WAD_BYTES];
        wad[..4].copy_from_slice(b"IWAD");
        wad[4..8].copy_from_slice(&65u32.to_le_bytes());
        let directory = PAYLOAD_BYTES - 8;
        wad[8..12].copy_from_slice(&(directory as u32).to_le_bytes());
        let mut config = vec![0; CONFIG_BYTES];
        config[..8].copy_from_slice(INIT_MAGIC);
        config[8..12].copy_from_slice(&1u32.to_le_bytes());
        config[12..16].copy_from_slice(&(WAD_BYTES as u32).to_le_bytes());
        config[16..48].copy_from_slice(&WAD_HASH);
        config[48] = 1;
        for (index, word) in WAD_HASH.chunks_exact(4).enumerate() {
            config[64 + index * 4..68 + index * 4]
                .copy_from_slice(&u32::from_be_bytes(word.try_into().unwrap()).to_le_bytes());
        }
        let payloads: [&[u8]; 5] = wad
            .chunks(PAYLOAD_BYTES)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        for (offset, error) in [
            (8, ProgramError::InvalidAccountData),
            (48, ProgramError::Custom(3)),
            (64, ProgramError::Custom(1)),
        ] {
            let mut bad = config.clone();
            bad[offset] ^= 1;
            let before = bad.clone();
            assert_eq!(directory_step(&mut bad, &payloads, 0), Err(error));
            assert_eq!(bad, before);
        }
        let initial = config.clone();
        assert_eq!(
            directory_step(&mut config, &payloads, 64),
            Err(ProgramError::Custom(2))
        );
        assert_eq!(config, initial);
        directory_step(&mut config, &payloads, 0).unwrap();
        assert_eq!(u32_at(&config, 268), 64);
        assert_eq!(config[280], 0);
        let before = config.clone();
        assert_eq!(
            directory_step(&mut config, &payloads, 0),
            Err(ProgramError::Custom(2))
        );
        assert_eq!(config, before);
        directory_step(&mut config, &payloads, 64).unwrap();
        assert_eq!(u32_at(&config, 268), 65);
        assert_eq!(config[280], 1);
        let before = config.clone();
        assert_eq!(
            directory_step(&mut config, &payloads, 64),
            Err(ProgramError::Custom(2))
        );
        assert_eq!(config, before);
        wad[directory..directory + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let payloads = wad
            .chunks(PAYLOAD_BYTES)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        config = initial.clone();
        assert_eq!(
            directory_step(&mut config, &payloads, 0),
            Err(ProgramError::InvalidAccountData)
        );
        assert_eq!(config, initial);
    }

    #[test]
    fn midstate_matches_standard_hash_across_account_boundaries() {
        let wad: Vec<u8> = (0..WAD_BYTES)
            .map(|i| (i.wrapping_mul(131) >> 5) as u8)
            .collect();
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
