//! SBF-only measurement harness for three isolated linuxdoom-1.10 hot loops.
//!
//! The program intentionally has no dependency on `chain/program`.  Every
//! renderer output byte is written into a writable account slice; no 64 KiB
//! framebuffer exists in the program heap or stack.

use solana_program::{
    account_info::AccountInfo, entrypoint, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

pub mod bring_up;
#[cfg(feature = "ctx-engine")]
pub mod continuation_sbf;
#[cfg(feature = "ctx-engine")]
pub mod continuation_split_sbf;
#[cfg(feature = "ctx-engine")]
pub mod lifecycle;
pub mod ex2;
#[cfg(feature = "ctx-engine")]
pub mod ex2_sbf;
#[cfg(feature = "diagnostic-profile")]
pub mod prof;
#[cfg(feature = "ctx-engine")]
pub mod render_snapshot;
#[cfg(feature = "ctx-engine")]
pub mod render_snapshot_v3;
#[cfg(feature = "ctx-engine")]
pub mod resource;
#[cfg(feature = "ctx-engine")]
pub mod resource_sbf;
#[cfg(feature = "ctx-engine")]
pub mod sim2;
#[cfg(feature = "ctx-engine")]
pub mod simulation;
#[cfg(feature = "ctx-engine")]
pub mod stage;
#[cfg(feature = "ctx-engine")]
pub mod static_resource;

pub const FRAME_BYTES: usize = 320 * 200;
pub const STATE_BYTES: usize = 4_608;
pub const W1_COLUMN: u8 = 0;
pub const W2_SPAN: u8 = 1;
pub const W3_FULL: u8 = 2;
pub const WRITE_STRIP: u8 = 3;
// Measurement controls used only to identify the two components of W3.
pub const W3_BSP_ONLY: u8 = 4;
pub const W3_FIXED_MUL_ONLY: u8 = 5;
pub const W3_FIXED_DIV_ONLY: u8 = 6;
pub const W3_FIXED_MUL_DIV: u8 = 7;
/// Writes one mutable C global and reads it back: the engine-port probe for
/// whether an SBF program image may hold mutable static state.  Present only
/// under the `static-write-probe` feature, because the mutable global it adds
/// puts a `.data` section in the image and the loader then refuses it.
#[cfg(feature = "static-write-probe")]
pub const W4_STATIC_WRITE: u8 = 8;
/// Same measurement, but the global has no initializer, so it is a `.bss`
/// (zero-fill) cell rather than a `.data` cell.  Present only under the
/// `zero-bss-probe` feature.  This decides whether the "no writable static
/// data" refusal M1006 measured is specific to nonzero `.data` content or
/// also covers a zero-initialized `.bss` cell.
#[cfg(feature = "zero-bss-probe")]
pub const W5_ZERO_BSS_WRITE: u8 = 9;
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

unsafe extern "C" {
    fn doom_draw_columns(framebuffer: *mut u8, state: *mut u8);
    fn doom_draw_spans(framebuffer: *mut u8, state: *mut u8);
    fn doom_bsp_descent(state: *mut u8) -> u64;
    fn doom_fixed_mul_only(state: *mut u8) -> u64;
    fn doom_fixed_div_only(state: *mut u8) -> u64;
    fn doom_fixed_mul_div(state: *mut u8) -> u64;
    fn doom_write_strip(strip: *mut u8, bytes: u32, state: *const u8);
    #[cfg(feature = "static-write-probe")]
    fn doom_probe_static_write(value: u64) -> u64;
    #[cfg(feature = "zero-bss-probe")]
    fn doom_probe_zero_bss_write(value: u64) -> u64;
    #[cfg(feature = "ctx-engine")]
    fn doom_ctx_probe(seen: *const u8, value: u64, out: *mut u64) -> i32;
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

#[cfg(not(feature = "sim-transport"))]
entrypoint!(process_instruction);
#[cfg(feature = "sim-transport")]
entrypoint!(transport_process_instruction);

/// The deployable DOOM-SIM/2 transport profile.  Only three instruction
/// families are reachable: E2R/5 reconstruction (PREPARE requires the context
/// signer and a zero checkpoint), DOOM-STG/1 seal-once staging, and SIM/2.
/// SIM/1 (permissionless INIT, test-only ROLLBACK), the earlier E2R/E2S
/// continuations, bring-up, CX1 and the hot-loop probes are not dispatched,
/// so no other instruction in this image can write a context, ST1, MP1, meta
/// or input-buffer account. DOOM-RSN/2 may only write its separate snapshot
/// PDA after validating the committed SIM2 cursor/root.
#[cfg(feature = "sim-transport")]
pub fn transport_process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let version = if instruction_data.len() >= 12 {
        u32::from_le_bytes(instruction_data[8..12].try_into().unwrap())
    } else {
        0
    };
    if instruction_data.starts_with(sim2::MAGIC) && version == sim2::VERSION {
        return sim2::process(program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(lifecycle::MAGIC) && version == lifecycle::VERSION {
        return lifecycle::process(program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(render_snapshot_v3::MAGIC)
        && version == render_snapshot_v3::VERSION
    {
        return render_snapshot_v3::process(program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(static_resource::MAGIC) && version == static_resource::VERSION {
        return static_resource::process(program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(render_snapshot::MAGIC) && version == render_snapshot::VERSION {
        return render_snapshot::process(program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(continuation_split_sbf::MAGIC)
        && version == continuation_split_sbf::VERSION
    {
        return continuation_split_sbf::process(program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(stage::MAGIC) && version == stage::VERSION {
        return stage::process(program_id, accounts, instruction_data);
    }
    Err(ProgramError::InvalidInstructionData)
}

fn write_result(data: &mut [u8], result: u64) -> ProgramResult {
    if data.len() < 8 {
        return Err(ProgramError::InvalidAccountData);
    }
    data[..8].copy_from_slice(&result.to_le_bytes());
    Ok(())
}

fn require_mutable_state(state: &AccountInfo) -> Result<(), ProgramError> {
    if !state.is_writable || state.data_len() < STATE_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn require_mutable_frame(frame: &AccountInfo) -> Result<(), ProgramError> {
    if !frame.is_writable || frame.data_len() != FRAME_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

pub fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    #[cfg(feature = "account-address-probe")]
    if instruction_data.starts_with(b"DOOM-AP1") {
        return process_address_probe(_program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(bring_up::DIRECTORY_MAGIC) {
        return bring_up::process_directory(_program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(bring_up::INIT_MAGIC) {
        return bring_up::process_init(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.starts_with(CTX_MAGIC) {
        return process_ctx_probe(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.len() >= 12
        && instruction_data.starts_with(static_resource::MAGIC)
        && u32::from_le_bytes(instruction_data[8..12].try_into().unwrap())
            == static_resource::VERSION
    {
        return static_resource::process(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.len() >= 12
        && instruction_data.starts_with(render_snapshot_v3::MAGIC)
        && u32::from_le_bytes(instruction_data[8..12].try_into().unwrap())
            == render_snapshot_v3::VERSION
    {
        return render_snapshot_v3::process(_program_id, accounts, instruction_data);
    }
    if instruction_data.len() >= 12
        && instruction_data.starts_with(render_snapshot::MAGIC)
        && u32::from_le_bytes(instruction_data[8..12].try_into().unwrap())
            == render_snapshot::VERSION
    {
        return render_snapshot::process(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.len() >= 12
        && instruction_data.starts_with(simulation::MAGIC)
        && u32::from_le_bytes(instruction_data[8..12].try_into().unwrap()) == simulation::VERSION
    {
        return simulation::process(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.len() >= 12
        && instruction_data.starts_with(resource_sbf::RI3_MAGIC)
        && u32::from_le_bytes(instruction_data[8..12].try_into().unwrap())
            == resource_sbf::RI3_VERSION
    {
        return resource_sbf::process(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.len() >= 12
        && instruction_data.starts_with(continuation_split_sbf::MAGIC)
        && u32::from_le_bytes(instruction_data[8..12].try_into().unwrap())
            == continuation_split_sbf::VERSION
    {
        return continuation_split_sbf::process(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.len() >= 12
        && instruction_data.starts_with(continuation_sbf::MAGIC)
        && u32::from_le_bytes(instruction_data[8..12].try_into().unwrap())
            == continuation_sbf::VERSION
    {
        return continuation_sbf::process(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.starts_with(resource_sbf::MAGIC) {
        return resource_sbf::process(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.starts_with(ex2_sbf::MAGIC) {
        return ex2_sbf::process(_program_id, accounts, instruction_data);
    }
    #[cfg(feature = "ctx-engine")]
    if instruction_data.starts_with(engine::MAGIC) {
        return engine::process(_program_id, accounts, instruction_data);
    }

    if accounts.iter().any(|account| {
        account.is_writable
            && matches!(
                account.data_len(),
                bring_up::CONFIG_BYTES | bring_up::CHUNK_BYTES
            )
    }) {
        return Err(ProgramError::InvalidAccountData);
    }
    if instruction_data.len() != 1 {
        return Err(ProgramError::InvalidInstructionData);
    }

    match instruction_data[0] {
        W1_COLUMN => {
            let state = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
            let frame = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
            require_mutable_state(state)?;
            require_mutable_frame(frame)?;
            if state.key == frame.key {
                return Err(ProgramError::InvalidAccountData);
            }
            // Both values are direct writable-account data slices.  The C
            // function gets their pointers only for the duration of these
            // borrows; it has no framebuffer allocation of its own.
            let mut state_data = state.data.borrow_mut();
            let mut frame_data = frame.data.borrow_mut();
            unsafe {
                doom_draw_columns(
                    (&mut frame_data[..]).as_mut_ptr(),
                    (&mut state_data[..]).as_mut_ptr(),
                )
            };
        }
        W2_SPAN => {
            let state = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
            let frame = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
            require_mutable_state(state)?;
            require_mutable_frame(frame)?;
            if state.key == frame.key {
                return Err(ProgramError::InvalidAccountData);
            }
            let mut state_data = state.data.borrow_mut();
            let mut frame_data = frame.data.borrow_mut();
            unsafe {
                doom_draw_spans(
                    (&mut frame_data[..]).as_mut_ptr(),
                    (&mut state_data[..]).as_mut_ptr(),
                )
            };
        }
        #[cfg(feature = "static-write-probe")]
        W4_STATIC_WRITE => {
            let state = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
            require_mutable_state(state)?;
            let mut state_data = state.data.borrow_mut();
            let result = unsafe { doom_probe_static_write(0x0123_4567_89ab_cdef) };
            write_result(&mut state_data[..], result)?;
        }
        #[cfg(feature = "zero-bss-probe")]
        W5_ZERO_BSS_WRITE => {
            let state = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
            require_mutable_state(state)?;
            let mut state_data = state.data.borrow_mut();
            let result = unsafe { doom_probe_zero_bss_write(0x0123_4567_89ab_cdef) };
            write_result(&mut state_data[..], result)?;
        }
        W3_FULL | W3_BSP_ONLY | W3_FIXED_MUL_ONLY | W3_FIXED_DIV_ONLY | W3_FIXED_MUL_DIV => {
            let state = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
            require_mutable_state(state)?;
            let mut state_data = state.data.borrow_mut();
            let result = unsafe {
                match instruction_data[0] {
                    W3_FULL => doom_bsp_descent((&mut state_data[..]).as_mut_ptr())
                        .wrapping_add(doom_fixed_mul_div((&mut state_data[..]).as_mut_ptr())),
                    W3_BSP_ONLY => doom_bsp_descent((&mut state_data[..]).as_mut_ptr()),
                    W3_FIXED_MUL_ONLY => doom_fixed_mul_only((&mut state_data[..]).as_mut_ptr()),
                    W3_FIXED_DIV_ONLY => doom_fixed_div_only((&mut state_data[..]).as_mut_ptr()),
                    W3_FIXED_MUL_DIV => doom_fixed_mul_div((&mut state_data[..]).as_mut_ptr()),
                    _ => unreachable!(),
                }
            };
            write_result(&mut state_data[..], result)?;
        }
        WRITE_STRIP => {
            let state = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
            let strip = accounts.get(1).ok_or(ProgramError::NotEnoughAccountKeys)?;
            if state.is_writable || !strip.is_writable || strip.data_len() == 0 {
                return Err(ProgramError::InvalidAccountData);
            }
            if state.key == strip.key {
                return Err(ProgramError::InvalidAccountData);
            }
            let state_data = state.data.borrow();
            if state_data.len() < STATE_BYTES {
                return Err(ProgramError::InvalidAccountData);
            }
            let mut strip_data = strip.data.borrow_mut();
            unsafe {
                doom_write_strip(
                    (&mut strip_data[..]).as_mut_ptr(),
                    strip_data.len() as u32,
                    (&state_data[..]).as_ptr(),
                )
            };
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    }
    Ok(())
}

#[cfg(feature = "account-address-probe")]
fn process_address_probe(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.len() != 9 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let account = accounts.last().ok_or(ProgramError::NotEnoughAccountKeys)?;
    if account.owner != program_id || !account.is_writable || account.data_len() < 24 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut data = account.try_borrow_mut_data()?;
    let address = data.as_mut_ptr() as u64;
    let previous = u64::from_le_bytes(data[..8].try_into().unwrap());
    data[8..16].copy_from_slice(&previous.to_le_bytes());
    data[..8].copy_from_slice(&address.to_le_bytes());
    data[16..24].copy_from_slice(&u64::from(instruction_data[8]).to_le_bytes());
    Ok(())
}

/// DOOM-CX1: the context account is the LAST account (mirroring M1021's
/// PREFIX control: at the honest single-account layout first == last and the
/// guard passes; with a preceding account the mapping moves and the guard
/// refuses).  The guard runs inside C against the address the VM actually
/// mapped, so a layout change fails loudly instead of aliasing engine state
/// onto the wrong account.  No engine init/tic/render runs here by design.
#[cfg(feature = "ctx-engine")]
fn process_ctx_probe(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.len() != 16 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let ctx = accounts.last().ok_or(ProgramError::NotEnoughAccountKeys)?;
    // Exact length: the scratch triple sits at a fixed offset past the
    // struct, so any size drift fails here or in the driver's sizeof
    // writeback assertion instead of aliasing engine state.
    if ctx.owner != program_id || !ctx.is_writable || ctx.data_len() != CTX_MIN_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }
    let value = u64::from_le_bytes(instruction_data[8..16].try_into().unwrap());
    let mut data = ctx.try_borrow_mut_data()?;
    let (body, scratch) = data.split_at_mut(CTX_MIN_BYTES - 24);
    let rc = unsafe { doom_ctx_probe(body.as_ptr(), value, scratch.as_mut_ptr() as *mut u64) };
    if rc != 0 {
        return Err(ProgramError::Custom(rc as u32));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Vec<u8> {
        let mut state = vec![0u8; STATE_BYTES];
        for (i, byte) in state.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(29).wrapping_add(7);
        }
        state
    }

    #[test]
    fn c_loops_write_framebuffer_account_bytes() {
        let mut state = state();
        let mut frame = vec![0u8; FRAME_BYTES];
        unsafe { doom_draw_columns(frame.as_mut_ptr(), state.as_mut_ptr()) };
        assert!(frame.iter().any(|&byte| byte != 0));
        frame.fill(0);
        unsafe { doom_draw_spans(frame.as_mut_ptr(), state.as_mut_ptr()) };
        assert!(frame.iter().any(|&byte| byte != 0));
    }

    #[test]
    fn c_fixed_math_and_strip_writer_are_observable() {
        let mut state = state();
        let result = unsafe {
            doom_bsp_descent(state.as_mut_ptr())
                .wrapping_add(doom_fixed_mul_div(state.as_mut_ptr()))
        };
        assert_ne!(result, 0);
        let mut strip = vec![0u8; FRAME_BYTES / 16];
        unsafe { doom_write_strip(strip.as_mut_ptr(), strip.len() as u32, state.as_ptr()) };
        assert!(strip.windows(2).any(|pair| pair[0] != pair[1]));
    }
}

// Keep the module declaration at the true end of this file.  The default
// image's byte identity is unverified: the feature-gated route above changes
// source line layout. Reproducible SBF comparison remains a deferred gate.
pub mod engine;
