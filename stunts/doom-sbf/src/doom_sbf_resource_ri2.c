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

extern int doom_ctx_guard(const void *data);
extern void doom_ctx_init(void);
extern int doom_sbf_bind_arena(void *base, unsigned long bytes, unsigned long zone_bytes);
extern int doom_sbf_arena_exhausted(void);
extern unsigned long doom_sbf_arena_high_water(void);
extern int doom_sbf_resource_import_graph(const uint8_t *image, unsigned long image_bytes);
extern int doom_sbf_fill_pool(uint8_t *pool, const uint8_t wad_hash[32]);

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
    unsigned long *frame_out_len, unsigned long *arena_high_water_out)
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
    G_Ticker();
    RI2_PHASE(580);
    clipangle = xtoviewangle[0]; fixedcolormap = 0;
    R_RenderPlayerView(&players[displayplayer]);
    RI2_PHASE(590);
    gametic++;
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
