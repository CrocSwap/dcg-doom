// SPDX-License-Identifier: GPL-3.0-only

//! Doom's extracted-DCG kernel manifest, round-3 stepping stone.
//!
//! This crate deliberately registers the future stateful kernels but does not
//! pretend the DCG v1 byte contract can drive Doom's multi-account SVM state.
//! Until an account-span dispatcher exists, each execution path fails closed.

use dcg_program::kernel::{
    ApplicationManifest, Kernel, KernelError, KernelId, KernelManifest, ModeId, PortLayout,
    ResourceLimits, StateSchema, StatefulKernel, VersionedId,
};

pub const APPLICATION_ID: &[u8] = b"basanos-doom/1";
pub const APPLICATION_VERSION: u16 = 1;

/// DCG's versioned consensus mode identity, repeated here as an app-owned
/// constant so the image's capability declaration is explicit.
pub const MODE_CONSENSUS_V1: ModeId = VersionedId {
    id: 0x434f_4e53,
    version: 1,
};

pub const SIM_KERNEL_ID: KernelId = KernelId(*b"doom-sim-step-v1");
pub const SNAPSHOT_KERNEL_ID: KernelId = KernelId(*b"doom-snapshot-v1");
pub const STRIP_KERNEL_ID: KernelId = KernelId(*b"doom-rstrip-v1\0\0");

// Account sizes are copied from the retained round-2 capture. The sim-state
// bundle size is a designed contiguous representation of context + ST1 + MP1;
// DCG v1 does not define how those separate accounts are bound to this schema.
pub const ENGINE_CONTEXT_BYTES: u32 = 9_734_160;
pub const ST1_ACCOUNT_BYTES: u32 = 126_976;
pub const MP1_ACCOUNT_BYTES: u32 = 65_536;
pub const SIM_ENGINE_STATE_BUNDLE_BYTES: u32 =
    ENGINE_CONTEXT_BYTES + ST1_ACCOUNT_BYTES + MP1_ACCOUNT_BYTES;
pub const SNAPSHOT_ACCOUNT_BYTES: u32 = 1_048_768;
pub const STRIP_ACCOUNT_BYTES: u32 = 8_128;

const SIM_MODES: [ModeId; 1] = [MODE_CONSENSUS_V1];
const SNAPSHOT_MODES: [ModeId; 1] = [MODE_CONSENSUS_V1];
const STRIP_MODES: [ModeId; 1] = [MODE_CONSENSUS_V1];

static SIM_MANIFEST: KernelManifest = KernelManifest {
    id: SIM_KERNEL_ID,
    semantic_version: 1,
    abi_version: 1,
    input: PortLayout {
        id: VersionedId { id: 1, version: 1 },
        max_bytes: 8,
        alignment: 4,
    },
    output: PortLayout {
        id: VersionedId { id: 2, version: 1 },
        max_bytes: 78,
        alignment: 1,
    },
    state: Some(StateSchema {
        id: VersionedId { id: 1, version: 1 },
        max_bytes: SIM_ENGINE_STATE_BUNDLE_BYTES,
    }),
    resources: ResourceLimits {
        max_input_bytes: 8,
        max_output_bytes: 78,
        max_state_bytes: SIM_ENGINE_STATE_BUNDLE_BYTES,
        max_operations: 64,
        // DCG v1 requires a number but has no Doom-shaped measured envelope.
        // This sentinel means the image makes no compute-ceiling claim.
        max_compute_units: u64::MAX,
    },
    modes: &SIM_MODES,
};

static SNAPSHOT_MANIFEST: KernelManifest = KernelManifest {
    id: SNAPSHOT_KERNEL_ID,
    semantic_version: 1,
    abi_version: 1,
    input: PortLayout {
        id: VersionedId { id: 3, version: 1 },
        max_bytes: 56,
        alignment: 1,
    },
    output: PortLayout {
        id: VersionedId { id: 4, version: 1 },
        max_bytes: SNAPSHOT_ACCOUNT_BYTES,
        alignment: 1,
    },
    state: Some(StateSchema {
        id: VersionedId { id: 1, version: 1 },
        max_bytes: SIM_ENGINE_STATE_BUNDLE_BYTES,
    }),
    resources: ResourceLimits {
        max_input_bytes: 56,
        max_output_bytes: SNAPSHOT_ACCOUNT_BYTES,
        max_state_bytes: SIM_ENGINE_STATE_BUNDLE_BYTES,
        max_operations: 1,
        max_compute_units: u64::MAX,
    },
    modes: &SNAPSHOT_MODES,
};

static STRIP_MANIFEST: KernelManifest = KernelManifest {
    id: STRIP_KERNEL_ID,
    semantic_version: 1,
    abi_version: 1,
    input: PortLayout {
        id: VersionedId { id: 5, version: 1 },
        max_bytes: 128,
        alignment: 1,
    },
    output: PortLayout {
        id: VersionedId { id: 6, version: 1 },
        max_bytes: STRIP_ACCOUNT_BYTES,
        alignment: 1,
    },
    state: Some(StateSchema {
        id: VersionedId { id: 2, version: 1 },
        max_bytes: SNAPSHOT_ACCOUNT_BYTES,
    }),
    resources: ResourceLimits {
        max_input_bytes: 128,
        max_output_bytes: STRIP_ACCOUNT_BYTES,
        max_state_bytes: SNAPSHOT_ACCOUNT_BYTES,
        max_operations: 1,
        max_compute_units: u64::MAX,
    },
    modes: &STRIP_MODES,
};

/// Fail-closed marker implementation shared by the three registered kernels.
/// The exact byte contract above is reviewable, but v1 has no way to supply
/// Doom's authenticated account bundle, WAD resources, or session input stream.
macro_rules! unavailable_kernel {
    ($name:ident, $manifest:ident) => {
        pub struct $name;

        impl Kernel for $name {
            fn manifest(&self) -> &'static KernelManifest {
                &$manifest
            }

            fn execute(&self, _input: &[u8], _output: &mut [u8]) -> Result<usize, KernelError> {
                Err(KernelError::Refused)
            }
        }

        impl StatefulKernel for $name {
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
        }
    };
}

unavailable_kernel!(DoomSimStepKernel, SIM_MANIFEST);
unavailable_kernel!(DoomSnapshotKernel, SNAPSHOT_MANIFEST);
unavailable_kernel!(DoomRenderStripKernel, STRIP_MANIFEST);

pub static SIM_STEP: DoomSimStepKernel = DoomSimStepKernel;
pub static SNAPSHOT: DoomSnapshotKernel = DoomSnapshotKernel;
pub static RENDER_STRIP: DoomRenderStripKernel = DoomRenderStripKernel;
pub static KERNELS: [&'static dyn Kernel; 3] = [&SIM_STEP, &SNAPSHOT, &RENDER_STRIP];

/// Exported from the linked SBF image so the application registry cannot be
/// stripped as unused metadata by the linker.
#[no_mangle]
#[used]
pub static DOOM_DCG_APPLICATION_MANIFEST: ApplicationManifest = ApplicationManifest {
    application_id: APPLICATION_ID,
    version: APPLICATION_VERSION,
    kernels: &KERNELS,
    optimistic_replays: &[],
    legacy_forms: &[],
    require_legacy_form_binding: false,
};

/// Retain the existing Doom SVM entrypoint in the composed image. It still
/// routes directly to DOOM-SIM/2 and RSN/3 handlers; the manifest kernels are
/// not invoked by that entrypoint.
#[cfg(feature = "sbf")]
#[no_mangle]
#[used]
pub static DOOM_DCG_EXISTING_ENTRYPOINT: unsafe extern "C" fn(*mut u8) -> u64 =
    doom_sbf::entrypoint;

/// A review/test helper; the application manifest is not dispatched by the
/// standalone Doom instruction handler in this round.
pub fn validate_manifest() -> Result<(), dcg_program::kernel::ManifestError> {
    DOOM_DCG_APPLICATION_MANIFEST.validate()
}

// Keep the exact existing Doom processor reachable in the SBF image. DCG v1
// does not yet expose an app-owned SVM entrypoint which can route its kernels.
#[cfg(feature = "sbf")]
#[no_mangle]
pub fn doom_dcg_existing_doom_processor<'a>(
    program_id: &solana_program::pubkey::Pubkey,
    accounts: &'a [solana_program::account_info::AccountInfo<'a>],
    instruction_data: &[u8],
) -> solana_program::entrypoint::ProgramResult {
    doom_sbf::transport_process_instruction(program_id, accounts, instruction_data)
}
