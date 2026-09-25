//! DOOM-STG/1: seal-once staging of the E2R/5 input accounts.
//!
//! ProgramTest injects the captured RI2 resource, WAD, ST1, MP1 and CI2
//! accounts at genesis.  On a real cluster a program-owned account can only be
//! written by the program, so the transport profile carries this one upload
//! path.  Its rules make every staged account immutable to it once complete:
//!
//! * the target must sign: only the holder of the key that created the
//!   account can stage it;
//! * the target's size selects its kind (RI2 334,384; WC1 1,048,576;
//!   ST1 126,976; MP1 65,536; CI2 64) -- the context, continuation, frame,
//!   meta and input buffer have other sizes and are never stageable;
//! * a write is accepted only while bytes 0..8 are still zero, and a write
//!   touching bytes 0..8 must start at 0 and carry the kind's magic.  Bodies
//!   are staged first and the header chunk last; that chunk seals the
//!   account.
//!
//! So after sealing, only E2R/5 (which authenticates these bytes against its
//! continuation) and SIM/2 ADVANCE (ST1/MP1 only, under the committed root)
//! can change them.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    program_memory::sol_memcpy, pubkey::Pubkey,
};

use crate::resource_sbf;

pub const MAGIC: &[u8; 8] = b"DOOM-STG";
pub const VERSION: u32 = 1;
pub const HEADER_BYTES: usize = 20;
pub const MAX_CHUNK_BYTES: usize = 1_024;
pub const ABI_DESCRIPTOR: &str = "DOOM-STG/1|header=20|accounts=target(signer,writable)|seal=offset0-magic-last|kinds=RI2:261056,WC1:1048576,ST1:126976,MP1:65536,CI2:64";

pub const REFUSE_ACCOUNT: u32 = 290;
pub const REFUSE_SEALED: u32 = 291;
pub const REFUSE_RANGE: u32 = 292;
pub const REFUSE_MAGIC: u32 = 293;
pub const REFUSE_MALFORMED: u32 = 294;

pub fn kind_magic(len: usize) -> Option<&'static [u8; 8]> {
    match len {
        resource_sbf::RI2_RESOURCE_IMAGE_BYTES => Some(b"DOOM-RI2"),
        resource_sbf::WAD_ACCOUNT_BYTES => Some(b"DOOM-WC1"),
        resource_sbf::STATE_MAX_BYTES => Some(b"DOOM-ST1"),
        resource_sbf::MIRROR_BYTES => Some(b"DOOM-MP1"),
        resource_sbf::CONTROLLER_BYTES => Some(b"DOOM-CI2"),
        _ => None,
    }
}

pub fn encode(offset: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES + bytes.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(bytes);
    out
}

pub fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < HEADER_BYTES
        || &data[..8] != MAGIC
        || u32::from_le_bytes(data[8..12].try_into().unwrap()) != VERSION
        || data[18..20] != [0, 0]
    {
        return Err(ProgramError::Custom(REFUSE_MALFORMED));
    }
    let offset = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
    let len = u16::from_le_bytes([data[16], data[17]]) as usize;
    if len == 0 || len > MAX_CHUNK_BYTES || data.len() != HEADER_BYTES + len {
        return Err(ProgramError::Custom(REFUSE_MALFORMED));
    }
    let [target] = accounts else {
        return Err(ProgramError::Custom(REFUSE_ACCOUNT));
    };
    let magic = kind_magic(target.data_len());
    if target.owner != program || !target.is_writable || !target.is_signer || magic.is_none() {
        return Err(ProgramError::Custom(REFUSE_ACCOUNT));
    }
    let mut bytes = target.try_borrow_mut_data()?;
    if bytes[..8] != [0; 8] {
        return Err(ProgramError::Custom(REFUSE_SEALED));
    }
    if offset.checked_add(len).is_none_or(|end| end > bytes.len()) {
        return Err(ProgramError::Custom(REFUSE_RANGE));
    }
    let chunk = &data[HEADER_BYTES..];
    if offset < 8 && (offset != 0 || len < 8 || chunk[..8] != magic.unwrap()[..]) {
        return Err(ProgramError::Custom(REFUSE_MAGIC));
    }
    sol_memcpy(&mut bytes[offset..offset + len], chunk, len);
    Ok(())
}
