//! Diagnostic render-phase accumulator shims (patch 0030).
//!
//! The C renderer accumulates per-phase compute-unit intervals into context
//! cells (`doom_sbf_prof[]`, declared in `headless/doom_ctx.h`) and the strip
//! driver emits them in five aggregated `sol_log_64` lines. Two things have to
//! come from Rust, and only two:
//!
//! * the remaining-CU meter, because the freestanding C headers in
//!   `sbfinc/` declare no syscall interface at all; and
//! * the log itself, for the same reason.
//!
//! The cells stay in the context account rather than in Rust statics on
//! purpose: the image must carry zero writable static data (M1006 measured the
//! loader refusal for both `.data` and `.bss`), so there is nowhere in Rust to
//! put an accumulator that survives across the C call.
//!
//! `diagnostic-profile` changes the image identity. No image built with this
//! module may be used for a positive qualification claim, and the `doom_sbf_*`
//! symbols are only referenced from C that compiles the same guard, so a
//! normal image does not link them.

/// Remaining compute units, as a C-callable function.
///
/// SBF cannot call the syscall from C without a declaration, and adding one
/// to `sbfinc/` would give the image a C-visible syscall surface it does not
/// otherwise have. Each `PROF_ENTER`/`PROF_EXIT` pair costs two of these; the
/// cost is identical in a `diagnostic-profile` image and a
/// `render-count-only` image, so it cancels in the store-cost difference
/// between the two.
#[cfg(feature = "diagnostic-profile")]
#[no_mangle]
pub extern "C" fn doom_sbf_prof_cu() -> u64 {
    solana_program::compute_units::sol_remaining_compute_units()
}

/// One aggregated profile line, as a C-callable function.
///
/// `magic` tags the line and `group` is 0..5, naming which three cells `a`,
/// `b` and `c` are. The caller is `rsr_prof_emit` in
/// `doom_sbf_resource_ri2.c`; the split into three is what keeps a
/// sixteen-cell report inside Solana's ten-line per-transaction program-log
/// budget, since the two leading arguments are spent on the tag and the
/// group.
#[cfg(feature = "diagnostic-profile")]
#[no_mangle]
pub extern "C" fn doom_sbf_prof_log(magic: u64, group: u64, a: u64, b: u64, c: u64) {
    solana_program::log::sol_log_64(magic, group, a, b, c);
}
