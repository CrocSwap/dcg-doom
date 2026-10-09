//! The Doom engine for the DCG program (`stunts/doom-dcg`): the SIM/2
//! simulation, the RSN/3 render snapshot and strip renderer, and the resource
//! ABI they share. This crate is a library; doom-dcg links it with
//! `no-entrypoint` and dispatches its handlers. The standalone R-series
//! program, its probes and its driver stay in the Basanos repository's history.

#[cfg(feature = "ctx-engine")]
pub mod continuation_split_sbf;
pub mod ex2;
#[cfg(feature = "ctx-engine")]
pub mod render_snapshot_v3;
#[cfg(feature = "ctx-engine")]
pub mod resource_sbf;
#[cfg(feature = "ctx-engine")]
pub mod sim2;
#[cfg(feature = "ctx-engine")]
pub mod static_resource;

pub const FRAME_BYTES: usize = 320 * 200;
pub const STATE_BYTES: usize = 4_608;
/// DOOM-CX1 instruction magic: guard, then write one context field through
/// the compile-time constant DOOM_CTX and read it back.  Present only under
/// the `ctx-engine` feature, which is the only build that links the engine.
#[cfg(feature = "ctx-engine")]
pub const CTX_MAGIC: &[u8; 8] = b"DOOM-CX1";
/// The instruction's own refusal code: the moved-position control must
/// observe exactly this (a guard refusal), never a generic loader or
/// validation error.  Mirrors DOOM_CTX_GUARD_REFUSED in doom_sbf_platform.c.
#[cfg(feature = "ctx-engine")]
pub const CTX_GUARD_REFUSED: u32 = 199;
/// Exact context-account length the instruction accepts: the generated
/// sizeof(doom_ctx_t) (host-measured, 3_440_952) plus one scratch triple
/// (observed base, SBF-side sizeof, readback) past the end of the struct.
/// The driver asserts the SBF-side sizeof the probe writes back against its
/// own copy, so layout drift between the host-measured constant and the SBF
/// struct fails loudly instead of aliasing engine state.
///
/// The constant has to be exact, not merely large enough: the static resource
/// header records CTX_STRUCT_BYTES at offset 20 and the C rebind compares it
/// against its own sizeof(doom_ctx_t), so an eight-byte drift here is a
/// Custom(302) at the first RENDER with nothing in the log to explain it.
///
/// Patch 0030's diagnostic render-phase accumulator is eighteen CU cells plus
/// four mark registers, 176 bytes, as the last member of doom_ctx_t. Those
/// members are inside `#ifdef DOOM_SBF_PHASE_PROFILE` in doom_ctx.h, which
/// build.rs defines for a `diagnostic-profile` image and nothing else, so the
/// 176 bytes exist in that image and in no other. Hence the two constants:
/// a production build must read exactly what it read before patch 0030, and
/// the first version of the patch, which added the 176 bytes everywhere,
/// moved BOOTSTRAP_OFFSET and ACCOUNT_BYTES with it and had to be reverted
/// (30088de1f). A production build's context layout is a consensus input to
/// the deployed images, the live accounts and the sender scripts; a
/// measurement-only patch has no business changing it.
///
/// `CTX_PHASE_PROFILE_BYTES` is the 176 on its own, unconditionally: a host
/// runner that measures a `diagnostic-profile` image has to preallocate
/// *that image's* layout, and it cannot read that image's features from its
/// own feature set.
pub const CTX_PHASE_PROFILE_BYTES: usize = 176;

#[cfg(all(feature = "ctx-engine", feature = "diagnostic-profile"))]
pub const CTX_MIN_BYTES: usize = 3_440_952 + CTX_PHASE_PROFILE_BYTES + 24;
#[cfg(all(feature = "ctx-engine", not(feature = "diagnostic-profile")))]
pub const CTX_MIN_BYTES: usize = 3_440_952 + 24;

/// Numeric phase-log magic used only by the bounded diagnostic SBF image.
/// The feature changes the image identity; no normal or positive qualification
/// run may use this instrumentation.
#[cfg(feature = "diagnostic-profile")]
const DIAGNOSTIC_PHASE_MAGIC: u64 = 0x444f_4f4d_5048_4153;

#[cfg(feature = "diagnostic-profile")]
#[no_mangle]
pub extern "C" fn doom_sbf_phase_marker(phase: u64) {
    solana_program::log::sol_log_64(
        DIAGNOSTIC_PHASE_MAGIC,
        phase,
        solana_program::compute_units::sol_remaining_compute_units(),
        0,
        0,
    );
}

#[inline(always)]
pub(crate) fn diagnostic_phase(phase: u64) {
    #[cfg(feature = "diagnostic-profile")]
    doom_sbf_phase_marker(phase);
    #[cfg(not(feature = "diagnostic-profile"))]
    let _ = phase;
}


/// A Doom I_Error (or any fail-closed platform abort) surfaces as a program
/// error rather than committed half-built account state.  Defined here so the
/// context-mode platform layer, which keeps no static data of its own, has
/// somewhere to go when the engine asks to die.
///
/// # Safety
///
/// Calling convention only: takes no arguments and never returns (it panics
/// through the SBF abort path), so there is no aliasing or validity contract
/// for a caller to uphold.
#[cfg(feature = "ctx-engine")]
#[no_mangle]
pub unsafe extern "C" fn doom_sbf_panic() -> ! {
    panic!("doom_sbf_panic");
}
