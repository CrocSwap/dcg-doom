//! DOOM-RSR/2: chunk-build renderer resources in the pinned account layout.
//!
//! The static resource account must be account ordinal zero so SBF maps its
//! data at the measured DOOM_CTX address. RSR/2 persists resumable BUILD
//! phases and validates the pinned base before RENDER. Relocation remains
//! unsupported by the SBF loader because writable .bss is not accepted.

use solana_program::{
    account_info::AccountInfo, bpf_loader_upgradeable, entrypoint::ProgramResult, hash::hashv,
    program::invoke_signed, program_error::ProgramError, pubkey::Pubkey, rent::Rent,
    system_instruction, system_program, sysvar::Sysvar,
};

use crate::{continuation_split_sbf, render_snapshot_v3 as rsn3, resource_sbf};

pub const MAGIC: &[u8; 8] = b"DOOM-RSR";
pub const VERSION: u32 = 2;
pub const OP_BUILD: u8 = 0;
pub const OP_RENDER: u8 = 1;
pub const OP_INIT: u8 = 2;
pub const OP_GROW: u8 = 3;
pub const OP_REBUILD: u8 = 4;
pub const OP_RENDER_BUFFERED: u8 = 5;
pub const REQUEST_BYTES: usize = 52;
pub const HEADER_BYTES: usize = 512;
pub const ROOTS_OFFSET: usize = 160;
pub const ROOT_COUNT: usize = 32;
pub const ROOT_BYTES: usize = ROOT_COUNT * 4;
pub const ACCOUNT_BYTES: usize = resource_sbf::CONTEXT_BYTES;
pub const HEADER_OFFSET: usize = resource_sbf::BOOTSTRAP_OFFSET;
pub const STATIC_SEED: &[u8] = b"DOOM-STATIC-RESOURCE/2";
pub const REFUSE_MALFORMED: u32 = 301;
pub const REFUSE_ACCOUNT: u32 = 302;
pub const REFUSE_BUILT: u32 = 303;
pub const REFUSE_BINDING: u32 = 304;
pub const REFUSE_STATIC: u32 = 305;
pub const REFUSE_AUTHORITY: u32 = 306;

pub const WAD_HASH: [u8; 32] = [
    0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9, 0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3, 0xe2, 0x9c,
    0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18, 0x16, 0xf6, 0x52, 0xa5, 0x26, 0xca, 0xc7, 0x71,
];
const WAD_CHUNK_BYTES: [usize; 5] = [1_048_544, 1_048_544, 1_048_544, 1_048_544, 1_844];
const WAD_CHUNK_HASHES: [[u8; 32]; 5] = [
    [
        0xb5, 0x75, 0xf1, 0x6c, 0x92, 0x6d, 0x32, 0x44, 0x23, 0x0f, 0xbd, 0x34, 0x84, 0xa9, 0x99,
        0x01, 0x30, 0x91, 0x45, 0x57, 0x73, 0x8a, 0xb2, 0x7c, 0x0f, 0x69, 0x3c, 0xcc, 0x97, 0x3f,
        0x9d, 0x25,
    ],
    [
        0xc9, 0x49, 0xad, 0xc4, 0xb0, 0x0c, 0x45, 0xce, 0x2a, 0x08, 0x84, 0xd1, 0xaf, 0xd3, 0x8d,
        0x9c, 0xb1, 0x0d, 0xd5, 0x09, 0xdd, 0x33, 0x94, 0x44, 0xf7, 0x76, 0x64, 0xd4, 0xc9, 0xd2,
        0xf3, 0x43,
    ],
    [
        0xdf, 0x8a, 0x3f, 0x4a, 0x92, 0x23, 0x59, 0x26, 0x39, 0xbe, 0x6c, 0xa2, 0x69, 0x1a, 0x04,
        0x16, 0xa3, 0x8f, 0x4b, 0x92, 0x29, 0x86, 0x14, 0xc3, 0x8e, 0x95, 0xc8, 0x75, 0x98, 0x1d,
        0x22, 0xd1,
    ],
    [
        0x27, 0x37, 0x08, 0x7b, 0xd1, 0xb2, 0x1b, 0x63, 0x49, 0x80, 0xd6, 0x56, 0x99, 0x90, 0xfb,
        0xc1, 0x01, 0xc4, 0x6f, 0xb2, 0xf2, 0xe8, 0xd0, 0x08, 0x30, 0x6a, 0x3b, 0xd1, 0xf4, 0x32,
        0xd4, 0xc0,
    ],
    [
        0x4f, 0x72, 0xa8, 0x3f, 0xba, 0xe0, 0xe6, 0xaa, 0xbd, 0xfd, 0x8b, 0xda, 0xf2, 0x00, 0x08,
        0x98, 0x6d, 0x5d, 0x2c, 0x6d, 0x07, 0x62, 0x07, 0xcf, 0x78, 0x54, 0x1e, 0xd1, 0xb6, 0x96,
        0x23, 0x35,
    ],
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
const BUILD_MAGIC: &[u8; 8] = b"DOOM-BLD";
pub const BUILD_WAD_CHUNKS: u32 = 5;
const BUILD_C_PHASES: u32 = 6;
pub const BUILD_FINAL_CURSOR: u32 = BUILD_WAD_CHUNKS + BUILD_C_PHASES - 1;

const H_BUILD_NEXT: usize = 288;
const H_BUILD_WAD_USED: usize = 292;
const H_BUILD_RESOURCE_HASH: usize = 296;
const H_BUILD_BASE: usize = 328;
const RSR_ABI_DESCRIPTOR: &str = "DOOM-RSR/2|request=52|build=5-WAD-chunks+6-resumable-C-phases|operations=BUILD/0+RENDER/1+INIT/2+GROW/3+REBUILD/4+RENDER-BUFFERED/5|rebuild=4:accounts=program+static+programdata+authority+context+continuation:loader-authority+context-signer|each-tx<=1400000-target|account=DOOM_CTX+WAD+bootstrap|roots=u32-account-offsets|internal-pointers=pinned-DOOM_CTX-base|pin=WAD-SHA256+WAD-slice-SHA256+RI2-SHA256|dynamic=DOOM-RSN/3|account-provision=INIT+COMMUTATIVE-GROW/1";
const MAX_ACCOUNT_GROWTH: usize = 10_240;

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

    pub const fn build_chunk(cursor: u32) -> Self {
        Self {
            operation: OP_BUILD,
            strip_id: 0,
            cursor,
            snapshot_hash: [0; 32],
        }
    }

    pub const fn init() -> Self {
        Self {
            operation: OP_INIT,
            strip_id: 0,
            cursor: 0,
            snapshot_hash: [0; 32],
        }
    }

    pub const fn grow() -> Self {
        Self {
            operation: OP_GROW,
            strip_id: 0,
            cursor: 0,
            snapshot_hash: [0; 32],
        }
    }

    pub const fn rebuild() -> Self {
        Self {
            operation: OP_REBUILD,
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

    pub const fn render_buffered(buffer: u8, strip_id: u8) -> Self {
        Self {
            operation: OP_RENDER_BUFFERED,
            strip_id,
            cursor: buffer as u32,
            snapshot_hash: [0; 32],
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
            || !matches!(
                data[12],
                OP_BUILD | OP_RENDER | OP_INIT | OP_GROW | OP_REBUILD | OP_RENDER_BUFFERED
            )
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
            && (request.strip_id != 0
                || request.cursor > BUILD_FINAL_CURSOR
                || request.snapshot_hash != [0; 32])
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        if request.operation == OP_RENDER && request.strip_id >= 8 {
            return Err(refuse(REFUSE_MALFORMED));
        }
        if request.operation == OP_RENDER_BUFFERED
            && (request.strip_id >= 8
                || request.cursor >= rsn3::BUFFER_COUNT as u32
                || request.snapshot_hash != [0; 32])
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        if matches!(request.operation, OP_INIT | OP_GROW | OP_REBUILD)
            && (request.strip_id != 0 || request.cursor != 0 || request.snapshot_hash != [0; 32])
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(request)
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn buffered_render_request_binds_buffer_without_client_snapshot_fields() {
        let request = Request::render_buffered(1, 6);
        let encoded = request.encode();
        assert_eq!(encoded[12], OP_RENDER_BUFFERED);
        assert_eq!(encoded[13], 6);
        assert_eq!(u32_at(&encoded, 16), 1);
        assert_eq!(encoded[20..52], [0; 32]);
        assert_eq!(Request::decode(&encoded), Ok(request));
        let mut bad = encoded;
        bad[20] = 1;
        assert!(Request::decode(&bad).is_err());
    }

    #[test]
    fn account_provision_requests_round_trip() {
        for request in [Request::init(), Request::grow(), Request::rebuild()] {
            assert_eq!(Request::decode(&request.encode()), Ok(request));
        }
    }

    #[test]
    fn account_provision_requests_reject_nonzero_payload_fields() {
        let mut bytes = Request::grow().encode();
        bytes[16] = 1;
        assert_eq!(Request::decode(&bytes), Err(refuse(REFUSE_MALFORMED)));
    }
}

pub fn static_address(program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[STATIC_SEED, &WAD_HASH], program)
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::CTX_MIN_BYTES;
    use resource_sbf::{ALLOC_ALIGNMENT, WAD_IMAGE_BYTES as WAD_IMAGE_BYTES_LEN};

    /// The static resource PDA is derived from the program, so it survives an
    /// image upgrade, and BOOTSTRAP_OFFSET is derived from `CTX_MIN_BYTES`.
    /// When a patch grows `doom_ctx_t` the header moves, and the two operations
    /// that could re-head the account -- REBUILD/4 and BUILD cursor 0 -- both
    /// read the header at the *current* offset.  A static resource built by the
    /// previous image therefore has bytes where the new header must be, and
    /// neither operation can clear them: REBUILD/4 refuses 303 because they
    /// are not magic, BUILD/0 refuses 303 because they are not zero.
    ///
    /// This is a one-way door on a program-derived account, measured on testnet
    /// (both refusals at the same static PDA, image ecea41a8), so it is pinned
    /// here rather than left to be rediscovered.
    #[test]
    fn a_header_move_makes_the_static_resource_a_one_way_door() {
        // The header lives exactly at the end of the WAD staging image, rounded
        // up to the allocator's alignment.  WAD_IMAGE_BYTES is 4 mod 16, so the
        // rounding is a real 12-byte step and not a no-op that happens to hold.
        assert_eq!(HEADER_OFFSET % ALLOC_ALIGNMENT, 0);
        assert_eq!(CTX_MIN_BYTES % ALLOC_ALIGNMENT, 0);
        assert_ne!(WAD_IMAGE_BYTES_LEN % ALLOC_ALIGNMENT, 0);
        let unaligned = CTX_MIN_BYTES + WAD_IMAGE_BYTES_LEN;
        assert_eq!(HEADER_OFFSET, (unaligned + ALLOC_ALIGNMENT - 1) / ALLOC_ALIGNMENT * ALLOC_ALIGNMENT);
        assert_eq!(ACCOUNT_BYTES, resource_sbf::CONTEXT_BYTES);

        // A grown doom_ctx_t moves the header; the 32-byte case is the one that
        // actually happened, and it must not be silently absorbed.
        let previous = HEADER_OFFSET - 32;
        assert_eq!(previous % ALLOC_ALIGNMENT, 0);
        assert_ne!(previous, HEADER_OFFSET);

        // REBUILD/4's gate: the header at the current offset must already be a
        // built header.  A previous image's header is not there at all.
        let mut account = vec![0u8; ACCOUNT_BYTES];
        account[previous..previous + 8].copy_from_slice(MAGIC);
        let header = &account[HEADER_OFFSET..HEADER_OFFSET + HEADER_BYTES];
        assert!(
            &header[..8] != MAGIC || u32_at(header, 8) != VERSION,
            "a stale header must not satisfy REBUILD/4's built-header gate"
        );

        // BUILD cursor 0's gate: the header at the current offset must be all
        // zero.  A previous image's header leaves the tail non-zero.
        assert!(
            header.iter().any(|byte| *byte != 0),
            "a stale header must not satisfy BUILD cursor 0's zero-header gate"
        );
    }

    /// The static account's own length is `ACCOUNT_BYTES`, and a growth beyond
    /// that is refused, so the account can be grown into a new layout but never
    /// shrunk back out of one.
    #[test]
    fn the_static_account_grows_into_a_layout_and_cannot_shrink_out() {
        assert_eq!(ACCOUNT_BYTES, 9_734_160);
        assert!(ACCOUNT_BYTES > 0);
    }
}

fn process_init(program: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    const STATIC: usize = 0;
    const PAYER: usize = 1;
    const SYSTEM: usize = 2;
    if accounts.len() != 3 || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    let static_account = &accounts[STATIC];
    let payer = &accounts[PAYER];
    let system = &accounts[SYSTEM];
    let (expected, bump) = static_address(program);
    if static_account.key != &expected
        || !static_account.is_writable
        || !payer.is_signer
        || !payer.is_writable
        || system.key != &system_program::id()
    {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    if static_account.owner == program && static_account.data_len() == 0 {
        return Ok(());
    }
    if static_account.owner != &system_program::id() || static_account.data_len() != 0 {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    let rent = Rent::get()?.minimum_balance(ACCOUNT_BYTES);
    let bump_seed = [bump];
    let seeds: &[&[u8]] = &[STATIC_SEED, &WAD_HASH, &bump_seed];
    invoke_signed(
        &system_instruction::create_account(payer.key, static_account.key, rent, 0, program),
        &[payer.clone(), static_account.clone(), system.clone()],
        &[seeds],
    )
}

fn process_grow(program: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let [static_account] = accounts else {
        return Err(refuse(REFUSE_ACCOUNT));
    };
    let (expected, _) = static_address(program);
    let current = static_account.data_len();
    if static_account.key != &expected
        || static_account.owner != program
        || !static_account.is_writable
        || current > ACCOUNT_BYTES
    {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    let next = current
        .saturating_add(MAX_ACCOUNT_GROWTH)
        .min(ACCOUNT_BYTES);
    if next != current {
        static_account.realloc(next, false)?;
    }
    Ok(())
}

fn abi_id() -> [u8; 32] {
    hashv(&[RSR_ABI_DESCRIPTOR.as_bytes()]).to_bytes()
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
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

fn require_upgrade_authority(
    program: &Pubkey,
    authority: &AccountInfo,
    program_data: &AccountInfo,
) -> ProgramResult {
    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let expected = bpf_loader_upgradeable::get_program_data_address(program);
    if program_data.key != &expected || program_data.owner != &bpf_loader_upgradeable::ID {
        return Err(refuse(REFUSE_AUTHORITY));
    }
    let body = program_data.try_borrow_data()?;
    if body.len() < 45
        || body[..4] != [3, 0, 0, 0]
        || body[12] != 1
        || body[13..45] != authority.key.to_bytes()
    {
        return Err(refuse(REFUSE_AUTHORITY));
    }
    Ok(())
}

fn process_rebuild(program: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    const PROGRAM: usize = 0;
    const STATIC: usize = 1;
    const PROGRAM_DATA: usize = 2;
    const AUTHORITY: usize = 3;
    const CONTEXT: usize = 4;
    const CONTINUATION: usize = 5;
    if accounts.len() != 6 || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    if accounts[PROGRAM].key != program
        || accounts[PROGRAM].owner != &bpf_loader_upgradeable::ID
        || !accounts[PROGRAM].executable
        || accounts[PROGRAM].is_writable
        || accounts[PROGRAM].is_signer
        || accounts[PROGRAM_DATA].is_writable
        || accounts[PROGRAM_DATA].is_signer
        || accounts[CONTEXT].owner != program
        || accounts[CONTEXT].data_len() != resource_sbf::CONTEXT_BYTES
        || !accounts[CONTEXT].is_writable
        || !accounts[CONTEXT].is_signer
        || accounts[CONTINUATION].owner != program
        || accounts[CONTINUATION].data_len() != continuation_split_sbf::CONTINUATION_BYTES
        || !accounts[CONTINUATION].is_writable
        || accounts[CONTINUATION].is_signer
    {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    validate_static_account(program, &accounts[STATIC], true)?;
    if accounts[STATIC].is_signer {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    require_upgrade_authority(program, &accounts[AUTHORITY], &accounts[PROGRAM_DATA])?;
    let mut data = accounts[STATIC].try_borrow_mut_data()?;
    let header = &mut data[HEADER_OFFSET..HEADER_OFFSET + HEADER_BYTES];
    if &header[..8] != MAGIC || u32_at(header, 8) != VERSION {
        return Err(refuse(REFUSE_BUILT));
    }
    header.fill(0);
    drop(data);
    accounts[CONTEXT].try_borrow_mut_data()?.fill(0);
    accounts[CONTINUATION].try_borrow_mut_data()?.fill(0);
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
        || u32_at(header, H_FLAGS) != 1
        || u32_at(header, H_BUILD_NEXT) != BUILD_FINAL_CURSOR + 1
        || u32_at(header, H_BUILD_WAD_USED) as usize != resource_sbf::WAD_IMAGE_BYTES
        || header[H_BUILD_RESOURCE_HASH..H_BUILD_RESOURCE_HASH + 32]
            != header[H_RESOURCE_HASH..H_RESOURCE_HASH + 32]
        || u64_at(header, H_BASE_ADDRESS) == 0
        || u64_at(header, H_BUILD_BASE) == 0
        || header[336..HEADER_BYTES].iter().any(|byte| *byte != 0)
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

fn initialize_build_header(header: &mut [u8], resource_hash: &[u8; 32], base: u64) {
    header.fill(0);
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
    header[H_WAD_HASH..H_WAD_HASH + 32].copy_from_slice(&WAD_HASH);
    header[H_RESOURCE_HASH..H_RESOURCE_HASH + 32].copy_from_slice(resource_hash);
    header[H_BUILD_RESOURCE_HASH..H_BUILD_RESOURCE_HASH + 32].copy_from_slice(resource_hash);
    put_u64(header, H_BUILD_BASE, base);
}

fn process_build(program: &Pubkey, accounts: &[AccountInfo], request: Request) -> ProgramResult {
    const STATIC: usize = 0;
    const RESOURCE: usize = 1;
    const WAD: usize = 2;
    let is_wad_chunk = (request.cursor as usize) < BUILD_WAD_CHUNKS as usize;
    if accounts.len() != if is_wad_chunk { 3 } else { 2 } || !distinct(accounts) {
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
    let static_base = static_data.as_ptr() as u64;
    let (pre_header, rest) = static_data.split_at_mut(HEADER_OFFSET);
    let (header, _) = rest.split_at_mut(HEADER_BYTES);
    if request.cursor == 0 {
        if header.iter().any(|byte| *byte != 0) {
            return Err(refuse(REFUSE_BUILT));
        }
        initialize_build_header(header, &resource_hash, static_base);
    } else if &header[..8] != MAGIC
        || u32_at(header, 8) != VERSION
        || u32_at(header, H_BUILD_NEXT) != request.cursor
        || header[H_RESOURCE_HASH..H_RESOURCE_HASH + 32] != resource_hash
        || u64_at(header, H_BUILD_BASE) != static_base
        || u32_at(header, H_FLAGS) != 0
    {
        return Err(refuse(REFUSE_BUILT));
    }

    if is_wad_chunk {
        let ordinal = request.cursor as usize;
        let account = &accounts[WAD];
        if !owned(program, account, resource_sbf::WAD_ACCOUNT_BYTES, false) || account.is_signer {
            return Err(refuse(REFUSE_ACCOUNT));
        }
        let wad_data = account.try_borrow_data()?;
        let payload_bytes = u32_at(&wad_data, 24) as usize;
        if wad_data.len() != resource_sbf::WAD_ACCOUNT_BYTES
            || &wad_data[..8] != b"DOOM-WC1"
            || u32_at(&wad_data, 8) != 1
            || u32_at(&wad_data, 12) as usize != resource_sbf::WAD_HEADER_BYTES
            || u32_at(&wad_data, 16) as usize != resource_sbf::WAD_ACCOUNT_BYTES
            || u32_at(&wad_data, 20) as usize != ordinal
            || payload_bytes != WAD_CHUNK_BYTES[ordinal]
            || hashv(&[&wad_data
                [resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + payload_bytes]])
            .to_bytes()
                != WAD_CHUNK_HASHES[ordinal]
        {
            return Err(refuse(REFUSE_BINDING));
        }
        let used = u32_at(header, H_BUILD_WAD_USED) as usize;
        let destination_end = used
            .checked_add(payload_bytes)
            .ok_or(refuse(REFUSE_BINDING))?;
        if destination_end > resource_sbf::WAD_IMAGE_BYTES {
            return Err(refuse(REFUSE_BINDING));
        }
        let start = resource_sbf::WAD_IMAGE_OFFSET + used;
        pre_header[start..start + payload_bytes].copy_from_slice(
            &wad_data
                [resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + payload_bytes],
        );
        put_u32(header, H_BUILD_WAD_USED, destination_end as u32);
        put_u32(header, H_BUILD_NEXT, request.cursor + 1);
        if ordinal + 1 == resource_sbf::WAD_ACCOUNT_COUNT {
            if destination_end != resource_sbf::WAD_IMAGE_BYTES {
                return Err(refuse(REFUSE_BINDING));
            }
        }
        return Ok(());
    }

    if u32_at(header, H_BUILD_WAD_USED) as usize != resource_sbf::WAD_IMAGE_BYTES {
        return Err(refuse(REFUSE_BUILT));
    }
    let phase = request.cursor - BUILD_WAD_CHUNKS;
    let (before_header, rest) = static_data.split_at_mut(HEADER_OFFSET);
    let (header, bootstrap) = rest.split_at_mut(HEADER_BYTES);
    let context = before_header.as_mut_ptr();
    let resource = resource_data.as_ptr();
    let resource_bytes = resource_data.len();
    let wad = &before_header[resource_sbf::WAD_IMAGE_OFFSET
        ..resource_sbf::WAD_IMAGE_OFFSET + resource_sbf::WAD_IMAGE_BYTES];
    let bootstrap_ptr = bootstrap.as_mut_ptr();
    let bootstrap_len = bootstrap.len();
    let header_ptr = header.as_mut_ptr();
    let header_len = header.len();
    let mut arena_high_water = 0usize;
    #[cfg(all(feature = "ctx-engine", target_os = "solana"))]
    let rc = unsafe {
        doom_engine_rsn3_build_static_phase(
            phase,
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
    if phase == BUILD_C_PHASES - 1 {
        let roots = header[ROOTS_OFFSET..ROOTS_OFFSET + ROOT_BYTES].to_vec();
        let base = u64_at(header, H_BASE_ADDRESS);
        header.fill(0);
        header[ROOTS_OFFSET..ROOTS_OFFSET + ROOT_BYTES].copy_from_slice(&roots);
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
        header[H_ABI_ID..H_ABI_ID + 32].copy_from_slice(&abi_id());
        put_u32(
            header,
            H_BUILD_ARENA_HIGH_WATER,
            arena_high_water.min(u32::MAX as usize) as u32,
        );
        put_u32(header, H_BUILD_NEXT, BUILD_FINAL_CURSOR + 1);
        put_u32(
            header,
            H_BUILD_WAD_USED,
            resource_sbf::WAD_IMAGE_BYTES as u32,
        );
        header[H_BUILD_RESOURCE_HASH..H_BUILD_RESOURCE_HASH + 32].copy_from_slice(&resource_hash);
        put_u64(header, H_BUILD_BASE, static_base);
        put_u64(header, H_BASE_ADDRESS, base);
    } else {
        put_u32(header, H_BUILD_NEXT, request.cursor + 1);
        put_u32(
            header,
            H_BUILD_WAD_USED,
            resource_sbf::WAD_IMAGE_BYTES as u32,
        );
        put_u32(header, H_FLAGS, 0);
        header[H_BUILD_RESOURCE_HASH..H_BUILD_RESOURCE_HASH + 32].copy_from_slice(&resource_hash);
        put_u64(header, H_BUILD_BASE, static_base);
    }
    Ok(())
}

fn render_snapshot_to_strip(
    program: &Pubkey,
    static_account: &AccountInfo,
    strip_account: &AccountInfo,
    snapshot: rsn3::SnapshotView<'_>,
    strip_id: u8,
) -> ProgramResult {
    let (static_wad_hash, static_resource_hash) = {
        let data = static_account.try_borrow_data()?;
        let hashes = validate_header(&data)?;
        if u64_at(&data, HEADER_OFFSET + H_BASE_ADDRESS) != data.as_ptr() as u64 {
            return Err(refuse(REFUSE_BINDING));
        }
        hashes
    };
    if static_wad_hash != snapshot.wad_hash || static_resource_hash != snapshot.resource_hash {
        return Err(refuse(REFUSE_BINDING));
    }

    let mut static_data = static_account.try_borrow_mut_data()?;
    let mut strip_data = strip_account.try_borrow_mut_data()?;
    let (before_header, rest) = static_data.split_at_mut(HEADER_OFFSET);
    let (header, bootstrap) = rest.split_at_mut(HEADER_BYTES);
    let context = before_header.as_mut_ptr();
    let bootstrap_ptr = bootstrap.as_mut_ptr();
    let bootstrap_len = bootstrap.len();
    let header_ptr = header.as_ptr();
    let header_len = header.len();
    let mut arena_high_water = 0usize;
    crate::diagnostic_phase(1203);
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
            u32::from(strip_id),
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
    crate::diagnostic_phase(1204);
    strip_data[..rsn3::STRIP_HEADER_BYTES].fill(0);
    strip_data[..8].copy_from_slice(b"DOOM-RS1");
    put_u32(&mut strip_data, 8, 1);
    put_u32(&mut strip_data, 12, rsn3::STRIP_HEADER_BYTES as u32);
    put_u32(&mut strip_data, 16, u32::from(strip_id));
    put_u32(&mut strip_data, 20, snapshot.cursor);
    put_u32(&mut strip_data, 24, snapshot.gametic);
    put_u32(&mut strip_data, 28, rsn3::STRIP_PIXELS_BYTES as u32);
    strip_data[32..64].copy_from_slice(&snapshot.snapshot_hash);
    strip_data[64..96].copy_from_slice(&snapshot.state_root);
    let strip_hash = hashv(&[
        rsn3::STRIP_HASH_DOMAIN,
        &u32::from(strip_id).to_le_bytes(),
        &snapshot.cursor.to_le_bytes(),
        &snapshot.gametic.to_le_bytes(),
        &snapshot.snapshot_hash,
        &strip_data[rsn3::STRIP_HEADER_BYTES..],
    ])
    .to_bytes();
    strip_data[96..128].copy_from_slice(&strip_hash);
    let _ = (program, arena_high_water);
    Ok(())
}

fn process_render(program: &Pubkey, accounts: &[AccountInfo], request: Request) -> ProgramResult {
    const STATIC: usize = 0;
    const SNAPSHOT: usize = 1;
    const STRIP: usize = 2;
    if request.strip_id >= 8 || accounts.len() != 3 || !distinct(accounts) {
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
    render_snapshot_to_strip(
        program,
        &accounts[STATIC],
        &accounts[STRIP],
        snapshot,
        request.strip_id,
    )
}

fn process_render_buffered(
    program: &Pubkey,
    accounts: &[AccountInfo],
    request: Request,
) -> ProgramResult {
    const STATIC: usize = 0;
    const CONTEXT: usize = 1;
    const SNAPSHOT: usize = 2;
    const STRIP: usize = 3;
    if request.strip_id >= 8
        || request.cursor >= rsn3::BUFFER_COUNT as u32
        || accounts.len() != 4
        || !distinct(accounts)
    {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    validate_static_account(program, &accounts[STATIC], true)?;
    if accounts.iter().any(|account| account.is_signer)
        || !owned(
            program,
            &accounts[CONTEXT],
            resource_sbf::CONTEXT_BYTES,
            false,
        )
        || !owned(program, &accounts[SNAPSHOT], rsn3::SNAPSHOT_BYTES, false)
        || !owned(program, &accounts[STRIP], rsn3::STRIP_ACCOUNT_BYTES, true)
    {
        return Err(refuse(REFUSE_ACCOUNT));
    }
    crate::diagnostic_phase(1201);
    let Some((expected_snapshot, _)) =
        rsn3::buffered_snapshot_address(program, accounts[CONTEXT].key, request.cursor as u8)
    else {
        return Err(refuse(REFUSE_MALFORMED));
    };
    if accounts[SNAPSHOT].key != &expected_snapshot {
        return Err(refuse(REFUSE_BINDING));
    }
    let (expected_strip, _) =
        rsn3::strip_address(program, accounts[SNAPSHOT].key, request.strip_id);
    if accounts[STRIP].key != &expected_strip {
        return Err(refuse(REFUSE_BINDING));
    }
    let snapshot_data = accounts[SNAPSHOT].try_borrow_data()?;
    let snapshot = rsn3::validate_buffered_snapshot_for(&snapshot_data, request.cursor as u8)?;
    crate::diagnostic_phase(1202);
    render_snapshot_to_strip(
        program,
        &accounts[STATIC],
        &accounts[STRIP],
        snapshot,
        request.strip_id,
    )
}

pub fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    crate::diagnostic_phase(1200);
    let request = Request::decode(data)?;
    match request.operation {
        OP_BUILD => process_build(program, accounts, request),
        OP_RENDER => process_render(program, accounts, request),
        OP_RENDER_BUFFERED => process_render_buffered(program, accounts, request),
        OP_INIT => process_init(program, accounts),
        OP_GROW => process_grow(program, accounts),
        OP_REBUILD => process_rebuild(program, accounts),
        _ => Err(refuse(REFUSE_MALFORMED)),
    }
}

#[cfg(all(feature = "ctx-engine", target_os = "solana"))]
unsafe extern "C" {
    fn doom_engine_rsn3_build_static_phase(
        phase: u32,
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
