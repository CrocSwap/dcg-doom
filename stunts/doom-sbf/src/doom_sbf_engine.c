/*
 * Step-6 engine entry gate.
 *
 * This symbol is deliberately not an engine call.  The generated patch-0007
 * context still contains C function pointers in `state_t` records, and the
 * account-persistent action representation has not been made pointer-free or
 * exercised on SBF.  Rust validates the DOOM-EX1 wire/account shape, then
 * calls this gate so an accidental driver run fails closed with an explicit
 * ABI blocker instead of looking like an engine result.
 */

#define DOOM_ENGINE_ACTION_POINTERS_UNRESOLVED 230

int doom_engine_entry_gate(void)
{
    return DOOM_ENGINE_ACTION_POINTERS_UNRESOLVED;
}

/* EX2 is an explicit host-native seam.  Keep a symbol with the same ABI in
 * the SBF archive so an accidental EX2 call fails closed with a distinct
 * resource-unbound value; the native archive supplies the real bridge. */
#ifdef DOOM_SBF_BUILD
int doom_engine_ex2_entry(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    const unsigned char *state, unsigned long state_len,
    const unsigned char *mirror, unsigned long mirror_len,
    const unsigned char *controller, unsigned long controller_len,
    unsigned char *state_out, unsigned long state_cap, unsigned long *state_out_len,
    unsigned char *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned char *frame_out, unsigned long frame_cap, unsigned long *frame_out_len)
{
    (void)wad_accounts; (void)wad_lens; (void)state; (void)state_len;
    (void)mirror; (void)mirror_len; (void)controller; (void)controller_len;
    (void)state_out; (void)state_cap; (void)state_out_len;
    (void)mirror_out; (void)mirror_cap; (void)mirror_out_len;
    (void)frame_out; (void)frame_cap; (void)frame_out_len;
    return 231;
}
#endif
