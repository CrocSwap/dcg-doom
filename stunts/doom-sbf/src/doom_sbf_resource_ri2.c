/*
 * DOOM-E2R/2 SBF consumer.
 *
 * This file is included by the source-matched RI2 translation unit generated
 * by build.rs.  It deliberately contains only the consumer entry: capture,
 * setup-based predecessor code, and host allocation are not part of this
 * image.  The resource preflight runs before the context, zone, or output
 * accounts are changed.
 */

#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>

#include "doom_ctx.h"
#include "doomdef.h"
#include "doomstat.h"
#include "g_game.h"
#include "p_setup.h"
#include "r_main.h"
#include "v_video.h"
#include "w_wad.h"
#include "z_zone.h"
#include "doom_state.h"
#include "doom_controller.h"
#include "doom_sbf_plan_layout.h"
#include "info.h"
#include "p_local.h"
#include "p_mobj.h"
#include "r_things.h"

extern int doom_ctx_guard(const void *data);
extern void doom_ctx_init(void);
extern int doom_sbf_bind_arena(void *base, unsigned long bytes, unsigned long zone_bytes);
extern int doom_sbf_rebind_arena(void *base, unsigned long bytes, unsigned long zone_bytes);
extern unsigned long doom_sbf_arena_mark(void);
extern int doom_sbf_arena_rewind(unsigned long mark);
extern int doom_sbf_arena_exhausted(void);
extern unsigned long doom_sbf_arena_high_water(void);
extern int doom_sbf_resource_import_graph(const uint8_t *image, unsigned long image_bytes);
extern int doom_sbf_fill_pool(uint8_t *pool, const uint8_t wad_hash[32]);
extern int DoomStateDecodeSplitPreflight(const uint8_t *image, size_t image_bytes);
extern int DoomStateDecodeSplitPrepare(void);
extern int DoomStateDecodeSplitParse(const uint8_t *image, size_t image_bytes);

#ifdef DOOM_SBF_PHASE_PROFILE
extern void doom_sbf_phase_marker(unsigned long phase);
#define RI2_PHASE(phase) doom_sbf_phase_marker((unsigned long)(phase))
#else
#define RI2_PHASE(phase) ((void)0)
#endif

#define RI2_REFUSE_STATE 232
#define RI2_REFUSE_POOL 233
#define RI2_REFUSE_RESOURCE 234
#define RI2_REFUSE_IMAGE 240
#define RI2_REFUSE_CONTROLLER 237
#define RI2_REFUSE_OUTPUT 238
#define RI2_REFUSE_ARENA 239
#define RI2_HEADER_BYTES 256u
#define RI2_FRAME_BYTES (320u * 200u)
#define RI2_STATE_MAX 126976u
#define RI2_MIRROR_BYTES 65536u
#define RI2_WAD_BYTES 4196020u
#define RI2_ZONE_BYTES 3145728u
#define RI2_BOOTSTRAP_MIN (2u * 1024u * 1024u)
#define RI2_MIRROR_HEADER_BYTES 4096u
#define RI2_MIRROR_SLOT_BYTES 160u
#define RI2_MIRROR_SLOTS 384u
#define RI2_CONTROLLER_BYTES 64u

static const uint8_t ri2_wad_hash[32] = {
    0x1d,0x7d,0x43,0xbe,0x50,0x1e,0x67,0xd9,0x27,0xe4,0x15,0xe0,0xb8,0xf3,0xe2,0x9c,
    0x3b,0xf3,0x30,0x75,0xe8,0x59,0x72,0x18,0x16,0xf6,0x52,0xa5,0x26,0xca,0xc7,0x71
};

static uint32_t ri2_u32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
        ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int ri2_zero(const uint8_t *p, size_t n)
{
    size_t i;
    for (i = 0; i < n; i++) if (p[i]) return 0;
    return 1;
}

static int ri2_state_shape(const uint8_t *state, size_t bytes)
{
    return state && bytes >= 64u && bytes <= RI2_STATE_MAX &&
        !memcmp(state, "DOOM-ST1", 8) && ri2_u32(state + 8) == 0 &&
        ri2_u32(state + 12) == 64u && (size_t)ri2_u32(state + 16) == bytes;
}

static int ri2_mirror_shape(const uint8_t *mirror, size_t bytes)
{
    return mirror && bytes == RI2_MIRROR_BYTES && !memcmp(mirror, "DOOM-MP1", 8) &&
        ri2_u32(mirror + 8) == 1u && ri2_u32(mirror + 12) == RI2_MIRROR_HEADER_BYTES &&
        ri2_u32(mirror + 16) == RI2_MIRROR_BYTES &&
        ri2_u32(mirror + 20) == RI2_MIRROR_SLOT_BYTES &&
        ri2_u32(mirror + 24) == RI2_MIRROR_SLOTS &&
        ri2_u32(mirror + 28) <= RI2_MIRROR_SLOTS &&
        !memcmp(mirror + 36, ri2_wad_hash, 32) &&
        ri2_zero(mirror + 68, RI2_MIRROR_HEADER_BYTES - 68u);
}

static int ri2_controller_shape(const uint8_t *controller, size_t bytes,
                                uint32_t expected_tic)
{
    return controller && bytes == RI2_CONTROLLER_BYTES &&
        !memcmp(controller, "DOOM-CI2", 8) && ri2_u32(controller + 8) == 1u &&
        ri2_u32(controller + 12) == 28u && ri2_u32(controller + 16) == RI2_CONTROLLER_BYTES &&
        ri2_u32(controller + 20) == expected_tic && ri2_zero(controller + 28, 36u);
}

static unsigned long ri2_split_arena_bytes(unsigned long bootstrap_bytes)
{
    return bootstrap_bytes > RI2_SPLIT_PLAN_RESERVE
        ? bootstrap_bytes - RI2_SPLIT_PLAN_RESERVE : 0;
}

static int ri2_split_arena_ok(unsigned long bootstrap_bytes)
{
    return !doom_sbf_arena_exhausted() &&
        doom_sbf_arena_high_water() <= ri2_split_arena_bytes(bootstrap_bytes);
}

/* The full RI2 preflight is authenticated in E2R/4 cursor 1.  Cursor 2
 * rebinds only the plan's resource pointers after Rust has re-hashed the
 * same resource account.  This avoids carrying native pointers in the
 * continuation while avoiding a second full variable-table/relocation walk.
 */
static int ri2_rebind_plan(const uint8_t *image, size_t image_bytes,
                           r2_section_t sections[5], r2_v2_plan_t *plan)
{
    const uint8_t *p;
    size_t at = 0;
    uint32_t id, bytes;
    if (!image || !plan || r2_sections(image, image_bytes, sections)) return -1;
    p = image + sections[R2_TABLES].offset;
    while (at < sections[R2_TABLES].bytes) {
        if (sections[R2_TABLES].bytes - at < 8u) return -1;
        id = r2_u32(p + at); bytes = r2_u32(p + at + 4); at += 8u;
        if (id == 0 || id >= 64 || bytes > sections[R2_TABLES].bytes - at) return -1;
        plan->table[id] = p + at;
        plan->table_bytes[id] = bytes;
        at += bytes;
    }
    return at == sections[R2_TABLES].bytes ? 0 : -1;
}

static int doom_engine_ex2_resource_sbf_ri2_run(
    int run_tic, uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *mirror, unsigned long mirror_len,
    const uint8_t *controller, unsigned long controller_len,
    uint8_t *frame_out, unsigned long frame_cap,
    uint8_t *state_out, unsigned long state_cap, unsigned long *state_out_len,
    uint8_t *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned long *frame_out_len, unsigned long *arena_high_water_out,
    uint32_t render_mode, uint32_t strip_id)
{
    r2_section_t sections[5];
    r2_v2_plan_t *plan;
    uint8_t *frame = 0, *encoded = 0, *derived = 0, *demo;
    size_t encoded_bytes = 0;
    int lump, demo_len, rc;
    uint32_t input_tic;

    /* No dereference or output account write precedes these checks. */
    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI2_HEADER_BYTES ||
        !wad_image || wad_bytes != RI2_WAD_BYTES ||
        !bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN ||
        !state || !state_out || !state_out_len ||
        !mirror || !mirror_out || !mirror_out_len ||
        !frame_out || !frame_out_len ||
        state_cap < RI2_STATE_MAX || mirror_cap < RI2_MIRROR_BYTES ||
        frame_cap < RI2_FRAME_BYTES || !ri2_state_shape(state, (size_t)state_len) ||
        !ri2_mirror_shape(mirror, (size_t)mirror_len)) return RI2_REFUSE_RESOURCE;
    if (render_mode > 2u || strip_id >= 8u ||
        (render_mode == 0u && strip_id != 0u)) return RI2_REFUSE_RESOURCE;
    input_tic = ri2_u32(state + 20);
    if (!ri2_controller_shape(controller, (size_t)controller_len, input_tic))
        return RI2_REFUSE_CONTROLLER;
    if (((uintptr_t)bootstrap & 15u) != 0) return RI2_REFUSE_ARENA;
    /* r2_v2_plan_t is intentionally large (the canonical table pointers and
     * scalar plan).  Keep it in the reserved high end of the caller-provided
     * bootstrap arena rather than the 4 KiB SBF stack.  The low end is the
     * source-matched malloc carrier used by WAD/state initialization, so the
     * plan cannot be overwritten by those allocations. */
    plan = (r2_v2_plan_t *)(bootstrap + bootstrap_bytes - sizeof(*plan));
    if (((uintptr_t)plan & 15u) != 0) return RI2_REFUSE_ARENA;
    if (r2_v2_preflight(resource, (size_t)resource_bytes, sections, plan))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(501); /* complete RI2/2 preflight */

    /* This is the first mutation of the context account. */
    memset((void *)DOOM_CTX, 0, sizeof(doom_ctx_t));
    doom_ctx_init();
    RI2_PHASE(510); /* context reset, before any engine init */
    if (!doom_sbf_bind_arena(bootstrap, bootstrap_bytes, RI2_ZONE_BYTES))
        return RI2_REFUSE_ARENA;
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0;
    displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init();
    RI2_PHASE(520);
    P_InitThinkers();
    RI2_PHASE(521);
    W_InitAccountImage(wad_image, (int)wad_bytes);
    RI2_PHASE(522);
    if (doom_sbf_resource_import_graph(resource + sections[R2_GRAPH].offset,
                                       sections[R2_GRAPH].bytes))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(530); /* imported graph and pointer tables */
    lastanim = anims + plan->scalar[R2_SC_ANIMCOUNT];
    numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    if (r2_walk(resource + sections[R2_SCALARS].offset,
                sections[R2_SCALARS].bytes, sections[R2_SCALARS].count,
                1, r2_apply_scalar, 0) ||
        r2_v2_alloc_resources(plan) ||
        r2_walk(resource + sections[R2_TABLES].offset,
                sections[R2_TABLES].bytes, sections[R2_TABLES].count,
                0, r2_v2_apply_table, 0) ||
        r2_v2_apply_lights(plan) || r2_apply_functions(resource, sections))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(540); /* imported scalars, resources, tables, relocs, functions */

    /* The four screen bases are internal scratch.  Only screens[0] is copied
     * to the caller's 64 KiB FB1 account after the real render returns. */
    frame = (uint8_t *)calloc(1, 4u * RI2_FRAME_BYTES);
    if (!frame) return RI2_REFUSE_OUTPUT;
    screens[0] = frame; screens[1] = frame + RI2_FRAME_BYTES;
    screens[2] = frame + 2u * RI2_FRAME_BYTES;
    screens[3] = frame + 3u * RI2_FRAME_BYTES; screens[4] = 0;
    viewimage = screens[0];
    for (rc = 0; rc < viewheight; rc++) ylookup[rc] = screens[0] + (size_t)rc * 320u;
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 :
        (SCREENHEIGHT - 32 - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0;
    setsizeneeded = false;
    RI2_PHASE(550);
    lump = W_GetNumForName("DEMO1");
    demo_len = W_LumpLength(lump);
    if (demo_len < 17) { free(frame); return RI2_REFUSE_RESOURCE; }
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo; demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    DoomStateSetAllocationFailure(-1);
    RI2_PHASE(560);
    if (DoomStateDecode(state, (size_t)state_len) ||
        DoomControllerArmV2(controller + 24) ||
        DoomControllerValidateCapturedDemoCursor(demo_p)) {
        free(frame); return RI2_REFUSE_STATE;
    }
    RI2_PHASE(570);
    /* ST1 contains gameplay state, not the derived view bases above. */
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 :
        (SCREENHEIGHT - 32 - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0;
    setsizeneeded = false;
    if (run_tic) {
        G_Ticker();
        RI2_PHASE(580);
        /* E2R/3 is enabled only in the diagnostic measurement image. */
        doom_render_fuzz_columns = render_mode
            ? ((render_mode << 30) | (strip_id << 27)) : 0u;
        clipangle = xtoviewangle[0]; fixedcolormap = 0;
        R_RenderPlayerView(&players[displayplayer]);
        RI2_PHASE(590);
        gametic++;
    } else {
        /* E2R/3 uses the real engine's pointer reconstruction and
         * state codec, but deliberately stops before the tic/render edge. */
        RI2_PHASE(585);
    }
    if (doom_sbf_arena_exhausted()) { free(frame); return RI2_REFUSE_ARENA; }
    derived = (uint8_t *)malloc(RI2_MIRROR_BYTES);
    if (!derived || doom_sbf_fill_pool(derived, ri2_wad_hash) ||
        DoomStateEncode(&encoded, &encoded_bytes) ||
        encoded_bytes > RI2_STATE_MAX || encoded_bytes > state_cap ||
        encoded_bytes < 64u) {
        free(derived); free(encoded); free(frame); return RI2_REFUSE_OUTPUT;
    }
    RI2_PHASE(600);
    memcpy(state_out, encoded, encoded_bytes);
    memcpy(mirror_out, derived, RI2_MIRROR_BYTES);
    memcpy(frame_out, screens[0], RI2_FRAME_BYTES);
    *state_out_len = encoded_bytes; *mirror_out_len = RI2_MIRROR_BYTES;
    *frame_out_len = RI2_FRAME_BYTES;
    RI2_PHASE(610);
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    DoomStateFree(encoded); free(derived); free(frame);
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_tic_entry(
    uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *mirror, unsigned long mirror_len,
    const uint8_t *controller, unsigned long controller_len,
    uint8_t *frame_out, unsigned long frame_cap,
    uint8_t *state_out, unsigned long state_cap, unsigned long *state_out_len,
    uint8_t *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned long *frame_out_len, unsigned long *arena_high_water_out,
    uint32_t render_mode, uint32_t strip_id)
{
    return doom_engine_ex2_resource_sbf_ri2_run(
        1, context, resource, resource_bytes, wad_image, wad_bytes,
        bootstrap, bootstrap_bytes, state, state_len, mirror, mirror_len,
        controller, controller_len, frame_out, frame_cap, state_out,
        state_cap, state_out_len, mirror_out, mirror_cap, mirror_out_len,
        frame_out_len, arena_high_water_out, render_mode, strip_id);
}

int doom_engine_ex2_resource_sbf_ri2_reconstruct(
    uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *mirror, unsigned long mirror_len,
    const uint8_t *controller, unsigned long controller_len,
    uint8_t *frame_out, unsigned long frame_cap,
    uint8_t *state_out, unsigned long state_cap, unsigned long *state_out_len,
    uint8_t *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned long *frame_out_len, unsigned long *arena_high_water_out)
{
    return doom_engine_ex2_resource_sbf_ri2_run(
        0, context, resource, resource_bytes, wad_image, wad_bytes,
        bootstrap, bootstrap_bytes, state, state_len, mirror, mirror_len,
        controller, controller_len, frame_out, frame_cap, state_out,
        state_cap, state_out_len, mirror_out, mirror_cap, mirror_out_len,
        frame_out_len, arena_high_water_out, 0u, 0u);
}

/* Render from an immutable DOOM-RSN image without advancing a tic or
 * publishing SIM2 state. The caller supplies a strip-owned writable context
 * workspace, the pinned RI2/WAD inputs, and an 8,000-byte pixel destination. */
int doom_engine_ex2_resource_sbf_ri2_render_strip(
    uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *mirror, unsigned long mirror_len,
    uint32_t strip_id, uint8_t *strip_out, unsigned long strip_cap,
    unsigned long *arena_high_water_out)
{
    r2_section_t sections[5];
    r2_v2_plan_t *plan;
    uint8_t *frame = 0, *demo;
    size_t strip_bytes = 40u * 200u;
    int rc, y, x, lump, demo_len;

    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI2_HEADER_BYTES ||
        !wad_image || wad_bytes != RI2_WAD_BYTES ||
        !bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN ||
        !state || !ri2_state_shape(state, (size_t)state_len) ||
        !mirror || !ri2_mirror_shape(mirror, (size_t)mirror_len) ||
        strip_id >= 8u || !strip_out || strip_cap != strip_bytes ||
        ((uintptr_t)bootstrap & 15u) != 0) return RI2_REFUSE_RESOURCE;

    plan = (r2_v2_plan_t *)(bootstrap + bootstrap_bytes - sizeof(*plan));
    if (((uintptr_t)plan & 15u) != 0 ||
        r2_v2_preflight(resource, (size_t)resource_bytes, sections, plan))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(701);

    memset((void *)DOOM_CTX, 0, sizeof(doom_ctx_t));
    doom_ctx_init();
    if (!doom_sbf_bind_arena(bootstrap, bootstrap_bytes, RI2_ZONE_BYTES))
        return RI2_REFUSE_ARENA;
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0;
    displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init();
    P_InitThinkers();
    RI2_PHASE(710); /* context, zone, and thinkers initialized */
    W_InitAccountImage(wad_image, (int)wad_bytes);
    RI2_PHASE(711); /* WAD directory parsed and account image bound */
    lump = W_GetNumForName("DEMO1");
    demo_len = W_LumpLength(lump);
    if (demo_len < 17) return RI2_REFUSE_RESOURCE;
    RI2_PHASE(712); /* demo lump looked up */
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    defdemoname = "DEMO1";
    demobuffer = demo;
    demo_p = demo;
    demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    RI2_PHASE(713); /* demo cache and captured bounds initialized */
    if (doom_sbf_resource_import_graph(resource + sections[R2_GRAPH].offset,
                                       sections[R2_GRAPH].bytes))
        return RI2_REFUSE_IMAGE;
    lastanim = anims + plan->scalar[R2_SC_ANIMCOUNT];
    numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    if (r2_walk(resource + sections[R2_SCALARS].offset,
                sections[R2_SCALARS].bytes, sections[R2_SCALARS].count,
                1, r2_apply_scalar, 0))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(741); /* scalar level and renderer globals imported */
    if (r2_v2_alloc_resources(plan)) return RI2_REFUSE_IMAGE;
    RI2_PHASE(7427); /* texture, patch, sprite, and colormap resources allocated */
    if (r2_walk(resource + sections[R2_TABLES].offset,
                sections[R2_TABLES].bytes, sections[R2_TABLES].count,
                0, r2_v2_apply_table, 0))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(743); /* remaining renderer lookup tables imported */
    if (r2_v2_apply_lights(plan) || r2_apply_functions(resource, sections))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(744); /* light selectors and function relocations applied */
    RI2_PHASE(702);

    frame = (uint8_t *)calloc(1, 4u * RI2_FRAME_BYTES);
    if (!frame) return RI2_REFUSE_OUTPUT;
    screens[0] = frame; screens[1] = frame + RI2_FRAME_BYTES;
    screens[2] = frame + 2u * RI2_FRAME_BYTES;
    screens[3] = frame + 3u * RI2_FRAME_BYTES; screens[4] = 0;
    viewimage = screens[0];
    for (rc = 0; rc < viewheight; rc++)
        ylookup[rc] = screens[0] + (size_t)rc * 320u;
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 :
        (SCREENHEIGHT - 32 - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0;
    setsizeneeded = false;
    RI2_PHASE(750); /* frame surfaces and initial view bases prepared */
    DoomStateSetAllocationFailure(-1);
    if (DoomStateDecode(state, (size_t)state_len)) {
        free(frame); return RI2_REFUSE_STATE;
    }
    RI2_PHASE(760); /* committed ST1 decoded into mutable engine structs */
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 :
        (SCREENHEIGHT - 32 - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0;
    setsizeneeded = false;
    RI2_PHASE(770); /* per-frame view bases rebuilt from decoded state */
    doom_render_fuzz_columns = (1u << 30) | (strip_id << 27);
    R_RenderPlayerView(&players[displayplayer]);
    RI2_PHASE(780); /* draw completed */
    if (doom_sbf_arena_exhausted()) {
        free(frame); return RI2_REFUSE_ARENA;
    }
    for (y = 0; y < 200; y++)
        for (x = 0; x < 40; x++)
            strip_out[(size_t)y * 40u + (size_t)x] =
                frame[(size_t)y * 320u + (size_t)strip_id * 40u + (size_t)x];
    RI2_PHASE(790); /* requested 40x200 pixel strip extracted */
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    RI2_PHASE(704);
    free(frame);
    return 0;
}

/* ------------------------------------------------------------------ */
/* DOOM-RENDER-SNAPSHOT/3 render-ready projection and restore           */

#define RSN3_DATA_HEADER_BYTES 128u
#define RSN3_SECTOR_BYTES 16u
#define RSN3_SIDE_BYTES 16u
#define RSN3_MOBJ_BYTES 52u
#define RSN3_PSPRITE_BYTES 16u
#define RSN3_REFUSE_SNAPSHOT 300

static void rsn3_put16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8);
}

static void rsn3_put32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16); p[3] = (uint8_t)(v >> 24);
}

static uint16_t rsn3_get16(const uint8_t *p)
{
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static uint32_t rsn3_get32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
        ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int rsn3_mobj_count(unsigned *count_out, uint32_t *player_out)
{
    thinker_t *t;
    unsigned count = 0, player = 0xffffffffu;
    if (displayplayer < 0 || displayplayer >= MAXPLAYERS || !players[displayplayer].mo)
        return -1;
    for (t = thinkercap.next; t != &thinkercap; t = t->next)
    {
        if (t->function.acp1 != (actionf_p1)P_MobjThinker) continue;
        if ((mobj_t *)t == players[displayplayer].mo) player = count;
        if (++count > 1024u) return -1;
    }
    if (player == 0xffffffffu) return -1;
    *count_out = count;
    *player_out = player;
    return 0;
}

static int rsn3_encode_dynamic(uint8_t *out, unsigned long out_cap,
                               unsigned long *out_len)
{
    thinker_t *t;
    unsigned nm = 0, player_index = 0, i;
    size_t need, cursor;
    player_t *player;
    uint8_t *p;
    if (!out || !out_len || numsectors < 0 || numsides < 0 ||
        numtextures < 0 || numflats < 0 || numsectors > 8192 ||
        numsides > 32768 || numtextures > 8191 || numflats > 8191 ||
        rsn3_mobj_count(&nm, &player_index)) return RSN3_REFUSE_SNAPSHOT;
    need = RSN3_DATA_HEADER_BYTES + (size_t)NUMPSPRITES * RSN3_PSPRITE_BYTES;
    need += ((size_t)numflats + 1u + (size_t)numtextures + 1u) * 4u;
    need += (size_t)numsectors * RSN3_SECTOR_BYTES;
    need += (size_t)numsides * RSN3_SIDE_BYTES;
    need += (size_t)nm * RSN3_MOBJ_BYTES;
    if (need > out_cap) return RSN3_REFUSE_SNAPSHOT;
    memset(out, 0, need);
    memcpy(out, "R3-DATA", 7);
    rsn3_put32(out + 8, 1u);
    rsn3_put32(out + 12, (uint32_t)numsectors);
    rsn3_put32(out + 16, (uint32_t)numsides);
    rsn3_put32(out + 20, (uint32_t)numtextures + 1u);
    rsn3_put32(out + 24, (uint32_t)numflats + 1u);
    rsn3_put32(out + 28, nm);
    rsn3_put32(out + 32, NUMPSPRITES);
    rsn3_put32(out + 36, player_index);
    rsn3_put32(out + 40, (uint32_t)displayplayer);
    player = &players[displayplayer];
    rsn3_put32(out + 44, (uint32_t)player->playerstate);
    rsn3_put32(out + 48, (uint32_t)player->mo->x);
    rsn3_put32(out + 52, (uint32_t)player->mo->y);
    rsn3_put32(out + 56, (uint32_t)player->viewz_m);
    rsn3_put32(out + 60, player->mo->angle);
    rsn3_put32(out + 64, 0u); /* This pinned Doom profile has no pitch field. */
    rsn3_put32(out + 68, (uint32_t)player->extralight_m);
    rsn3_put32(out + 72, (uint32_t)player->fixedcolormap_m);
    rsn3_put32(out + 76, (uint32_t)player->viewheight_m);
    rsn3_put32(out + 80, (uint32_t)player->deltaviewheight);
    rsn3_put32(out + 84, (uint32_t)player->bob);
    rsn3_put32(out + 88, (uint32_t)skytexture);
    rsn3_put32(out + 92, (uint32_t)skyflatnum);
    cursor = RSN3_DATA_HEADER_BYTES;
    for (i = 0; i < NUMPSPRITES; i++)
    {
        int32_t state = player->psprites[i].state
            ? (int32_t)(player->psprites[i].state - states) : -1;
        rsn3_put32(out + cursor, (uint32_t)state);
        rsn3_put32(out + cursor + 4u, (uint32_t)player->psprites[i].tics);
        rsn3_put32(out + cursor + 8u, (uint32_t)player->psprites[i].sx);
        rsn3_put32(out + cursor + 12u, (uint32_t)player->psprites[i].sy);
        cursor += RSN3_PSPRITE_BYTES;
    }
    for (i = 0; i <= (unsigned)numflats; i++, cursor += 4u)
        rsn3_put32(out + cursor, (uint32_t)flattranslation[i]);
    for (i = 0; i <= (unsigned)numtextures; i++, cursor += 4u)
        rsn3_put32(out + cursor, (uint32_t)texturetranslation[i]);
    for (i = 0; i < (unsigned)numsectors; i++)
    {
        p = out + cursor;
        rsn3_put32(p, (uint32_t)sectors[i].floorheight);
        rsn3_put32(p + 4u, (uint32_t)sectors[i].ceilingheight);
        rsn3_put16(p + 8u, (uint16_t)sectors[i].floorpic);
        rsn3_put16(p + 10u, (uint16_t)sectors[i].ceilingpic);
        rsn3_put16(p + 12u, (uint16_t)sectors[i].lightlevel);
        cursor += RSN3_SECTOR_BYTES;
    }
    for (i = 0; i < (unsigned)numsides; i++)
    {
        p = out + cursor;
        rsn3_put32(p, (uint32_t)sides[i].textureoffset);
        rsn3_put32(p + 4u, (uint32_t)sides[i].rowoffset);
        rsn3_put16(p + 8u, (uint16_t)sides[i].toptexture);
        rsn3_put16(p + 10u, (uint16_t)sides[i].bottomtexture);
        rsn3_put16(p + 12u, (uint16_t)sides[i].midtexture);
        cursor += RSN3_SIDE_BYTES;
    }
    for (t = thinkercap.next; t != &thinkercap; t = t->next)
    {
        mobj_t *o;
        int32_t state;
        if (t->function.acp1 != (actionf_p1)P_MobjThinker) continue;
        o = (mobj_t *)t;
        state = o->state ? (int32_t)(o->state - states) : -1;
        p = out + cursor;
        rsn3_put32(p, (uint32_t)o->x); rsn3_put32(p + 4u, (uint32_t)o->y);
        rsn3_put32(p + 8u, (uint32_t)o->z); rsn3_put32(p + 12u, o->angle);
        rsn3_put32(p + 16u, (uint32_t)o->sprite); rsn3_put32(p + 20u, (uint32_t)o->frame);
        rsn3_put32(p + 24u, (uint32_t)o->flags); rsn3_put32(p + 28u, (uint32_t)o->type);
        rsn3_put32(p + 32u, (uint32_t)o->radius); rsn3_put32(p + 36u, (uint32_t)o->height);
        rsn3_put32(p + 40u, (uint32_t)o->floorz); rsn3_put32(p + 44u, (uint32_t)o->ceilingz);
        rsn3_put32(p + 48u, (uint32_t)state);
        cursor += RSN3_MOBJ_BYTES;
    }
    if (cursor != need) return RSN3_REFUSE_SNAPSHOT;
    *out_len = (unsigned long)need;
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_snapshot_rsn3(
    uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *mirror, unsigned long mirror_len,
    uint8_t *payload_out, unsigned long payload_cap, unsigned long *payload_len_out)
{
    r2_section_t sections[5];
    r2_v2_plan_t *plan;
    uint8_t *demo;
    int rc, lump, demo_len;
    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI2_HEADER_BYTES || !wad_image ||
        wad_bytes != RI2_WAD_BYTES || !bootstrap ||
        bootstrap_bytes < RI2_BOOTSTRAP_MIN || !ri2_state_shape(state, (size_t)state_len) ||
        !ri2_mirror_shape(mirror, (size_t)mirror_len) || !payload_out ||
        payload_cap < RSN3_DATA_HEADER_BYTES || !payload_len_out ||
        ((uintptr_t)bootstrap & 15u) != 0) return RI2_REFUSE_RESOURCE;
    plan = (r2_v2_plan_t *)(bootstrap + bootstrap_bytes - sizeof(*plan));
    if (((uintptr_t)plan & 15u) != 0 ||
        r2_v2_preflight(resource, (size_t)resource_bytes, sections, plan))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(801);
    memset((void *)DOOM_CTX, 0, sizeof(doom_ctx_t));
    doom_ctx_init();
    if (!doom_sbf_bind_arena(bootstrap, bootstrap_bytes, RI2_ZONE_BYTES)) return RI2_REFUSE_ARENA;
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0;
    displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init(); P_InitThinkers();
    W_InitAccountImage(wad_image, (int)wad_bytes);
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump);
    if (demo_len < 17) return RI2_REFUSE_RESOURCE;
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo; demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    if (doom_sbf_resource_import_graph(resource + sections[R2_GRAPH].offset, sections[R2_GRAPH].bytes))
        return RI2_REFUSE_IMAGE;
    lastanim = anims + plan->scalar[R2_SC_ANIMCOUNT];
    numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    if (r2_walk(resource + sections[R2_SCALARS].offset, sections[R2_SCALARS].bytes,
                 sections[R2_SCALARS].count, 1, r2_apply_scalar, 0) ||
        r2_v2_alloc_resources(plan) ||
        r2_walk(resource + sections[R2_TABLES].offset, sections[R2_TABLES].bytes,
                 sections[R2_TABLES].count, 0, r2_v2_apply_table, 0) ||
        r2_v2_apply_lights(plan) || r2_apply_functions(resource, sections))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(802);
    DoomStateSetAllocationFailure(-1);
    if (DoomStateDecode(state, (size_t)state_len)) return RI2_REFUSE_STATE;
    RI2_PHASE(803);
    return rsn3_encode_dynamic(payload_out, payload_cap, payload_len_out);
}

static int rsn3_apply_dynamic(const uint8_t *image, size_t image_bytes)
{
    uint32_t nsectors, nsides, ntextures, nflats, nmobj, npsprite;
    uint32_t player_index, player_num, player_state;
    size_t need, cursor;
    unsigned i;
    uint8_t *p;
    mobj_t **mobjs;
    if (!image || image_bytes < RSN3_DATA_HEADER_BYTES || memcmp(image, "R3-DATA", 7) ||
        rsn3_get32(image + 8) != 1u) return RSN3_REFUSE_SNAPSHOT;
    nsectors = rsn3_get32(image + 12); nsides = rsn3_get32(image + 16);
    ntextures = rsn3_get32(image + 20); nflats = rsn3_get32(image + 24);
    nmobj = rsn3_get32(image + 28); npsprite = rsn3_get32(image + 32);
    player_index = rsn3_get32(image + 36); player_num = rsn3_get32(image + 40);
    player_state = rsn3_get32(image + 44);
    if (nsectors != (uint32_t)numsectors || nsides != (uint32_t)numsides ||
        ntextures != (uint32_t)numtextures + 1u || nflats != (uint32_t)numflats + 1u ||
        nmobj > 1024u || npsprite != NUMPSPRITES || player_num >= MAXPLAYERS ||
        player_index >= nmobj || player_state > PST_REBORN) return RSN3_REFUSE_SNAPSHOT;
    need = RSN3_DATA_HEADER_BYTES + (size_t)npsprite * RSN3_PSPRITE_BYTES +
        ((size_t)nflats + (size_t)ntextures) * 4u +
        (size_t)nsectors * RSN3_SECTOR_BYTES + (size_t)nsides * RSN3_SIDE_BYTES +
        (size_t)nmobj * RSN3_MOBJ_BYTES;
    if (need != image_bytes) return RSN3_REFUSE_SNAPSHOT;
    cursor = RSN3_DATA_HEADER_BYTES;
    memset(&players[player_num], 0, sizeof(players[player_num]));
    players[player_num].playerstate = (playerstate_t)player_state;
    players[player_num].viewz_m = (fixed_t)rsn3_get32(image + 56);
    players[player_num].extralight_m = (int)rsn3_get32(image + 68);
    players[player_num].fixedcolormap_m = (int)rsn3_get32(image + 72);
    players[player_num].viewheight_m = (fixed_t)rsn3_get32(image + 76);
    players[player_num].deltaviewheight = (fixed_t)rsn3_get32(image + 80);
    players[player_num].bob = (fixed_t)rsn3_get32(image + 84);
    displayplayer = (int)player_num;
    skytexture = (int)rsn3_get32(image + 88);
    skyflatnum = (int)rsn3_get32(image + 92);
    for (i = 0; i < npsprite; i++)
    {
        int32_t state = (int32_t)rsn3_get32(image + cursor);
        if (state < -1 || state >= NUMSTATES) return RSN3_REFUSE_SNAPSHOT;
        players[player_num].psprites[i].state = state < 0 ? 0 : &states[state];
        players[player_num].psprites[i].tics = (int)rsn3_get32(image + cursor + 4u);
        players[player_num].psprites[i].sx = (fixed_t)rsn3_get32(image + cursor + 8u);
        players[player_num].psprites[i].sy = (fixed_t)rsn3_get32(image + cursor + 12u);
        cursor += RSN3_PSPRITE_BYTES;
    }
    for (i = 0; i < nflats; i++, cursor += 4u) flattranslation[i] = (int)rsn3_get32(image + cursor);
    for (i = 0; i < ntextures; i++, cursor += 4u) texturetranslation[i] = (int)rsn3_get32(image + cursor);
    for (i = 0; i < nsectors; i++)
    {
        p = (uint8_t *)(image + cursor);
        sectors[i].floorheight = (fixed_t)rsn3_get32(p);
        sectors[i].ceilingheight = (fixed_t)rsn3_get32(p + 4u);
        sectors[i].floorpic = (short)rsn3_get16(p + 8u);
        sectors[i].ceilingpic = (short)rsn3_get16(p + 10u);
        sectors[i].lightlevel = (short)rsn3_get16(p + 12u);
        sectors[i].thinglist = 0;
        cursor += RSN3_SECTOR_BYTES;
    }
    for (i = 0; i < nsides; i++)
    {
        p = (uint8_t *)(image + cursor);
        sides[i].textureoffset = (fixed_t)rsn3_get32(p);
        sides[i].rowoffset = (fixed_t)rsn3_get32(p + 4u);
        sides[i].toptexture = (short)rsn3_get16(p + 8u);
        sides[i].bottomtexture = (short)rsn3_get16(p + 10u);
        sides[i].midtexture = (short)rsn3_get16(p + 12u);
        cursor += RSN3_SIDE_BYTES;
    }
    P_InitThinkers();
    memset(activeceilings, 0, sizeof(activeceilings));
    memset(activeplats, 0, sizeof(activeplats));
    memset(blocklinks, 0, (size_t)bmapwidth * bmapheight * sizeof(*blocklinks));
    mobjs = nmobj ? (mobj_t **)calloc(nmobj, sizeof(*mobjs)) : 0;
    if (nmobj && !mobjs) return RI2_REFUSE_ARENA;
    for (i = 0; i < nmobj; i++)
    {
        mobj_t *o = (mobj_t *)calloc(1, sizeof(*o));
        int32_t type, state;
        if (!o) { free(mobjs); return RI2_REFUSE_ARENA; }
        p = (uint8_t *)(image + cursor);
        o->x = (fixed_t)rsn3_get32(p); o->y = (fixed_t)rsn3_get32(p + 4u);
        o->z = (fixed_t)rsn3_get32(p + 8u); o->angle = rsn3_get32(p + 12u);
        o->sprite = (spritenum_t)rsn3_get32(p + 16u); o->frame = (int)rsn3_get32(p + 20u);
        o->flags = (int)rsn3_get32(p + 24u); type = (int32_t)rsn3_get32(p + 28u);
        o->radius = (fixed_t)rsn3_get32(p + 32u); o->height = (fixed_t)rsn3_get32(p + 36u);
        o->floorz = (fixed_t)rsn3_get32(p + 40u); o->ceilingz = (fixed_t)rsn3_get32(p + 44u);
        state = (int32_t)rsn3_get32(p + 48u);
        if (type < 0 || type >= NUMMOBJTYPES || state < -1 || state >= NUMSTATES ||
            o->sprite < 0 || o->sprite >= NUMSPRITES) { free(mobjs); return RSN3_REFUSE_SNAPSHOT; }
        o->type = (mobjtype_t)type; o->info = &mobjinfo[type];
        o->state = state < 0 ? 0 : &states[state];
        o->thinker.function.acp1 = (actionf_p1)P_MobjThinker;
        mobjs[i] = o;
        P_SetThingPosition(o);
        P_AddThinker(&o->thinker);
        cursor += RSN3_MOBJ_BYTES;
    }
    players[player_num].mo = mobjs[player_index];
    free(mobjs);
    if (cursor != image_bytes) return RSN3_REFUSE_SNAPSHOT;
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_render_rsn3(
    uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *payload, unsigned long payload_bytes, uint32_t strip_id,
    uint8_t *strip_out, unsigned long strip_cap,
    unsigned long *arena_high_water_out)
{
    r2_section_t sections[5];
    r2_v2_plan_t *plan;
    uint8_t *frame = 0, *demo;
    size_t strip_bytes = 40u * 200u;
    int rc, y, x, lump, demo_len;
    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI2_HEADER_BYTES || !wad_image ||
        wad_bytes != RI2_WAD_BYTES || !bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN ||
        !payload || !strip_out || strip_cap != strip_bytes || strip_id >= 8u ||
        ((uintptr_t)bootstrap & 15u) != 0) return RI2_REFUSE_RESOURCE;
    plan = (r2_v2_plan_t *)(bootstrap + bootstrap_bytes - sizeof(*plan));
    if (((uintptr_t)plan & 15u) != 0 || r2_v2_preflight(resource, (size_t)resource_bytes, sections, plan))
        return RI2_REFUSE_IMAGE;
    memset((void *)DOOM_CTX, 0, sizeof(doom_ctx_t)); doom_ctx_init();
    if (!doom_sbf_bind_arena(bootstrap, bootstrap_bytes, RI2_ZONE_BYTES)) return RI2_REFUSE_ARENA;
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0;
    displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init(); P_InitThinkers(); W_InitAccountImage(wad_image, (int)wad_bytes);
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump);
    if (demo_len < 17) return RI2_REFUSE_RESOURCE;
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo; demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    if (doom_sbf_resource_import_graph(resource + sections[R2_GRAPH].offset, sections[R2_GRAPH].bytes)) return RI2_REFUSE_IMAGE;
    lastanim = anims + plan->scalar[R2_SC_ANIMCOUNT]; numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    if (r2_walk(resource + sections[R2_SCALARS].offset, sections[R2_SCALARS].bytes,
                 sections[R2_SCALARS].count, 1, r2_apply_scalar, 0) ||
        r2_v2_alloc_resources(plan) ||
        r2_walk(resource + sections[R2_TABLES].offset, sections[R2_TABLES].bytes,
                 sections[R2_TABLES].count, 0, r2_v2_apply_table, 0) ||
        r2_v2_apply_lights(plan) || r2_apply_functions(resource, sections)) return RI2_REFUSE_IMAGE;
    rc = rsn3_apply_dynamic(payload, (size_t)payload_bytes);
    if (rc) return rc;
    frame = (uint8_t *)calloc(1, 4u * RI2_FRAME_BYTES);
    if (!frame) return RI2_REFUSE_OUTPUT;
    screens[0] = frame; screens[1] = frame + RI2_FRAME_BYTES;
    screens[2] = frame + 2u * RI2_FRAME_BYTES; screens[3] = frame + 3u * RI2_FRAME_BYTES; screens[4] = 0;
    viewimage = screens[0];
    for (rc = 0; rc < viewheight; rc++) ylookup[rc] = screens[0] + (size_t)rc * 320u;
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 : (SCREENHEIGHT - 32 - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH; pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true;
    doom_render_fuzz_columns = (1u << 30) | (strip_id << 27);
    R_RenderPlayerView(&players[displayplayer]);
    if (doom_sbf_arena_exhausted()) { free(frame); return RI2_REFUSE_ARENA; }
    for (y = 0; y < 200; y++) for (x = 0; x < 40; x++)
        strip_out[(size_t)y * 40u + (size_t)x] = frame[(size_t)y * 320u + (size_t)strip_id * 40u + (size_t)x];
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    free(frame);
    return 0;
}

/* ------------------------------------------------------------------ */
/* DOOM-E2R/4 split consumer                                           */
/*
 * These entries intentionally duplicate the source-matched phase order
 * above instead of calling the old all-in-one entry.  E2R/3 therefore keeps
 * its old behavior and receipt identity, while E2R/4 can commit the live
 * context and arena after setup, decode, and publication independently.
 * The only cross-transaction state is the pinned context account; no C
 * pointer is copied into the Rust continuation account.
 */

int doom_engine_ex2_resource_sbf_ri2_split_preflight(
    uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    unsigned long *arena_high_water_out)
{
    r2_section_t sections[5];
    _Static_assert(sizeof(r2_v2_plan_t) <= RI2_SPLIT_RI2_PLAN_RESERVE,
                   "RI2 plan exceeds its reserved tail slice");
    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI2_HEADER_BYTES ||
        !bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN ||
        ((uintptr_t)bootstrap & 15u) != 0) return RI2_REFUSE_RESOURCE;
    /* Bind only the caller-provided carrier.  E2R/4 has not initialized the
     * engine yet; this field is the durable plan-storage base and is reset by
     * cursor 2's source-matched context initialization. */
    if (!ri2_split_arena_bytes(bootstrap_bytes) ||
        !doom_sbf_bind_arena(bootstrap, ri2_split_arena_bytes(bootstrap_bytes), RI2_ZONE_BYTES))
        return RI2_REFUSE_ARENA;
    if (r2_v2_preflight(resource, (size_t)resource_bytes, sections,
                        (r2_v2_plan_t *)(bootstrap + bootstrap_bytes - sizeof(r2_v2_plan_t))))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(501);
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_split_setup(
    uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *mirror, unsigned long mirror_len,
    const uint8_t *controller, unsigned long controller_len,
    unsigned long *arena_high_water_out)
{
    r2_section_t sections[5];
    r2_v2_plan_t *plan;
    int rc;
    uint32_t input_tic;

    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI2_HEADER_BYTES ||
        !wad_image || wad_bytes != RI2_WAD_BYTES ||
        !bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN ||
        !state || !mirror || !controller ||
        !ri2_state_shape(state, (size_t)state_len) ||
        !ri2_mirror_shape(mirror, (size_t)mirror_len)) return RI2_REFUSE_RESOURCE;
    input_tic = ri2_u32(state + 20);
    if (!ri2_controller_shape(controller, (size_t)controller_len, input_tic))
        return RI2_REFUSE_CONTROLLER;
    if (((uintptr_t)bootstrap & 15u) != 0) return RI2_REFUSE_ARENA;
    plan = (r2_v2_plan_t *)(bootstrap + bootstrap_bytes - sizeof(*plan));
    if (((uintptr_t)plan & 15u) != 0) return RI2_REFUSE_ARENA;
    if (ri2_rebind_plan(resource, (size_t)resource_bytes, sections, plan))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(502); /* re-bound the authenticated RI2/2 plan */

    memset((void *)DOOM_CTX, 0, sizeof(doom_ctx_t));
    doom_ctx_init();
    RI2_PHASE(510);
    if (!ri2_split_arena_bytes(bootstrap_bytes) ||
        !doom_sbf_bind_arena(bootstrap, ri2_split_arena_bytes(bootstrap_bytes), RI2_ZONE_BYTES))
        return RI2_REFUSE_ARENA;
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0;
    displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init();
    RI2_PHASE(520);
    P_InitThinkers();
    RI2_PHASE(521);
    W_InitAccountImage(wad_image, (int)wad_bytes);
    RI2_PHASE(522);
    if (!ri2_split_arena_ok(bootstrap_bytes))
        return RI2_REFUSE_ARENA;
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_split_setup_continue(
    uint8_t *context, const uint8_t *resource, unsigned long resource_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *mirror, unsigned long mirror_len,
    const uint8_t *controller, unsigned long controller_len,
    unsigned long *arena_high_water_out)
{
    r2_section_t sections[5];
    r2_v2_plan_t *plan;
    uint8_t *frame = 0, *demo;
    int lump, demo_len, rc;
    uint32_t input_tic;

    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI2_HEADER_BYTES ||
        !bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN ||
        !state || !mirror || !controller ||
        !ri2_state_shape(state, (size_t)state_len) ||
        !ri2_mirror_shape(mirror, (size_t)mirror_len)) return RI2_REFUSE_RESOURCE;
    input_tic = ri2_u32(state + 20);
    if (!ri2_controller_shape(controller, (size_t)controller_len, input_tic))
        return RI2_REFUSE_CONTROLLER;
    if (((uintptr_t)bootstrap & 15u) != 0) return RI2_REFUSE_ARENA;
    if (!ri2_split_arena_bytes(bootstrap_bytes) ||
        !doom_sbf_rebind_arena(bootstrap, ri2_split_arena_bytes(bootstrap_bytes), RI2_ZONE_BYTES))
        return RI2_REFUSE_ARENA;
    plan = (r2_v2_plan_t *)(bootstrap + bootstrap_bytes - sizeof(*plan));
    if (((uintptr_t)plan & 15u) != 0) return RI2_REFUSE_ARENA;
    if (ri2_rebind_plan(resource, (size_t)resource_bytes, sections, plan))
        return RI2_REFUSE_IMAGE;
    if (doom_sbf_resource_import_graph(resource + sections[R2_GRAPH].offset,
                                       sections[R2_GRAPH].bytes))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(530);
    lastanim = anims + plan->scalar[R2_SC_ANIMCOUNT];
    numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    if (r2_walk(resource + sections[R2_SCALARS].offset,
                sections[R2_SCALARS].bytes, sections[R2_SCALARS].count,
                1, r2_apply_scalar, 0) ||
        r2_v2_alloc_resources(plan) ||
        r2_walk(resource + sections[R2_TABLES].offset,
                sections[R2_TABLES].bytes, sections[R2_TABLES].count,
                0, r2_v2_apply_table, 0) ||
        r2_v2_apply_lights(plan) || r2_apply_functions(resource, sections))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(540);

    frame = (uint8_t *)calloc(1, 4u * RI2_FRAME_BYTES);
    if (!frame) return RI2_REFUSE_OUTPUT;
    screens[0] = frame; screens[1] = frame + RI2_FRAME_BYTES;
    screens[2] = frame + 2u * RI2_FRAME_BYTES;
    screens[3] = frame + 3u * RI2_FRAME_BYTES; screens[4] = 0;
    viewimage = screens[0];
    for (rc = 0; rc < viewheight; rc++) ylookup[rc] = screens[0] + (size_t)rc * 320u;
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 :
        (SCREENHEIGHT - 32 - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0;
    setsizeneeded = false;
    RI2_PHASE(550);
    lump = W_GetNumForName("DEMO1");
    demo_len = W_LumpLength(lump);
    if (demo_len < 17) return RI2_REFUSE_RESOURCE;
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo; demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    DoomStateSetAllocationFailure(-1);
    RI2_PHASE(560); /* setup is now durable in the pinned context */
    if (!ri2_split_arena_ok(bootstrap_bytes))
        return RI2_REFUSE_ARENA;
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    return 0;
}

static int ri2_split_decode_inputs(uint8_t *context, uint8_t *bootstrap,
                                   unsigned long bootstrap_bytes,
                                   const uint8_t *state, unsigned long state_len,
                                   const uint8_t *controller, unsigned long controller_len)
{
    uint32_t input_tic;
    if (doom_ctx_guard(context)) return 199;
    if (!bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN ||
        !state || !controller || !ri2_state_shape(state, (size_t)state_len))
        return RI2_REFUSE_RESOURCE;
    input_tic = ri2_u32(state + 20);
    if (!ri2_controller_shape(controller, (size_t)controller_len, input_tic))
        return RI2_REFUSE_CONTROLLER;
    if (!ri2_split_arena_bytes(bootstrap_bytes) ||
        !doom_sbf_rebind_arena(bootstrap, ri2_split_arena_bytes(bootstrap_bytes), RI2_ZONE_BYTES))
        return RI2_REFUSE_ARENA;
    DoomStateSetAllocationFailure(-1);
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_split_decode_preflight(
    uint8_t *context, uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *controller, unsigned long controller_len,
    unsigned long *arena_high_water_out)
{
    int rc = ri2_split_decode_inputs(context, bootstrap, bootstrap_bytes,
                                     state, state_len, controller, controller_len);
    if (rc || DoomStateDecodeSplitPreflight(state, (size_t)state_len))
        return rc ? rc : RI2_REFUSE_STATE;
    RI2_PHASE(571);
    if (!ri2_split_arena_ok(bootstrap_bytes)) return RI2_REFUSE_ARENA;
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_split_decode_prepare(
    uint8_t *context, uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *controller, unsigned long controller_len,
    unsigned long *arena_high_water_out)
{
    int rc = ri2_split_decode_inputs(context, bootstrap, bootstrap_bytes,
                                     state, state_len, controller, controller_len);
    if (rc || DoomStateDecodeSplitPrepare())
        return rc ? rc : RI2_REFUSE_STATE;
    RI2_PHASE(572);
    if (!ri2_split_arena_ok(bootstrap_bytes)) return RI2_REFUSE_ARENA;
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_split_decode_parse(
    uint8_t *context, uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len,
    const uint8_t *controller, unsigned long controller_len,
    unsigned long *arena_high_water_out)
{
    int rc = ri2_split_decode_inputs(context, bootstrap, bootstrap_bytes,
                                     state, state_len, controller, controller_len);
    if (rc || DoomStateDecodeSplitParse(state, (size_t)state_len))
        return rc ? rc : RI2_REFUSE_STATE;
    if (DoomControllerArmV2(controller + 24) ||
        DoomControllerValidateCapturedDemoCursor(demo_p))
        return RI2_REFUSE_STATE;
    RI2_PHASE(573); /* source-matched parse/commit and CI2 cursor arm */
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 :
        (SCREENHEIGHT - 32 - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0;
    setsizeneeded = false;
    if (!ri2_split_arena_ok(bootstrap_bytes)) return RI2_REFUSE_ARENA;
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    return 0;
}

int doom_engine_ex2_resource_sbf_ri2_split_finalize(
    uint8_t *context, uint8_t *bootstrap, unsigned long bootstrap_bytes,
    uint8_t *state_out, unsigned long state_cap, unsigned long *state_out_len,
    uint8_t *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    uint8_t *frame_out, unsigned long frame_cap, unsigned long *frame_out_len,
    unsigned long *arena_high_water_out)
{
    uint8_t *encoded = 0, *derived = 0;
    size_t encoded_bytes = 0;
    if (doom_ctx_guard(context)) return 199;
    if (!bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN ||
        !state_out || !state_out_len || !mirror_out || !mirror_out_len ||
        !frame_out || !frame_out_len || state_cap < RI2_STATE_MAX ||
        mirror_cap < RI2_MIRROR_BYTES || frame_cap < RI2_FRAME_BYTES)
        return RI2_REFUSE_OUTPUT;
    if (!ri2_split_arena_bytes(bootstrap_bytes) ||
        !doom_sbf_rebind_arena(bootstrap, ri2_split_arena_bytes(bootstrap_bytes), RI2_ZONE_BYTES))
        return RI2_REFUSE_ARENA;
    if (!ri2_split_arena_ok(bootstrap_bytes)) return RI2_REFUSE_ARENA;
    derived = (uint8_t *)malloc(RI2_MIRROR_BYTES);
    if (!derived || doom_sbf_fill_pool(derived, ri2_wad_hash) ||
        DoomStateEncode(&encoded, &encoded_bytes) ||
        encoded_bytes > RI2_STATE_MAX || encoded_bytes > state_cap ||
        encoded_bytes < 64u) {
        free(derived); free(encoded); return RI2_REFUSE_OUTPUT;
    }
    RI2_PHASE(600);
    memcpy(state_out, encoded, encoded_bytes);
    memcpy(mirror_out, derived, RI2_MIRROR_BYTES);
    memcpy(frame_out, screens[0], RI2_FRAME_BYTES);
    *state_out_len = encoded_bytes; *mirror_out_len = RI2_MIRROR_BYTES;
    *frame_out_len = RI2_FRAME_BYTES;
    RI2_PHASE(610);
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    DoomStateFree(encoded); free(derived);
    return 0;
}

/* DOOM-SIM/1: one persistent, no-render transition after E2R/4 has published
 * the initialized context.  The caller keeps DOOM_CTX and the arena mapped
 * across instructions; this function only applies one captured tic command,
 * runs the authoritative game ticker, and re-encodes ST1/MP1.  In particular,
 * it does not call any RI2 setup, WAD import, state reconstruction, or render
 * routine. */
int doom_sbf_sim_tick(
    uint8_t *context, const uint8_t *command, unsigned long command_len,
    uint8_t *state_out, unsigned long state_cap, unsigned long *state_out_len,
    uint8_t *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len)
{
    uint8_t *derived = 0, *encoded = 0;
    size_t encoded_bytes = 0;
    unsigned long transient_mark, previous_state_bytes;
    RI2_PHASE(810);
    if (doom_ctx_guard(context)) return 199;
    if (!command || command_len != 4u || !state_out || !state_out_len ||
        !mirror_out || !mirror_out_len || state_cap < RI2_STATE_MAX ||
        mirror_cap < RI2_MIRROR_BYTES) return 232;
    if (DoomControllerArmV2(command) ||
        DoomControllerValidateCapturedDemoCursor(demo_p)) return 237;
    RI2_PHASE(811);
    G_Ticker();
    gametic++;
    RI2_PHASE(812);
    if (doom_sbf_arena_exhausted()) return 239;
    transient_mark = doom_sbf_arena_mark();
    derived = (uint8_t *)malloc(RI2_MIRROR_BYTES);
    if (!derived) return 238;
    RI2_PHASE(813);
    if (doom_sbf_fill_pool(derived, ri2_wad_hash)) {
        free(derived); return 233;
    }
    RI2_PHASE(814);
    if (DoomStateEncode(&encoded, &encoded_bytes)) {
        free(derived); free(encoded); return 236;
    }
    RI2_PHASE(815);
    if (encoded_bytes > state_cap || encoded_bytes < 64u) {
        free(derived); free(encoded); return 238;
    }
    previous_state_bytes = ri2_u32(state_out + 16);
    if (previous_state_bytes < 64u || previous_state_bytes > state_cap) {
        free(derived); free(encoded); return 238;
    }
    memcpy(state_out, encoded, encoded_bytes);
    RI2_PHASE(816);
    {
        unsigned long i;
        for (i = (unsigned long)encoded_bytes; i < previous_state_bytes; i++)
            state_out[i] = 0;
    }
    RI2_PHASE(817);
    memcpy(mirror_out, derived, RI2_MIRROR_BYTES);
    RI2_PHASE(818);
    *state_out_len = encoded_bytes;
    *mirror_out_len = RI2_MIRROR_BYTES;
    DoomStateFree(encoded); free(derived);
    if (!doom_sbf_arena_rewind(transient_mark)) return 239;
    RI2_PHASE(819);
    return 0;
}

/* DOOM-SIM/2: the same persistent no-render transition as SIM/1, split so
 * that one ADVANCE instruction can execute several consecutive staged tics
 * against the resident engine and derive ST1/MP1 exactly once, at the
 * committed cursor.  doom_sbf_sim2_step never allocates, encodes, or renders;
 * doom_sbf_sim2_publish is the only ST1/MP1 writer.  The pre-tic guard in
 * doom_sim2_guard.h is shared verbatim with the host session oracle. */
#include "doom_sim2_guard.h"

int doom_sbf_sim2_step(uint8_t *context, const uint8_t *command,
                       unsigned long command_len)
{
    int halt;
    RI2_PHASE(900);
    if (doom_ctx_guard(context)) return 199;
    if (!command || command_len != 4u) return 232;
    halt = doom_sim2_pretic_guard(command);
    if (halt) return halt;
    if (DoomControllerArmV2(command)) return 237;
    RI2_PHASE(901);
    G_Ticker();
    gametic++;
    RI2_PHASE(902);
    if (doom_sbf_arena_exhausted()) return 239;
    return 0;
}

int doom_sbf_sim2_publish(
    uint8_t *context, uint8_t *state_out, unsigned long state_cap,
    unsigned long *state_out_len, uint8_t *mirror_out, unsigned long mirror_cap)
{
    uint8_t *encoded = 0;
    size_t encoded_bytes = 0;
    unsigned long transient_mark;
    RI2_PHASE(910);
    if (doom_ctx_guard(context)) return 199;
    if (!state_out || !state_out_len || !mirror_out ||
        state_cap < RI2_STATE_MAX || mirror_cap < RI2_MIRROR_BYTES) return 232;
    if (doom_sbf_arena_exhausted()) return 239;
    transient_mark = doom_sbf_arena_mark();
    /* fill_pool clears the whole 64 KiB pool before writing its header and
     * slots, so the mirror account needs no staging buffer. */
    if (doom_sbf_fill_pool(mirror_out, ri2_wad_hash)) return 233;
    RI2_PHASE(911);
    if (DoomStateEncode(&encoded, &encoded_bytes)) {
        free(encoded); return 236;
    }
    RI2_PHASE(912);
    if (encoded_bytes > state_cap || encoded_bytes < 64u) {
        free(encoded); return 238;
    }
    memcpy(state_out, encoded, encoded_bytes);
    memset(state_out + encoded_bytes, 0, state_cap - encoded_bytes);
    *state_out_len = encoded_bytes;
    DoomStateFree(encoded);
    if (!doom_sbf_arena_rewind(transient_mark)) return 239;
    RI2_PHASE(913);
    return 0;
}

/* ------------------------------------------------------------------ */
/* DOOM-RSR/1 static renderer resource account                         */

#define RSR_HEADER_BYTES 320u
#define RSR_ROOTS_OFFSET 160u
#define RSR_ROOT_COUNT 32u
#define RSR_REFUSE_LAYOUT 302u

static uint32_t rsr_get32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
        ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static void rsr_put32(uint8_t *p, uint32_t value)
{
    p[0] = (uint8_t)value; p[1] = (uint8_t)(value >> 8);
    p[2] = (uint8_t)(value >> 16); p[3] = (uint8_t)(value >> 24);
}

static void rsr_put64(uint8_t *p, uint64_t value)
{
    unsigned i;
    for (i = 0; i < 8; i++) p[i] = (uint8_t)(value >> (i * 8u));
}

/* These are the top-level pointer roots for the imported RI2 graph, its
 * renderer tables, and the cached DEMO1 bytes.  The structures they reach
 * already contain native pointers.  Those remain valid because account zero
 * is pinned to DOOM_CTX's measured address on every BUILD/RENDER call; this
 * directory records the account-relative root offsets and reconstructs them
 * before each render. */
static void *rsr_root_value(unsigned index)
{
    switch (index) {
    case 0: return vertexes;
    case 1: return sectors;
    case 2: return sides;
    case 3: return lines;
    case 4: return segs;
    case 5: return subsectors;
    case 6: return nodes;
    case 7: return blockmaplump;
    case 8: return blockmap;
    case 9: return rejectmatrix;
    case 10: return blocklinks;
    case 11: return textures;
    case 12: return texturecolumnlump;
    case 13: return texturecolumnofs;
    case 14: return texturecomposite;
    case 15: return texturecompositesize;
    case 16: return textureheight;
    case 17: return texturetranslation;
    case 18: return texturewidthmask;
    case 19: return flattranslation;
    case 20: return spritewidth;
    case 21: return spriteoffset;
    case 22: return spritetopoffset;
    case 23: return sprites;
    case 24: return colormaps;
    case 25: return translationtables;
    case 26: return spritelights;
    case 27: return walllights;
    case 28: return planezlight;
    case 29: return demobuffer;
    case 30: return demo_p;
    case 31: return demoend;
    default: return 0;
    }
}

static int rsr_root_assign(unsigned index, void *value)
{
    switch (index) {
    case 0: vertexes = (vertex_t *)value; break;
    case 1: sectors = (sector_t *)value; break;
    case 2: sides = (side_t *)value; break;
    case 3: lines = (line_t *)value; break;
    case 4: segs = (seg_t *)value; break;
    case 5: subsectors = (subsector_t *)value; break;
    case 6: nodes = (node_t *)value; break;
    case 7: blockmaplump = (short *)value; break;
    case 8: blockmap = (short *)value; break;
    case 9: rejectmatrix = (byte *)value; break;
    case 10: blocklinks = (mobj_t **)value; break;
    case 11: textures = (texture_t **)value; break;
    case 12: texturecolumnlump = (short **)value; break;
    case 13: texturecolumnofs = (unsigned short **)value; break;
    case 14: texturecomposite = (byte **)value; break;
    case 15: texturecompositesize = (int *)value; break;
    case 16: textureheight = (fixed_t *)value; break;
    case 17: texturetranslation = (int *)value; break;
    case 18: texturewidthmask = (int *)value; break;
    case 19: flattranslation = (int *)value; break;
    case 20: spritewidth = (fixed_t *)value; break;
    case 21: spriteoffset = (fixed_t *)value; break;
    case 22: spritetopoffset = (fixed_t *)value; break;
    case 23: sprites = (spritedef_t *)value; break;
    case 24: colormaps = (lighttable_t *)value; break;
    case 25: translationtables = (byte *)value; break;
    case 26: spritelights = (lighttable_t *)value; break;
    case 27: walllights = (lighttable_t *)value; break;
    case 28: planezlight = (lighttable_t *)value; break;
    case 29: demobuffer = (byte *)value; break;
    case 30: demo_p = (byte *)value; break;
    case 31: demoend = (byte *)value; break;
    default: return -1;
    }
    return 0;
}

static int rsr_write_layout(uint8_t *context, unsigned long account_bytes,
                            uint8_t *header, unsigned long header_bytes)
{
    uintptr_t base = (uintptr_t)context, end = base + account_bytes;
    unsigned i;
    if (!header || header_bytes != RSR_HEADER_BYTES || end < base) return -1;
    memset(header, 0, RSR_HEADER_BYTES);
    memcpy(header, "DOOM-RSR", 8);
    rsr_put32(header + 8, 1u);
    rsr_put32(header + 12, RSR_HEADER_BYTES);
    rsr_put32(header + 40, RSR_ROOT_COUNT);
    rsr_put64(header + 112, (uint64_t)base);
    for (i = 0; i < RSR_ROOT_COUNT; i++) {
        uintptr_t pointer = (uintptr_t)rsr_root_value(i);
        uint32_t offset = 0xffffffffu;
        if (pointer) {
            if (pointer < base || pointer >= end || pointer - base > 0xffffffffu)
                return -1;
            offset = (uint32_t)(pointer - base);
        }
        rsr_put32(header + RSR_ROOTS_OFFSET + i * 4u, offset);
    }
    return 0;
}

static int rsr_rebind_layout(uint8_t *context, unsigned long account_bytes,
                             const uint8_t *header, unsigned long header_bytes)
{
    uintptr_t base = (uintptr_t)context;
    unsigned i;
    if (!header || header_bytes != RSR_HEADER_BYTES ||
        memcmp(header, "DOOM-RSR", 8) || rsr_get32(header + 8) != 1u ||
        rsr_get32(header + 12) != RSR_HEADER_BYTES ||
        rsr_get32(header + 16) != account_bytes ||
        rsr_get32(header + 20) != sizeof(doom_ctx_t) ||
        rsr_get32(header + 40) != RSR_ROOT_COUNT ||
        base != (uintptr_t)DOOM_CTX) return -1;
    for (i = 0; i < RSR_ROOT_COUNT; i++) {
        uint32_t offset = rsr_get32(header + RSR_ROOTS_OFFSET + i * 4u);
        void *value = 0;
        if (offset != 0xffffffffu) {
            if (offset >= account_bytes) return -1;
            value = context + offset;
        }
        if (rsr_root_assign(i, value)) return -1;
    }
    return 0;
}

int doom_engine_rsn3_build_static(
    uint8_t *context, unsigned long account_bytes,
    const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    uint8_t *header, unsigned long header_bytes,
    unsigned long *arena_high_water_out)
{
    r2_section_t sections[5];
    r2_v2_plan_t *plan;
    uint8_t *demo;
    int lump, demo_len, rc;
    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI2_HEADER_BYTES || !wad_image ||
        wad_bytes != RI2_WAD_BYTES || !bootstrap ||
        bootstrap_bytes < RI2_BOOTSTRAP_MIN - RSR_HEADER_BYTES || !header ||
        header_bytes != RSR_HEADER_BYTES ||
        account_bytes < sizeof(doom_ctx_t) ||
        ((uintptr_t)bootstrap & 15u) != 0) return RI2_REFUSE_RESOURCE;
    plan = (r2_v2_plan_t *)(bootstrap + bootstrap_bytes - sizeof(*plan));
    if (((uintptr_t)plan & 15u) != 0 ||
        r2_v2_preflight(resource, (size_t)resource_bytes, sections, plan))
        return RI2_REFUSE_IMAGE;
    RI2_PHASE(1001);

    /* BUILD alone clears the context and creates all static renderer state.
     * The account carries the live C structs, zone, WAD bytes, and malloc
     * carrier at fixed offsets; later RENDER calls never repeat this setup. */
    memset((void *)DOOM_CTX, 0, sizeof(doom_ctx_t));
    doom_ctx_init();
    if (!ri2_split_arena_bytes(bootstrap_bytes) ||
        !doom_sbf_bind_arena(bootstrap, ri2_split_arena_bytes(bootstrap_bytes),
                             RI2_ZONE_BYTES)) return RI2_REFUSE_ARENA;
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0;
    displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init(); P_InitThinkers();
    W_InitAccountImage(wad_image, (int)wad_bytes);
    if (doom_sbf_resource_import_graph(resource + sections[R2_GRAPH].offset,
                                       sections[R2_GRAPH].bytes))
        return RI2_REFUSE_IMAGE;
    lastanim = anims + plan->scalar[R2_SC_ANIMCOUNT];
    numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    if (r2_walk(resource + sections[R2_SCALARS].offset,
                sections[R2_SCALARS].bytes, sections[R2_SCALARS].count,
                1, r2_apply_scalar, 0) ||
        r2_v2_alloc_resources(plan) ||
        r2_walk(resource + sections[R2_TABLES].offset,
                sections[R2_TABLES].bytes, sections[R2_TABLES].count,
                0, r2_v2_apply_table, 0) ||
        r2_v2_apply_lights(plan) || r2_apply_functions(resource, sections))
        return RI2_REFUSE_IMAGE;
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump);
    if (demo_len < 17) return RI2_REFUSE_RESOURCE;
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    if (!demo) return RI2_REFUSE_RESOURCE;
    defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo; demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    DoomStateSetAllocationFailure(-1);
    if (doom_sbf_arena_exhausted() ||
        rsr_write_layout(context, account_bytes, header, header_bytes))
        return RSR_REFUSE_LAYOUT;
    /* RENDER validates this prefix before applying any account-relative root
     * offsets. Rust fills the remaining authenticated descriptor fields. */
    rsr_put32(header + 16, (uint32_t)account_bytes);
    rsr_put32(header + 20, (uint32_t)sizeof(doom_ctx_t));
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    return 0;
}

int doom_engine_rsn3_render_static(
    uint8_t *context, unsigned long account_bytes,
    const uint8_t *header, unsigned long header_bytes,
    uint8_t *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *payload, unsigned long payload_bytes, uint32_t strip_id,
    uint8_t *strip_out, unsigned long strip_cap,
    unsigned long *arena_high_water_out)
{
    uint8_t *frame;
    size_t strip_bytes = 40u * 200u;
    unsigned long transient_mark;
    int rc, y, x;
    if (doom_ctx_guard(context)) return 199;
    if (!bootstrap || bootstrap_bytes < RI2_BOOTSTRAP_MIN - RSR_HEADER_BYTES || !payload ||
        !strip_out || strip_cap != strip_bytes || strip_id >= 8u ||
        ((uintptr_t)bootstrap & 15u) != 0 ||
        rsr_rebind_layout(context, account_bytes, header, header_bytes))
        return RSR_REFUSE_LAYOUT;
    /* BUILD leaves cached lump metadata and renderer tables at the front of
     * this carrier.  Preserve its allocation cursor so the per-frame mobj
     * and framebuffer allocations cannot overwrite those static resources. */
    if (!ri2_split_arena_bytes(bootstrap_bytes) ||
        !doom_sbf_rebind_arena(bootstrap, ri2_split_arena_bytes(bootstrap_bytes),
                               RI2_ZONE_BYTES))
        return RI2_REFUSE_ARENA;
    transient_mark = doom_sbf_arena_mark();
    rc = rsn3_apply_dynamic(payload, (size_t)payload_bytes);
    if (rc) return rc;
    frame = (uint8_t *)calloc(1, 4u * RI2_FRAME_BYTES);
    if (!frame) return RI2_REFUSE_OUTPUT;
    screens[0] = frame; screens[1] = frame + RI2_FRAME_BYTES;
    screens[2] = frame + 2u * RI2_FRAME_BYTES;
    screens[3] = frame + 3u * RI2_FRAME_BYTES; screens[4] = 0;
    viewimage = screens[0];
    for (rc = 0; rc < viewheight; rc++) ylookup[rc] = screens[0] + (size_t)rc * 320u;
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 :
        (SCREENHEIGHT - 32 - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true;
    doom_render_fuzz_columns = (1u << 30) | (strip_id << 27);
    R_RenderPlayerView(&players[displayplayer]);
    if (doom_sbf_arena_exhausted()) {
        free(frame);
        (void)doom_sbf_arena_rewind(transient_mark);
        return RI2_REFUSE_ARENA;
    }
    for (y = 0; y < 200; y++) for (x = 0; x < 40; x++)
        strip_out[(size_t)y * 40u + (size_t)x] =
            frame[(size_t)y * 320u + (size_t)strip_id * 40u + (size_t)x];
    if (arena_high_water_out) *arena_high_water_out = doom_sbf_arena_high_water();
    free(frame);
    if (!doom_sbf_arena_rewind(transient_mark)) return RI2_REFUSE_ARENA;
    return 0;
}
