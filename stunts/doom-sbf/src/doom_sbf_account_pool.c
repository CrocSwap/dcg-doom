/*
 * Doom-local SBF mirror codec.
 *
 * The host account adapter's fill_pool() is intentionally file-oriented and
 * lives in doom_account.c.  The SBF EX2 entry needs only its pure derivation
 * from the live engine thinker graph, so this copy keeps the same MP1 field
 * map without importing the host manifest/filesystem path.  No static
 * writable state is used.
 */

#include <stdint.h>
#include <string.h>

#include "doom_ctx.h"
#include "doomdef.h"
#include "doomstat.h"
#include "p_mobj.h"
#include "p_local.h"
#include "info.h"

#define DOOM_SBF_POOL_BYTES 65536u
#define DOOM_SBF_POOL_HEADER_BYTES 4096u
#define DOOM_SBF_POOL_SLOT_BYTES 160u
#define DOOM_SBF_POOL_SLOTS 384u

static void sbf_p32(uint8_t *p, uint32_t x)
{
    p[0] = (uint8_t)x;
    p[1] = (uint8_t)(x >> 8);
    p[2] = (uint8_t)(x >> 16);
    p[3] = (uint8_t)(x >> 24);
}

static void sbf_p16(uint8_t *p, uint16_t x)
{
    p[0] = (uint8_t)x;
    p[1] = (uint8_t)(x >> 8);
}

static int sbf_state_of(state_t *state, int32_t *out)
{
    uintptr_t current;
    uintptr_t base;
    if (!state) return -1;
    current = (uintptr_t)state;
    base = (uintptr_t)states;
    if (current < base || current - base >= sizeof(states)
        || (current - base) % sizeof(*states)) return -1;
    *out = (int32_t)((current - base) / sizeof(*states));
    return *out >= 0 && *out < NUMSTATES ? 0 : -1;
}

static int sbf_player_of(player_t *player, int32_t *out)
{
    uintptr_t current;
    uintptr_t base;
    if (!player) {
        *out = 0;
        return 0;
    }
    current = (uintptr_t)player;
    base = (uintptr_t)players;
    if (current < base || current - base >= sizeof(players)
        || (current - base) % sizeof(*players)) return -1;
    *out = (int32_t)((current - base) / sizeof(*players)) + 1;
    return 0;
}

int doom_sbf_fill_pool(uint8_t *pool, const uint8_t wad_hash[32])
{
    mobj_t *objects[DOOM_SBF_POOL_SLOTS];
    thinker_t *thinker;
    unsigned count = 0;
    unsigned i;
    if (!pool || !wad_hash) return -1;
    memset(pool, 0, DOOM_SBF_POOL_BYTES);
    memcpy(pool, "DOOM-MP1", 8);
    sbf_p32(pool + 8, 1);
    sbf_p32(pool + 12, DOOM_SBF_POOL_HEADER_BYTES);
    sbf_p32(pool + 16, DOOM_SBF_POOL_BYTES);
    sbf_p32(pool + 20, DOOM_SBF_POOL_SLOT_BYTES);
    sbf_p32(pool + 24, DOOM_SBF_POOL_SLOTS);
    for (thinker = thinkercap.next; thinker != &thinkercap;
         thinker = thinker->next) {
        if (thinker->function.acp1 == (actionf_p1)P_MobjThinker) {
            if (count == DOOM_SBF_POOL_SLOTS) return -1;
            objects[count++] = (mobj_t *)thinker;
        }
    }
    sbf_p32(pool + 28, count);
    sbf_p32(pool + 32, count < DOOM_SBF_POOL_SLOTS ? count : 0xffffffffU);
    memcpy(pool + 36, wad_hash, 32);
    for (i = 0; i < count; i++) {
        sbf_p32(pool + DOOM_SBF_POOL_HEADER_BYTES + i * DOOM_SBF_POOL_SLOT_BYTES + 68,
            (uint32_t)objects[i]->validcount_m);
        objects[i]->validcount_m = (int32_t)(i + 1);
    }
    for (i = 0; i < count; i++) {
        mobj_t *object = objects[i];
        int32_t state_id;
        int32_t player_id;
        uint32_t target;
        uint32_t tracer;
        uint8_t *slot;
        if (object->type < 0 || object->type >= NUMMOBJTYPES
            || sbf_state_of(object->state, &state_id)
            || sbf_player_of(object->player, &player_id)
            || (object->target && ((target = (uint32_t)object->target->validcount_m) == 0 ||
                target > count || objects[target - 1] != object->target))
            || (object->tracer && ((tracer = (uint32_t)object->tracer->validcount_m) == 0 ||
                tracer > count || objects[tracer - 1] != object->tracer))) goto fail;
        if (!object->target) target = 0;
        if (!object->tracer) tracer = 0;
        slot = pool + DOOM_SBF_POOL_HEADER_BYTES + i * DOOM_SBF_POOL_SLOT_BYTES;
        sbf_p32(slot + 0, i + 1);
        sbf_p32(slot + 4, 1);
        sbf_p32(slot + 8, target);
        sbf_p32(slot + 12, tracer);
        sbf_p32(slot + 16, (uint32_t)object->x);
        sbf_p32(slot + 20, (uint32_t)object->y);
        sbf_p32(slot + 24, (uint32_t)object->z);
        sbf_p32(slot + 28, (uint32_t)object->angle);
        sbf_p32(slot + 32, (uint32_t)object->sprite);
        sbf_p32(slot + 36, (uint32_t)object->frame);
        sbf_p32(slot + 40, (uint32_t)object->floorz);
        sbf_p32(slot + 44, (uint32_t)object->ceilingz);
        sbf_p32(slot + 48, (uint32_t)object->radius);
        sbf_p32(slot + 52, (uint32_t)object->height);
        sbf_p32(slot + 56, (uint32_t)object->momx);
        sbf_p32(slot + 60, (uint32_t)object->momy);
        sbf_p32(slot + 64, (uint32_t)object->momz);
        sbf_p32(slot + 72, (uint32_t)object->type);
        sbf_p32(slot + 76, (uint32_t)state_id);
        sbf_p32(slot + 80, (uint32_t)object->tics);
        sbf_p32(slot + 84, (uint32_t)object->flags);
        sbf_p32(slot + 88, (uint32_t)object->health);
        sbf_p32(slot + 92, (uint32_t)object->movedir);
        sbf_p32(slot + 96, (uint32_t)object->movecount);
        sbf_p32(slot + 100, (uint32_t)object->reactiontime);
        sbf_p32(slot + 104, (uint32_t)object->threshold);
        sbf_p32(slot + 108, (uint32_t)player_id);
        sbf_p32(slot + 112, (uint32_t)object->lastlook);
        sbf_p16(slot + 116, (uint16_t)object->spawnpoint.x);
        sbf_p16(slot + 118, (uint16_t)object->spawnpoint.y);
        sbf_p16(slot + 120, (uint16_t)object->spawnpoint.angle);
        sbf_p16(slot + 122, (uint16_t)object->spawnpoint.type);
        sbf_p16(slot + 124, (uint16_t)object->spawnpoint.options);
    }
    for (i = 0; i < count; i++) {
        const uint8_t *saved = pool + DOOM_SBF_POOL_HEADER_BYTES + i * DOOM_SBF_POOL_SLOT_BYTES + 68;
        objects[i]->validcount_m = (int32_t)((uint32_t)saved[0] |
            ((uint32_t)saved[1] << 8) | ((uint32_t)saved[2] << 16) |
            ((uint32_t)saved[3] << 24));
    }
    return 0;
fail:
    for (i = 0; i < count; i++) {
        const uint8_t *saved = pool + DOOM_SBF_POOL_HEADER_BYTES + i * DOOM_SBF_POOL_SLOT_BYTES + 68;
        objects[i]->validcount_m = (int32_t)((uint32_t)saved[0] |
            ((uint32_t)saved[1] << 8) | ((uint32_t)saved[2] << 16) |
            ((uint32_t)saved[3] << 24));
    }
    return -1;
}
