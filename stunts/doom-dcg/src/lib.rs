// SPDX-License-Identifier: GPL-3.0-only

//! Stateful-v2 adapter boundary for the pinned Doom engine.
//!
//! V2 supplies the session, indexed command stream, state spans, resource
//! commitment, and phased view accounts.  The Doom kernel is deliberately
//! fail-closed at the engine boundary: the linked engine still pins
//! `DOOM_CTX` to the first account's fixed SVM address, while v2 puts the
//! state spans after authority/session/stream.  Keeping the invocation-local
//! address check here makes that mismatch explicit and keeps a pointer out of
//! committed state.

use core::sync::atomic::{AtomicPtr, Ordering};

use dcg_program::kernel::{
    ApplicationManifest, Kernel, KernelError, KernelId, KernelManifest, ModeId, PortLayout,
    ResourceLimits, StateSchema, StateSpanMut, StatefulKernel, VersionedId, ViewAbi, ViewPhase,
};

pub const APPLICATION_ID: &[u8] = b"basanos-doom/2";
pub const APPLICATION_VERSION: u16 = 2;

/// DCG's v2 consensus mode.  The v1 mode is not compatible with this kernel.
pub const MODE_CONSENSUS_V2: ModeId = dcg_program::stateful::v2::MODE_CONSENSUS_V2;

/// One composite v2 kernel owns simulation and its three RSN/3 view ABIs.
/// V2 binds one kernel identity per session, so the former separate v1
/// simulator/snapshot/strip IDs cannot describe one Doom session.
pub const DOOM_KERNEL_ID: KernelId = KernelId(*b"basanos-doom-v2\0");
pub const STATE_SCHEMA: VersionedId = VersionedId {
    id: 0x4d4f_4f44, // ASCII "DOOM" interpreted little-endian.
    version: 1,
};
pub const INPUT_SCHEMA: VersionedId = VersionedId {
    id: 0x494f_4f44, // ASCII "DOOI" interpreted little-endian.
    version: 1,
};
pub const OUTPUT_SCHEMA: VersionedId = VersionedId {
    id: 0x4f4f_4f44, // ASCII "DOOO" interpreted little-endian.
    version: 1,
};
pub const RESOURCE_SCHEMA: VersionedId = VersionedId {
    id: 0x5352_4744, // ASCII "DGRS" interpreted little-endian.
    version: 1,
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
const MAX_ADVANCE_TICS: u32 = 8;
const STEP_COMPUTE_CEILING: u64 = 175_000;
const VIEW_PHASE_BYTES: u32 = 65_536;
const VIEW_PHASE_COMPUTE_CEILING: u32 = 1_400_000;

const SNAPSHOT_ABI: [u8; 32] = [0x52; 32];
const STRIP_ABIS: [[u8; 32]; 8] = [
    [0x53; 32], [0x54; 32], [0x55; 32], [0x56; 32], [0x57; 32], [0x58; 32], [0x59; 32], [0x5a; 32],
];
static MODES: [ModeId; 1] = [MODE_CONSENSUS_V2];
static VIEW_ABIS: [ViewAbi; 9] = [
    ViewAbi {
        role: 0,
        id: SNAPSHOT_ABI,
        max_bytes: SNAPSHOT_BYTES,
    },
    ViewAbi {
        role: 1,
        id: STRIP_ABIS[0],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 2,
        id: STRIP_ABIS[1],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 3,
        id: STRIP_ABIS[2],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 4,
        id: STRIP_ABIS[3],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 5,
        id: STRIP_ABIS[4],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 6,
        id: STRIP_ABIS[5],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 7,
        id: STRIP_ABIS[6],
        max_bytes: STRIP_BYTES,
    },
    ViewAbi {
        role: 8,
        id: STRIP_ABIS[7],
        max_bytes: STRIP_BYTES,
    },
];

static MANIFEST: KernelManifest = KernelManifest {
    id: DOOM_KERNEL_ID,
    semantic_version: 2,
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
        // Designed per-step declaration: 8 * 175,000 is below v2's
        // 1,400,000-CU transaction ceiling. This is not a measurement of the
        // Doom engine's worst one-tic cost.
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
            || dcg_program::hash::sha256(&[wad]) != DOOM_WAD_DIGEST
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

/// The engine binding is held only by a kernel value on the program's stack
/// while one v2 callback sequence is active. The static manifest kernel below
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
        if resource.key != RESOURCE_KEY
            || resource.schema != RESOURCE_SCHEMA
            || resource.owner == [0; 32]
            || dcg_program::hash::sha256(&[resource.data]) != *commitment
        {
            return Err(KernelError::Refused);
        }
        DoomResourceBundle::decode(resource.data)
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

    fn require_legacy_context_address(pointer: *mut u8) -> Result<(), KernelError> {
        // Local shim for the pinned 0007 engine ABI. V2's state account is not
        // account zero, so this check currently refuses before any C dereference.
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

    fn initial_state_spans_with_resources(
        &self,
        resources: &[dcg_program::kernel::AccountSpan<'_>],
        commitment: &[u8; 32],
        spans: &mut [StateSpanMut<'_>],
    ) -> Result<usize, KernelError> {
        // Authenticate and parse the v2 resource before touching state. The
        // complete RI2/WAD digest itself is too large for v2's one-shot init
        // contract under the current SVM ceiling; see the round-5 record.
        let bundle = self.validate_resources(resources, commitment)?;
        let _ = (bundle.ri2.len(), bundle.wad.len());
        Self::validate_spans(spans)?;
        let context = spans[0].data_address();
        if self.context_ptr()? != context {
            return Err(KernelError::Refused);
        }
        Self::require_legacy_context_address(context)?;
        // The 0007 engine has no new-game import entry accepting an arbitrary
        // span base. The local shim must refuse until that contract is added.
        Err(KernelError::Refused)
    }

    fn bind_invocation_state(&self, spans: &mut [StateSpanMut<'_>]) -> Result<(), KernelError> {
        Self::validate_spans(spans)?;
        let address = spans[0].data_address();
        if address.is_null() {
            return Err(KernelError::Refused);
        }
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
        Self::validate_spans(state)?;
        if input.len() != 4 || output.len() < 4 {
            return Err(KernelError::InvalidInput);
        }
        if input[3] & 0x80 != 0 {
            return Err(KernelError::Refused);
        }
        let context = self.context_ptr()?;
        if context != state[0].data.as_mut_ptr() {
            return Err(KernelError::Refused);
        }
        Self::require_legacy_context_address(context)?;
        #[cfg(all(feature = "sbf", target_os = "solana"))]
        {
            unsafe extern "C" {
                fn doom_sbf_sim2_step(
                    context: *mut u8,
                    command: *const u8,
                    command_len: usize,
                ) -> i32;
                fn doom_sbf_sim2_publish(
                    context: *mut u8,
                    state_out: *mut u8,
                    state_cap: usize,
                    state_out_len: *mut usize,
                    mirror_out: *mut u8,
                    mirror_cap: usize,
                ) -> i32;
            }
            // A halt code must be surfaced to the caller so it can issue the
            // separate v2 HALT_SESSION operation at the committed cursor.
            // Until v2 exposes a transition stop result, multi-step ADVANCE
            // cannot commit a prefix and halt atomically.
            let step = unsafe { doom_sbf_sim2_step(context, input.as_ptr(), input.len()) };
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
            Ok(4)
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
        VIEW_PHASE_BYTES
    }

    fn view_phase_compute_units(&self) -> u32 {
        VIEW_PHASE_COMPUTE_CEILING
    }

    fn render_view_phase(
        &self,
        phase: ViewPhase,
        state: &[dcg_program::kernel::AccountSpan<'_>],
        output: &mut [u8],
    ) -> Result<usize, KernelError> {
        // Validate a deterministic, contiguous render chunk before the local
        // shim refuses: v2 does not pass the committed RI2/WAD resource or a
        // writable workspace account to this callback.
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
        let _ = self;
        Err(KernelError::Refused)
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
        VIEW_PHASE_BYTES
    }
    fn view_phase_compute_units(&self) -> u32 {
        VIEW_PHASE_COMPUTE_CEILING
    }
}

static MANIFEST_KERNEL: ManifestKernel = ManifestKernel;
static KERNELS: [&'static dyn Kernel; 1] = [&MANIFEST_KERNEL];

/// Exported metadata contains no mutable address cell and is not the runtime
/// kernel instance used by a v2 instruction.
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
    if instruction_data.first().is_some_and(|tag| {
        (dcg_program::stateful::TAG_OPEN_SESSION..=dcg_program::stateful::TAG_ANCHOR).contains(tag)
    }) {
        dcg_program::stateful::process_with_kernel(program_id, accounts, instruction_data, &kernel)
    } else {
        doom_sbf::transport_process_instruction(program_id, accounts, instruction_data)
    }
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
