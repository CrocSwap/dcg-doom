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
mod step5;
pub use step5::*;

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

unsafe extern "C" {
    fn doom_draw_columns(framebuffer: *mut u8, state: *mut u8);
    fn doom_draw_spans(framebuffer: *mut u8, state: *mut u8);
    fn doom_bsp_descent(state: *mut u8) -> u64;
    fn doom_fixed_mul_only(state: *mut u8) -> u64;
    fn doom_fixed_div_only(state: *mut u8) -> u64;
    fn doom_fixed_mul_div(state: *mut u8) -> u64;
    fn doom_write_strip(strip: *mut u8, bytes: u32, state: *const u8);
}

entrypoint!(process_instruction);

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
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if is_step5_instruction(instruction_data) {
        return process_step5(program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(bring_up::DIRECTORY_MAGIC) {
        return bring_up::process_directory(program_id, accounts, instruction_data);
    }
    if instruction_data.starts_with(bring_up::INIT_MAGIC) {
        return bring_up::process_init(program_id, accounts, instruction_data);
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
