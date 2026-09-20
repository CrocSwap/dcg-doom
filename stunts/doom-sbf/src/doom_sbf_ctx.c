/*
 * Context-mode half of the SBF platform layer (see doom_sbf_platform.c).
 *
 * This is the ONLY platform file compiled with -include doom_ctx.h (plus
 * -DDOOM_CTX_NO_MACROS: members are spelled DOOM_CTX->field explicitly, and
 * with the macros on DOOM_CTX->FIELD would re-expand FIELD).  It holds every
 * platform entry point that touches context state: the constant-address
 * guard and probe, the arena, I_ZoneBase/I_AllocLow, the malloc family,
 * I_Error, I_BaseTiccmd and D_AdvanceDemo.  It defines no printf-family
 * function (SBF cannot define a variadic, and the engine headers declare
 * those variadic): those live in doom_sbf_platform.c, which never sees
 * doom_ctx.h.
 *
 * No mutable global, no const-pointer table: the image stays 0 .data +
 * 0 .bss.  memcpy comes from doom_sbf_platform.c; doom_sbf_panic from Rust.
 */

#include <stddef.h>
#include <stdint.h>

typedef unsigned char byte_t;
typedef unsigned long u64_t;

#ifdef DOOM_SBF_BUILD
extern void sol_memset_(void *dst, uint8_t value, uint64_t n);
#endif

void doom_sbf_panic(void);
void *memcpy(void *dst, const void *src, size_t n);

/* ------------------------------------------------------------------ */
/* Constant-address guard and probe                                     */
/* ------------------------------------------------------------------ */

/* The instruction's own refusal code: the control that passes the context
 * account at a moved position must observe exactly this, not a generic
 * loader or validation error.  Mirrored as CTX_GUARD_REFUSED in src/lib.rs. */
#define DOOM_CTX_GUARD_REFUSED 199
#define DOOM_SBF_ALLOC_ALIGNMENT 16u

/* The context account's data must map at the pinned address (M1021: the
 * first account's data at a fixed layout).  Any other mapping means the
 * instruction's account order changed, and dereferencing DOOM_CTX would
 * corrupt whatever account did map there: refuse instead. */
int doom_ctx_guard(const void *data)
{
    return data == (const void *)DOOM_CTX ? 0 : DOOM_CTX_GUARD_REFUSED;
}

/* The engine image must KEEP the whole 0007 cut resident: static archives
 * only link referenced objects, so an unreferenced engine would silently
 * drop out of the image and the loader check would prove nothing about it.
 * One defined symbol per content-bearing TU, addressed but never called
 * (all 39 are already declared with true types by the forced -include
 * doom_ctx.h; the casts below take addresses only, call nothing).
 * doomdef/doomstat define no symbols at all (verified: empty objects), so
 * 39 entries cover the 41-TU cut.  The table itself is const addresses
 * (.data.rel.ro, loader-tolerated per M1021). */
/* Anchors with no prototype in the forced-include header set (vanilla Doom
 * calls them through implicit declarations; modern clang errors on those).
 * First seven copied from the defining TUs; the rest are parameterless
 * declarations, which are compatible with any definition.  Address-taking
 * only in all cases: nothing here is ever called. */
int G_CmdChecksum(ticcmd_t *cmd);
int M_CheckParm(char *check);
void M_ClearBox(fixed_t *box);
int P_Random(void);
unsigned short SwapSHORT(unsigned short x);
void Z_ClearZone(memzone_t *zone);
void strupr(char *s);
void P_RecursiveSound();
void P_GiveAmmo();
void T_FireFlicker();
void PIT_StompThing();
void P_SetPsprite();
void P_LoadVertexes();
void P_DivlineSide();
void P_Thrust();
void R_DrawColumnInCache();
void R_InitSkyMap(void);
void R_InstallSpriteLump();
#define DOOM_ENGINE_ANCHOR_COUNT 39
/* volatile: the addresses are trivially nonzero, so without volatile the
 * compiler folds every `!= 0` at -O2, the table becomes unreferenced, and
 * the engine silently drops out of the image again.  Volatile reads force
 * the count to happen at runtime. */
static const void *const volatile doom_engine_anchor[DOOM_ENGINE_ANCHOR_COUNT] = {
    (const void *)G_CmdChecksum, (const void *)M_CheckParm,
    (const void *)M_ClearBox, (const void *)FixedMul,
    (const void *)P_Random, (const void *)SwapSHORT,
    (const void *)SlopeDiv, (const void *)Z_ClearZone,
    (const void *)strupr, (const void *)V_MarkRect,
    (const void *)T_MoveCeiling, (const void *)T_VerticalDoor,
    (const void *)P_RecursiveSound, (const void *)T_MovePlane,
    (const void *)P_GiveAmmo, (const void *)T_FireFlicker,
    (const void *)PIT_StompThing, (const void *)P_AproxDistance,
    (const void *)P_SetMobjState, (const void *)T_PlatRaise,
    (const void *)P_SetPsprite, (const void *)P_LoadVertexes,
    (const void *)P_DivlineSide, (const void *)P_InitPicAnims,
    (const void *)P_InitSwitchList, (const void *)EV_Teleport,
    (const void *)P_InitThinkers, (const void *)P_Thrust,
    (const void *)R_ClearDrawSegs, (const void *)R_DrawColumnInCache,
    (const void *)R_DrawColumn, (const void *)R_AddPointToBox,
    (const void *)R_InitPlanes, (const void *)R_RenderMaskedSegRange,
    (const void *)R_InitSkyMap, (const void *)R_InstallSpriteLump,
    (const void *)endmsg, (const void *)weaponinfo,
    (const void *)sprnames
};

/* Counts resident anchor entries.  Called unconditionally by the probe: the
 * call is what keeps the table (and through it the engine) linked.  Reads
 * addresses only; calls no engine code and touches no engine state. */
unsigned doom_engine_resident(void)
{
    unsigned i, n = 0;
    for (i = 0; i < DOOM_ENGINE_ANCHOR_COUNT; i++)
	n += (doom_engine_anchor[i] != 0);
    return n;
}

/* Link-time property checked at runtime: a dropped TU fails the link
 * (undefined anchor), so this branch is dead defense-in-depth.  Its code
 * keeps the failure loud instead of committing a probe result for a
 * partial engine. */
#define DOOM_CTX_ENGINE_DEFECT 1

/* Guard, then write one context field through the constant and read it
 * back.  out[0] is always the observed mapping (so the driver pins the
 * constant to measurement); out[1]/out[2] are the SBF-side sizeof and the
 * readback, written only when the guard passes.  Nothing is written on
 * refusal. */
int doom_ctx_probe(const void *seen, unsigned long value, unsigned long *out)
{
    out[0] = (unsigned long)(const byte_t *)seen;
    if (doom_ctx_guard(seen))
	return DOOM_CTX_GUARD_REFUSED;
    if (doom_engine_resident() != DOOM_ENGINE_ANCHOR_COUNT)
	return DOOM_CTX_ENGINE_DEFECT;
    DOOM_CTX->doom_ctx_probe_cell = value;
    out[1] = (unsigned long)sizeof(doom_ctx_t);
    out[2] = DOOM_CTX->doom_ctx_probe_cell;
    return 0;
}

/* ------------------------------------------------------------------ */
/* Arena                                                               */
/* ------------------------------------------------------------------ */

/* All bump-allocator state lives in the context account (PLATFORM_KEEP in
 * the 0007 generator).  doom_sbf_bind_arena records a carrier for the
 * malloc/I_AllocLow calls the WAD/init paths make; the zone itself always
 * comes from DOOM_CTX->zone (see I_ZoneBase), never from this carrier.  An
 * unbound carrier fails closed: every take returns NULL. */
int doom_sbf_bind_arena(void *base, u64_t bytes, u64_t zone_bytes)
{
    if (!base || ((uintptr_t)base & (DOOM_SBF_ALLOC_ALIGNMENT - 1u)) != 0) {
	DOOM_CTX->arena_base = (byte_t *)0;
	DOOM_CTX->arena_bytes = 0;
	DOOM_CTX->arena_used = 0;
	DOOM_CTX->arena_zone_bytes = zone_bytes;
	DOOM_CTX->arena_exhausted = 1;
	DOOM_CTX->arena_high_water = 0;
	return 0;
    }
    DOOM_CTX->arena_base = (byte_t *)base;
    DOOM_CTX->arena_bytes = bytes;
    DOOM_CTX->arena_used = 0;
    DOOM_CTX->arena_zone_bytes = zone_bytes;
    DOOM_CTX->arena_exhausted = 0;
    DOOM_CTX->arena_high_water = 0;
    return 1;
}

u64_t doom_sbf_arena_high_water(void)
{
    return DOOM_CTX->arena_high_water;
}

int doom_sbf_arena_exhausted(void)
{
    return DOOM_CTX->arena_exhausted;
}

static void *arena_take(u64_t bytes)
{
    u64_t start = (DOOM_CTX->arena_used + 15u) & ~(u64_t)15u;
	if (DOOM_CTX->arena_base &&
	    ((uintptr_t)DOOM_CTX->arena_base & (DOOM_SBF_ALLOC_ALIGNMENT - 1u)))
	{
	    DOOM_CTX->arena_exhausted = 1;
	    return (void *)0;
	}
    if (!DOOM_CTX->arena_base || bytes > DOOM_CTX->arena_bytes ||
	start > DOOM_CTX->arena_bytes - bytes)
    {
	DOOM_CTX->arena_exhausted = 1;
	return (void *)0;
    }
    DOOM_CTX->arena_used = start + bytes;
    if (DOOM_CTX->arena_used > DOOM_CTX->arena_high_water)
	DOOM_CTX->arena_high_water = DOOM_CTX->arena_used;
    return DOOM_CTX->arena_base + start;
}

/* ------------------------------------------------------------------ */
/* Stateful Doom platform entry points                                  */
/* ------------------------------------------------------------------ */

/* Host stub parity (0007 doom_stubs.c): cap the heap size the engine sees
 * at the measured zone floor. */
int I_GetHeapSize(void)
{
    int want = DOOM_CTX->mb_used * 1024 * 1024;
    return want < DOOM_ZONE_BYTES ? want : DOOM_ZONE_BYTES;
}

/* Host stub parity: the zone is the context-resident arena, calloc-zeroed
 * on first use.  (ProgramTest zero-fills fresh accounts; the explicit loop
 * keeps re-entry behavior identical to the host.) */
byte_t *I_ZoneBase(int *size)
{
    int want = DOOM_CTX->mb_used * 1024 * 1024;
    byte_t *z;
    if (want > DOOM_ZONE_BYTES)
	want = DOOM_ZONE_BYTES;
    *size = want;
    z = DOOM_CTX->zone.bytes;
#ifdef DOOM_SBF_BUILD
    if (want > 0)
	sol_memset_(z, 0, (uint64_t)want);
#else
    {
	unsigned i;
	for (i = 0; i < (unsigned)want; i++)
	    z[i] = 0;
    }
#endif
    return z;
}

byte_t *I_AllocLow(int length)
{
    byte_t *p = (byte_t *)arena_take((u64_t)(length < 0 ? 0 : length));
    int i;
    if (!p)
	return p;
#ifdef DOOM_SBF_BUILD
    if (length > 0)
	sol_memset_(p, 0, (uint64_t)length);
#else
    for (i = 0; i < length; i++)
	p[i] = 0;
#endif
    return p;
}

/* Allocator and abort below collide with host libc names: SBF-only, for the
 * same reason as the gated block in doom_sbf_platform.c.  Native feature
 * builds keep the real libc; the SBF image has no libc. */
#ifdef DOOM_SBF_BUILD
void *malloc(size_t n) { return arena_take((u64_t)n); }
void *calloc(size_t n, size_t size)
{
    u64_t total = (u64_t)n * (u64_t)size;
    byte_t *p = (byte_t *)arena_take(total);
    if (p)
	sol_memset_(p, 0, total);
    return p;
}
/* The engine's only realloc is w_wad.c's lumpinfo growth, which happens once
 * per WAD file and never shrinks.  Copying forward in the arena is correct;
 * the old block is simply left behind. */
void *realloc(void *old, size_t n)
{
    byte_t *p = (byte_t *)arena_take((u64_t)n);
    if (p && old)
	memcpy(p, old, n);
    return p;
}
void free(void *p) { (void)p; }
void exit(int code) { (void)code; doom_sbf_panic(); }
void abort(void) { doom_sbf_panic(); }
#endif /* DOOM_SBF_BUILD */

/* I_Error is Doom's fail-closed exit.  On SBF it must abort the instruction:
 * a program that kept running after I_Error would commit half-built state. */
void I_Error(char *error)
{
    (void)error;
    DOOM_CTX->doom_error_latched = 1;
    doom_sbf_panic();
}

/* ticcmd scratch for I_BaseTiccmd, shared with the host stub: the field, not
 * a static. */
void *I_BaseTiccmd(void) { return (void *)&DOOM_CTX->emptycmd; }

void D_AdvanceDemo(void) { DOOM_CTX->doom_sbf_demo_done = 1; }
