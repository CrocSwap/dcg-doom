// SPDX-License-Identifier: GPL-3.0-only

//! Stateful-v3 adapter for the pinned Doom engine.
//!
//! The context is DCG's headerless primary state account, so account zero
//! retains the fixed address checked by `doom_ctx_guard`. The WAD is copied
//! through a sealed v3 resource into resumable engine initialization phases.

use core::sync::atomic::{AtomicPtr, Ordering};

use dcg_program::kernel::{
    AccountSpan, ApplicationManifest, InitializationPhase, Kernel, KernelError, KernelId,
    KernelManifest, ModeId, PortLayout, ResourceLimits, StateSchema, StateSpanMut, StatefulKernel,
    TransitionDisposition, TransitionOutcome, VersionedId, ViewAbi, ViewPhase,
};

pub const APPLICATION_ID: &[u8] = b"basanos-doom/3";
pub const APPLICATION_VERSION: u16 = 3;

/// DCG's stateful-v3 consensus mode.
pub const MODE_CONSENSUS_V3: ModeId = dcg_program::stateful::v3::MODE_CONSENSUS_V3;

/// One composite v3 kernel owns simulation and its three RSN/3 view ABIs.
/// V3 binds one kernel identity per session, so the former separate v1
/// simulator/snapshot/strip IDs cannot describe one Doom session.
pub const DOOM_KERNEL_ID: KernelId = KernelId(*b"basanos-doom-v3\0");
pub const STATE_SCHEMA: VersionedId = VersionedId {
    id: 0x4d4f_4f44, // ASCII "DOOM" interpreted little-endian.
    version: 2,
};
pub const INPUT_SCHEMA: VersionedId = VersionedId {
    id: 0x494f_4f44, // ASCII "DOOI" interpreted little-endian.
    version: 2,
};
pub const OUTPUT_SCHEMA: VersionedId = VersionedId {
    id: 0x4f4f_4f44, // ASCII "DOOO" interpreted little-endian.
    version: 2,
};
pub const RESOURCE_SCHEMA: VersionedId = VersionedId {
    id: 0x5352_4744, // ASCII "DGRS" interpreted little-endian.
    version: 2,
};

/// Fixture identity used by the local ProgramTest adapter. A deployable
/// resource account must be created at this pinned key and sealed by the
/// application program before sessions are opened.
pub const RESOURCE_KEY: [u8; 32] = [0xd0; 32];

pub const ENGINE_CONTEXT_BYTES: u32 = 9_734_160;
pub const ST1_ACCOUNT_BYTES: u32 = 126_976;
pub const MP1_ACCOUNT_BYTES: u32 = 65_536;
pub const STATE_BYTES: u32 = ENGINE_CONTEXT_BYTES + ST1_ACCOUNT_BYTES + MP1_ACCOUNT_BYTES;
pub const SNAPSHOT_BYTES: u32 = 1_048_768;
pub const STRIP_BYTES: u32 = 8_128;
pub const RESOURCE_HEADER_BYTES: usize = 32;
pub const RI2_RESOURCE_BYTES: usize = 261_056;
pub const WAD_IMAGE_BYTES: usize = 4_196_020;
pub const RESOURCE_BYTES: usize = RESOURCE_HEADER_BYTES + RI2_RESOURCE_BYTES + WAD_IMAGE_BYTES;

pub const RESOURCE_MAGIC: &[u8; 8] = b"DOOMDGR1";
pub const DOOM_CONTEXT_ADDRESS: usize = 0x400000060;
pub const ENGINE_BINDING_REFUSAL: u32 = 3_501;
pub const RESOURCE_REFUSAL: u32 = 3_502;
pub const STATE_REFUSAL: u32 = 3_503;
pub const INPUT_REFUSAL: u32 = 3_504;

const ST1_OFFSET: u32 = ENGINE_CONTEXT_BYTES;
const MP1_OFFSET: u32 = ENGINE_CONTEXT_BYTES + ST1_ACCOUNT_BYTES;
const MAX_ADVANCE_TICS: u32 = 1;
const STEP_COMPUTE_CEILING: u64 = 1_400_000;
pub const V3_INITIALIZATION_PHASE_BYTES: u32 = 16_384;
pub const V3_INITIALIZATION_PHASE_COMPUTE_UNITS: u32 = 1_400_000;
// Each publication phase is at most one 8,128-byte strip. The view byte
// stream is the snapshot followed by eight strips. The renderer caches each
// complete strip in the preserved workspace, then invalidates that cache when
// the next frame starts so split phases do not rerun the C renderer.
pub const V3_VIEW_PHASE_BYTES: u32 = STRIP_BYTES;
pub const V3_VIEW_PHASE_COMPUTE_CEILING: u32 = 1_400_000;
const RSR_HEADER_BYTES: usize = 512;
const WAD_IMAGE_OFFSET: usize = 3_440_976;
const BOOTSTRAP_OFFSET: usize = 7_637_008;
const BOOTSTRAP_BYTES: usize = 2 * 1024 * 1024;
const BOOTSTRAP_ARENA_OFFSET: usize = BOOTSTRAP_OFFSET + RSR_HEADER_BYTES;
const BOOTSTRAP_ARENA_BYTES: usize = BOOTSTRAP_BYTES - RSR_HEADER_BYTES;
const RENDER_CACHE_BYTES: usize = 60;
const RENDER_PIXEL_BYTES: usize = 40 * 200;
const RENDER_FRAME_BYTES: usize = 320 * 200;
// The fixed 10 MiB DCG workspace cap leaves room for the 9.7 MiB engine
// context, strip cache, and render scratch only if the cached RSN3 projection
// stays within this checked bound.
const SNAPSHOT_CACHE_BYTES: usize = 640 * 1024;
const SNAPSHOT_CACHE_OFFSET: usize = ENGINE_CONTEXT_BYTES as usize;
const STRIP_CACHE_BYTES: usize = STRIP_BYTES as usize * 8;
const STRIP_CACHE_OFFSET: usize = SNAPSHOT_CACHE_OFFSET + SNAPSHOT_CACHE_BYTES;
const RENDER_TEMP_BYTES: usize = RENDER_CACHE_BYTES + RENDER_PIXEL_BYTES;
const RENDER_TEMP_OFFSET: usize = STRIP_CACHE_OFFSET + STRIP_CACHE_BYTES;
const ARENA_HIGH_WATER_OFFSET: usize = RENDER_TEMP_OFFSET + RENDER_TEMP_BYTES;
pub const V3_VIEW_WORKSPACE_BYTES: u32 = (ARENA_HIGH_WATER_OFFSET + 4) as u32;
pub const V3_RENDER_ARENA_HIGH_WATER_OFFSET: u32 = ARENA_HIGH_WATER_OFFSET as u32;
pub const V3_RENDER_ARENA_MAX_BYTES: u32 = BOOTSTRAP_ARENA_BYTES as u32;
const INITIAL_ENGINE_PHASES: u32 = 6 + 71 + 37 + 1;

pub const V3_SNAPSHOT_ABI: [u8; 32] = [0x52; 32];
pub const V3_STRIP_ABIS: [[u8; 32]; 8] = [
    [0x53; 32], [0x54; 32], [0x55; 32], [0x56; 32], [0x57; 32], [0x58; 32], [0x59; 32], [0x5a; 32],
];
static MODES: [ModeId; 1] = [MODE_CONSENSUS_V3];
static VIEW_ABIS: [ViewAbi; 9] = [
    ViewAbi {
        role: 0,
        id: V3_SNAPSHOT_ABI,
        max_bytes: SNAPSHOT_BYTES,
    },
    ViewAbi {
        role: 1,
        id: V3_STRIP_ABIS[0],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 2,
        id: V3_STRIP_ABIS[1],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 3,
        id: V3_STRIP_ABIS[2],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 4,
        id: V3_STRIP_ABIS[3],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 5,
        id: V3_STRIP_ABIS[4],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 6,
        id: V3_STRIP_ABIS[5],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 7,
        id: V3_STRIP_ABIS[6],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 8,
        id: V3_STRIP_ABIS[7],
        max_bytes: STRIP_BYTES,
    },
];

static MANIFEST: KernelManifest = KernelManifest {
    id: DOOM_KERNEL_ID,
    semantic_version: 3,
    abi_version: 1,
    input: PortLayout {
        id: INPUT_SCHEMA,
        max_bytes: 4,
        alignment: 1,
    },
    output: PortLayout {
        id: OUTPUT_SCHEMA,
        max_bytes: 4,
        alignment: 1,
    },
    state: Some(StateSchema {
        id: STATE_SCHEMA,
        max_bytes: STATE_BYTES,
    }),
    resources: ResourceLimits {
        max_input_bytes: 4,
        max_output_bytes: 4,
        max_state_bytes: STATE_BYTES,
        max_operations: MAX_ADVANCE_TICS,
        // One engine tic per ADVANCE keeps the worst-case declaration within
        // the SVM transaction ceiling. The r6 receipt records measured costs.
        max_compute_units: STEP_COMPUTE_CEILING,
    },
    modes: &MODES,
};

/// Resource slice view over the compact account envelope created by
/// scripts/pack-dcg-resource.py.
pub struct DoomResourceBundle<'a> {
    pub ri2: &'a [u8],
    pub wad: &'a [u8],
}

impl<'a> DoomResourceBundle<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, KernelError> {
        let bundle = Self::decode_shape(bytes)?;
        let ri2_matches = dcg_program::hash::sha256(&[bundle.ri2]) == DOOM_RI2_DIGEST;
        let wad_matches = dcg_program::hash::sha256(&[bundle.wad]) == DOOM_WAD_DIGEST;
        if !ri2_matches || !wad_matches {
            return Err(KernelError::InvalidInput);
        }
        Ok(bundle)
    }

    fn decode_shape(bytes: &'a [u8]) -> Result<Self, KernelError> {
        if bytes.len() != RESOURCE_BYTES
            || bytes.get(..8) != Some(RESOURCE_MAGIC.as_slice())
            || u32_at(bytes, 8) != 1
            || u32_at(bytes, 12) as usize != RESOURCE_HEADER_BYTES
            || u32_at(bytes, 16) as usize != RESOURCE_BYTES
            || u32_at(bytes, 20) as usize != RI2_RESOURCE_BYTES
            || u32_at(bytes, 24) as usize != WAD_IMAGE_BYTES
            || bytes[28..32] != [0; 4]
        {
            return Err(KernelError::InvalidInput);
        }
        let ri2_end = RESOURCE_HEADER_BYTES + RI2_RESOURCE_BYTES;
        let ri2 = &bytes[RESOURCE_HEADER_BYTES..ri2_end];
        let wad = &bytes[ri2_end..];
        if ri2.get(..8) != Some(b"DOOM-RI2".as_slice())
            || u32_at(ri2, 8) != 4
            || u32_at(ri2, 16) as usize != RI2_RESOURCE_BYTES
            || ri2[72..104] != DOOM_WAD_DIGEST
        {
            return Err(KernelError::InvalidInput);
        }
        Ok(Self { ri2, wad })
    }
}

/// The golden's authenticated WAD payload digest, after the five account
/// headers have been removed and their payloads concatenated in ordinal order.
pub const DOOM_WAD_DIGEST: [u8; 32] = [
    0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9, 0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3, 0xe2, 0x9c,
    0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18, 0x16, 0xf6, 0x52, 0xa5, 0x26, 0xca, 0xc7, 0x71,
];

/// DCG chunk-Merkle root for the pinned RI2/WAD resource. The sealed resource
/// account authenticates every uploaded chunk against this value, so the
/// callback can check the whole resource in constant time instead of hashing
/// 4.4 MB again inside one SVM instruction.
pub const DOOM_RESOURCE_COMMITMENT: [u8; 32] = [
    0x26, 0xcc, 0x68, 0xc1, 0x99, 0x43, 0x53, 0x5e, 0xe0, 0x91, 0x01, 0x69, 0x07, 0x80, 0xff, 0xb3,
    0x56, 0x92, 0xa5, 0xe4, 0xae, 0xf2, 0xc0, 0xe0, 0x54, 0x67, 0xd0, 0x2f, 0xff, 0x97, 0xa5, 0x3e,
];

/// RI2 v4 resource image digest from the retained SIM/2 renderer golden.
pub const DOOM_RI2_DIGEST: [u8; 32] = [
    0xa8, 0xa7, 0x16, 0xed, 0xc7, 0x9b, 0x58, 0xc4, 0x33, 0xfd, 0xbe, 0xde, 0xfc, 0x6b, 0xe8, 0x42,
    0x53, 0x6b, 0x5e, 0xc0, 0x0c, 0x2a, 0x11, 0x40, 0x76, 0x8a, 0x6b, 0xe1, 0xe3, 0xab, 0x25, 0x0a,
];

/// The engine binding is held only by a kernel value on the program's stack
/// while one v3 callback sequence is active. The static manifest kernel below
/// has no mutable pointer state.
pub struct DoomKernel {
    invocation_context: AtomicPtr<u8>,
}

impl Default for DoomKernel {
    fn default() -> Self {
        Self {
            invocation_context: AtomicPtr::new(core::ptr::null_mut()),
        }
    }
}

impl DoomKernel {
    fn context_ptr(&self) -> Result<*mut u8, KernelError> {
        let pointer = self.invocation_context.load(Ordering::Relaxed);
        if pointer.is_null() {
            Err(KernelError::Refused)
        } else {
            Ok(pointer)
        }
    }

    fn validate_resources<'a>(
        &self,
        resources: &[dcg_program::kernel::AccountSpan<'a>],
        commitment: &[u8; 32],
    ) -> Result<DoomResourceBundle<'a>, KernelError> {
        let [resource] = resources else {
            return Err(KernelError::InvalidInput);
        };
        // The callback sees the program-owned sealed resource-copy PDA, not
        // the external source key supplied at open-session time.
        if resource.schema != RESOURCE_SCHEMA {
            return Err(KernelError::Refused);
        }
        if resource.owner == [0; 32] || *commitment == [0; 32] {
            return Err(KernelError::Refused);
        }
        if *commitment != DOOM_RESOURCE_COMMITMENT {
            return Err(KernelError::InvalidInput);
        }
        DoomResourceBundle::decode_shape(resource.data)
    }

    fn parse_resources<'a>(
        &self,
        resources: &[AccountSpan<'a>],
        commitment: &[u8; 32],
    ) -> Result<DoomResourceBundle<'a>, KernelError> {
        let [resource] = resources else {
            return Err(KernelError::InvalidInput);
        };
        if resource.schema != RESOURCE_SCHEMA || resource.owner == [0; 32] || *commitment == [0; 32]
        {
            return Err(KernelError::Refused);
        }
        DoomResourceBundle::decode_shape(resource.data)
    }

    fn validate_spans(spans: &[StateSpanMut<'_>]) -> Result<(), KernelError> {
        if spans.len() != 3
            || spans.iter().any(|span| span.schema != STATE_SCHEMA)
            || spans[0].offset != 0
            || spans[0].data.len() != ENGINE_CONTEXT_BYTES as usize
            || spans[1].offset != ST1_OFFSET
            || spans[1].data.len() != ST1_ACCOUNT_BYTES as usize
            || spans[2].offset != MP1_OFFSET
            || spans[2].data.len() != MP1_ACCOUNT_BYTES as usize
        {
            return Err(KernelError::InvalidInput);
        }
        Ok(())
    }

    fn require_context_address(pointer: *mut u8) -> Result<(), KernelError> {
        // The v3 workspace-first RUN_PHASE mapping must retain the pinned
        // fixed address while committed state stays read-only.
        if pointer as usize == DOOM_CONTEXT_ADDRESS {
            Ok(())
        } else {
            Err(KernelError::Refused)
        }
    }
}

impl Kernel for DoomKernel {
    fn manifest(&self) -> &'static KernelManifest {
        &MANIFEST
    }

    fn execute(&self, _input: &[u8], _output: &mut [u8]) -> Result<usize, KernelError> {
        Err(KernelError::Refused)
    }
}

#[cfg(all(feature = "sbf", target_os = "solana"))]
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
    fn doom_engine_ex2_resource_sbf_ri2_split_newgame_phase(
        phase: u32,
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
    fn doom_sbf_sim2_step(context: *mut u8, command: *const u8, command_len: usize) -> i32;
    fn doom_sbf_sim2_publish(
        context: *mut u8,
        state_out: *mut u8,
        state_cap: usize,
        state_out_len: *mut usize,
        mirror_out: *mut u8,
        mirror_cap: usize,
    ) -> i32;
    fn doom_engine_ex2_resource_sbf_ri2_snapshot_rsn3_live(
        context: *mut u8,
        payload_out: *mut u8,
        payload_cap: usize,
        payload_len_out: *mut usize,
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
        render_cache: *mut u8,
        render_cache_bytes: usize,
        snapshot_hash: *const u8,
        strip_out: *mut u8,
        strip_cap: usize,
        arena_high_water: *mut usize,
    ) -> i32;
}

fn engine_error(rc: i32) -> Result<(), KernelError> {
    if rc == 0 {
        Ok(())
    } else {
        Err(KernelError::Refused)
    }
}

fn write_state_range(
    spans: &mut [StateSpanMut<'_>],
    start: usize,
    end: usize,
    source: Option<&[u8]>,
) -> Result<(), KernelError> {
    if start > end || source.is_some_and(|bytes| bytes.len() != end - start) {
        return Err(KernelError::InvalidInput);
    }
    let mut span_start = 0usize;
    for span in spans {
        let span_end = span_start
            .checked_add(span.data.len())
            .ok_or(KernelError::InvalidInput)?;
        let from = start.max(span_start);
        let to = end.min(span_end);
        if from < to {
            let target_start = from - span_start;
            let target_end = to - span_start;
            if let Some(bytes) = source {
                let source_start = from - start;
                span.data[target_start..target_end]
                    .copy_from_slice(&bytes[source_start..source_start + to - from]);
            } else {
                span.data[target_start..target_end].fill(0);
            }
        }
        span_start = span_end;
    }
    if end > span_start {
        return Err(KernelError::InvalidInput);
    }
    Ok(())
}

fn view_shape(role: u8) -> Result<(u32, u32), KernelError> {
    match role {
        0 => Ok((SNAPSHOT_BYTES, SNAPSHOT_BYTES)),
        1..=8 => Ok((STRIP_BYTES, STRIP_BYTES)),
        _ => Err(KernelError::InvalidInput),
    }
}

#[cfg(all(feature = "sbf", target_os = "solana"))]
fn finish_rsr_header(
    context: &mut [u8],
    resource_hash: [u8; 32],
    arena_high_water: usize,
) -> Result<(), KernelError> {
    use doom_sbf::resource_sbf;
    if context.len() != ENGINE_CONTEXT_BYTES as usize {
        return Err(KernelError::InvalidInput);
    }
    let header_start = BOOTSTRAP_OFFSET;
    let mut build_header = [0u8; RSR_HEADER_BYTES];
    build_header.copy_from_slice(&context[header_start..header_start + RSR_HEADER_BYTES]);
    let mut roots = [0u8; 128];
    roots.copy_from_slice(&build_header[160..288]);
    let base = u64::from_le_bytes(build_header[112..120].try_into().unwrap());
    let header = &mut context[header_start..header_start + RSR_HEADER_BYTES];
    header.fill(0);
    header[160..288].copy_from_slice(&roots);
    header[..8].copy_from_slice(b"DOOM-RSR");
    put_u32(header, 8, 2);
    put_u32(header, 12, RSR_HEADER_BYTES as u32);
    put_u32(header, 16, resource_sbf::CONTEXT_BYTES as u32);
    put_u32(header, 20, resource_sbf::CTX_STRUCT_BYTES as u32);
    put_u32(header, 24, resource_sbf::WAD_IMAGE_OFFSET as u32);
    put_u32(header, 28, resource_sbf::WAD_IMAGE_BYTES as u32);
    put_u32(header, 32, resource_sbf::BOOTSTRAP_OFFSET as u32);
    put_u32(header, 36, resource_sbf::BOOTSTRAP_BYTES as u32);
    put_u32(header, 40, 32);
    put_u32(header, 44, 1);
    header[48..80].copy_from_slice(&DOOM_WAD_DIGEST);
    header[80..112].copy_from_slice(&resource_hash);
    header[112..120].copy_from_slice(&base.to_le_bytes());
    header[120..152].copy_from_slice(&[0x72; 32]);
    put_u32(header, 152, arena_high_water.min(u32::MAX as usize) as u32);
    put_u32(header, 288, 11);
    put_u32(header, 292, WAD_IMAGE_BYTES as u32);
    header[296..328].copy_from_slice(&resource_hash);
    header[328..336].copy_from_slice(&base.to_le_bytes());
    Ok(())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(all(feature = "sbf", target_os = "solana"))]
fn render_doom_view(
    kernel: &DoomKernel,
    phase: ViewPhase,
    state: &[AccountSpan<'_>],
    resources: &[AccountSpan<'_>],
    commitment: &[u8; 32],
    workspace: &mut [u8],
    output: &mut [u8],
) -> Result<usize, KernelError> {
    use doom_sbf::render_snapshot_v3 as rsn3;
    validate_render_state(state)?;
    if workspace.len() < V3_VIEW_WORKSPACE_BYTES as usize
        || phase.role != 0
        || phase.source_offset != 0
        || output.is_empty()
    {
        return Err(KernelError::InvalidInput);
    }
    let end = phase
        .output_offset
        .checked_add(output.len() as u32)
        .ok_or(KernelError::InvalidInput)?;
    if end > SNAPSHOT_BYTES {
        return Err(KernelError::InvalidInput);
    }

    if phase.output_offset == 0 {
        let bundle = kernel.validate_resources(resources, commitment)?;
        let resource_hash = dcg_program::hash::sha256(&[bundle.ri2]);
        let engine_header = &state[0].data[BOOTSTRAP_OFFSET..BOOTSTRAP_OFFSET + RSR_HEADER_BYTES];
        if engine_header[..8] != *b"DOOM-RSR"
            || engine_header[48..80] != DOOM_WAD_DIGEST
            || engine_header[80..112] != resource_hash
        {
            return Err(KernelError::Refused);
        }
        let (_, state_root) = doom_sbf::sim2::verified_state_root(state[1].data, state[2].data)
            .map_err(|_| KernelError::Refused)?;
        let source_context = state[0].data.as_ptr().cast_mut();
        DoomKernel::require_context_address(source_context)?;
        let snapshot =
            &mut workspace[SNAPSHOT_CACHE_OFFSET..SNAPSHOT_CACHE_OFFSET + SNAPSHOT_CACHE_BYTES];
        snapshot[..rsn3::HEADER_BYTES].fill(0);
        let payload_out = &mut snapshot[rsn3::HEADER_BYTES..];
        let mut payload_len = 0usize;
        let rc = unsafe {
            doom_engine_ex2_resource_sbf_ri2_snapshot_rsn3_live(
                source_context,
                payload_out.as_mut_ptr(),
                payload_out.len(),
                &mut payload_len,
            )
        };
        engine_error(rc)?;
        if payload_len > rsn3::MAX_PAYLOAD_BYTES
            || payload_len + rsn3::HEADER_BYTES > SNAPSHOT_CACHE_BYTES
        {
            return Err(KernelError::InvalidInput);
        }
        snapshot[..8].copy_from_slice(rsn3::MAGIC);
        put_u32(snapshot, 8, rsn3::VERSION);
        put_u32(snapshot, 12, rsn3::HEADER_BYTES as u32);
        put_u32(snapshot, 16, SNAPSHOT_BYTES);
        put_u32(snapshot, 20, phase.state_cursor);
        put_u32(snapshot, 24, u32_at(state[1].data, 20));
        put_u32(snapshot, 28, payload_len as u32);
        snapshot[32..64].copy_from_slice(&state_root);
        snapshot[64..96].copy_from_slice(&DOOM_WAD_DIGEST);
        snapshot[96..128].copy_from_slice(&resource_hash);
        snapshot[160..192].copy_from_slice(&V3_SNAPSHOT_ABI);
        let zero_hash = [0; 32];
        let snapshot_hash = dcg_program::hash::sha256(&[
            rsn3::HASH_DOMAIN,
            &snapshot[..128],
            &zero_hash,
            &snapshot[160..192],
            &snapshot[rsn3::HEADER_BYTES..rsn3::HEADER_BYTES + payload_len],
        ]);
        snapshot[128..160].copy_from_slice(&snapshot_hash);

        workspace[STRIP_CACHE_OFFSET..STRIP_CACHE_OFFSET + STRIP_CACHE_BYTES].fill(0);
        put_u32(workspace, ARENA_HIGH_WATER_OFFSET, 0);
    }

    let payload_len = {
        let snapshot =
            &workspace[SNAPSHOT_CACHE_OFFSET..SNAPSHOT_CACHE_OFFSET + SNAPSHOT_CACHE_BYTES];
        let payload_len = u32_at(snapshot, 28) as usize;
        if snapshot[..8] != *rsn3::MAGIC
            || payload_len + rsn3::HEADER_BYTES > SNAPSHOT_CACHE_BYTES
            || snapshot[160..192] != V3_SNAPSHOT_ABI
        {
            return Err(KernelError::Refused);
        }
        payload_len
    };

    // Distribute the 9.7 MB context seed over the aligned snapshot phases.
    // Each phase copies a bounded proportional range; the last phase reaches
    // the exact context end before any strip callback can run.
    let copy_start =
        (phase.output_offset as u64 * ENGINE_CONTEXT_BYTES as u64 / SNAPSHOT_BYTES as u64) as usize;
    let copy_end = (end as u64 * ENGINE_CONTEXT_BYTES as u64 / SNAPSHOT_BYTES as u64)
        .min(ENGINE_CONTEXT_BYTES as u64) as usize;
    if copy_end < copy_start || copy_end > ENGINE_CONTEXT_BYTES as usize {
        return Err(KernelError::InvalidInput);
    }
    workspace[copy_start..copy_end].copy_from_slice(&state[0].data[copy_start..copy_end]);

    output.fill(0);
    let snapshot = &workspace[SNAPSHOT_CACHE_OFFSET..SNAPSHOT_CACHE_OFFSET + SNAPSHOT_CACHE_BYTES];
    let snapshot_end = rsn3::HEADER_BYTES + payload_len;
    let output_start = phase.output_offset as usize;
    if output_start < snapshot_end {
        let count = (snapshot_end - output_start).min(output.len());
        output[..count].copy_from_slice(&snapshot[output_start..output_start + count]);
    }
    Ok(output.len())
}

#[cfg(all(feature = "sbf", target_os = "solana"))]
fn render_doom_view_at_workspace_base(
    kernel: &DoomKernel,
    phase: ViewPhase,
    state: &[AccountSpan<'_>],
    resources: &[AccountSpan<'_>],
    commitment: &[u8; 32],
    workspace_header: &mut [u8],
    workspace: &mut [u8],
    output: &mut [u8],
) -> Result<usize, KernelError> {
    use doom_sbf::render_snapshot_v3 as rsn3;
    use doom_sbf::resource_sbf;

    validate_render_state(state)?;
    if workspace_header.len() != 128
        || workspace.len() < V3_VIEW_WORKSPACE_BYTES as usize
        || phase.source_offset != 0
        || output.is_empty()
    {
        return Err(KernelError::InvalidInput);
    }
    // The final snapshot bytes share one RUN_PHASE chunk with the first strip.
    // Snapshotting still reads the committed context; only strip rendering
    // needs the workspace account mapped at the fixed engine address.
    if phase.role == 0 {
        return render_doom_view(
            kernel, phase, state, resources, commitment, workspace, output,
        );
    }
    if !(1..=8).contains(&phase.role) {
        return Err(KernelError::InvalidInput);
    }
    let bundle = kernel.validate_resources(resources, commitment)?;
    let payload_len = u32_at(workspace, SNAPSHOT_CACHE_OFFSET + 28) as usize;
    if workspace[SNAPSHOT_CACHE_OFFSET..SNAPSHOT_CACHE_OFFSET + 8] != *rsn3::MAGIC
        || payload_len + rsn3::HEADER_BYTES > SNAPSHOT_CACHE_BYTES
        || workspace[SNAPSHOT_CACHE_OFFSET + 160..SNAPSHOT_CACHE_OFFSET + 192] != V3_SNAPSHOT_ABI
        || u32_at(workspace, SNAPSHOT_CACHE_OFFSET + 20) != phase.state_cursor
    {
        return Err(KernelError::Refused);
    }
    let (_, state_root) = doom_sbf::sim2::verified_state_root(state[1].data, state[2].data)
        .map_err(|_| KernelError::Refused)?;
    if workspace[SNAPSHOT_CACHE_OFFSET + 32..SNAPSHOT_CACHE_OFFSET + 64] != state_root
        || u32_at(workspace, SNAPSHOT_CACHE_OFFSET + 24) != u32_at(state[1].data, 20)
    {
        return Err(KernelError::Refused);
    }

    let engine_header = &workspace[BOOTSTRAP_OFFSET..BOOTSTRAP_OFFSET + RSR_HEADER_BYTES];
    let resource_hash = dcg_program::hash::sha256(&[bundle.ri2]);
    if engine_header[..8] != *b"DOOM-RSR"
        || engine_header[48..80] != DOOM_WAD_DIGEST
        || engine_header[80..112] != resource_hash
    {
        return Err(KernelError::Refused);
    }

    let strip_id = u32::from(phase.role - 1);
    let strip_start = STRIP_CACHE_OFFSET + strip_id as usize * STRIP_BYTES as usize;
    let strip_end = strip_start + STRIP_BYTES as usize;
    let phase_end = (phase.output_offset as usize)
        .checked_add(output.len())
        .ok_or(KernelError::InvalidInput)?;
    if phase_end > STRIP_BYTES as usize || strip_end > workspace.len() {
        return Err(KernelError::InvalidInput);
    }

    if workspace[strip_start..strip_start + 8] != *b"DOOM-RS1" {
        let snapshot_hash: [u8; 32] = workspace
            [SNAPSHOT_CACHE_OFFSET + 128..SNAPSHOT_CACHE_OFFSET + 160]
            .try_into()
            .map_err(|_| KernelError::InvalidInput)?;
        let cursor = u32_at(workspace, SNAPSHOT_CACHE_OFFSET + 20);
        let gametic = u32_at(workspace, SNAPSHOT_CACHE_OFFSET + 24);
        let mut saved_workspace_header = [0u8; 128];
        saved_workspace_header.copy_from_slice(workspace_header);
        let context_ptr = workspace_header.as_mut_ptr();
        DoomKernel::require_context_address(context_ptr)?;
        // The logical workspace stores a complete context at payload offset
        // zero. Account memory begins 128 bytes earlier, at workspace_header,
        // so move the context tail down before using the account as a
        // contiguous fixed-address engine image.
        workspace_header.copy_from_slice(&workspace[..128]);
        workspace.copy_within(128..ENGINE_CONTEXT_BYTES as usize, 0);
        let header_ptr = unsafe { context_ptr.add(BOOTSTRAP_OFFSET) };
        let bootstrap_ptr = unsafe { context_ptr.add(BOOTSTRAP_ARENA_OFFSET) };
        let temp = &mut workspace[RENDER_TEMP_OFFSET..RENDER_TEMP_OFFSET + RENDER_TEMP_BYTES];
        let cache_magic = u32_at(temp, 0);
        let cache_version = u32_at(temp, 8);
        let cache_base = u32_at(temp, 48);
        let cache_dynamic = u32_at(temp, 52);
        let cache_end = u32_at(temp, 56);
        let cache_ptr = temp.as_mut_ptr();
        let pixels_ptr = unsafe { cache_ptr.add(RENDER_CACHE_BYTES) };
        let workspace_ptr = workspace.as_ptr();
        let payload_ptr = unsafe { workspace_ptr.add(SNAPSHOT_CACHE_OFFSET + rsn3::HEADER_BYTES) };
        let snapshot_hash_ptr = unsafe { workspace_ptr.add(SNAPSHOT_CACHE_OFFSET + 128) };
        let rsr_header = unsafe { core::slice::from_raw_parts(header_ptr, RSR_HEADER_BYTES) };
        let rsr_base = u64::from_le_bytes(
            rsr_header[112..120]
                .try_into()
                .map_err(|_| KernelError::InvalidInput)?,
        );
        let mut bad_root_index = u32::MAX;
        let mut bad_root_offset = u32::MAX;
        for root_index in 0..32usize {
            let root_offset = u32_at(rsr_header, 160 + root_index * 4);
            if root_offset != u32::MAX && root_offset >= resource_sbf::CONTEXT_BYTES as u32 {
                bad_root_index = root_index as u32;
                bad_root_offset = root_offset;
                break;
            }
        }
        solana_program::msg!(
            "Doom workspace RSR layout: ptr={} base={} magic={} version={} header_bytes={} account_bytes={} context_bytes={} roots={} bad_root={} bad_offset={} cache_magic={} cache_version={} cache_base={} cache_dynamic={} cache_end={}",
            context_ptr as usize,
            rsr_base,
            u32_at(rsr_header, 0),
            u32_at(rsr_header, 8),
            u32_at(rsr_header, 12),
            u32_at(rsr_header, 16),
            u32_at(rsr_header, 20),
            u32_at(rsr_header, 40),
            bad_root_index,
            bad_root_offset,
            cache_magic,
            cache_version,
            cache_base,
            cache_dynamic,
            cache_end
        );
        let mut arena_high_water = 0usize;
        let rc = unsafe {
            doom_engine_rsn3_render_static(
                context_ptr,
                resource_sbf::CONTEXT_BYTES,
                header_ptr,
                RSR_HEADER_BYTES,
                bootstrap_ptr,
                BOOTSTRAP_ARENA_BYTES,
                payload_ptr,
                payload_len,
                strip_id,
                cache_ptr,
                RENDER_CACHE_BYTES,
                snapshot_hash_ptr,
                pixels_ptr,
                RENDER_PIXEL_BYTES,
                &mut arena_high_water,
            )
        };
        // Restore the logical context layout before preserving its updated
        // prefix in the payload and restoring the authenticated DCG header.
        workspace.copy_within(0..ENGINE_CONTEXT_BYTES as usize - 128, 128);
        workspace[..128].copy_from_slice(workspace_header);
        workspace_header.copy_from_slice(&saved_workspace_header);
        if rc != 0 {
            solana_program::msg!(
                "Doom workspace render refused: rc={} cursor={} strip={} payload_bytes={}",
                rc,
                cursor,
                strip_id,
                payload_len
            );
        }
        engine_error(rc)?;
        if arena_high_water > BOOTSTRAP_ARENA_BYTES {
            solana_program::msg!(
                "Doom workspace arena overflow: used={} limit={}",
                arena_high_water,
                BOOTSTRAP_ARENA_BYTES
            );
            return Err(KernelError::Refused);
        }
        let prior_high_water = u32_at(workspace, ARENA_HIGH_WATER_OFFSET) as usize;
        put_u32(
            workspace,
            ARENA_HIGH_WATER_OFFSET,
            prior_high_water
                .max(arena_high_water)
                .min(u32::MAX as usize) as u32,
        );
        let pixels = unsafe { core::slice::from_raw_parts(pixels_ptr, RENDER_PIXEL_BYTES) };
        let strip = &mut workspace[strip_start..strip_end];
        strip.fill(0);
        strip[..8].copy_from_slice(b"DOOM-RS1");
        put_u32(strip, 8, 1);
        put_u32(strip, 12, 128);
        put_u32(strip, 16, strip_id);
        put_u32(strip, 20, cursor);
        put_u32(strip, 24, gametic);
        put_u32(strip, 28, RENDER_PIXEL_BYTES as u32);
        strip[32..64].copy_from_slice(&snapshot_hash);
        strip[64..96].copy_from_slice(&state_root);
        strip[128..].copy_from_slice(pixels);
        let strip_hash = dcg_program::hash::sha256(&[
            rsn3::STRIP_HASH_DOMAIN,
            &strip_id.to_le_bytes(),
            &cursor.to_le_bytes(),
            &gametic.to_le_bytes(),
            &snapshot_hash,
            &strip[128..],
        ]);
        strip[96..128].copy_from_slice(&strip_hash);
    }

    output.copy_from_slice(
        &workspace[strip_start + phase.output_offset as usize..strip_start + phase_end],
    );
    Ok(output.len())
}

fn validate_render_state(state: &[AccountSpan<'_>]) -> Result<(), KernelError> {
    if state.len() != 3
        || state
            .iter()
            .any(|span| span.schema != STATE_SCHEMA || span.is_writable)
        || state[0].offset != 0
        || state[0].data.len() != ENGINE_CONTEXT_BYTES as usize
        || state[1].offset != ST1_OFFSET
        || state[1].data.len() != ST1_ACCOUNT_BYTES as usize
        || state[2].offset != MP1_OFFSET
        || state[2].data.len() != MP1_ACCOUNT_BYTES as usize
    {
        return Err(KernelError::InvalidInput);
    }
    Ok(())
}

impl StatefulKernel for DoomKernel {
    fn initial_state(&self, _output: &mut [u8]) -> Result<usize, KernelError> {
        Err(KernelError::Refused)
    }

    fn transition(
        &self,
        _input: &[u8],
        _prior_state: &[u8],
        _output: &mut [u8],
        _next_state: &mut [u8],
    ) -> Result<(usize, usize), KernelError> {
        Err(KernelError::Refused)
    }

    fn initial_state_spans(&self, _spans: &mut [StateSpanMut<'_>]) -> Result<usize, KernelError> {
        Err(KernelError::Refused)
    }

    fn bind_invocation_state(&self, spans: &mut [StateSpanMut<'_>]) -> Result<(), KernelError> {
        Self::validate_spans(spans)?;
        let address = spans[0].data_address();
        if address.is_null() {
            return Err(KernelError::Refused);
        }
        Self::require_context_address(address)?;
        self.invocation_context.store(address, Ordering::Relaxed);
        Ok(())
    }

    fn unbind_invocation_state(&self) {
        self.invocation_context
            .store(core::ptr::null_mut(), Ordering::Relaxed);
    }

    fn transition_spans(
        &self,
        input: &[u8],
        state: &mut [StateSpanMut<'_>],
        output: &mut [u8],
    ) -> Result<usize, KernelError> {
        let outcome = self.transition_spans_with_outcome(input, state, output)?;
        match outcome.disposition {
            TransitionDisposition::Continue => Ok(outcome.output_bytes),
            TransitionDisposition::HaltBefore { .. } | TransitionDisposition::HaltAfter { .. } => {
                Err(KernelError::Refused)
            }
        }
    }

    fn transition_spans_with_outcome(
        &self,
        input: &[u8],
        state: &mut [StateSpanMut<'_>],
        output: &mut [u8],
    ) -> Result<TransitionOutcome, KernelError> {
        Self::validate_spans(state)?;
        if input.len() != 4 || output.len() < 4 || input[3] & 0x80 != 0 {
            return Err(KernelError::InvalidInput);
        }
        let context = self.context_ptr()?;
        if context != state[0].data.as_mut_ptr() {
            return Err(KernelError::Refused);
        }
        Self::require_context_address(context)?;
        #[cfg(all(feature = "sbf", target_os = "solana"))]
        {
            let step = unsafe { doom_sbf_sim2_step(context, input.as_ptr(), input.len()) };
            if (1..=5).contains(&step) {
                return Ok(TransitionOutcome {
                    output_bytes: 0,
                    disposition: TransitionDisposition::HaltBefore {
                        reason: step as u32,
                    },
                });
            }
            if step != 0 {
                return Err(KernelError::Refused);
            }
            let mut state_len = 0usize;
            let publish = unsafe {
                doom_sbf_sim2_publish(
                    context,
                    state[1].data.as_mut_ptr(),
                    state[1].data.len(),
                    &mut state_len,
                    state[2].data.as_mut_ptr(),
                    state[2].data.len(),
                )
            };
            if publish != 0 || state_len < 64 || state_len > state[1].data.len() {
                return Err(KernelError::Refused);
            }
            output[..4].copy_from_slice(input);
            Ok(TransitionOutcome {
                output_bytes: 4,
                disposition: TransitionDisposition::Continue,
            })
        }
        #[cfg(not(all(feature = "sbf", target_os = "solana")))]
        {
            let _ = context;
            Err(KernelError::Refused)
        }
    }

    fn view_abis(&self) -> &'static [ViewAbi] {
        &VIEW_ABIS
    }

    fn max_view_phase_bytes(&self) -> u32 {
        V3_VIEW_PHASE_BYTES
    }

    fn view_phase_compute_units(&self) -> u32 {
        V3_VIEW_PHASE_COMPUTE_CEILING
    }

    fn max_initialization_phase_bytes(&self) -> u32 {
        V3_INITIALIZATION_PHASE_BYTES
    }

    fn initialization_phase_compute_units(&self) -> u32 {
        V3_INITIALIZATION_PHASE_COMPUTE_UNITS
    }

    fn initialize_state_phase(
        &self,
        phase: InitializationPhase,
        resources: &[AccountSpan<'_>],
        commitment: &[u8; 32],
        state: &mut [StateSpanMut<'_>],
    ) -> Result<usize, KernelError> {
        Self::validate_spans(state)?;
        let bundle = (if phase.cursor == 0 {
            self.validate_resources(resources, commitment)
        } else {
            self.parse_resources(resources, commitment)
        })?;
        let context = self.context_ptr()?;
        if context != state[0].data.as_mut_ptr() {
            return Err(KernelError::Refused);
        }
        Self::require_context_address(context)?;
        if phase.total_bytes != STATE_BYTES
            || phase.compute_units != V3_INITIALIZATION_PHASE_COMPUTE_UNITS
            || phase.cursor % V3_INITIALIZATION_PHASE_BYTES != 0
        {
            return Err(KernelError::InvalidInput);
        }
        let start = phase.cursor as usize;
        let remaining = (phase.total_bytes - phase.cursor) as usize;
        let count = remaining.min(V3_INITIALIZATION_PHASE_BYTES as usize);
        let end = start.checked_add(count).ok_or(KernelError::InvalidInput)?;
        let wad_start = WAD_IMAGE_OFFSET;
        let wad_end = wad_start
            .checked_add(bundle.wad.len())
            .ok_or(KernelError::InvalidInput)?;

        // Before the final WAD byte arrives, initialize this range to zero
        // and copy the corresponding WAD slice. Account growth is zeroed too;
        // after the copy completes, later engine phases must preserve writes
        // made by earlier C calls in the bootstrap arena.
        let first_engine_chunk = (wad_end - 1) / V3_INITIALIZATION_PHASE_BYTES as usize;
        let current_chunk = start / V3_INITIALIZATION_PHASE_BYTES as usize;
        if current_chunk <= first_engine_chunk {
            write_state_range(state, start, end, None)?;
            let copy_from = start.max(wad_start);
            let copy_to = end.min(wad_end);
            if copy_from < copy_to {
                let src_start = copy_from - wad_start;
                write_state_range(
                    state,
                    copy_from,
                    copy_to,
                    Some(&bundle.wad[src_start..src_start + copy_to - copy_from]),
                )?;
            }
        }

        #[cfg(all(feature = "sbf", target_os = "solana"))]
        if end >= wad_end {
            let phase_index = current_chunk - first_engine_chunk;
            if phase_index < INITIAL_ENGINE_PHASES as usize {
                let context_bytes = state[0].data.len();
                let context_ptr = state[0].data.as_mut_ptr();
                let bootstrap_ptr = unsafe { context_ptr.add(BOOTSTRAP_ARENA_OFFSET) };
                let bootstrap_bytes = BOOTSTRAP_ARENA_BYTES;
                let build_header_ptr = unsafe { context_ptr.add(BOOTSTRAP_OFFSET) };
                let mut high_water = 0usize;
                let rc = unsafe {
                    if phase_index < 6 {
                        doom_engine_rsn3_build_static_phase(
                            phase_index as u32,
                            context_ptr,
                            context_bytes,
                            bundle.ri2.as_ptr(),
                            bundle.ri2.len(),
                            context_ptr.add(wad_start),
                            bundle.wad.len(),
                            bootstrap_ptr,
                            bootstrap_bytes,
                            build_header_ptr,
                            RSR_HEADER_BYTES,
                            &mut high_water,
                        )
                    } else if phase_index < 6 + 71 {
                        doom_engine_ex2_resource_sbf_ri2_split_newgame_prepare_phase(
                            (phase_index - 6) as u32,
                            context_ptr,
                            bootstrap_ptr,
                            bootstrap_bytes,
                            &mut high_water,
                        )
                    } else if phase_index < 6 + 71 + 37 {
                        doom_engine_ex2_resource_sbf_ri2_split_newgame_phase(
                            (phase_index - 6 - 71) as u32,
                            context_ptr,
                            bootstrap_ptr,
                            bootstrap_bytes,
                            &mut high_water,
                        )
                    } else {
                        let mut state_len = 0usize;
                        let mut mirror_len = 0usize;
                        let mut frame_len = 0usize;
                        let finalize_rc = doom_engine_ex2_resource_sbf_ri2_split_finalize(
                            context_ptr,
                            bootstrap_ptr,
                            bootstrap_bytes,
                            state[1].data.as_mut_ptr(),
                            state[1].data.len(),
                            &mut state_len,
                            state[2].data.as_mut_ptr(),
                            state[2].data.len(),
                            &mut mirror_len,
                            state[2].data.as_mut_ptr(),
                            state[2].data.len(),
                            &mut frame_len,
                            &mut high_water,
                        );
                        if finalize_rc != 0
                            || state_len < 64
                            || state_len > state[1].data.len()
                            || mirror_len != state[2].data.len()
                            || frame_len != RENDER_FRAME_BYTES
                        {
                            return Err(KernelError::Refused);
                        }
                        let publish_rc = doom_sbf_sim2_publish(
                            context_ptr,
                            state[1].data.as_mut_ptr(),
                            state[1].data.len(),
                            &mut state_len,
                            state[2].data.as_mut_ptr(),
                            state[2].data.len(),
                        );
                        publish_rc
                    }
                };
                engine_error(rc)?;
                if phase_index == 5 {
                    finish_rsr_header(
                        state[0].data,
                        dcg_program::hash::sha256(&[bundle.ri2]),
                        high_water,
                    )?;
                }
            }
        }
        #[cfg(not(all(feature = "sbf", target_os = "solana")))]
        {
            let _ = (phase, bundle);
            return Err(KernelError::Refused);
        }
        Ok(count)
    }

    fn max_view_workspace_bytes(&self) -> u32 {
        V3_VIEW_WORKSPACE_BYTES
    }

    fn clear_view_workspace_on_begin(&self) -> bool {
        false
    }

    fn view_workspace_at_account_base(&self) -> bool {
        true
    }

    fn render_view_phase(
        &self,
        phase: ViewPhase,
        state: &[dcg_program::kernel::AccountSpan<'_>],
        output: &mut [u8],
    ) -> Result<usize, KernelError> {
        let (expected_len, max_bytes) = if phase.role == 0 {
            (SNAPSHOT_BYTES, SNAPSHOT_BYTES)
        } else if (1..=8).contains(&phase.role) {
            (STRIP_BYTES, STRIP_BYTES)
        } else {
            return Err(KernelError::InvalidInput);
        };
        let end = phase
            .output_offset
            .checked_add(output.len() as u32)
            .ok_or(KernelError::InvalidInput)?;
        if phase.source_offset != 0 || output.is_empty() || end > expected_len || max_bytes == 0 {
            return Err(KernelError::InvalidInput);
        }
        let mut offset = 0u32;
        for span in state {
            if span.offset != offset {
                return Err(KernelError::InvalidInput);
            }
            offset = offset
                .checked_add(span.data.len() as u32)
                .ok_or(KernelError::InvalidInput)?;
        }
        if offset != STATE_BYTES {
            return Err(KernelError::InvalidInput);
        }
        Err(KernelError::Refused)
    }

    fn render_view_phase_with_resources(
        &self,
        phase: ViewPhase,
        state: &[AccountSpan<'_>],
        resources: &[AccountSpan<'_>],
        commitment: &[u8; 32],
        workspace: &mut [u8],
        output: &mut [u8],
    ) -> Result<usize, KernelError> {
        #[cfg(all(feature = "sbf", target_os = "solana"))]
        {
            render_doom_view(self, phase, state, resources, commitment, workspace, output)
        }
        #[cfg(not(all(feature = "sbf", target_os = "solana")))]
        {
            let _ = (phase, state, resources, commitment, workspace);
            Err(KernelError::Refused)
        }
    }

    fn render_view_phase_with_workspace_header(
        &self,
        phase: ViewPhase,
        state: &[AccountSpan<'_>],
        resources: &[AccountSpan<'_>],
        commitment: &[u8; 32],
        workspace_header: &mut [u8],
        workspace: &mut [u8],
        output: &mut [u8],
    ) -> Result<usize, KernelError> {
        #[cfg(all(feature = "sbf", target_os = "solana"))]
        {
            render_doom_view_at_workspace_base(
                self,
                phase,
                state,
                resources,
                commitment,
                workspace_header,
                workspace,
                output,
            )
        }
        #[cfg(not(all(feature = "sbf", target_os = "solana")))]
        {
            let _ = (
                phase,
                state,
                resources,
                commitment,
                workspace_header,
                workspace,
                output,
            );
            Err(KernelError::Refused)
        }
    }
}

struct ManifestKernel;

impl Kernel for ManifestKernel {
    fn manifest(&self) -> &'static KernelManifest {
        &MANIFEST
    }
    fn execute(&self, _input: &[u8], _output: &mut [u8]) -> Result<usize, KernelError> {
        Err(KernelError::Refused)
    }
}

impl StatefulKernel for ManifestKernel {
    fn initial_state(&self, _output: &mut [u8]) -> Result<usize, KernelError> {
        Err(KernelError::Refused)
    }
    fn transition(
        &self,
        _input: &[u8],
        _prior_state: &[u8],
        _output: &mut [u8],
        _next_state: &mut [u8],
    ) -> Result<(usize, usize), KernelError> {
        Err(KernelError::Refused)
    }
    fn initial_state_spans(&self, _spans: &mut [StateSpanMut<'_>]) -> Result<usize, KernelError> {
        Err(KernelError::Refused)
    }
    fn transition_spans(
        &self,
        _input: &[u8],
        _state: &mut [StateSpanMut<'_>],
        _output: &mut [u8],
    ) -> Result<usize, KernelError> {
        Err(KernelError::Refused)
    }
    fn initial_state_spans_with_resources(
        &self,
        _resources: &[dcg_program::kernel::AccountSpan<'_>],
        _commitment: &[u8; 32],
        _spans: &mut [StateSpanMut<'_>],
    ) -> Result<usize, KernelError> {
        Err(KernelError::Refused)
    }
    fn view_abis(&self) -> &'static [ViewAbi] {
        &VIEW_ABIS
    }
    fn max_view_phase_bytes(&self) -> u32 {
        V3_VIEW_PHASE_BYTES
    }
    fn view_phase_compute_units(&self) -> u32 {
        V3_VIEW_PHASE_COMPUTE_CEILING
    }
}

static MANIFEST_KERNEL: ManifestKernel = ManifestKernel;
static KERNELS: [&'static dyn Kernel; 1] = [&MANIFEST_KERNEL];

/// Exported metadata contains no mutable address cell and is not the runtime
/// kernel instance used by a v3 instruction.
#[no_mangle]
#[used]
pub static DOOM_DCG_APPLICATION_MANIFEST: ApplicationManifest = ApplicationManifest {
    application_id: APPLICATION_ID,
    version: APPLICATION_VERSION,
    kernels: &KERNELS,
    optimistic_replays: &[],
    legacy_forms: &[],
    require_legacy_form_binding: false,
    hooks: &dcg_program::compatibility::REVISION8_COMPATIBILITY,
    decision_routes: &dcg_program::compatibility::REVISION8_COMPATIBILITY,
};

pub fn validate_manifest() -> Result<(), dcg_program::kernel::ManifestError> {
    DOOM_DCG_APPLICATION_MANIFEST.validate()
}

#[cfg(feature = "sbf")]
pub fn process_instruction<'a>(
    program_id: &solana_program::pubkey::Pubkey,
    accounts: &'a [solana_program::account_info::AccountInfo<'a>],
    instruction_data: &[u8],
) -> solana_program::entrypoint::ProgramResult {
    let kernel = DoomKernel::default();
    dcg_program::stateful::v3::process_with_kernel(program_id, accounts, instruction_data, &kernel)
}

#[cfg(feature = "sbf")]
solana_program::entrypoint!(process_instruction);

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .expect("validated fixed width"),
    )
}
