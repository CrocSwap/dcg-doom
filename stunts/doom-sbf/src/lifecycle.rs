//! DOOM-LIF/2: authority-signed, staged in-place restart and rent recovery.
//!
//! REINIT clears the old game bytes without decoding them, rebuilds a new game
//! from the RI2 image currently loaded in the program, and reuses the same
//! accounts. Large render buffers are cleared one account per instruction.
//! CLOSE uses only SIM2 metadata and the derived renderer PDAs; it never reads
//! ST1 or the pinned C context.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, hash::hashv, program_error::ProgramError,
    program_memory::sol_memcpy, pubkey::Pubkey, system_program,
};

use crate::{
    continuation_split_sbf as e2r, render_snapshot_v3 as rsn3, resource_sbf, sim2, static_resource,
};

pub const MAGIC: &[u8; 8] = b"DOOM-LIF";
pub const VERSION: u32 = 2;
pub const BEGIN: u8 = 1;
pub const CLEAR_RENDER: u8 = 2;
pub const STEP: u8 = 3;
pub const CLOSE: u8 = 4;
pub const REQUEST_BYTES: usize = 24;
pub const REINIT_WAD_STAGES: u32 = resource_sbf::WAD_ACCOUNT_COUNT as u32;
pub const REINIT_RSR_STAGES: u32 = 6;
pub const REINIT_PREPARE_STAGES: u32 = 61;
pub const REINIT_GAME_STAGES: u32 = REINIT_PREPARE_STAGES + 1;
pub const REINIT_FINALIZE_STAGES: u32 = 2;
pub const REINIT_STEP_COUNT: u32 =
    REINIT_WAD_STAGES + REINIT_RSR_STAGES + REINIT_GAME_STAGES + REINIT_FINALIZE_STAGES;

pub const REFUSE_MALFORMED: u32 = 320;
pub const REFUSE_ACCOUNTS: u32 = 321;
pub const REFUSE_AUTHORITY: u32 = 322;
pub const REFUSE_PENDING: u32 = 323;
pub const REFUSE_CLEAR: u32 = 324;
pub const REFUSE_CLOSE: u32 = 325;

const BASE_ACCOUNT_COUNT: usize = 15;
pub const CLOSE_RENDER_TARGETS: usize = 36;
pub const CLOSE_RESIZE_FIRST_PHASE: u32 = CLOSE_RENDER_TARGETS as u32 + 1;
const CLOSE_PRE_SHARED_ACCOUNT_COUNT: usize = 9;
const CLOSE_PRE_SEPARATE_ACCOUNT_COUNT: usize = 10;
const CLOSE_BASE_SHARED_ACCOUNT_COUNT: usize = 4;
const CLOSE_BASE_SEPARATE_ACCOUNT_COUNT: usize = 5;
pub const CLOSE_FINAL_PHASE: u32 = CLOSE_RESIZE_FIRST_PHASE;
const CLOSE_MAGIC: &[u8; 8] = b"DOOM-CL1";
const CLOSE_VERSION: u32 = 1;
const CLOSE_CONTEXT: usize = 16;
const CLOSE_AUTHORITY: usize = 48;
const CLOSE_META: usize = 80;
const CLOSE_PAYER: usize = 112;
const CONTEXT: usize = 0;
const RESOURCE: usize = 1;
const WAD_FIRST: usize = 2;
const STATE: usize = 7;
const MIRROR: usize = 8;
const CONTROLLER: usize = 9;
const FRAME: usize = 10;
const CONTINUATION: usize = 11;
const AUTHORITY: usize = 12;
const META: usize = 13;
const BUFFER: usize = 14;

const MARKER_BYTES: usize = 356;
const MARKER_MAGIC: &[u8; 8] = b"DOOM-RI2";
const MR_PHASE: usize = 12;
const MR_CONTEXT: usize = 16;
const MR_AUTHORITY: usize = 48;
const MR_META: usize = 80;
const MR_BUFFER: usize = 112;
const MR_STATE: usize = 144;
const MR_MIRROR: usize = 176;
const MR_CONTROLLER: usize = 208;
const MR_FRAME: usize = 240;
const MR_CAPACITY: usize = 272;
const MR_BUMP: usize = 276;
const MR_CLEARED: usize = 280;
const MR_RENT_PAYER: usize = 288;
const MR_ARENA_HIGH_WATER: usize = 320;
const MR_GENERATION: usize = 352;
const RENDER_BUFFER_COUNT: usize = 36;

fn refuse(code: u32) -> ProgramError {
    ProgramError::Custom(code)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub operation: u8,
    pub argument: u32,
    pub generation: u32,
}

impl Request {
    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != REQUEST_BYTES
            || &data[..8] != MAGIC
            || u32_at(data, 8) != VERSION
            || !matches!(data[12], BEGIN | CLEAR_RENDER | STEP | CLOSE)
            || data[13..16] != [0; 3]
        {
            return Err(refuse(REFUSE_MALFORMED));
        }
        let argument = u32_at(data, 16);
        let generation = u32_at(data, 20);
        let valid_argument = match data[12] {
            BEGIN => argument == 0 && generation != 0,
            CLEAR_RENDER => argument < RENDER_BUFFER_COUNT as u32 && generation != 0,
            STEP => (1..=REINIT_STEP_COUNT).contains(&argument) && generation != 0,
            CLOSE => argument <= CLOSE_FINAL_PHASE && generation == 0,
            _ => false,
        };
        if !valid_argument {
            return Err(refuse(REFUSE_MALFORMED));
        }
        Ok(Self {
            operation: data[12],
            argument,
            generation,
        })
    }

    pub fn encode(self) -> [u8; REQUEST_BYTES] {
        let mut out = [0; REQUEST_BYTES];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12] = self.operation;
        out[16..20].copy_from_slice(&self.argument.to_le_bytes());
        out[20..24].copy_from_slice(&self.generation.to_le_bytes());
        out
    }
}

fn owned(program: &Pubkey, account: &AccountInfo, bytes: usize, writable: bool) -> bool {
    account.owner == program && account.data_len() == bytes && account.is_writable == writable
}

fn distinct(accounts: &[AccountInfo]) -> bool {
    (0..accounts.len())
        .all(|i| ((i + 1)..accounts.len()).all(|j| accounts[i].key != accounts[j].key))
}

fn buffer_bytes(data: &[u8], offset: usize) -> Result<(usize, u32), ProgramError> {
    let capacity = u32_at(data, offset) as usize;
    let Some(bytes) = sim2::buffer_bytes(capacity) else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    Ok((bytes, capacity as u32))
}

fn read_meta_bindings(
    program: &Pubkey,
    accounts: &[AccountInfo],
) -> Result<(u32, u8, Pubkey), ProgramError> {
    let meta = &accounts[META];
    let data = meta.try_borrow_data()?;
    let version = u32_at(&data, 8);
    let context = &accounts[CONTEXT];
    let expected_authority = &accounts[AUTHORITY];
    let valid_version = version == sim2::VERSION || version == sim2::META_VERSION;
    let (expected_meta, bump) = sim2::meta_address(program, context.key);
    let buffer_key = &accounts[BUFFER].key;
    if !valid_version
        || &data[..8] != b"SIM2META"
        || data.len() != sim2::META_BYTES
        || meta.key != &expected_meta
        || data[sim2::M_BUMP] != bump
        || data[sim2::M_CONTEXT..sim2::M_CONTEXT + 32] != context.key.as_ref()[..]
        || data[sim2::M_STATE..sim2::M_STATE + 32] != accounts[STATE].key.as_ref()[..]
        || data[sim2::M_MIRROR..sim2::M_MIRROR + 32] != accounts[MIRROR].key.as_ref()[..]
        || data[sim2::M_BUFFER..sim2::M_BUFFER + 32] != buffer_key.as_ref()[..]
        || data[sim2::M_AUTHORITY..sim2::M_AUTHORITY + 32] != expected_authority.key.as_ref()[..]
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    if !expected_authority.is_signer {
        return Err(refuse(REFUSE_AUTHORITY));
    }
    let (buffer_len, capacity) = buffer_bytes(&data, sim2::M_CAPACITY)?;
    if accounts[BUFFER].data_len() != buffer_len {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let payer = if version == sim2::META_VERSION {
        let bytes: [u8; 32] = data[sim2::M_RENT_PAYER..sim2::M_RENT_PAYER + 32]
            .try_into()
            .unwrap();
        let payer = Pubkey::new_from_array(bytes);
        if payer == Pubkey::default() || data[372..sim2::META_BYTES].iter().any(|byte| *byte != 0) {
            return Err(refuse(REFUSE_ACCOUNTS));
        }
        payer
    } else {
        Pubkey::new_from_array(expected_authority.key.to_bytes())
    };
    Ok((capacity, bump, payer))
}

fn validate_base(program: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    if accounts.len() != BASE_ACCOUNT_COUNT || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let expected = [
        (CONTEXT, resource_sbf::CONTEXT_BYTES, true),
        (RESOURCE, resource_sbf::RI2_RESOURCE_IMAGE_BYTES, false),
        (STATE, resource_sbf::STATE_MAX_BYTES, true),
        (MIRROR, resource_sbf::MIRROR_BYTES, true),
        (CONTROLLER, resource_sbf::CONTROLLER_BYTES, true),
        (FRAME, resource_sbf::RI2_FRAME_BYTES, true),
        (CONTINUATION, e2r::CONTINUATION_BYTES, true),
        (META, sim2::META_BYTES, true),
    ];
    for (index, bytes, writable) in expected {
        if !owned(program, &accounts[index], bytes, writable) {
            return Err(refuse(REFUSE_ACCOUNTS));
        }
    }
    if accounts[BUFFER].owner != program
        || !accounts[BUFFER].is_writable
        || accounts[AUTHORITY].is_writable
        || !accounts[AUTHORITY].is_signer
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    for index in WAD_FIRST..WAD_FIRST + resource_sbf::WAD_ACCOUNT_COUNT {
        if !owned(
            program,
            &accounts[index],
            resource_sbf::WAD_ACCOUNT_BYTES,
            false,
        ) {
            return Err(refuse(REFUSE_ACCOUNTS));
        }
    }
    e2r::validate_resource(&accounts[RESOURCE].try_borrow_data()?)?;
    for ordinal in 0..resource_sbf::WAD_ACCOUNT_COUNT {
        e2r::validate_wad(&accounts[WAD_FIRST + ordinal].try_borrow_data()?, ordinal)?;
    }
    Ok(())
}

fn meta_data_is_zero(meta: &AccountInfo) -> Result<bool, ProgramError> {
    let data = meta.try_borrow_data()?;
    Ok(data.iter().all(|byte| *byte == 0))
}

fn marker_write(
    data: &mut [u8],
    accounts: &[AccountInfo],
    capacity: u32,
    bump: u8,
    payer: &Pubkey,
    generation: u32,
) {
    data.fill(0);
    data[..8].copy_from_slice(MARKER_MAGIC);
    put_u32(data, 8, VERSION);
    put_u32(data, MR_PHASE, 0);
    for (offset, index) in [
        (MR_CONTEXT, CONTEXT),
        (MR_AUTHORITY, AUTHORITY),
        (MR_META, META),
        (MR_BUFFER, BUFFER),
        (MR_STATE, STATE),
        (MR_MIRROR, MIRROR),
        (MR_CONTROLLER, CONTROLLER),
        (MR_FRAME, FRAME),
    ] {
        data[offset..offset + 32].copy_from_slice(accounts[index].key.as_ref());
    }
    put_u32(data, MR_CAPACITY, capacity);
    data[MR_BUMP] = bump;
    data[MR_RENT_PAYER..MR_RENT_PAYER + 32].copy_from_slice(payer.as_ref());
    put_u32(data, MR_GENERATION, generation);
}

fn marker_matches(data: &[u8], accounts: &[AccountInfo], generation: u32) -> bool {
    data.len() == e2r::CONTINUATION_BYTES
        && &data[..8] == MARKER_MAGIC
        && u32_at(data, 8) == VERSION
        && u32_at(data, MR_GENERATION) == generation
        && data[MR_CONTEXT..MR_CONTEXT + 32] == accounts[CONTEXT].key.as_ref()[..]
        && data[MR_AUTHORITY..MR_AUTHORITY + 32] == accounts[AUTHORITY].key.as_ref()[..]
        && data[MR_META..MR_META + 32] == accounts[META].key.as_ref()[..]
        && data[MR_BUFFER..MR_BUFFER + 32] == accounts[BUFFER].key.as_ref()[..]
        && data[MR_STATE..MR_STATE + 32] == accounts[STATE].key.as_ref()[..]
        && data[MR_MIRROR..MR_MIRROR + 32] == accounts[MIRROR].key.as_ref()[..]
        && data[MR_CONTROLLER..MR_CONTROLLER + 32] == accounts[CONTROLLER].key.as_ref()[..]
        && data[MR_FRAME..MR_FRAME + 32] == accounts[FRAME].key.as_ref()[..]
        && data[MR_GENERATION + 4..MARKER_BYTES]
            .iter()
            .all(|byte| *byte == 0)
}

fn buffer_preinit(data: &mut [u8], capacity: u32) -> ProgramResult {
    let Some(total) = sim2::buffer_bytes(capacity as usize) else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if data.len() != total {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    data.fill(0);
    if total > sim2::MAX_BUFFER_GROWTH {
        data[..8].copy_from_slice(sim2::BUFFER_GROW_MAGIC);
        put_u32(data, 8, sim2::BUFFER_FORMAT_VERSION);
        put_u32(data, 12, capacity);
        put_u32(data, 16, total as u32);
    }
    Ok(())
}

fn init_placeholders(accounts: &[AccountInfo]) -> ProgramResult {
    let mut state = accounts[STATE].try_borrow_mut_data()?;
    state.fill(0);
    state[..8].copy_from_slice(b"DOOM-ST1");
    put_u32(&mut state, 8, 0);
    put_u32(&mut state, 12, 64);
    put_u32(&mut state, 16, 64);
    put_u32(&mut state, 20, 0);
    drop(state);

    let mut mirror = accounts[MIRROR].try_borrow_mut_data()?;
    mirror.fill(0);
    mirror[..8].copy_from_slice(b"DOOM-MP1");
    put_u32(&mut mirror, 8, 1);
    put_u32(&mut mirror, 12, 4096);
    put_u32(&mut mirror, 16, resource_sbf::MIRROR_BYTES as u32);
    put_u32(&mut mirror, 20, 160);
    put_u32(&mut mirror, 24, 384);
    mirror[36..68].copy_from_slice(&e2r::WAD_HASH);
    drop(mirror);

    let mut controller = accounts[CONTROLLER].try_borrow_mut_data()?;
    controller.fill(0);
    controller[..8].copy_from_slice(b"DOOM-CI2");
    put_u32(&mut controller, 8, 1);
    put_u32(&mut controller, 12, 28);
    put_u32(&mut controller, 16, resource_sbf::CONTROLLER_BYTES as u32);
    put_u32(&mut controller, 20, 0);
    drop(controller);

    accounts[FRAME].try_borrow_mut_data()?.fill(0);
    Ok(())
}

fn process_begin(program: &Pubkey, accounts: &[AccountInfo], request: Request) -> ProgramResult {
    validate_base(program, accounts)?;
    if !accounts[META].is_writable || !accounts[BUFFER].is_writable {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    {
        let continuation = accounts[CONTINUATION].try_borrow_data()?;
        if marker_matches(&continuation, accounts, request.generation) {
            return Ok(());
        }
        if meta_data_is_zero(&accounts[META])?
            && continuation[..8] == MARKER_MAGIC[..]
            && u32_at(&continuation, 8) == VERSION
        {
            return Err(refuse(REFUSE_PENDING));
        }
    }
    // BEGIN zeroes SIM2 metadata. A retry of the same generation must match
    // the continuation marker before trying to decode that now-zero metadata.
    let (capacity, bump, rent_payer) = read_meta_bindings(program, accounts)?;
    init_placeholders(accounts)?;
    accounts[CONTEXT].try_borrow_mut_data()?[e2r::CONTEXT_CHECKPOINT_OFFSET
        ..e2r::CONTEXT_CHECKPOINT_OFFSET + e2r::CONTEXT_CHECKPOINT_BYTES]
        .fill(0);
    accounts[META].try_borrow_mut_data()?.fill(0);
    buffer_preinit(&mut accounts[BUFFER].try_borrow_mut_data()?, capacity)?;
    let mut continuation = accounts[CONTINUATION].try_borrow_mut_data()?;
    marker_write(
        &mut continuation,
        accounts,
        capacity,
        bump,
        &rent_payer,
        request.generation,
    );
    Ok(())
}

fn expected_render_account(
    program: &Pubkey,
    context: &Pubkey,
    index: u8,
) -> Result<(Pubkey, usize), ProgramError> {
    if index < 4 {
        let (snapshot, _) = rsn3::buffered_snapshot_address(program, context, index)
            .ok_or_else(|| refuse(REFUSE_CLEAR))?;
        Ok((snapshot, rsn3::SNAPSHOT_BYTES))
    } else {
        let flat = usize::from(index) - 4;
        let buffer = (flat / 8) as u8;
        let strip = (flat % 8) as u8;
        let (snapshot, _) = rsn3::buffered_snapshot_address(program, context, buffer)
            .ok_or_else(|| refuse(REFUSE_CLEAR))?;
        let (strip_key, _) = rsn3::strip_address(program, &snapshot, strip);
        Ok((strip_key, rsn3::STRIP_ACCOUNT_BYTES))
    }
}

fn process_clear_render(
    program: &Pubkey,
    accounts: &[AccountInfo],
    request: Request,
) -> ProgramResult {
    let [context, meta, continuation, target, authority] = accounts else {
        return Err(refuse(REFUSE_ACCOUNTS));
    };
    if !authority.is_signer
        || !authority.is_writable
        || !continuation.is_writable
        || !target.is_writable
        || context.owner != program
        || context.data_len() != resource_sbf::CONTEXT_BYTES
        || meta.owner != program
        || meta.data_len() != sim2::META_BYTES
        || !meta.is_writable
        || continuation.owner != program
        || continuation.data_len() != e2r::CONTINUATION_BYTES
        || target.owner != program
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let marker = continuation.try_borrow_data()?;
    if &marker[..8] != MARKER_MAGIC
        || u32_at(&marker, 8) != VERSION
        || u32_at(&marker, MR_GENERATION) != request.generation
        || marker[MR_CONTEXT..MR_CONTEXT + 32] != context.key.as_ref()[..]
        || marker[MR_META..MR_META + 32] != meta.key.as_ref()[..]
        || marker[MR_AUTHORITY..MR_AUTHORITY + 32] != authority.key.as_ref()[..]
    {
        return Err(refuse(REFUSE_PENDING));
    }
    let bit = 1u64 << request.argument;
    let cleared = u64_at(&marker, MR_CLEARED);
    let complete = u32_at(&marker, MR_PHASE) == REINIT_STEP_COUNT;
    let index = request.argument as u8;
    let (expected, expected_bytes) = expected_render_account(program, context.key, index)?;
    if target.key != &expected || target.data_len() != expected_bytes {
        return Err(refuse(REFUSE_CLEAR));
    }
    let meta_zero = meta_data_is_zero(meta)?;
    if !meta_zero {
        let meta_data = meta.try_borrow_data()?;
        let (expected_meta, bump) = sim2::meta_address(program, context.key);
        if !complete
            || cleared & bit == 0
            || meta.key != &expected_meta
            || &meta_data[..8] != b"SIM2META"
            || !matches!(u32_at(&meta_data, 8), sim2::VERSION | sim2::META_VERSION)
            || meta_data[sim2::M_BUMP] != bump
            || meta_data[sim2::M_CONTEXT..sim2::M_CONTEXT + 32] != context.key.as_ref()[..]
            || meta_data[sim2::M_AUTHORITY..sim2::M_AUTHORITY + 32] != authority.key.as_ref()[..]
        {
            return Err(refuse(REFUSE_PENDING));
        }
    }
    if cleared & bit != 0 {
        return Ok(());
    }
    if !meta_zero || u32_at(&marker, MR_PHASE) != 0 {
        return Err(refuse(REFUSE_PENDING));
    }
    drop(marker);
    target.try_borrow_mut_data()?.fill(0);
    let mut marker = continuation.try_borrow_mut_data()?;
    let bitmap = u64_at(&marker, MR_CLEARED) | (1u64 << index);
    put_u64(&mut marker, MR_CLEARED, bitmap);
    Ok(())
}

#[cfg(target_os = "solana")]
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
    fn doom_engine_ex2_resource_sbf_ri2_split_newgame_prepare_phase(
        phase: u32,
        context: *mut u8,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        arena_high_water: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_split_newgame_start(
        context: *mut u8,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        arena_high_water: *mut usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_split_finalize(
        context: *mut u8,
        bootstrap: *mut u8,
        bootstrap_bytes: usize,
        state_out: *mut u8,
        state_cap: usize,
        state_out_len: *mut usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
        mirror_out_len: *mut usize,
        frame_out: *mut u8,
        frame_cap: usize,
        frame_out_len: *mut usize,
        arena_high_water: *mut usize,
    ) -> i32;
}

#[cfg(target_os = "solana")]
fn context_parts(context: &mut [u8]) -> Result<(*mut u8, *mut u8, usize), ProgramError> {
    let wad_end = resource_sbf::WAD_IMAGE_OFFSET + resource_sbf::WAD_IMAGE_BYTES;
    let bootstrap_start = resource_sbf::BOOTSTRAP_OFFSET;
    let bootstrap_end = bootstrap_start + resource_sbf::BOOTSTRAP_BYTES;
    if wad_end > bootstrap_start || bootstrap_end > context.len() {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let context_ptr = context.as_mut_ptr();
    let (_, tail) = context.split_at_mut(bootstrap_start);
    let bootstrap = &mut tail[..resource_sbf::BOOTSTRAP_BYTES];
    if (context_ptr as usize) % resource_sbf::ALLOC_ALIGNMENT != 0
        || (bootstrap.as_mut_ptr() as usize) % resource_sbf::ALLOC_ALIGNMENT != 0
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    Ok((context_ptr, bootstrap.as_mut_ptr(), bootstrap.len()))
}

#[cfg(target_os = "solana")]
fn c_result(rc: i32) -> ProgramResult {
    if rc == 0 {
        Ok(())
    } else {
        Err(ProgramError::Custom(rc.max(0) as u32))
    }
}

fn copy_wad_chunk_to_context(
    accounts: &[AccountInfo],
    context: &mut [u8],
    ordinal: usize,
) -> ProgramResult {
    if ordinal >= static_resource::WAD_CHUNK_BYTES.len() {
        return Err(refuse(REFUSE_MALFORMED));
    }
    let wad = accounts[WAD_FIRST + ordinal].try_borrow_data()?;
    let payload = u32_at(&wad, 24) as usize;
    let payload_bytes =
        &wad[resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + payload];
    if payload != static_resource::WAD_CHUNK_BYTES[ordinal]
        || hashv(&[payload_bytes]).to_bytes() != static_resource::WAD_CHUNK_HASHES[ordinal]
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let before: usize = static_resource::WAD_CHUNK_BYTES[..ordinal].iter().sum();
    let start = resource_sbf::WAD_IMAGE_OFFSET + before;
    let end = start + payload;
    if end > resource_sbf::WAD_IMAGE_OFFSET + resource_sbf::WAD_IMAGE_BYTES {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    sol_memcpy(
        &mut context[start..end],
        &wad[resource_sbf::WAD_HEADER_BYTES..resource_sbf::WAD_HEADER_BYTES + payload],
        payload,
    );
    Ok(())
}

fn process_step(program: &Pubkey, accounts: &[AccountInfo], request: Request) -> ProgramResult {
    validate_base(program, accounts)?;
    let mut marker = accounts[CONTINUATION].try_borrow_data()?;
    if !marker_matches(&marker, accounts, request.generation) {
        return Err(refuse(REFUSE_PENDING));
    }
    let completed_phase = u32_at(&marker, MR_PHASE);
    let meta_zero = meta_data_is_zero(&accounts[META])?;
    if !meta_zero {
        if completed_phase != REINIT_STEP_COUNT {
            return Err(refuse(REFUSE_PENDING));
        }
        read_meta_bindings(program, accounts)?;
    }
    if request.argument <= completed_phase {
        return Ok(());
    }
    if !meta_zero
        || request.argument != completed_phase + 1
        || u64_at(&marker, MR_CLEARED) != ((1u64 << RENDER_BUFFER_COUNT) - 1)
    {
        return Err(refuse(REFUSE_PENDING));
    }
    let capacity = u32_at(&marker, MR_CAPACITY);
    let bump = marker[MR_BUMP];
    let payer = Pubkey::new_from_array(
        marker[MR_RENT_PAYER..MR_RENT_PAYER + 32]
            .try_into()
            .unwrap(),
    );
    let prior_high_water = u32_at(&marker, MR_ARENA_HIGH_WATER) as usize;
    drop(marker);

    #[cfg(not(target_os = "solana"))]
    {
        let _ = (capacity, bump, payer, prior_high_water);
        return Err(ProgramError::InvalidInstructionData);
    }

    #[cfg(target_os = "solana")]
    {
        let mut high_water = prior_high_water;
        let rc = if request.argument <= REINIT_WAD_STAGES {
            let mut context = accounts[CONTEXT].try_borrow_mut_data()?;
            copy_wad_chunk_to_context(accounts, &mut context, request.argument as usize - 1)?;
            0
        } else if request.argument <= REINIT_WAD_STAGES + REINIT_RSR_STAGES {
            let phase = request.argument - REINIT_WAD_STAGES - 1;
            let mut context = accounts[CONTEXT].try_borrow_mut_data()?;
            let resource = accounts[RESOURCE].try_borrow_data()?;
            let wad_image_ptr = context
                .as_ptr()
                .wrapping_add(resource_sbf::WAD_IMAGE_OFFSET);
            let wad_image_len = resource_sbf::WAD_IMAGE_BYTES;
            let (context_ptr, bootstrap_ptr, bootstrap_len) = context_parts(&mut context)?;
            let mut state = accounts[STATE].try_borrow_mut_data()?;
            unsafe {
                doom_engine_rsn3_build_static_phase(
                    phase,
                    context_ptr,
                    context.len(),
                    resource.as_ptr(),
                    resource.len(),
                    wad_image_ptr,
                    wad_image_len,
                    bootstrap_ptr,
                    bootstrap_len,
                    state.as_mut_ptr(),
                    512,
                    &mut high_water,
                )
            }
        } else if request.argument <= REINIT_WAD_STAGES + REINIT_RSR_STAGES + REINIT_GAME_STAGES {
            let mut context = accounts[CONTEXT].try_borrow_mut_data()?;
            let (context_ptr, bootstrap_ptr, bootstrap_len) = context_parts(&mut context)?;
            unsafe {
                if request.argument <= REINIT_WAD_STAGES + REINIT_RSR_STAGES + REINIT_PREPARE_STAGES
                {
                    let phase = request.argument - REINIT_WAD_STAGES - REINIT_RSR_STAGES - 1;
                    doom_engine_ex2_resource_sbf_ri2_split_newgame_prepare_phase(
                        phase,
                        context_ptr,
                        bootstrap_ptr,
                        bootstrap_len,
                        &mut high_water,
                    )
                } else {
                    doom_engine_ex2_resource_sbf_ri2_split_newgame_start(
                        context_ptr,
                        bootstrap_ptr,
                        bootstrap_len,
                        &mut high_water,
                    )
                }
            }
        } else if request.argument == REINIT_STEP_COUNT - 1 {
            let mut context = accounts[CONTEXT].try_borrow_mut_data()?;
            let (context_ptr, bootstrap_ptr, bootstrap_len) = context_parts(&mut context)?;
            let mut state = accounts[STATE].try_borrow_mut_data()?;
            let mut mirror = accounts[MIRROR].try_borrow_mut_data()?;
            let mut frame = accounts[FRAME].try_borrow_mut_data()?;
            let mut state_len = 0usize;
            let mut mirror_len = 0usize;
            let mut frame_len = 0usize;
            let rc = unsafe {
                doom_engine_ex2_resource_sbf_ri2_split_finalize(
                    context_ptr,
                    bootstrap_ptr,
                    bootstrap_len,
                    state.as_mut_ptr(),
                    state.len(),
                    &mut state_len,
                    mirror.as_mut_ptr(),
                    mirror.len(),
                    &mut mirror_len,
                    frame.as_mut_ptr(),
                    frame.len(),
                    &mut frame_len,
                    &mut high_water,
                )
            };
            if rc == 0
                && (state_len < 64
                    || state_len > state.len()
                    || mirror_len != mirror.len()
                    || frame_len != frame.len())
            {
                return Err(refuse(REFUSE_ACCOUNTS));
            }
            rc
        } else {
            c_result(0)?;
            let high_water = high_water.min(u32::MAX as usize) as u32;
            e2r::write_published_checkpoint(program, &accounts[..13], high_water as usize)?;
            let checkpoint = {
                let context_data = accounts[CONTEXT].try_borrow_data()?;
                context_data[e2r::CONTEXT_CHECKPOINT_OFFSET
                    ..e2r::CONTEXT_CHECKPOINT_OFFSET + e2r::CONTEXT_CHECKPOINT_BYTES]
                    .to_vec()
            };
            let state = accounts[STATE].try_borrow_data()?;
            let mirror = accounts[MIRROR].try_borrow_data()?;
            let state_len = sim2::state_len_checked(&state)?;
            let (verified_len, state_root) = sim2::verified_state_root(&state, &mirror)?;
            if verified_len != state_len || u32_at(&state, 20) != 0 {
                return Err(refuse(REFUSE_ACCOUNTS));
            }
            let first_tic = u32_at(&state, 20);
            let checkpoint_hash = sim2::checkpoint_hash(accounts[CONTEXT].key, &checkpoint);
            drop(mirror);
            drop(state);
            let mut meta = accounts[META].try_borrow_mut_data()?;
            let mut buffer = accounts[BUFFER].try_borrow_mut_data()?;
            sim2::init_layout(
                &mut meta,
                &mut buffer,
                &sim2::InitParams {
                    context: *accounts[CONTEXT].key,
                    state: *accounts[STATE].key,
                    mirror: *accounts[MIRROR].key,
                    buffer: *accounts[BUFFER].key,
                    meta: *accounts[META].key,
                    authority: *accounts[AUTHORITY].key,
                    bump,
                    capacity,
                    first_tic,
                    state_len: state_len as u32,
                    state_root,
                    checkpoint_hash,
                },
            )?;
            meta[sim2::M_RENT_PAYER..sim2::M_RENT_PAYER + 32].copy_from_slice(payer.as_ref());
            let mut marker = accounts[CONTINUATION].try_borrow_mut_data()?;
            put_u32(&mut marker, MR_PHASE, REINIT_STEP_COUNT);
            put_u32(&mut marker, MR_ARENA_HIGH_WATER, high_water);
            return Ok(());
        };
        c_result(rc)?;
        let mut marker = accounts[CONTINUATION].try_borrow_mut_data()?;
        put_u32(&mut marker, MR_PHASE, request.argument);
        put_u32(
            &mut marker,
            MR_ARENA_HIGH_WATER,
            high_water.min(u32::MAX as usize) as u32,
        );
        Ok(())
    }
}

fn close_meta_payer(
    program: &Pubkey,
    context: &AccountInfo,
    meta: &AccountInfo,
    authority: &AccountInfo,
) -> Result<Pubkey, ProgramError> {
    if context.owner != program
        || context.data_len() == 0
        || context.data_len() > resource_sbf::CONTEXT_BYTES
        || !context.is_writable
        || !owned(program, meta, sim2::META_BYTES, true)
        || !authority.is_signer
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let data = meta.try_borrow_data()?;
    let version = u32_at(&data, 8);
    if &data[..8] != b"SIM2META"
        || data.len() != sim2::META_BYTES
        || !matches!(version, sim2::VERSION | sim2::META_VERSION)
        || data[sim2::M_CONTEXT..sim2::M_CONTEXT + 32] != context.key.as_ref()[..]
        || data[sim2::M_AUTHORITY..sim2::M_AUTHORITY + 32] != authority.key.as_ref()[..]
    {
        return Err(refuse(REFUSE_CLOSE));
    }
    let (expected_meta, bump) = sim2::meta_address(program, context.key);
    if meta.key != &expected_meta || data[sim2::M_BUMP] != bump {
        return Err(refuse(REFUSE_CLOSE));
    }
    let payer = if version == sim2::META_VERSION {
        Pubkey::new_from_array(
            data[sim2::M_RENT_PAYER..sim2::M_RENT_PAYER + 32]
                .try_into()
                .unwrap(),
        )
    } else {
        *authority.key
    };
    if payer == Pubkey::default() {
        return Err(refuse(REFUSE_CLOSE));
    }
    Ok(payer)
}

fn close_payer<'a>(
    accounts: &'a [AccountInfo<'a>],
    authority_index: usize,
    payer_index: usize,
    rent_payer: &Pubkey,
) -> Result<&'a AccountInfo<'a>, ProgramError> {
    let authority = &accounts[authority_index];
    if authority.key == rent_payer {
        if !authority.is_writable {
            return Err(refuse(REFUSE_ACCOUNTS));
        }
        Ok(authority)
    } else {
        let payer = accounts.get(payer_index).ok_or(refuse(REFUSE_ACCOUNTS))?;
        if payer.key != rent_payer || !payer.is_writable {
            return Err(refuse(REFUSE_CLOSE));
        }
        Ok(payer)
    }
}

fn close_one(account: &AccountInfo, payer: &AccountInfo) -> ProgramResult {
    let next = payer
        .lamports()
        .checked_add(account.lamports())
        .ok_or(refuse(REFUSE_CLOSE))?;
    **payer.try_borrow_mut_lamports()? = next;
    **account.try_borrow_mut_lamports()? = 0;
    account.realloc(0, false)?;
    account.assign(&system_program::ID);
    Ok(())
}

fn close_marker_write(
    continuation: &AccountInfo,
    context: &Pubkey,
    authority: &Pubkey,
    meta: &Pubkey,
    payer: &Pubkey,
    phase: u32,
    render_cursor: usize,
) -> ProgramResult {
    let mut data = continuation.try_borrow_mut_data()?;
    // The continuation can still contain bytes from the old engine image.
    // CLOSE never interprets that tail, and the final phase removes the whole
    // account. Touch only this bounded marker instead of scanning or clearing
    // the 256 KiB continuation on every close phase.
    data[..MARKER_BYTES].fill(0);
    data[..8].copy_from_slice(CLOSE_MAGIC);
    put_u32(&mut data, 8, CLOSE_VERSION);
    put_u32(&mut data, 12, phase);
    put_u32(&mut data, 144, render_cursor as u32);
    for (offset, key) in [
        (CLOSE_CONTEXT, context),
        (CLOSE_AUTHORITY, authority),
        (CLOSE_META, meta),
        (CLOSE_PAYER, payer),
    ] {
        data[offset..offset + 32].copy_from_slice(key.as_ref());
    }
    Ok(())
}

fn close_marker_matches(
    continuation: &AccountInfo,
    context: &Pubkey,
    authority: &Pubkey,
    meta: &Pubkey,
    payer: &Pubkey,
    phase: u32,
    render_cursor: usize,
) -> Result<bool, ProgramError> {
    let data = continuation.try_borrow_data()?;
    Ok(&data[..8] == CLOSE_MAGIC
        && u32_at(&data, 8) == CLOSE_VERSION
        && u32_at(&data, 12) == phase
        && u32_at(&data, 144) as usize == render_cursor
        && data[CLOSE_CONTEXT..CLOSE_CONTEXT + 32] == context.as_ref()[..]
        && data[CLOSE_AUTHORITY..CLOSE_AUTHORITY + 32] == authority.as_ref()[..]
        && data[CLOSE_META..CLOSE_META + 32] == meta.as_ref()[..]
        && data[CLOSE_PAYER..CLOSE_PAYER + 32] == payer.as_ref()[..]
        && data[148..MARKER_BYTES].iter().all(|byte| *byte == 0))
}

fn process_close_pre<'a>(program: &Pubkey, accounts: &'a [AccountInfo<'a>]) -> ProgramResult {
    if accounts.len() < 9 {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let context = &accounts[0];
    if context.data_len() != resource_sbf::CONTEXT_BYTES {
        return Err(refuse(REFUSE_CLOSE));
    }
    let state = &accounts[1];
    let mirror = &accounts[2];
    let meta = &accounts[3];
    let buffer = &accounts[4];
    let controller = &accounts[5];
    let frame = &accounts[6];
    let continuation = &accounts[7];
    let authority = &accounts[8];
    let rent_payer = close_meta_payer(program, context, meta, authority)?;
    let shared_payer = authority.key == &rent_payer;
    let expected_count = if shared_payer {
        CLOSE_PRE_SHARED_ACCOUNT_COUNT
    } else {
        CLOSE_PRE_SEPARATE_ACCOUNT_COUNT
    };
    if accounts.len() != expected_count || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let payer = close_payer(accounts, 8, 9, &rent_payer)?;
    if !owned(program, state, resource_sbf::STATE_MAX_BYTES, true)
        || !owned(program, mirror, resource_sbf::MIRROR_BYTES, true)
        || !owned(program, controller, resource_sbf::CONTROLLER_BYTES, true)
        || !owned(program, frame, resource_sbf::RI2_FRAME_BYTES, true)
        || !owned(program, continuation, e2r::CONTINUATION_BYTES, true)
        || buffer.owner != program
        || !buffer.is_writable
    {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let meta_data = meta.try_borrow_data()?;
    let (expected_buffer_len, _) = buffer_bytes(&meta_data, sim2::M_CAPACITY)?;
    if buffer.data_len() != expected_buffer_len
        || meta_data[sim2::M_STATE..sim2::M_STATE + 32] != state.key.as_ref()[..]
        || meta_data[sim2::M_MIRROR..sim2::M_MIRROR + 32] != mirror.key.as_ref()[..]
        || meta_data[sim2::M_BUFFER..sim2::M_BUFFER + 32] != buffer.key.as_ref()[..]
    {
        return Err(refuse(REFUSE_CLOSE));
    }
    drop(meta_data);
    for index in [1usize, 2, 4, 5, 6] {
        close_one(&accounts[index], payer)?;
    }
    close_marker_write(
        continuation,
        context.key,
        authority.key,
        meta.key,
        &rent_payer,
        1,
        0,
    )
}

fn process_close_render_target<'a>(
    program: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    phase: u32,
) -> ProgramResult {
    if phase == 0 || phase >= CLOSE_RESIZE_FIRST_PHASE {
        return Err(refuse(REFUSE_CLOSE));
    }
    if accounts.len() < 4 {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let context = &accounts[0];
    let meta = &accounts[1];
    let continuation = &accounts[2];
    let authority = &accounts[3];
    let rent_payer = close_meta_payer(program, context, meta, authority)?;
    let shared_payer = authority.key == &rent_payer;
    let render_start = if shared_payer { 4 } else { 5 };
    let render_index = phase as usize - 1;
    let expected_count = render_start + 1;
    if accounts.len() != expected_count || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let payer = close_payer(accounts, 3, 4, &rent_payer)?;
    if !owned(program, continuation, e2r::CONTINUATION_BYTES, true)
        || !close_marker_matches(
            continuation,
            context.key,
            authority.key,
            meta.key,
            &rent_payer,
            phase,
            render_index,
        )?
        || context.data_len() != resource_sbf::CONTEXT_BYTES
    {
        return Err(refuse(REFUSE_CLOSE));
    }
    let target = &accounts[render_start];
    let (expected, expected_bytes) =
        expected_render_account(program, context.key, render_index as u8)
            .map_err(|_| refuse(REFUSE_CLOSE))?;
    let target_len = target.data_len();
    if target.key != &expected
        || target.owner != program
        || !target.is_writable
        || target_len != expected_bytes
    {
        return Err(refuse(REFUSE_CLOSE));
    }
    close_one(target, payer)?;
    close_marker_write(
        continuation,
        context.key,
        authority.key,
        meta.key,
        &rent_payer,
        phase + 1,
        render_index + 1,
    )
}

fn close_base_accounts<'a>(
    program: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> Result<
    (
        &'a AccountInfo<'a>,
        &'a AccountInfo<'a>,
        &'a AccountInfo<'a>,
        &'a AccountInfo<'a>,
        &'a AccountInfo<'a>,
        Pubkey,
    ),
    ProgramError,
> {
    if accounts.len() < CLOSE_BASE_SHARED_ACCOUNT_COUNT {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let context = &accounts[0];
    let meta = &accounts[1];
    let continuation = &accounts[2];
    let authority = &accounts[3];
    let rent_payer = close_meta_payer(program, context, meta, authority)?;
    let shared_payer = authority.key == &rent_payer;
    let expected_count = if shared_payer {
        CLOSE_BASE_SHARED_ACCOUNT_COUNT
    } else {
        CLOSE_BASE_SEPARATE_ACCOUNT_COUNT
    };
    if accounts.len() != expected_count || !distinct(accounts) {
        return Err(refuse(REFUSE_ACCOUNTS));
    }
    let payer = close_payer(accounts, 3, 4, &rent_payer)?;
    if !owned(program, continuation, e2r::CONTINUATION_BYTES, true) {
        return Err(refuse(REFUSE_CLOSE));
    }
    Ok((context, meta, continuation, authority, payer, rent_payer))
}

fn process_close_final<'a>(program: &Pubkey, accounts: &'a [AccountInfo<'a>]) -> ProgramResult {
    let (context, meta, continuation, authority, payer, rent_payer) =
        close_base_accounts(program, accounts)?;
    if context.data_len() != resource_sbf::CONTEXT_BYTES
        || !close_marker_matches(
            continuation,
            context.key,
            authority.key,
            meta.key,
            &rent_payer,
            CLOSE_FINAL_PHASE,
            CLOSE_RENDER_TARGETS,
        )?
    {
        return Err(refuse(REFUSE_CLOSE));
    }
    close_one(context, payer)?;
    close_one(meta, payer)?;
    close_one(continuation, payer)
}

fn process_close<'a>(
    program: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    phase: u32,
) -> ProgramResult {
    match phase {
        0 => process_close_pre(program, accounts),
        phase if (1..CLOSE_RESIZE_FIRST_PHASE).contains(&phase) => {
            process_close_render_target(program, accounts, phase)
        }
        CLOSE_FINAL_PHASE => process_close_final(program, accounts),
        _ => Err(refuse(REFUSE_MALFORMED)),
    }
}

pub fn process<'a>(
    program: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    data: &[u8],
) -> ProgramResult {
    let request = Request::decode(data)?;
    match request.operation {
        BEGIN => process_begin(program, accounts, request),
        CLEAR_RENDER => process_clear_render(program, accounts, request),
        STEP => process_step(program, accounts, request),
        CLOSE => process_close(program, accounts, request.argument),
        _ => Err(refuse(REFUSE_MALFORMED)),
    }
}

pub fn encode_begin(generation: u32) -> [u8; REQUEST_BYTES] {
    Request {
        operation: BEGIN,
        argument: 0,
        generation,
    }
    .encode()
}

pub fn encode_clear_render(index: u8, generation: u32) -> [u8; REQUEST_BYTES] {
    Request {
        operation: CLEAR_RENDER,
        argument: u32::from(index),
        generation,
    }
    .encode()
}

pub fn encode_step(phase: u32, generation: u32) -> [u8; REQUEST_BYTES] {
    Request {
        operation: STEP,
        argument: phase,
        generation,
    }
    .encode()
}

pub fn encode_close() -> [u8; REQUEST_BYTES] {
    encode_close_phase(0)
}

pub fn encode_close_phase(phase: u8) -> [u8; REQUEST_BYTES] {
    Request {
        operation: CLOSE,
        argument: u32::from(phase),
        generation: 0,
    }
    .encode()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_and_stage_bounds() {
        for request in [
            Request {
                operation: BEGIN,
                argument: 0,
                generation: 7,
            },
            Request {
                operation: CLEAR_RENDER,
                argument: 35,
                generation: 7,
            },
            Request {
                operation: STEP,
                argument: REINIT_STEP_COUNT,
                generation: 7,
            },
            Request {
                operation: CLOSE,
                argument: 0,
                generation: 0,
            },
            Request {
                operation: CLOSE,
                argument: CLOSE_FINAL_PHASE,
                generation: 0,
            },
        ] {
            assert_eq!(Request::decode(&request.encode()).unwrap(), request);
        }
        assert!(Request::decode(
            &Request {
                operation: CLEAR_RENDER,
                argument: 36,
                generation: 7,
            }
            .encode()
        )
        .is_err());
        assert!(Request::decode(
            &Request {
                operation: STEP,
                argument: REINIT_STEP_COUNT + 1,
                generation: 7,
            }
            .encode()
        )
        .is_err());
        assert!(Request::decode(
            &Request {
                operation: CLOSE,
                argument: CLOSE_FINAL_PHASE + 1,
                generation: 0,
            }
            .encode()
        )
        .is_err());
        assert!(Request::decode(
            &Request {
                operation: BEGIN,
                argument: 0,
                generation: 0,
            }
            .encode()
        )
        .is_err());
        assert!(Request::decode(
            &Request {
                operation: STEP,
                argument: 1,
                generation: 0,
            }
            .encode()
        )
        .is_err());
        assert!(Request::decode(
            &Request {
                operation: CLOSE,
                argument: 0,
                generation: 1,
            }
            .encode()
        )
        .is_err());
        assert_eq!(REINIT_STEP_COUNT, 75);
        assert_eq!(CLOSE_FINAL_PHASE, CLOSE_RENDER_TARGETS as u32 + 1);
    }
}
