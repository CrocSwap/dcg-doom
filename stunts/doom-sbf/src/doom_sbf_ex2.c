/*
 * DOOM-EX2 native bridge.
 *
 * This file is compiled only in the host ctx-engine archive.  It imports the
 * real retained ST1 state into a freshly initialized context, derives/checks
 * the real MP1 pool, consumes one producer-emitted CI2 command through the
 * retained controlled-reentry path, and exports ST1/MP1/frame after an actual
 * G_Ticker and R_RenderPlayerView.  No pointer or native struct is emitted.
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

extern void R_ExecuteSetViewSize(void);
extern void doom_sbf_bind_arena(void *base, unsigned long bytes, unsigned long zone_bytes);

/* Keep the exact retained native account pool codec in this translation unit.
 * Its fill_pool helper is static by design; including the source here avoids
 * copying or weakening its field mapping while keeping the bridge independent
 * of the file-oriented manifest loader. */
#include "doom_account.c"

#define EX2_OK 0
#define EX2_REFUSE_STATE 232
#define EX2_REFUSE_POOL 233
#define EX2_REFUSE_RESOURCE 234
#define EX2_REFUSE_IMAGE 235
#define EX2_REFUSE_ROLE 236
#define EX2_REFUSE_CONTROLLER 237
#define EX2_REFUSE_OUTPUT 238

#define EX2_WAD_COUNT 5
#define EX2_WAD_ACCOUNT_BYTES 1048576u
#define EX2_WAD_HEADER_BYTES 32u
#define EX2_WAD_PAYLOAD_BYTES (EX2_WAD_ACCOUNT_BYTES - EX2_WAD_HEADER_BYTES)
#define EX2_POOL_BYTES 65536u
#define EX2_FRAME_BYTES (320u * 200u)
#define EX2_STATE_MAX 126976u
#define EX2_LOW_ARENA_BYTES (1024u * 1024u)

static const uint8_t ex2_pinned_wad[32] = {
    0x1d, 0x7d, 0x43, 0xbe, 0x50, 0x1e, 0x67, 0xd9,
    0x27, 0xe4, 0x15, 0xe0, 0xb8, 0xf3, 0xe2, 0x9c,
    0x3b, 0xf3, 0x30, 0x75, 0xe8, 0x59, 0x72, 0x18,
    0x16, 0xf6, 0x52, 0xa5, 0x26, 0xca, 0xc7, 0x71,
};

static void ex2_hex(const uint8_t digest[32], char out[65])
{
    static const char digits[] = "0123456789abcdef";
    unsigned i;
    for (i = 0; i < 32; i++) {
        out[2 * i] = digits[digest[i] >> 4];
        out[2 * i + 1] = digits[digest[i] & 15];
    }
    out[64] = 0;
}

/* Keep oracle hashing in the retained C codec.  The V2 host's SHA helper is
 * intentionally the comparison oracle for this seam; Rust's SHA is used for
 * envelope/fixture checks, not to silently redefine the host trajectory. */
void doom_engine_ex2_frame_hash(
    const uint8_t wad_hash[32], uint32_t tic_value,
    const uint8_t *frame, unsigned long frame_bytes, char out[65])
{
    static const char tag[] = "DOOM-FB1-FRAME";
    sha256_t s;
    uint8_t d[32], x[4];
    if (!wad_hash || !frame || frame_bytes != EX2_FRAME_BYTES || !out) return;
    shi(&s);
    shu(&s, tag, sizeof(tag) - 1);
    x[0] = x[1] = x[2] = x[3] = 0; shu(&s, x, 4);
    shu(&s, wad_hash, 32);
    x[0] = (uint8_t)tic_value; x[1] = (uint8_t)(tic_value >> 8);
    x[2] = (uint8_t)(tic_value >> 16); x[3] = (uint8_t)(tic_value >> 24); shu(&s, x, 4);
    /* The retained host full-frame record uses strip_id=0; -1 is only the
     * JSON presentation marker used for that record. */
    x[0] = x[1] = x[2] = x[3] = 0; shu(&s, x, 4);
    shu(&s, frame, frame_bytes);
    shf(&s, d);
    ex2_hex(d, out);
}

static uint32_t ex2_u32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8)
        | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int ex2_zero(const uint8_t *p, size_t n)
{
    size_t i;
    for (i = 0; i < n; i++) if (p[i]) return 0;
    return 1;
}

static int ex2_wad(
    const unsigned char *const accounts[EX2_WAD_COUNT],
    const unsigned long lens[EX2_WAD_COUNT],
    uint8_t **out, size_t *out_bytes, uint8_t hash[32])
{
    uint8_t *wad;
    size_t total = 0, i, length;
    uint32_t index;
    if (!accounts || !lens || !out || !out_bytes || !hash) return -1;
    wad = (uint8_t *)malloc(EX2_WAD_COUNT * EX2_WAD_PAYLOAD_BYTES);
    if (!wad) return -1;
    for (i = 0; i < EX2_WAD_COUNT; i++) {
        const uint8_t *a = accounts[i];
        if (!a || lens[i] != EX2_WAD_ACCOUNT_BYTES
            || memcmp(a, "DOOM-WC1", 8)
            || ex2_u32(a + 8) != 1
            || ex2_u32(a + 12) != EX2_WAD_HEADER_BYTES
            || ex2_u32(a + 16) != EX2_WAD_ACCOUNT_BYTES
            || ex2_u32(a + 20) != (uint32_t)i) {
            free(wad); return -1;
        }
        length = ex2_u32(a + 24);
        if (length > EX2_WAD_PAYLOAD_BYTES
            || !ex2_zero(a + EX2_WAD_HEADER_BYTES + length,
                         EX2_WAD_ACCOUNT_BYTES - EX2_WAD_HEADER_BYTES - length)) {
            free(wad); return -1;
        }
        memcpy(wad + total, a + EX2_WAD_HEADER_BYTES, length);
        total += length;
    }
    if (hash_bytes(wad, total, hash) || memcmp(hash, ex2_pinned_wad, 32)) {
        free(wad); return -1;
    }
    index = (uint32_t)total;
    if (!index || (size_t)index != total) {
        free(wad); return -1;
    }
    *out = wad;
    *out_bytes = total;
    return 0;
}

static int ex2_pool_shape(const uint8_t *pool, size_t bytes, const uint8_t wad_hash[32])
{
    if (!pool || bytes != EX2_POOL_BYTES || memcmp(pool, "DOOM-MP1", 8)
        || ex2_u32(pool + 8) != 1 || ex2_u32(pool + 12) != 4096
        || ex2_u32(pool + 16) != EX2_POOL_BYTES || ex2_u32(pool + 20) != 160
        || ex2_u32(pool + 24) != 384 || ex2_u32(pool + 28) > 384
        || !ex2_zero(pool + 68, 4096 - 68)
        || memcmp(pool + 36, wad_hash, 32)) return -1;
    return 0;
}

static int ex2_controller_shape(const uint8_t *controller, size_t bytes, uint32_t expected_tic)
{
    if (!controller || bytes != 64 || memcmp(controller, "DOOM-CI2", 8)
        || ex2_u32(controller + 8) != 1 || ex2_u32(controller + 12) != 28
        || ex2_u32(controller + 16) != 64 || ex2_u32(controller + 20) != expected_tic
        || !ex2_zero(controller + 28, 36)) return -1;
    return 0;
}

int doom_engine_ex2_entry(
    const unsigned char *const wad_accounts[EX2_WAD_COUNT],
    const unsigned long wad_lens[EX2_WAD_COUNT],
    const unsigned char *state, unsigned long state_len,
    const unsigned char *mirror, unsigned long mirror_len,
    const unsigned char *controller, unsigned long controller_len,
    unsigned char *state_out, unsigned long state_cap, unsigned long *state_out_len,
    unsigned char *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned char *frame_out, unsigned long frame_cap, unsigned long *frame_out_len)
{
    uint8_t *wad = 0, *encoded = 0, *derived = 0, *low_arena = 0;
    uint8_t wad_hash[32];
    size_t wad_bytes = 0, encoded_bytes = 0;
    doom_account_t account;
    int lump, demo_len;
    uint8_t *demo;
    uint32_t input_tic;

    if (!state || state_len < 64 || state_len > EX2_STATE_MAX
        || !mirror || mirror_len != EX2_POOL_BYTES || !controller
        || controller_len != 64 || !state_out || !mirror_out || !frame_out
        || !state_out_len || !mirror_out_len || !frame_out_len
        || state_cap < EX2_STATE_MAX || mirror_cap < EX2_POOL_BYTES
        || frame_cap < EX2_FRAME_BYTES) {
        return EX2_REFUSE_RESOURCE;
    }
    if (memcmp(state, "DOOM-ST1", 8) || ex2_u32(state + 8) != 0
        || ex2_u32(state + 12) != 64 || ex2_u32(state + 16) != state_len)
        return EX2_REFUSE_STATE;
    input_tic = ex2_u32(state + 20);
    if (ex2_pool_shape(mirror, mirror_len, (const uint8_t *)(mirror + 36)))
        return EX2_REFUSE_POOL;
    if (ex2_controller_shape(controller, controller_len, input_tic))
        return EX2_REFUSE_CONTROLLER;
    if (ex2_wad(wad_accounts, wad_lens, &wad, &wad_bytes, wad_hash)) {
        return EX2_REFUSE_RESOURCE;
    }
    if (memcmp(mirror + 36, wad_hash, 32)) {
        free(wad); return EX2_REFUSE_RESOURCE;
    }
    low_arena = (uint8_t *)calloc(1, EX2_LOW_ARENA_BYTES);
    if (!low_arena) {
        free(wad); return EX2_REFUSE_RESOURCE;
    }

    /* Every invocation starts with a new generated context and fresh engine
     * setup.  This is the actual import boundary, not a structural probe. */
    doom_ctx_init();
    doom_sbf_bind_arena(low_arena, EX2_LOW_ARENA_BYTES, DOOM_ZONE_BYTES);
    gamemode = shareware;
    gamemission = doom;
    language = english;
    DOOM_CTX->deathmatch = 0;
    DOOM_CTX->consoleplayer = 0;
    displayplayer = 0;
    myargc = 0;
    myargv = 0;
    Z_Init();
    W_InitAccountImage(wad, (int)wad_bytes);
    V_Init();
    R_Init();
    R_ExecuteSetViewSize();
    P_Init();
    lump = W_GetNumForName("DEMO1");
    demo_len = W_LumpLength(lump);
    if (demo_len < 17) { free(low_arena); free(wad); return EX2_REFUSE_RESOURCE; }
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    defdemoname = "DEMO1";
    demobuffer = demo;
    demo_p = demo;
    G_DoPlayDemo();
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    if (DoomStateDecode(state, (size_t)state_len)) {
        free(low_arena); free(wad); return EX2_REFUSE_STATE;
    }
    if (ex2_controller_shape(controller, controller_len, input_tic)
        || DoomControllerArmV2(controller + 24)) {
        free(low_arena); free(wad); return EX2_REFUSE_CONTROLLER;
    }

    memset(&account, 0, sizeof(account));
    account.wad = wad;
    account.wad_bytes = wad_bytes;
    memcpy(account.wad_hash, wad_hash, sizeof(account.wad_hash));
    derived = (uint8_t *)malloc(EX2_POOL_BYTES);
    if (!derived || fill_pool(derived, &account) || memcmp(derived, mirror, EX2_POOL_BYTES)) {
        free(derived); free(low_arena); free(wad); return EX2_REFUSE_POOL;
    }
    if (DoomControllerValidateCapturedDemoCursor(demo_p)) {
        free(derived); free(low_arena); free(wad); return EX2_REFUSE_CONTROLLER;
    }
    G_Ticker();
    R_RenderPlayerView(&players[displayplayer]);
    gametic++;
    if (DoomStateEncode(&encoded, &encoded_bytes)
        || encoded_bytes > EX2_STATE_MAX || encoded_bytes > state_cap
        || encoded_bytes < 64 || !screens[0]) {
        free(encoded); free(derived); free(low_arena); free(wad); return EX2_REFUSE_OUTPUT;
    }
    if (fill_pool(derived, &account)) {
        free(encoded); free(derived); free(low_arena); free(wad); return EX2_REFUSE_POOL;
    }
    memcpy(state_out, encoded, encoded_bytes);
    memcpy(mirror_out, derived, EX2_POOL_BYTES);
    memcpy(frame_out, screens[0], EX2_FRAME_BYTES);
    *state_out_len = encoded_bytes;
    *mirror_out_len = EX2_POOL_BYTES;
    *frame_out_len = EX2_FRAME_BYTES;
    DoomStateFree(encoded);
    free(derived);
    free(low_arena);
    free(wad);
    return EX2_OK;
}
