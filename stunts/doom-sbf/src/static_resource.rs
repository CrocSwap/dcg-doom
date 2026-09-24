//! DOOM-RSR/1: build renderer resources once into an account-resident C layout.
//!
//! The static resource account is passed at account ordinal zero. The existing
//! constant-address guard pins its data base to `DOOM_CTX`; its native pointers
//! therefore remain account-relative across instructions. RSR/1 records the
//! map/table/cache root offsets and fixes those roots from the account base on
//! every render. The dynamic input remains the existing DOOM-RSN/3 payload.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, hash::hashv, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::{render_snapshot_v3 as rsn3, resource_sbf};

pub const MAGIC: &[u8; 8] = b"DOOM-RSR";
pub const VERSION: u32 = 1;
pub const OP_BUILD: u8 = 0;
pub const OP_RENDER: u8 = 1;
pub const REQUEST_BYTES: usize = 52;
pub const HEADER_BYTES: usize = 320;
pub const ROOTS_OFFSET: usize = 160;
pub const ROOT_COUNT: usize = 32;
pub const ROOT_BYTES: usize = ROOT_COUNT * 4;
pub const ACCOUNT_BYTES: usize = resource_sbf::CONTEXT_BYTES;
pub const HEADER_OFFSET: usize = resource_sbf::BOOTSTRAP_OFFSET;
pub const STATIC_SEED: &[u8] = b"DOOM-STATIC-RESOURCE/1";
pub const REFUSE_MALFORMED: u32 = 301;
pub const REFUSE_ACCOUNT: u32 = 302;
pub const REFUSE_BUILT: u32 = 303;
pub const REFUSE_BINDING: u32 = 304;
pub const REFUSE_STATIC: u32 = 305;

pub const WAD_HASH: [u8; 32] = [
    0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9, 0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3, 0xe2, 0x9c,
    0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18, 0x16, 0xf6, 0x52, 0xa5, 0x26, 0xca, 0xc7, 0x71,
];

const H_ACCOUNT_BYTES: usize = 16;
const H_CONTEXT_BYTES: usize = 20;
const H_WAD_OFFSET: usize = 24;
const H_WAD_BYTES: usize = 28;
const H_BOOTSTRAP_OFFSET: usize = 32;
const H_BOOTSTRAP_BYTES: usize = 36;
const H_ROOT_COUNT: usize = 40;
const H_FLAGS: usize = 44;
const H_WAD_HASH: usize = 48;
const H_RESOURCE_HASH: usize = 80;
const H_BASE_ADDRESS: usize = 112;
const H_ABI_ID: usize = 120;
const H_BUILD_ARENA_HIGH_WATER: usize = 152;

const RSR_ABI_DESCRIPTOR: &str =
    "DOOM-RSR/1|account=DOOM_CTX+WAD+bootstrap|roots=u32-account-offsets|base=DOOM_CTX-fixed|pin=WAD-SHA256|dynamic=DOOM-RSN/3";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub operation: u8,
    pub strip_id: u8,
    pub cursor: u32,
    pub snapshot_hash: [u8; 32],
}

impl Request {
    pub const fn build() -> Self {
        Self {
            operation: OP_BUILD,
            strip_id: 0,
            cursor: 0,
            snapshot_hash: [0; 32],
        }
    }

    pub const fn render(strip_id: u8, cursor: u32, snapshot_hash: [u8; 32]) -> Self {
        Self {
            operation: OP_RENDER,
            strip_id,
            cursor,
            snapshot_hash,
        }
    }

    pub fn encode(self) -> [u8; REQUEST_BYTES] {
        let mut out = [0; REQUEST_BYTES];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12] = self.operation;
        out[13] = self.strip_id;
        out[16..20].copy_from_slice(&self.cursor.to_le_bytes());
        out[20..52].copy_from_slice(&self.snapshot_hash);
        out
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != REQUEST_BYTES
            || &data[..8] != MAGIC
            || u32_at(data, 8) != VERSION
            || data[14..16] != [0, 0]
            || !matches!(data[12], OP_BUILD | OP_RENDER)
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        let request = Self {
            operation: data[12],
            strip_id: data[13],
            cursor: u32_at(data, 16),
            snapshot_hash: data[20..52].try_into().unwrap(),
        };
        if request.operation == OP_BUILD
            && (request.strip_id != 0 || request.cursor != 0 || request.snapshot_hash != [0; 32])
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        if request.operation == OP_RENDER && request.strip_id >= 8 {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(request)
    }
}

pub fn static_address(program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[STATIC_SEED, &WAD_HASH], program)
}

fn abi_id() -> [u8; 32] {
    hashv(&[RSR_ABI_DESCRIPTOR.as_bytes()]).to_bytes()
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn owned(program: &Pubkey, account: &AccountInfo, bytes: usize, writable: bool) -> bool {
    account.owner == program && account.data_len() == bytes && account.is_writable == writable
}

fn distinct(accounts: &[AccountInfo]) -> bool {
    accounts
        .iter()
        .enumerate()
        .all(|(i, account)| !accounts[..i].iter().any(|prior| prior.key == account.key))
}

fn validate_static_account(
    program: &Pubkey,
    account: &AccountInfo,
    writable: bool,
) -> ProgramResult {
    let (expected, _) = static_address(program);
    if account.key != &expected || !owned(program, account, ACCOUNT_BYTES, writable) {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    Ok(())
}

fn validate_header(data: &[u8]) -> Result<([u8; 32], [u8; 32]), ProgramError> {
    let end = HEADER_OFFSET
        .checked_add(HEADER_BYTES)
        .ok_or(refuse(REFUSE_STATIC))?;
    if data.len() != ACCOUNT_BYTES || end > data.len() {
        return Err(refuse(REFUSE_STATIC));
    }
    let header = &data[HEADER_OFFSET..end];
    let expected_bootstrap_bytes = resource_sbf::BOOTSTRAP_BYTES as u32;
    if &header[..8] != MAGIC
        || u32_at(header, 8) != VERSION
        || u32_at(header, 12) as usize != HEADER_BYTES
        || u32_at(header, H_ACCOUNT_BYTES) as usize != ACCOUNT_BYTES
        || u32_at(header, H_CONTEXT_BYTES) as usize != resource_sbf::CTX_STRUCT_BYTES
        || u32_at(header, H_WAD_OFFSET) as usize != resource_sbf::WAD_IMAGE_OFFSET
        || u32_at(header, H_WAD_BYTES) as usize != resource_sbf::WAD_IMAGE_BYTES
        || u32_at(header, H_BOOTSTRAP_OFFSET) as usize != HEADER_OFFSET
        || u32_at(header, H_BOOTSTRAP_BYTES) != expected_bootstrap_bytes
        || u32_at(header, H_ROOT_COUNT) as usize != ROOT_COUNT
        || u32_at(header, H_FLAGS) != 1
        || header[H_WAD_HASH..H_WAD_HASH + 32] != WAD_HASH
        || header[H_ABI_ID..H_ABI_ID + 32] != abi_id()
        || header[156..160] != [0, 0, 0, 0]
        || header[ROOTS_OFFSET + ROOT_BYTES..HEADER_BYTES]
            .iter()
            .any(|byte| *byte != 0)
        || header[ROOTS_OFFSET..ROOTS_OFFSET + ROOT_BYTES]
            .chunks_exact(4)
            .any(|raw| {
                let offset = u32::from_le_bytes(raw.try_into().unwrap());
                offset != u32::MAX && offset as usize >= ACCOUNT_BYTES
            })
    {
        return Err(refuse(REFUSE_STATIC));
    }
    let resource_hash = header[H_RESOURCE_HASH..H_RESOURCE_HASH + 32]
        .try_into()
        .unwrap();
    let wad_hash = header[H_WAD_HASH..H_WAD_HASH + 32].try_into().unwrap();
    Ok((wad_hash, resource_hash))
}

/// Read the WAD and RI2 commitments from a built static-resource account.
/// The account key and owner are checked before its header is trusted.
pub fn snapshot_binding_hashes(
    program: &Pubkey,
    account: &AccountInfo,
) -> Result<([u8; 32], [u8; 32]), ProgramError> {
    validate_static_account(program, account, false)?;
    let data = account.try_borrow_data()?;
    validate_header(&data)
}

fn process_build(program: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    const STATIC: usize = 0;
    const RESOURCE: usize = 1;
    const WAD_FIRST: usize = 2;
    if accounts.len() != WAD_FIRST + resource_sbf::WAD_ACCOUNT_COUNT || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    validate_static_account(program, &accounts[STATIC], true)?;
    if accounts.iter().any(|account| account.is_signer)
        || !owned(
            program,
            &accounts[RESOURCE],
            resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES,
            false,
        )
    {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    resource_sbf::validate_resource_header_ri2(
        &accounts[RESOURCE],
        resource_sbf::RI2_RESOURCE_ACCOUNT_BYTES,
    )?;
    let resource_data = accounts[RESOURCE].try_borrow_data()?;
    if resource_data[72..104] != WAD_HASH {
        return Err(refuse(REFUSE_BINDING));
    }
    let resource_hash = hashv(&[&resource_data]).to_bytes();

    let mut static_data = accounts[STATIC].try_borrow_mut_data()?;
    if static_data[HEADER_OFFSET..HEADER_OFFSET + 8]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(refuse(REFUSE_BUILT));
    }
    let wad_start = resource_sbf::WAD_IMAGE_OFFSET;
    let wad_end = wad_start + resource_sbf::WAD_IMAGE_BYTES;
    let mut wad_cursor = wad_start;
    let zeroes = [0u8; 256];
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        let account = &accounts[WAD_FIRST + ordinal];
        if !owned(program, account, resource_sbf::WAD_ACCOUNT_BYTES, false) {
            return Err(refuse(REFUSE_ACCOUNT));
        }
        resource_sbf::validate_wad_account(account, ordinal, &zeroes)?;
        let wad_data = account.try_borrow_data()?;
        let payload_bytes = u32_at(&wad_data, 24) as usize;
        let payload_end = resource_sbf::WAD_HEADER_BYTES + payload_bytes;
        let destination_end = wad_cursor + payload_bytes;
        if destination_end > wad_end {
            return Err(refuse(REFUSE_BINDING));
        }
        static_data[wad_cursor..destination_end]
            .copy_from_slice(&wad_data[resource_sbf::WAD_HEADER_BYTES..payload_end]);
        wad_cursor = destination_end;
    }
    if wad_cursor != wad_end || hashv(&[&static_data[wad_start..wad_end]]).to_bytes() != WAD_HASH {
        return Err(refuse(REFUSE_BINDING));
    }

    let (before_header, rest) = static_data.split_at_mut(HEADER_OFFSET);
    let (header, bootstrap) = rest.split_at_mut(HEADER_BYTES);
    let context = before_header.as_mut_ptr();
    let wad = &before_header[wad_start..wad_end];
    let resource = resource_data.as_ptr();
    let resource_bytes = resource_data.len();
    let bootstrap_len = bootstrap.len();
    let bootstrap_ptr = bootstrap.as_mut_ptr();
    let header_ptr = header.as_mut_ptr();
    let header_len = header.len();
    let mut arena_high_water = 0usize;
    #[cfg(all(feature = "ctx-engine", target_os = "solana"))]
    let rc = unsafe {
        doom_engine_rsn3_build_static(
            context,
            ACCOUNT_BYTES,
            resource,
            resource_bytes,
            wad.as_ptr(),
            wad.len(),
            bootstrap_ptr,
            bootstrap_len,
            header_ptr,
            header_len,
            &mut arena_high_water,
        )
    };
    #[cfg(not(all(feature = "ctx-engine", target_os = "solana")))]
    let rc: i32 = -1;
    if rc != 0 {
        return Err(ProgramError::Custom(rc.unsigned_abs()));
    }
    let base_address = header[H_BASE_ADDRESS..H_BASE_ADDRESS + 8].to_vec();
    header[..ROOTS_OFFSET].fill(0);
    header[..8].copy_from_slice(MAGIC);
    put_u32(header, 8, VERSION);
    put_u32(header, 12, HEADER_BYTES as u32);
    put_u32(header, H_ACCOUNT_BYTES, ACCOUNT_BYTES as u32);
    put_u32(
        header,
        H_CONTEXT_BYTES,
        resource_sbf::CTX_STRUCT_BYTES as u32,
    );
    put_u32(header, H_WAD_OFFSET, resource_sbf::WAD_IMAGE_OFFSET as u32);
    put_u32(header, H_WAD_BYTES, resource_sbf::WAD_IMAGE_BYTES as u32);
    put_u32(header, H_BOOTSTRAP_OFFSET, HEADER_OFFSET as u32);
    put_u32(
        header,
        H_BOOTSTRAP_BYTES,
        resource_sbf::BOOTSTRAP_BYTES as u32,
    );
    put_u32(header, H_ROOT_COUNT, ROOT_COUNT as u32);
    put_u32(header, H_FLAGS, 1);
    header[H_WAD_HASH..H_WAD_HASH + 32].copy_from_slice(&WAD_HASH);
    header[H_RESOURCE_HASH..H_RESOURCE_HASH + 32].copy_from_slice(&resource_hash);
    header[H_BASE_ADDRESS..H_BASE_ADDRESS + 8].copy_from_slice(&base_address);
    header[H_ABI_ID..H_ABI_ID + 32].copy_from_slice(&abi_id());
    put_u32(
        header,
        H_BUILD_ARENA_HIGH_WATER,
        arena_high_water.min(u32::MAX as usize) as u32,
    );
    if header[H_BASE_ADDRESS..H_BASE_ADDRESS + 8]
        .iter()
        .all(|byte| *byte == 0)
    {
        return Err(refuse(REFUSE_STATIC));
    }
    Ok(())
}

fn process_render(program: &Pubkey, accounts: &[AccountInfo], request: Request) -> ProgramResult {
    const STATIC: usize = 0;
    const SNAPSHOT: usize = 1;
    const STRIP: usize = 2;
    if accounts.len() != 3 || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    validate_static_account(program, &accounts[STATIC], true)?;
    if accounts.iter().any(|account| account.is_signer)
        || !owned(program, &accounts[SNAPSHOT], rsn3::SNAPSHOT_BYTES, false)
        || !owned(program, &accounts[STRIP], rsn3::STRIP_ACCOUNT_BYTES, true)
    {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    let (expected_strip, _) =
        rsn3::strip_address(program, accounts[SNAPSHOT].key, request.strip_id);
    if accounts[STRIP].key != &expected_strip {
        return Err(refuse(REFUSE_BINDING));
    }
    let snapshot_data = accounts[SNAPSHOT].try_borrow_data()?;
    let snapshot = rsn3::validate_snapshot(&snapshot_data)?;
    if snapshot.cursor != request.cursor || snapshot.snapshot_hash != request.snapshot_hash {
        return Err(refuse(REFUSE_BINDING));
    }
    let (static_wad_hash, static_resource_hash) = {
        let data = accounts[STATIC].try_borrow_data()?;
        validate_header(&data)?
    };
    if static_wad_hash != snapshot.wad_hash || static_resource_hash != snapshot.resource_hash {
        return Err(refuse(REFUSE_BINDING));
    }

    let mut static_data = accounts[STATIC].try_borrow_mut_data()?;
    let mut strip_data = accounts[STRIP].try_borrow_mut_data()?;
    let (before_header, rest) = static_data.split_at_mut(HEADER_OFFSET);
    let (header, bootstrap) = rest.split_at_mut(HEADER_BYTES);
    let context = before_header.as_mut_ptr();
    let bootstrap_ptr = bootstrap.as_mut_ptr();
    let bootstrap_len = bootstrap.len();
    let header_ptr = header.as_ptr();
    let header_len = header.len();
    let mut arena_high_water = 0usize;
    #[cfg(all(feature = "ctx-engine", target_os = "solana"))]
    let rc = unsafe {
        doom_engine_rsn3_render_static(
            context,
            ACCOUNT_BYTES,
            header_ptr,
            header_len,
            bootstrap_ptr,
            bootstrap_len,
            snapshot.payload.as_ptr(),
            snapshot.payload.len(),
            u32::from(request.strip_id),
            strip_data[rsn3::STRIP_HEADER_BYTES..].as_mut_ptr(),
            rsn3::STRIP_PIXELS_BYTES,
            &mut arena_high_water,
        )
    };
    #[cfg(not(all(feature = "ctx-engine", target_os = "solana")))]
    let rc: i32 = -1;
    if rc != 0 {
        return Err(ProgramError::Custom(rc.unsigned_abs()));
    }
    strip_data[..rsn3::STRIP_HEADER_BYTES].fill(0);
    strip_data[..8].copy_from_slice(b"DOOM-RS1");
    put_u32(&mut strip_data, 8, 1);
    put_u32(&mut strip_data, 12, rsn3::STRIP_HEADER_BYTES as u32);
    put_u32(&mut strip_data, 16, u32::from(request.strip_id));
    put_u32(&mut strip_data, 20, snapshot.cursor);
    put_u32(&mut strip_data, 24, snapshot.gametic);
    put_u32(&mut strip_data, 28, rsn3::STRIP_PIXELS_BYTES as u32);
    strip_data[32..64].copy_from_slice(&snapshot.snapshot_hash);
    strip_data[64..96].copy_from_slice(&snapshot.state_root);
    let strip_hash = hashv(&[
        rsn3::STRIP_HASH_DOMAIN,
        &u32::from(request.strip_id).to_le_bytes(),
        &snapshot.cursor.to_le_bytes(),
        &snapshot.gametic.to_le_bytes(),
        &snapshot.snapshot_hash,
        &strip_data[rsn3::STRIP_HEADER_BYTES..],
    ])
    .to_bytes();
    strip_data[96..128].copy_from_slice(&strip_hash);
    let _ = arena_high_water;
    Ok(())
}

pub fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let request = Request::decode(data)?;
    match request.operation {
        OP_BUILD => process_build(program, accounts),
        OP_RENDER => process_render(program, accounts, request),
        _ => Err(refuse(REFUSE_MALFORMED)),
    }
}

#[cfg(all(feature = "ctx-engine", target_os = "solana"))]
unsafe extern "C" {
    fn doom_engine_rsn3_build_static(
        context: *mut u8,
        account_bytes: usize,
        resource: *const u8,
        resource_bytes: usize,
        wad_image: *const u8,
        wad_bytes: usize,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        header: *mut u8,
        header_bytes: usize,
        arena_high_water: *mut usize,
    ) -> i32;
    fn doom_engine_rsn3_render_static(
        context: *mut u8,
        account_bytes: usize,
        header: *const u8,
        header_bytes: usize,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        payload: *const u8,
        payload_bytes: usize,
        strip_id: u32,
        strip_out: *mut u8,
        strip_cap: usize,
        arena_high_water: *mut usize,
    ) -> i32;
}

fn refuse(code: u32) -> ProgramError {
    ProgramError::Custom(code)
}
