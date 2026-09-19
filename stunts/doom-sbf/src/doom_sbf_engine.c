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
