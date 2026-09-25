/*
 * DOOM-RI2/2 initialized-resource capture and native consumer.
 *
 * RI1/E2R1 is frozen in doom_resource.c.  RI2 adds an explicit, additive
 * capture of the source-matched Z/W/V/R/view/P/demo initialization boundary.
 * It contains only named scalar/table records, symbolic callback IDs, and
 * region-relative pointer relocations.  Native addresses, C function
 * addresses, allocator headers, and unspecified memory are never serialized.
 */

#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "doom_ctx.h"
#include "doomdef.h"
#include "doomstat.h"
#include "g_game.h"
#include "p_setup.h"
#include "r_main.h"
#include "r_draw.h"
#include "r_things.h"
#include "v_video.h"
#include "w_wad.h"
#include "z_zone.h"
#include "doom_state.h"
#include "doom_controller.h"

#ifdef DOOM_SBF_PHASE_PROFILE
extern void doom_sbf_phase_marker(unsigned long phase);
#define R2_PHASE(phase) doom_sbf_phase_marker((unsigned long)(phase))
#else
#define R2_PHASE(phase) ((void)0)
#endif

extern int doom_engine_ex2_make_wad(
    const unsigned char *const accounts[5], const unsigned long lens[5],
    uint8_t **out, size_t *out_bytes, uint8_t hash[32]);
extern int doom_engine_ex2_fill_mirror(
    uint8_t *out, const uint8_t *wad, size_t wad_bytes, const uint8_t hash[32]);
extern int doom_sbf_bind_arena(void *base, unsigned long bytes, unsigned long zone_bytes);
extern int doom_resource_ri1_setup(const uint8_t *wad, size_t wad_bytes, uint8_t **carrier);
extern int doom_resource_ri1_encode_graph(uint8_t **out, size_t *out_bytes, uintptr_t *anchor);
extern int doom_resource_ri1_import_graph(const uint8_t *image, size_t image_bytes,
                                          uintptr_t *anchor);
extern int doom_resource_ri1_validate_graph(const uint8_t *image, size_t image_bytes);

/* The protocol's pinned start level, recorded once for this file.  The
   producer writes the name into the image header and every validator (the
   on-chain preflight included) compares it, so a graph captured from a
   different map cannot be paired with a start state, and a capture from this
   map cannot be paired with a different graph.  This block is inside the
   slice build.rs hands the SBF consumer, so the on-chain validator pins the
   same level as the native producer.  The two compile-time checks below keep
   the name from drifting from the numbers and from being the wrong length. */
#define RI2_LEVEL_EPISODE 1
#define RI2_LEVEL_MAP 1
#define RI2_LEVEL_NAME "E1M1\0\0\0\0"
/* The two halves are separate so the front end folds both to plain constants:
   a typedef whose size expression indexes a string literal is diagnosed as a
   folded variable-length array, which is a warning in a build that has to stay
   clean. */
typedef char r2_level_name_check_bytes[sizeof(RI2_LEVEL_NAME) == 9 ? 1 : -1];
enum {
    RI2_LEVEL_NAME_SPELLS_THE_LEVEL = (RI2_LEVEL_NAME[0] == 'E' &&
        RI2_LEVEL_NAME[1] == '0' + RI2_LEVEL_EPISODE && RI2_LEVEL_NAME[2] == 'M' &&
        RI2_LEVEL_NAME[3] == '0' + RI2_LEVEL_MAP && RI2_LEVEL_NAME[4] == 0),
};
typedef char r2_level_name_check_spelling[RI2_LEVEL_NAME_SPELLS_THE_LEVEL ? 1 : -1];

#define RI2_MAGIC "DOOM-RI2"
#define RI2_VERSION_V1 1u
/* Version 2 is the retired E1M5 image: the shareware DEMO1 start state, the
   level every retained context was provisioned at.  It is named here so the
   one-way step to the pinned new-game start image is visible in the source,
   and it is no longer accepted: `r2_sections` takes only the version the
   protocol pins, so an E1M5 image is refused on chain. */
#define RI2_VERSION_E1M5 2u
/* Version 4 is the pinned E1M1 new-game start image.  It is 4 and not 3
   because 3 is the number the diagnostic-only clipped-render measurement
   request (DOOM-RC3, resource_sbf::RI3_VERSION) already uses, and the Rust
   entry point dispatches on the version field alone: an image version of 3
   would make the two requests indistinguishable there. */
#define RI2_VERSION_E1M1 4u
#define RI2_VERSION RI2_VERSION_E1M1
#define RI2_HEADER_BYTES 256u
#define RI2_SCHEMA 2u
#define RI2_SECTION_COUNT 5u
#define RI2_SECTION_DIR_OFFSET RI2_HEADER_BYTES
#define RI2_MAX_BYTES (16u * 1024u * 1024u)
#define RI2_FRAME_BYTES (320u * 200u)
#define RI2_STATE_MAX 126976u
#define RI2_MIRROR_BYTES 65536u
#define RI2_ENGINE_ID_BYTES 32u
#define RI2_ZONE_BYTES 3145728u
#define RI2_NULL 0xffffffffu
#define R2_DISTMAP 2
#define R2_SBARHEIGHT 32

/* RI1 graph section IDs.  The graph payload is validated by the frozen RI1
 * codec; RI2 does not duplicate its record definitions. */
enum { R2_GRAPH_VERTEXES = 0, R2_GRAPH_SECTORS, R2_GRAPH_SIDES,
       R2_GRAPH_LINES, R2_GRAPH_SEGS, R2_GRAPH_SUBSECTORS, R2_GRAPH_NODES,
       R2_GRAPH_BLOCKMAP, R2_GRAPH_REJECT, R2_GRAPH_SECTOR_LINES };

enum { R2_GRAPH = 0, R2_SCALARS = 1, R2_TABLES = 2,
       R2_RELOCS = 3, R2_FUNCTIONS = 4 };

enum {
    R2_SC_SETBLOCKS = 1, R2_SC_SETDETAIL, R2_SC_SCREENBLOCKS,
    R2_SC_DETAILLEVEL, R2_SC_VIEWWIDTH, R2_SC_VIEWHEIGHT,
    R2_SC_SCALEDVIEWWIDTH, R2_SC_CENTERX, R2_SC_CENTERY,
    R2_SC_CENTERXFRAC, R2_SC_CENTERYFRAC, R2_SC_PROJECTION,
    R2_SC_DETAILSHIFT, R2_SC_GAMEEPISODE, R2_SC_GAMEMAP,
    R2_SC_GAMESKILL, R2_SC_GAMESTATE, R2_SC_GAMETIC,
    R2_SC_GAMEACTION, R2_SC_GAMEMODE, R2_SC_GAMEMISSION,
    R2_SC_LANGUAGE, R2_SC_CONSOLEPLAYER, R2_SC_DISPLAYPLAYER,
    R2_SC_FRAMECOUNT, R2_SC_LEVELTIME, R2_SC_NUMTEXTURES,
    R2_SC_NUMFLATS, R2_SC_NUMSPRITELUMPS, R2_SC_NUMSPRITES,
    R2_SC_NUMSWITCHES, R2_SC_FIRSTFLAT, R2_SC_LASTFLAT,
    R2_SC_FIRSTSPRITELUMP, R2_SC_LASTSPRITELUMP,
    R2_SC_PRECACHE, R2_SC_DEMOPLAYBACK, R2_SC_ANIMCOUNT
};

enum {
    R2_T_VIEWANGLETOX = 1, R2_T_XTOVIEWANGLE, R2_T_YSLOPE,
    R2_T_DISTSCALE, R2_T_SCREENHEIGHTARRAY, R2_T_COLUMNOFS,
    R2_T_SWITCHLIST, R2_T_ANIMS, R2_T_TRANSLATIONS,
    R2_T_TEXTUREWIDTHMASK, R2_T_TEXTUREHEIGHT, R2_T_TEXTURECOMPOSITESIZE,
    R2_T_TEXTURETRANSLATION, R2_T_FLATTRANSLATION,
    R2_T_SPRITEWIDTH, R2_T_SPRITEOFFSET, R2_T_SPRITETOPOFFSET,
    R2_T_SPRTEMP,
    /* v2 records are canonical payloads.  They contain no host pointers. */
    R2_T_TEXTUREDEFS, R2_T_TEXTURECOLUMNLUMP, R2_T_TEXTURECOLUMNOFS,
    R2_T_TEXTURECOMPOSITEBYTES, R2_T_SPRITEDEFS, R2_T_COLORMAPS,
    R2_T_TRANSLATIONTABLEBYTES, R2_T_SCALIGHTOFFSETS,
    R2_T_SCALIGHTFIXEDOFFSETS, R2_T_ZLIGHTOFFSETS, R2_T_LIGHTSELECTORS
};

enum {
    R2_PTR_VERTEXES = 1, R2_PTR_SECTORS, R2_PTR_SIDES, R2_PTR_LINES,
    R2_PTR_SEGS, R2_PTR_SUBSECTORS, R2_PTR_NODES, R2_PTR_BLOCKMAPLUMP,
    R2_PTR_BLOCKMAP, R2_PTR_REJECT, R2_PTR_BLOCKLINKS,
    R2_PTR_SCREENS, R2_PTR_YLOOKUP, R2_PTR_TEXTURES,
    R2_PTR_TEXTURECOLUMNLUMP, R2_PTR_TEXTURECOLUMNOFS,
    R2_PTR_TEXTURECOMPOSITE, R2_PTR_TEXTURECOMPOSITESIZE,
    R2_PTR_TEXTUREHEIGHT, R2_PTR_TEXTURETRANSLATION,
    R2_PTR_TEXTUREWIDTHMASK, R2_PTR_FLATTRANSLATION,
    R2_PTR_SPRITEWIDTH, R2_PTR_SPRITEOFFSET, R2_PTR_SPRITETOPOFFSET,
    R2_PTR_SPRITES, R2_PTR_COLORMAPS, R2_PTR_TRANSLATIONTABLES,
    R2_PTR_SCALIGHT, R2_PTR_SCALIGHTFIXED, R2_PTR_ZLIGHT,
    R2_PTR_SPRITELIGHTS, R2_PTR_WALLLIGHTS, R2_PTR_PLANEZLIGHT,
    R2_PTR_DEMOBUFFER, R2_PTR_DEMOP, R2_PTR_DEMOEND
};

enum { R2_REGION_GRAPH = 1, R2_REGION_FRAME = 2,
       R2_REGION_WAD = 3, R2_REGION_ZONE = 4, R2_REGION_RUNTIME = 5 };

enum { R2_FN_R_DRAW_COLUMN = 1, R2_FN_R_DRAW_COLUMN_LOW,
       R2_FN_R_DRAW_FUZZ_COLUMN, R2_FN_R_DRAW_TRANSLATED_COLUMN,
       R2_FN_R_DRAW_SPAN, R2_FN_R_DRAW_SPAN_LOW };

typedef struct { uint32_t offset, bytes, count, record_bytes; } r2_section_t;
typedef struct { uint8_t *bytes; size_t cap, used; uint32_t count; } r2_builder_t;

static const uint8_t r2_engine_id[RI2_ENGINE_ID_BYTES] = {
    'D','O','O','M','-','R','I','1','/','l','i','n','u','x','d','o','o','m',
    '-','a','7','7','d','f','b','9','6','/','0','0','1','3'
};
static const uint8_t r2_wad_hash[32] = {
    0x1d,0x7d,0x43,0xbe,0x50,0x1e,0x67,0xd9,0x27,0xe4,0x15,0xe0,0xb8,0xf3,0xe2,0x9c,
    0x3b,0xf3,0x30,0x75,0xe8,0x59,0x72,0x18,0x16,0xf6,0x52,0xa5,0x26,0xca,0xc7,0x71
};

static uint32_t r2_u32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
        ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static void r2_put32(uint8_t *p, uint32_t x)
{
    p[0] = (uint8_t)x; p[1] = (uint8_t)(x >> 8);
    p[2] = (uint8_t)(x >> 16); p[3] = (uint8_t)(x >> 24);
}

static int r2_push(r2_builder_t *b, const uint8_t *p, size_t n)
{
    if (!b || !p || n > b->cap - b->used) return -1;
    memcpy(b->bytes + b->used, p, n); b->used += n; return 0;
}

static int r2_scalar(r2_builder_t *b, uint32_t id, uint32_t value)
{
    uint8_t r[12]; r2_put32(r, id); r2_put32(r + 4, 4); r2_put32(r + 8, value);
    if (r2_push(b, r, sizeof(r))) return -1; b->count++; return 0;
}

static int r2_table(r2_builder_t *b, uint32_t id, const void *data, uint32_t bytes)
{
    uint8_t h[8];
    if (!data || (size_t)bytes > b->cap - b->used - 8u) return -1;
    r2_put32(h, id); r2_put32(h + 4, bytes);
    if (r2_push(b, h, sizeof(h)) || r2_push(b, (const uint8_t *)data, bytes)) return -1;
    b->count++; return 0;
}

static int r2_reloc(r2_builder_t *b, uint32_t field, uint32_t index,
                    uint32_t region, uint32_t offset, uint32_t bytes)
{
    uint8_t r[24];
    r2_put32(r, field); r2_put32(r + 4, index); r2_put32(r + 8, region);
    r2_put32(r + 12, offset); r2_put32(r + 16, bytes); r2_put32(r + 20, 0);
    if (r2_push(b, r, sizeof(r))) return -1; b->count++; return 0;
}

static int r2_function(r2_builder_t *b, uint32_t field, uint32_t id)
{
    uint8_t r[16]; r2_put32(r, field); r2_put32(r + 4, 0);
    r2_put32(r + 8, id); r2_put32(r + 12, 0);
    if (r2_push(b, r, sizeof(r))) return -1; b->count++; return 0;
}

static void r2_write_section(uint8_t *image, unsigned id, const r2_section_t *s)
{
    uint8_t *p = image + RI2_SECTION_DIR_OFFSET + id * 16u;
    r2_put32(p, s->offset); r2_put32(p + 4, s->bytes);
    r2_put32(p + 8, s->count); r2_put32(p + 12, s->record_bytes);
}

static int r2_sections_version(const uint8_t *image, size_t bytes,
                               r2_section_t out[5], uint32_t version)
{
    uint32_t previous, directory_end, end; unsigned i;
    if (!image || bytes < RI2_HEADER_BYTES + RI2_SECTION_COUNT * 16u ||
        memcmp(image, RI2_MAGIC, 8) || r2_u32(image + 8) != version ||
        r2_u32(image + 12) != RI2_HEADER_BYTES || r2_u32(image + 16) != bytes ||
        r2_u32(image + 20) != RI2_SCHEMA || r2_u32(image + 24) != 1 ||
        r2_u32(image + 28) != 5 || r2_u32(image + 32) != RI2_SECTION_COUNT ||
        r2_u32(image + 36) != RI2_SECTION_DIR_OFFSET ||
        memcmp(image + 40, r2_engine_id, RI2_ENGINE_ID_BYTES) ||
        memcmp(image + 72, r2_wad_hash, 32) || memcmp(image + 104, RI2_LEVEL_NAME, 8)) return -1;
    directory_end = RI2_SECTION_DIR_OFFSET + RI2_SECTION_COUNT * 16u;
    previous = directory_end;
    for (i = 0; i < RI2_SECTION_COUNT; i++) {
        const uint8_t *p = image + RI2_SECTION_DIR_OFFSET + i * 16u;
        out[i].offset = r2_u32(p); out[i].bytes = r2_u32(p + 4);
        out[i].count = r2_u32(p + 8); out[i].record_bytes = r2_u32(p + 12);
        end = out[i].offset + out[i].bytes;
        if ((out[i].offset & 15u) || out[i].offset < directory_end ||
            end < out[i].offset || end > bytes || out[i].offset < previous) return -1;
        previous = end;
    }
    if (out[R2_GRAPH].bytes != r2_u32(image + 128) ||
        out[R2_RELOCS].bytes != out[R2_RELOCS].count * 24u ||
        out[R2_FUNCTIONS].bytes != out[R2_FUNCTIONS].count * 16u) return -1;
    return 0;
}

static int r2_sections(const uint8_t *image, size_t bytes, r2_section_t out[5])
{
    return r2_sections_version(image, bytes, out, RI2_VERSION);
}

static int r2_sections_v1(const uint8_t *image, size_t bytes, r2_section_t out[5])
{
    return r2_sections_version(image, bytes, out, RI2_VERSION_V1);
}

static int r2_walk(const uint8_t *bytes, size_t length, uint32_t expected,
                   int fixed, int (*visit)(uint32_t, uint32_t, const uint8_t *, void *),
                   void *opaque)
{
    size_t at = 0; uint32_t count = 0, id, n; int visit_rc;
    while (at < length) {
        if (length - at < 8) return -1;
        id = r2_u32(bytes + at); n = r2_u32(bytes + at + 4); at += 8;
        if ((size_t)n > length - at || (fixed && n != 4) || !visit) return -1;
        visit_rc = visit(id, n, bytes + at, opaque);
        if (visit_rc) return visit_rc;
        at += n; count++;
    }
    return count == expected ? 0 : -1;
}

static int r2_function_id(void (*fn)(void))
{
    if (fn == R_DrawColumn) return R2_FN_R_DRAW_COLUMN;
    if (fn == R_DrawColumnLow) return R2_FN_R_DRAW_COLUMN_LOW;
    if (fn == R_DrawFuzzColumn) return R2_FN_R_DRAW_FUZZ_COLUMN;
    if (fn == R_DrawTranslatedColumn) return R2_FN_R_DRAW_TRANSLATED_COLUMN;
    if (fn == R_DrawSpan) return R2_FN_R_DRAW_SPAN;
    if (fn == R_DrawSpanLow) return R2_FN_R_DRAW_SPAN_LOW;
    return 0;
}

static void (*r2_function_value(uint32_t id))(void)
{
    switch (id) {
    case R2_FN_R_DRAW_COLUMN: return R_DrawColumn;
    case R2_FN_R_DRAW_COLUMN_LOW: return R_DrawColumnLow;
    case R2_FN_R_DRAW_FUZZ_COLUMN: return R_DrawFuzzColumn;
    case R2_FN_R_DRAW_TRANSLATED_COLUMN: return R_DrawTranslatedColumn;
    case R2_FN_R_DRAW_SPAN: return R_DrawSpan;
    case R2_FN_R_DRAW_SPAN_LOW: return R_DrawSpanLow;
    default: return 0;
    }
}

#define R2_SCALAR(field, id) if (r2_scalar(scalars, (id), (uint32_t)(field))) return -1
static int r2_capture_scalars(r2_builder_t *scalars)
{
    R2_SCALAR(setblocks, R2_SC_SETBLOCKS); R2_SCALAR(setdetail, R2_SC_SETDETAIL);
    R2_SCALAR(screenblocks, R2_SC_SCREENBLOCKS); R2_SCALAR(detailLevel, R2_SC_DETAILLEVEL);
    R2_SCALAR(viewwidth, R2_SC_VIEWWIDTH); R2_SCALAR(viewheight, R2_SC_VIEWHEIGHT);
    R2_SCALAR(scaledviewwidth, R2_SC_SCALEDVIEWWIDTH); R2_SCALAR(centerx, R2_SC_CENTERX);
    R2_SCALAR(centery, R2_SC_CENTERY); R2_SCALAR(centerxfrac, R2_SC_CENTERXFRAC);
    R2_SCALAR(centeryfrac, R2_SC_CENTERYFRAC); R2_SCALAR(projection, R2_SC_PROJECTION);
    R2_SCALAR(detailshift, R2_SC_DETAILSHIFT); R2_SCALAR(gameepisode, R2_SC_GAMEEPISODE);
    R2_SCALAR(gamemap, R2_SC_GAMEMAP); R2_SCALAR(gameskill, R2_SC_GAMESKILL);
    R2_SCALAR(gamestate, R2_SC_GAMESTATE); R2_SCALAR(gametic, R2_SC_GAMETIC);
    R2_SCALAR(gameaction, R2_SC_GAMEACTION); R2_SCALAR(gamemode, R2_SC_GAMEMODE);
    R2_SCALAR(gamemission, R2_SC_GAMEMISSION); R2_SCALAR(language, R2_SC_LANGUAGE);
    R2_SCALAR(DOOM_CTX->consoleplayer, R2_SC_CONSOLEPLAYER); R2_SCALAR(displayplayer, R2_SC_DISPLAYPLAYER);
    R2_SCALAR(framecount, R2_SC_FRAMECOUNT); R2_SCALAR(leveltime, R2_SC_LEVELTIME);
    R2_SCALAR(numtextures, R2_SC_NUMTEXTURES); R2_SCALAR(numflats, R2_SC_NUMFLATS);
    R2_SCALAR(numspritelumps, R2_SC_NUMSPRITELUMPS); R2_SCALAR(numsprites, R2_SC_NUMSPRITES);
    R2_SCALAR(numswitches, R2_SC_NUMSWITCHES); R2_SCALAR(firstflat, R2_SC_FIRSTFLAT);
    R2_SCALAR(lastflat, R2_SC_LASTFLAT); R2_SCALAR(firstspritelump, R2_SC_FIRSTSPRITELUMP);
    R2_SCALAR(lastspritelump, R2_SC_LASTSPRITELUMP); R2_SCALAR(precache, R2_SC_PRECACHE);
    R2_SCALAR(demoplayback, R2_SC_DEMOPLAYBACK); R2_SCALAR(lastanim - anims, R2_SC_ANIMCOUNT);
    return 0;
}
#undef R2_SCALAR

static int r2_capture_tables(r2_builder_t *tables)
{
    if (r2_table(tables, R2_T_VIEWANGLETOX, viewangletox, sizeof(viewangletox)) ||
        r2_table(tables, R2_T_XTOVIEWANGLE, xtoviewangle, sizeof(xtoviewangle)) ||
        r2_table(tables, R2_T_YSLOPE, yslope, sizeof(yslope)) ||
        r2_table(tables, R2_T_DISTSCALE, distscale, sizeof(distscale)) ||
        r2_table(tables, R2_T_SCREENHEIGHTARRAY, screenheightarray, sizeof(screenheightarray)) ||
        r2_table(tables, R2_T_COLUMNOFS, columnofs, sizeof(columnofs)) ||
        r2_table(tables, R2_T_SWITCHLIST, switchlist, sizeof(switchlist)) ||
        r2_table(tables, R2_T_ANIMS, anims, sizeof(anims)) ||
        r2_table(tables, R2_T_TRANSLATIONS, translations, sizeof(translations)) ||
        r2_table(tables, R2_T_SPRTEMP, sprtemp, sizeof(sprtemp))) return -1;
    if (numtextures < 0 || numtextures > 4096 || numflats < 0 || numflats > 4096 ||
        numspritelumps < 0 || numspritelumps > 4096 || !texturewidthmask || !textureheight ||
        !texturecompositesize || !texturetranslation || !flattranslation || !spritewidth ||
        !spriteoffset || !spritetopoffset) return -1;
    if (r2_table(tables, R2_T_TEXTUREWIDTHMASK, texturewidthmask, (uint32_t)numtextures * 4u) ||
        r2_table(tables, R2_T_TEXTUREHEIGHT, textureheight, (uint32_t)numtextures * 4u) ||
        r2_table(tables, R2_T_TEXTURECOMPOSITESIZE, texturecompositesize, (uint32_t)numtextures * 4u) ||
        r2_table(tables, R2_T_TEXTURETRANSLATION, texturetranslation, (uint32_t)(numtextures + 1) * 4u) ||
        r2_table(tables, R2_T_FLATTRANSLATION, flattranslation, (uint32_t)(numflats + 1) * 4u) ||
        r2_table(tables, R2_T_SPRITEWIDTH, spritewidth, (uint32_t)numspritelumps * 4u) ||
        r2_table(tables, R2_T_SPRITEOFFSET, spriteoffset, (uint32_t)numspritelumps * 4u) ||
        r2_table(tables, R2_T_SPRITETOPOFFSET, spritetopoffset, (uint32_t)numspritelumps * 4u)) return -1;
    return 0;
}

/* v2 canonical payload helpers.  These deliberately describe object fields
 * instead of copying the engine's pointer-bearing structs. */
static int r2_blob_u32(uint8_t *blob, size_t cap, size_t *at, uint32_t value)
{
    if (!blob || !at || *at > cap - 4u) return -1;
    r2_put32(blob + *at, value); *at += 4u; return 0;
}

static int r2_blob_bytes(uint8_t *blob, size_t cap, size_t *at,
                         const void *data, size_t bytes)
{
    if (!blob || !at || (!data && bytes) || bytes > cap - *at) return -1;
    if (bytes) memcpy(blob + *at, data, bytes); *at += bytes; return 0;
}

static int r2_capture_texture_defs(r2_builder_t *tables)
{
    size_t cap = 2u * 1024u * 1024u, at = 0, i, j; uint8_t *blob;
    if (numtextures < 0 || numtextures > 4096) return -1;
    blob = (uint8_t *)malloc(cap); if (!blob) return -1;
    if (r2_blob_u32(blob, cap, &at, (uint32_t)numtextures)) goto fail;
    for (i = 0; i < (size_t)numtextures; i++) {
        texture_t *texture = textures ? textures[i] : 0;
        if (!texture || texture->width <= 0 || texture->width > 4096 ||
            texture->height <= 0 || texture->height > 4096 ||
            texture->patchcount < 0 || texture->patchcount > 4096) goto fail;
        if (r2_blob_bytes(blob, cap, &at, texture->name, 8) ||
            r2_blob_u32(blob, cap, &at, (uint32_t)(int32_t)texture->width) ||
            r2_blob_u32(blob, cap, &at, (uint32_t)(int32_t)texture->height) ||
            r2_blob_u32(blob, cap, &at, (uint32_t)(int32_t)texture->patchcount)) goto fail;
        for (j = 0; j < (size_t)texture->patchcount; j++) {
            if (r2_blob_u32(blob, cap, &at, (uint32_t)(int32_t)texture->patches[j].originx) ||
                r2_blob_u32(blob, cap, &at, (uint32_t)(int32_t)texture->patches[j].originy) ||
                r2_blob_u32(blob, cap, &at, (uint32_t)(int32_t)texture->patches[j].patch)) goto fail;
        }
    }
    if (r2_table(tables, R2_T_TEXTUREDEFS, blob, (uint32_t)at)) goto fail;
    free(blob); return 0;
fail:
    free(blob); return -1;
}

static int r2_capture_texture_columns(r2_builder_t *tables, uint32_t id, int offsets)
{
    size_t cap = 2u * 1024u * 1024u, at = 0, i; uint8_t *blob;
    if (numtextures < 0 || numtextures > 4096 || !textures) return -1;
    blob = (uint8_t *)malloc(cap); if (!blob) return -1;
    if (r2_blob_u32(blob, cap, &at, (uint32_t)numtextures)) goto fail;
    for (i = 0; i < (size_t)numtextures; i++) {
        size_t width, bytes;
        const void *data = offsets ? (const void *)texturecolumnofs[i] : (const void *)texturecolumnlump[i];
        if (!textures[i]) goto fail;
        width = (size_t)textures[i]->width; bytes = width * sizeof(short);
        if (width == 0 || width > 4096 || !data || bytes > 65536u ||
            r2_blob_u32(blob, cap, &at, (uint32_t)width) ||
            r2_blob_u32(blob, cap, &at, (uint32_t)bytes) ||
            r2_blob_bytes(blob, cap, &at, data, bytes)) goto fail;
    }
    if (r2_table(tables, id, blob, (uint32_t)at)) goto fail;
    free(blob); return 0;
fail:
    free(blob); return -1;
}

static int r2_capture_composites(r2_builder_t *tables)
{
    size_t cap = 4u * 1024u * 1024u, at = 0, i; uint8_t *blob;
    if (numtextures < 0 || numtextures > 4096 || !texturecompositesize) return -1;
    blob = (uint8_t *)malloc(cap); if (!blob) return -1;
    if (r2_blob_u32(blob, cap, &at, (uint32_t)numtextures)) goto fail;
    for (i = 0; i < (size_t)numtextures; i++) {
        uint32_t bytes = (texturecomposite[i] && texturecompositesize[i] > 0) ? (uint32_t)texturecompositesize[i] : 0u;
        if (bytes > 4u * 1024u * 1024u ||
            r2_blob_u32(blob, cap, &at, bytes) ||
            r2_blob_bytes(blob, cap, &at, texturecomposite[i], bytes)) goto fail;
    }
    if (r2_table(tables, R2_T_TEXTURECOMPOSITEBYTES, blob, (uint32_t)at)) goto fail;
    free(blob); return 0;
fail:
    free(blob); return -1;
}

static int r2_capture_sprite_defs(r2_builder_t *tables)
{
    size_t cap = 2u * 1024u * 1024u, at = 0, i, j, k; uint8_t *blob;
    if (numsprites < 0 || numsprites > 4096) return -1;
    blob = (uint8_t *)malloc(cap); if (!blob) return -1;
    if (r2_blob_u32(blob, cap, &at, (uint32_t)numsprites)) goto fail;
    for (i = 0; i < (size_t)numsprites; i++) {
        spritedef_t *sprite = sprites ? &sprites[i] : 0;
        if (!sprite || sprite->numframes < 0 || sprite->numframes > 64 ||
            (sprite->numframes && !sprite->spriteframes) ||
            r2_blob_u32(blob, cap, &at, (uint32_t)sprite->numframes)) goto fail;
        for (j = 0; j < (size_t)sprite->numframes; j++) {
            spriteframe_t *frame = &sprite->spriteframes[j];
            if (r2_blob_u32(blob, cap, &at, (uint32_t)frame->rotate)) goto fail;
            for (k = 0; k < 8; k++)
                if (r2_blob_u32(blob, cap, &at, (uint32_t)(int32_t)frame->lump[k])) goto fail;
            for (k = 0; k < 8; k++)
                if (r2_blob_u32(blob, cap, &at, (uint32_t)frame->flip[k])) goto fail;
        }
    }
    if (r2_table(tables, R2_T_SPRITEDEFS, blob, (uint32_t)at)) goto fail;
    free(blob); return 0;
fail:
    free(blob); return -1;
}

static int r2_capture_light_offsets(r2_builder_t *tables, uint32_t id,
                                    lighttable_t *const *values, size_t count)
{
    size_t i; uint8_t *blob = (uint8_t *)malloc(count * 4u);
    uintptr_t base = (uintptr_t)colormaps, end = base + (uintptr_t)W_LumpLength(W_GetNumForName("COLORMAP"));
    if (!blob || !colormaps) { free(blob); return -1; }
    for (i = 0; i < count; i++) {
        uintptr_t p = (uintptr_t)values[i];
        if (!values[i]) { r2_put32(blob + i * 4u, RI2_NULL); continue; }
        if (end - base < 256u || p < base || p > end - 256u) { free(blob); return -1; }
        r2_put32(blob + i * 4u, (uint32_t)(p - base));
    }
    if (r2_table(tables, id, blob, (uint32_t)(count * 4u))) { free(blob); return -1; }
    free(blob); return 0;
}

static int r2_capture_zlight_offsets(r2_builder_t *tables)
{
    size_t i, j; uint8_t *blob = (uint8_t *)malloc(LIGHTLEVELS * MAXLIGHTZ * 4u);
    if (!blob || !colormaps) { free(blob); return -1; }
    /* R_InitLightTables is a pure compile-constant schedule over the WAD
     * colormap bytes.  Re-emit its integer schedule; never serialize the
     * source pointer array (which is absent on a portable consumer). */
    for (i = 0; i < LIGHTLEVELS; i++) for (j = 0; j < MAXLIGHTZ; j++) {
        int scale = FixedDiv((SCREENWIDTH / 2 * FRACUNIT), ((int)j + 1) << LIGHTZSHIFT);
        int level = ((LIGHTLEVELS - 1 - (int)i) * 2) * NUMCOLORMAPS / LIGHTLEVELS;
        scale >>= LIGHTSCALESHIFT; level -= scale / R2_DISTMAP;
        if (level < 0) level = 0;
        if (level >= NUMCOLORMAPS) level = NUMCOLORMAPS - 1;
        r2_put32(blob + (i * MAXLIGHTZ + j) * 4u, (uint32_t)level * 256u);
    }
    if (r2_table(tables, R2_T_ZLIGHTOFFSETS, blob, LIGHTLEVELS * MAXLIGHTZ * 4u)) { free(blob); return -1; }
    free(blob); return 0;
}

static uint32_t r2_light_selector(lighttable_t **value)
{
    int i;
    if (!value) return RI2_NULL;
    if (value == scalelightfixed) return 0x40000000u;
    for (i = 0; i < LIGHTLEVELS; i++) if (value == scalelight[i]) return (uint32_t)i;
    for (i = 0; i < LIGHTLEVELS; i++) if (value == zlight[i]) return 0x80000000u | (uint32_t)i;
    return RI2_NULL - 1u;
}

static int r2_capture_v2_tables(r2_builder_t *tables)
{
    uint8_t selectors[12]; uint32_t values[3]; size_t i;
    if (r2_capture_texture_defs(tables)) return -101;
    if (r2_capture_texture_columns(tables, R2_T_TEXTURECOLUMNLUMP, 0)) return -102;
    if (r2_capture_texture_columns(tables, R2_T_TEXTURECOLUMNOFS, 1)) return -103;
    if (r2_capture_composites(tables)) return -104;
    if (r2_capture_sprite_defs(tables)) return -105;
    if (!colormaps || !translationtables ||
        r2_table(tables, R2_T_COLORMAPS, colormaps, (uint32_t)W_LumpLength(W_GetNumForName("COLORMAP"))) ||
        r2_table(tables, R2_T_TRANSLATIONTABLEBYTES, translationtables, 256u * 3u) ||
        r2_capture_light_offsets(tables, R2_T_SCALIGHTOFFSETS,
                                 (lighttable_t *const *)scalelight,
                                 LIGHTLEVELS * MAXLIGHTSCALE) ||
        r2_capture_light_offsets(tables, R2_T_SCALIGHTFIXEDOFFSETS,
                                 (lighttable_t *const *)scalelightfixed, MAXLIGHTSCALE) ||
        r2_capture_zlight_offsets(tables)) return -106;
    values[0] = r2_light_selector(spritelights);
    values[1] = r2_light_selector(walllights);
    values[2] = r2_light_selector(planezlight);
    for (i = 0; i < 3; i++) if (values[i] == RI2_NULL - 1u) return -1;
    for (i = 0; i < 3; i++) r2_put32(selectors + i * 4u, values[i]);
    return r2_table(tables, R2_T_LIGHTSELECTORS, selectors, sizeof(selectors));
}

static int r2_zone_offset(const void *pointer, uint32_t bytes, uint32_t *offset)
{
    uintptr_t base = (uintptr_t)DOOM_CTX->zone.bytes, p = (uintptr_t)pointer;
    if (!pointer || p < base || p - base > RI2_ZONE_BYTES || bytes > RI2_ZONE_BYTES - (p - base)) return -1;
    *offset = (uint32_t)(p - base); return 0;
}

static int r2_capture_zone_reloc(r2_builder_t *relocs, uint32_t field,
                                 const void *pointer, uint32_t bytes)
{
    uint32_t offset;
    if (r2_zone_offset(pointer, bytes, &offset)) return -1;
    return r2_reloc(relocs, field, 0, R2_REGION_ZONE, offset, bytes);
}

static int r2_capture_relocs(r2_builder_t *relocs, uint32_t demo_len)
{
    if (!vertexes) return -280;
    if (!sectors) return -281;
    if (!sides) return -282;
    if (!lines) return -283;
    if (!segs) return -284;
    if (!subsectors) return -285;
    if (!nodes) return -286;
    if (!blockmaplump) return -287;
    if (!rejectmatrix) return -288;
    if (!screens[0]) return -289;
    if (!demobuffer) return -290;
    if (!demo_p) return -291;
    if (r2_reloc(relocs, R2_PTR_VERTEXES, 0, R2_REGION_GRAPH, R2_GRAPH_VERTEXES, numvertexes * 8u) ||
        r2_reloc(relocs, R2_PTR_SECTORS, 0, R2_REGION_GRAPH, R2_GRAPH_SECTORS, numsectors * 64u) ||
        r2_reloc(relocs, R2_PTR_SIDES, 0, R2_REGION_GRAPH, R2_GRAPH_SIDES, numsides * 20u) ||
        r2_reloc(relocs, R2_PTR_LINES, 0, R2_REGION_GRAPH, R2_GRAPH_LINES, (uint32_t)DOOM_CTX->numlines * 64u) ||
        r2_reloc(relocs, R2_PTR_SEGS, 0, R2_REGION_GRAPH, R2_GRAPH_SEGS, numsegs * 48u) ||
        r2_reloc(relocs, R2_PTR_SUBSECTORS, 0, R2_REGION_GRAPH, R2_GRAPH_SUBSECTORS, numsubsectors * 16u) ||
        r2_reloc(relocs, R2_PTR_NODES, 0, R2_REGION_GRAPH, R2_GRAPH_NODES, DOOM_CTX->numnodes * 64u) ||
        r2_reloc(relocs, R2_PTR_BLOCKMAPLUMP, 0, R2_REGION_GRAPH, R2_GRAPH_BLOCKMAP, 0) ||
        r2_reloc(relocs, R2_PTR_REJECT, 0, R2_REGION_GRAPH, R2_GRAPH_REJECT, 0)) return -251;
    if (r2_reloc(relocs, R2_PTR_SCREENS, 0, R2_REGION_FRAME, 0, RI2_FRAME_BYTES) ||
        r2_reloc(relocs, R2_PTR_SCREENS, 1, R2_REGION_FRAME, RI2_FRAME_BYTES, RI2_FRAME_BYTES) ||
        r2_reloc(relocs, R2_PTR_SCREENS, 2, R2_REGION_FRAME, 2u * RI2_FRAME_BYTES, RI2_FRAME_BYTES) ||
        r2_reloc(relocs, R2_PTR_SCREENS, 3, R2_REGION_FRAME, 3u * RI2_FRAME_BYTES, RI2_FRAME_BYTES)) return -252;
    {
        int i, rows = viewheight;
        if (rows < 0 || rows > 200) return -1;
        for (i = 0; i < rows; i++)
            if (r2_reloc(relocs, R2_PTR_YLOOKUP, (uint32_t)i, R2_REGION_FRAME,
                         (uint32_t)i * 320u, RI2_FRAME_BYTES)) return -253;
    }
    if (r2_capture_zone_reloc(relocs, R2_PTR_BLOCKLINKS, blocklinks,
                              (uint32_t)(bmapwidth * bmapheight * (int)sizeof(*blocklinks)))) return -254;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TEXTURES, textures, numtextures * 8u)) return -255;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TEXTURECOLUMNLUMP, texturecolumnlump, numtextures * 8u)) return -256;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TEXTURECOLUMNOFS, texturecolumnofs, numtextures * 8u)) return -257;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TEXTURECOMPOSITE, texturecomposite, numtextures * 8u)) return -258;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TEXTURECOMPOSITESIZE, texturecompositesize, numtextures * 4u)) return -259;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TEXTUREHEIGHT, textureheight, numtextures * 4u)) return -260;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TEXTURETRANSLATION, texturetranslation, (numtextures + 1u) * 4u)) return -261;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TEXTUREWIDTHMASK, texturewidthmask, numtextures * 4u)) return -262;
    if (r2_capture_zone_reloc(relocs, R2_PTR_FLATTRANSLATION, flattranslation, (numflats + 1u) * 4u)) return -263;
    if (r2_capture_zone_reloc(relocs, R2_PTR_SPRITEWIDTH, spritewidth, numspritelumps * 4u)) return -264;
    if (r2_capture_zone_reloc(relocs, R2_PTR_SPRITEOFFSET, spriteoffset, numspritelumps * 4u)) return -265;
    if (r2_capture_zone_reloc(relocs, R2_PTR_SPRITETOPOFFSET, spritetopoffset, numspritelumps * 4u)) return -266;
    if (r2_capture_zone_reloc(relocs, R2_PTR_SPRITES, sprites, numsprites * 8u)) return -267;
    if (r2_capture_zone_reloc(relocs, R2_PTR_COLORMAPS, colormaps, 1)) return -268;
    if (r2_capture_zone_reloc(relocs, R2_PTR_TRANSLATIONTABLES, translationtables, 1)) return -269;
    /* scalelight/zlight and the wall/sprite/plane light views are context
     * resident pointer arrays.  Their entries point into colormaps or into
     * other context arrays, so the first vertical deliberately leaves them
     * to the exact source-matched R_Init/R_ExecuteSetViewSize setup.  A
     * future RI2 schema can add indexed context-field relocations; treating
     * the address of the pointer array as a zone pointer would be an ABI
     * error, not a capture. */
    if (r2_capture_zone_reloc(relocs, R2_PTR_DEMOBUFFER, demobuffer, demo_len)) return -276;
    if (r2_capture_zone_reloc(relocs, R2_PTR_DEMOP, demo_p, demo_len > 13 ? demo_len - 13u : 0)) return -277;
    /* DEMO1 playback has no `demoend`; the controller's explicit captured
     * bounds are the semantic end marker for this source-matched path. */
    return 0;
}

static int r2_capture_functions(r2_builder_t *functions)
{
    int a = r2_function_id(colfunc), b = r2_function_id(basecolfunc),
        c = r2_function_id(fuzzcolfunc), d = r2_function_id(transcolfunc),
        e = r2_function_id(spanfunc);
    if (!a || !b || !c || !d || !e || r2_function(functions, 1, (uint32_t)a) ||
        r2_function(functions, 2, (uint32_t)b) || r2_function(functions, 3, (uint32_t)c) ||
        r2_function(functions, 4, (uint32_t)d) || r2_function(functions, 5, (uint32_t)e)) return -1;
    return 0;
}

static int r2_build(const uint8_t *graph, size_t graph_bytes, r2_builder_t *scalars,
                    r2_builder_t *tables, r2_builder_t *relocs, r2_builder_t *functions,
                    uint8_t **out, size_t *out_bytes)
{
    r2_section_t sections[5]; const uint8_t *payloads[5]; size_t lengths[5];
    uint32_t cursor, total; unsigned i; uint8_t *image;
    if (!graph || graph_bytes > 0xffffffffu || !out || !out_bytes) return -1;
    payloads[0] = graph; lengths[0] = graph_bytes;
    payloads[1] = scalars->bytes; lengths[1] = scalars->used;
    payloads[2] = tables->bytes; lengths[2] = tables->used;
    payloads[3] = relocs->bytes; lengths[3] = relocs->used;
    payloads[4] = functions->bytes; lengths[4] = functions->used;
    cursor = (RI2_HEADER_BYTES + RI2_SECTION_COUNT * 16u + 15u) & ~15u;
    for (i = 0; i < 5; i++) {
        sections[i].offset = cursor; sections[i].bytes = (uint32_t)lengths[i];
        sections[i].count = i == 0 ? 1u : i == 1 ? scalars->count : i == 2 ? tables->count : i == 3 ? relocs->count : functions->count;
        sections[i].record_bytes = i == 3 ? 24u : i == 4 ? 16u : 0u;
        cursor = (cursor + sections[i].bytes + 15u) & ~15u;
    }
    total = cursor; if (total > RI2_MAX_BYTES) return -1;
    image = (uint8_t *)calloc(1, total); if (!image) return -1;
    memcpy(image, RI2_MAGIC, 8); r2_put32(image + 8, RI2_VERSION);
    r2_put32(image + 12, RI2_HEADER_BYTES); r2_put32(image + 16, total);
    r2_put32(image + 20, RI2_SCHEMA); r2_put32(image + 24, 1); r2_put32(image + 28, 5);
    r2_put32(image + 32, RI2_SECTION_COUNT); r2_put32(image + 36, RI2_SECTION_DIR_OFFSET);
    memcpy(image + 40, r2_engine_id, 32); memcpy(image + 72, r2_wad_hash, 32);
    memcpy(image + 104, RI2_LEVEL_NAME, 8); r2_put32(image + 112, scalars->count);
    r2_put32(image + 116, tables->count); r2_put32(image + 120, relocs->count);
    r2_put32(image + 124, functions->count); r2_put32(image + 128, (uint32_t)graph_bytes);
    for (i = 0; i < 5; i++) {
        memcpy(image + sections[i].offset, payloads[i], lengths[i]); r2_write_section(image, i, &sections[i]);
    }
    *out = image; *out_bytes = total; return 0;
}

int doom_resource_encode_initialized(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    unsigned char *out, unsigned long out_cap, unsigned long *out_len,
    unsigned long *graph_anchor)
{
    uint8_t *wad = 0, *carrier = 0, *graph = 0, *image = 0, hash[32];
    size_t wad_bytes = 0, graph_bytes = 0, image_bytes = 0; uintptr_t anchor = 0;
    uint8_t scalar_bytes[4096], table_bytes[256000], reloc_bytes[8192], function_bytes[256];
    r2_builder_t scalars = { scalar_bytes, sizeof(scalar_bytes), 0, 0 };
    r2_builder_t tables = { table_bytes, sizeof(table_bytes), 0, 0 };
    r2_builder_t relocs = { reloc_bytes, sizeof(reloc_bytes), 0, 0 };
    r2_builder_t functions = { function_bytes, sizeof(function_bytes), 0, 0 };
    int lump, demo_len, rc;
    if (!out || !out_len || out_cap < 1024 ||
        doom_engine_ex2_make_wad(wad_accounts, wad_lens, &wad, &wad_bytes, hash) ||
        memcmp(hash, r2_wad_hash, 32)) return 234;
    if (doom_resource_ri1_setup(wad, wad_bytes, &carrier)) { free(carrier); free(wad); return 234; }
    graph = (uint8_t *)malloc(RI2_MAX_BYTES); if (!graph) { free(carrier); free(wad); return 238; }
    rc = doom_resource_ri1_encode_graph(&graph, &graph_bytes, &anchor);
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump);
    if (rc) { free(graph); free(carrier); free(wad); return 242; }
    if (r2_capture_scalars(&scalars)) { free(graph); free(carrier); free(wad); return 243; }
    if (r2_capture_tables(&tables)) { free(graph); free(carrier); free(wad); return 244; }
    rc = r2_capture_v2_tables(&tables);
    if (rc) { free(graph); free(carrier); free(wad); return (unsigned long)(-rc); }
    rc = r2_capture_relocs(&relocs, (uint32_t)demo_len);
    if (rc) { free(graph); free(carrier); free(wad); return (unsigned long)(-rc); }
    if (r2_capture_functions(&functions)) { free(graph); free(carrier); free(wad); return 246; }
    if (r2_build(graph, graph_bytes, &scalars, &tables, &relocs, &functions, &image, &image_bytes)) {
        free(graph); free(carrier); free(wad); return 247;
    }
    if (image_bytes > out_cap) { free(image); free(graph); free(carrier); free(wad); return 248; }
    memcpy(out, image, image_bytes); *out_len = (unsigned long)image_bytes;
    if (graph_anchor) *graph_anchor = (unsigned long)anchor;
    free(image); free(graph); free(carrier); free(wad); return 0;
}

static int r2_apply_scalar(uint32_t id, uint32_t bytes, const uint8_t *p, void *opaque)
{
    (void)opaque; if (bytes != 4) return -1;
#define R2_APPLY(field, number) case number: field = (int)r2_u32(p); break
    switch (id) {
    R2_APPLY(setblocks, R2_SC_SETBLOCKS); R2_APPLY(setdetail, R2_SC_SETDETAIL);
    R2_APPLY(screenblocks, R2_SC_SCREENBLOCKS); R2_APPLY(detailLevel, R2_SC_DETAILLEVEL);
    R2_APPLY(viewwidth, R2_SC_VIEWWIDTH); R2_APPLY(viewheight, R2_SC_VIEWHEIGHT);
    R2_APPLY(scaledviewwidth, R2_SC_SCALEDVIEWWIDTH); R2_APPLY(centerx, R2_SC_CENTERX);
    R2_APPLY(centery, R2_SC_CENTERY); R2_APPLY(centerxfrac, R2_SC_CENTERXFRAC);
    R2_APPLY(centeryfrac, R2_SC_CENTERYFRAC); R2_APPLY(projection, R2_SC_PROJECTION);
    R2_APPLY(detailshift, R2_SC_DETAILSHIFT); R2_APPLY(gameepisode, R2_SC_GAMEEPISODE);
    R2_APPLY(gamemap, R2_SC_GAMEMAP); R2_APPLY(gameskill, R2_SC_GAMESKILL);
    R2_APPLY(gamestate, R2_SC_GAMESTATE); R2_APPLY(gametic, R2_SC_GAMETIC);
    R2_APPLY(gameaction, R2_SC_GAMEACTION); R2_APPLY(gamemode, R2_SC_GAMEMODE);
    R2_APPLY(gamemission, R2_SC_GAMEMISSION); R2_APPLY(language, R2_SC_LANGUAGE);
    R2_APPLY(DOOM_CTX->consoleplayer, R2_SC_CONSOLEPLAYER); R2_APPLY(displayplayer, R2_SC_DISPLAYPLAYER);
    R2_APPLY(framecount, R2_SC_FRAMECOUNT); R2_APPLY(leveltime, R2_SC_LEVELTIME);
    R2_APPLY(numtextures, R2_SC_NUMTEXTURES); R2_APPLY(numflats, R2_SC_NUMFLATS);
    R2_APPLY(numspritelumps, R2_SC_NUMSPRITELUMPS); R2_APPLY(numsprites, R2_SC_NUMSPRITES);
    R2_APPLY(numswitches, R2_SC_NUMSWITCHES); R2_APPLY(firstflat, R2_SC_FIRSTFLAT);
    R2_APPLY(lastflat, R2_SC_LASTFLAT); R2_APPLY(firstspritelump, R2_SC_FIRSTSPRITELUMP);
    R2_APPLY(lastspritelump, R2_SC_LASTSPRITELUMP); R2_APPLY(precache, R2_SC_PRECACHE);
    R2_APPLY(demoplayback, R2_SC_DEMOPLAYBACK);
    case R2_SC_ANIMCOUNT: if (r2_u32(p) > MAXANIMS) return -1; lastanim = anims + r2_u32(p); break;
    default: return -(1000 + (int)id);
    }
#undef R2_APPLY
    return 0;
}

static int r2_apply_table(uint32_t id, uint32_t n, const uint8_t *p, void *opaque)
{
    (void)opaque;
#define R2_COPY(number, field) if (id == (number)) { if (n != sizeof(field)) return -1; memcpy(field, p, n); return 0; }
    R2_COPY(R2_T_VIEWANGLETOX, viewangletox); R2_COPY(R2_T_XTOVIEWANGLE, xtoviewangle);
    R2_COPY(R2_T_YSLOPE, yslope); R2_COPY(R2_T_DISTSCALE, distscale);
    R2_COPY(R2_T_SCREENHEIGHTARRAY, screenheightarray); R2_COPY(R2_T_COLUMNOFS, columnofs);
    R2_COPY(R2_T_SWITCHLIST, switchlist); R2_COPY(R2_T_ANIMS, anims);
    R2_COPY(R2_T_TRANSLATIONS, translations); R2_COPY(R2_T_SPRTEMP, sprtemp);
    if (id == R2_T_TEXTUREWIDTHMASK && (size_t)n == (size_t)numtextures * 4u) { memcpy(texturewidthmask,p,n); return 0; }
    if (id == R2_T_TEXTUREHEIGHT && (size_t)n == (size_t)numtextures * 4u) { memcpy(textureheight,p,n); return 0; }
    if (id == R2_T_TEXTURECOMPOSITESIZE && (size_t)n == (size_t)numtextures * 4u) { memcpy(texturecompositesize,p,n); return 0; }
    if (id == R2_T_TEXTURETRANSLATION && (size_t)n == (size_t)(numtextures + 1) * 4u) { memcpy(texturetranslation,p,n); return 0; }
    if (id == R2_T_FLATTRANSLATION && (size_t)n == (size_t)(numflats + 1) * 4u) { memcpy(flattranslation,p,n); return 0; }
    if (id == R2_T_SPRITEWIDTH && (size_t)n == (size_t)numspritelumps * 4u) { memcpy(spritewidth,p,n); return 0; }
    if (id == R2_T_SPRITEOFFSET && (size_t)n == (size_t)numspritelumps * 4u) { memcpy(spriteoffset,p,n); return 0; }
    if (id == R2_T_SPRITETOPOFFSET && (size_t)n == (size_t)numspritelumps * 4u) { memcpy(spritetopoffset,p,n); return 0; }
    return -1;
#undef R2_COPY
}

static int r2_apply_functions(const uint8_t *image, const r2_section_t *s)
{
    size_t at; const uint8_t *p = image + s[R2_FUNCTIONS].offset;
    for (at = 0; at < s[R2_FUNCTIONS].bytes; at += 16) {
        uint32_t field = r2_u32(p + at), id = r2_u32(p + at + 8); void (*fn)(void) = r2_function_value(id);
        if (r2_u32(p + at + 4) || r2_u32(p + at + 12) || !fn) return -1;
        switch (field) { case 1: colfunc = fn; break; case 2: basecolfunc = fn; break;
        case 3: fuzzcolfunc = fn; break; case 4: transcolfunc = fn; break;
        case 5: spanfunc = fn; break; default: return -1; }
    }
    return 0;
}

static int r2_apply_relocs(const uint8_t *image, const r2_section_t *s)
{
    const uint8_t *p = image + s[R2_RELOCS].offset; size_t at; uint32_t field,index,region,offset,bytes;
    uint8_t *zone = DOOM_CTX->zone.bytes;
    for (at = 0; at < s[R2_RELOCS].bytes; at += 24) {
        field = r2_u32(p + at); index = r2_u32(p + at + 4); region = r2_u32(p + at + 8);
        offset = r2_u32(p + at + 12); bytes = r2_u32(p + at + 16);
        if (r2_u32(p + at + 20) || region < R2_REGION_GRAPH || region > R2_REGION_RUNTIME) return -(1000 + (int)field);
        if (region == R2_REGION_ZONE && ((uint64_t)offset + bytes > RI2_ZONE_BYTES)) return -(1000 + (int)field);
        if (field == R2_PTR_SCREENS) {
            if (index >= 4 || region != R2_REGION_FRAME || bytes != RI2_FRAME_BYTES || offset != index * RI2_FRAME_BYTES || !screens[index]) return -(1000 + (int)field);
        } else if (field == R2_PTR_YLOOKUP) {
            if (region != R2_REGION_FRAME || index >= 200 || offset != index * 320u || bytes != RI2_FRAME_BYTES || !screens[0]) return -(1000 + (int)field);
            ylookup[index] = screens[0] + offset;
        } else if (region == R2_REGION_GRAPH) {
            if (index || offset > R2_GRAPH_SECTOR_LINES) return -(1000 + (int)field);
        } else if (region == R2_REGION_ZONE) {
            if (index) return -(1000 + (int)field);
            void *address = zone + offset;
            switch (field) {
            case R2_PTR_BLOCKLINKS: blocklinks = (mobj_t **)address; break;
            case R2_PTR_TEXTURES: textures = (texture_t **)address; break;
            case R2_PTR_TEXTURECOLUMNLUMP: texturecolumnlump = (short **)address; break;
            case R2_PTR_TEXTURECOLUMNOFS: texturecolumnofs = (unsigned short **)address; break;
            case R2_PTR_TEXTURECOMPOSITE: texturecomposite = (byte **)address; break;
            case R2_PTR_TEXTURECOMPOSITESIZE: texturecompositesize = (int *)address; break;
            case R2_PTR_TEXTUREHEIGHT: textureheight = (fixed_t *)address; break;
            case R2_PTR_TEXTURETRANSLATION: texturetranslation = (int *)address; break;
            case R2_PTR_TEXTUREWIDTHMASK: texturewidthmask = (int *)address; break;
            case R2_PTR_FLATTRANSLATION: flattranslation = (int *)address; break;
            case R2_PTR_SPRITEWIDTH: spritewidth = (fixed_t *)address; break;
            case R2_PTR_SPRITEOFFSET: spriteoffset = (fixed_t *)address; break;
            case R2_PTR_SPRITETOPOFFSET: spritetopoffset = (fixed_t *)address; break;
            case R2_PTR_SPRITES: sprites = (spritedef_t *)address; break;
            case R2_PTR_COLORMAPS: colormaps = (lighttable_t *)address; break;
            case R2_PTR_TRANSLATIONTABLES: translationtables = (byte *)address; break;
            /* These are pointer arrays, not assignable scalar globals.  The
             * source-matched setup has already installed their entries; RI2
             * records their region-relative bases and validates the range. */
            case R2_PTR_SCALIGHT: case R2_PTR_SCALIGHTFIXED: case R2_PTR_ZLIGHT:
            case R2_PTR_SPRITELIGHTS: case R2_PTR_WALLLIGHTS: case R2_PTR_PLANEZLIGHT: break;
            case R2_PTR_DEMOBUFFER: demobuffer = (byte *)address; break;
            case R2_PTR_DEMOP: demo_p = (byte *)address; break;
            case R2_PTR_DEMOEND: demoend = (byte *)address; break;
            default: return -(1000 + (int)field);
            }
        } else if (region != R2_REGION_RUNTIME) return -(1000 + (int)field);
    }
    return 0;
}

static int r2_apply(const uint8_t *image, const r2_section_t *s)
{
    int rc;
    rc = r2_walk(image + s[R2_SCALARS].offset, s[R2_SCALARS].bytes,
                 s[R2_SCALARS].count, 1, r2_apply_scalar, 0);
    if (rc) return rc;
    rc = r2_walk(image + s[R2_TABLES].offset, s[R2_TABLES].bytes,
                 s[R2_TABLES].count, 0, r2_apply_table, 0);
    if (rc) return rc == -1 ? -2 : rc;
    rc = r2_apply_relocs(image, s);
    if (rc) return rc;
    if (r2_apply_functions(image, s)) return -4;
    return 0;
}

static int r2_validate_record_shape(const uint8_t *bytes, size_t length,
                                    uint32_t expected, int fixed)
{
    size_t at = 0; uint32_t count = 0, n;
    while (at < length) {
        if (length - at < 8) return -1;
        n = r2_u32(bytes + at + 4); at += 8;
        if ((size_t)n > length - at || (fixed && n != 4)) return -1;
        at += n; count++;
    }
    return count == expected ? 0 : -1;
}

static int doom_resource_import_initialized_tic_v1(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    const unsigned char *image, unsigned long image_len,
    const unsigned char *state, unsigned long state_len,
    const unsigned char *mirror, unsigned long mirror_len,
    const unsigned char *controller, unsigned long controller_len,
    unsigned char *state_out, unsigned long state_cap, unsigned long *state_out_len,
    unsigned char *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned char *frame_out, unsigned long frame_cap, unsigned long *frame_out_len,
    unsigned long *graph_anchor)
{
    r2_section_t s[5]; uint8_t *wad = 0, *carrier = 0, *graph = 0, *encoded = 0, *derived = 0;
    uint8_t hash[32], *demo; size_t wad_bytes = 0, graph_bytes, encoded_bytes = 0; uintptr_t anchor = 0;
    int lump, demo_len, rc;
    if (!image || image_len > RI2_MAX_BYTES || r2_sections_v1(image, image_len, s) ||
        r2_validate_record_shape(image + s[R2_SCALARS].offset, s[R2_SCALARS].bytes, s[R2_SCALARS].count, 1) ||
        r2_validate_record_shape(image + s[R2_TABLES].offset, s[R2_TABLES].bytes, s[R2_TABLES].count, 0)) return 240;
    if (!state || state_len < 64 || state_len > RI2_STATE_MAX || !mirror || mirror_len != RI2_MIRROR_BYTES ||
        !controller || controller_len != 64 || !state_out || !mirror_out || !frame_out ||
        !state_out_len || !mirror_out_len || !frame_out_len || state_cap < RI2_STATE_MAX ||
        mirror_cap < RI2_MIRROR_BYTES || frame_cap < RI2_FRAME_BYTES) return 232;
    graph_bytes = s[R2_GRAPH].bytes; graph = (uint8_t *)malloc(graph_bytes);
    if (!graph) return 238; memcpy(graph, image + s[R2_GRAPH].offset, graph_bytes);
    if (doom_engine_ex2_make_wad(wad_accounts, wad_lens, &wad, &wad_bytes, hash) || memcmp(hash, r2_wad_hash, 32)) {
        free(graph); return 234;
    }
    if (doom_resource_ri1_setup(wad, wad_bytes, &carrier)) {
        free(graph); free(carrier); free(wad); return 234;
    }
    if (doom_resource_ri1_import_graph(graph, graph_bytes, &anchor)) {
        free(graph); free(carrier); free(wad); return 242;
    }
    rc = r2_apply(image, s);
    if (rc) {
        unsigned long code = rc <= -1000 ? 300u + (unsigned long)(-rc - 1000) : 242u - (unsigned long)rc;
        free(graph); free(carrier); free(wad); return (int)code;
    }
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump); if (demo_len < 17) { free(graph); free(carrier); free(wad); return 234; }
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC); DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    DoomStateSetAllocationFailure(-1);
    if (DoomStateDecode(state, state_len) || DoomControllerArmV2(controller + 24) || DoomControllerValidateCapturedDemoCursor(demo_p)) {
        free(graph); free(carrier); free(wad); return 232;
    }
    G_Ticker(); R_RenderPlayerView(&players[displayplayer]); gametic++;
    derived = (uint8_t *)malloc(RI2_MIRROR_BYTES);
    if (!derived || doom_engine_ex2_fill_mirror(derived, wad, wad_bytes, hash) ||
        DoomStateEncode(&encoded, &encoded_bytes) || encoded_bytes > state_cap || !screens[0]) {
        free(derived); free(encoded); free(graph); free(carrier); free(wad); return 238;
    }
    memcpy(state_out, encoded, encoded_bytes); memcpy(mirror_out, derived, RI2_MIRROR_BYTES);
    memcpy(frame_out, screens[0], RI2_FRAME_BYTES); *state_out_len = encoded_bytes;
    *mirror_out_len = RI2_MIRROR_BYTES; *frame_out_len = RI2_FRAME_BYTES;
    if (graph_anchor) *graph_anchor = (unsigned long)anchor;
    DoomStateFree(encoded); free(derived); free(graph); free(carrier); free(wad); return 0;
}

typedef struct {
    uint32_t scalar[64];
    const uint8_t *table[64];
    uint32_t table_bytes[64];
} r2_v2_plan_t;

static int r2_v2_scalar_plan(const r2_section_t *s, const uint8_t *image,
                             r2_v2_plan_t *plan)
{
    uint8_t seen[64] = {0}; size_t at = 0; uint32_t id, n; unsigned i;
    const uint8_t *p = image + s[R2_SCALARS].offset;
    while (at < s[R2_SCALARS].bytes) {
        if (s[R2_SCALARS].bytes - at < 12u) return -1;
        id = r2_u32(p + at); n = r2_u32(p + at + 4); at += 8u;
        if (id == 0 || id >= 64 || seen[id] || n != 4 ||
            n > s[R2_SCALARS].bytes - at) return -1;
        seen[id] = 1; plan->scalar[id] = r2_u32(p + at); at += n;
    }
    if (at != s[R2_SCALARS].bytes || s[R2_SCALARS].count != 38u) return -1;
    for (i = R2_SC_SETBLOCKS; i <= R2_SC_ANIMCOUNT; i++) if (!seen[i]) return -1;
    if (plan->scalar[R2_SC_NUMTEXTURES] > 4096u || plan->scalar[R2_SC_NUMFLATS] > 4096u ||
        plan->scalar[R2_SC_NUMSPRITELUMPS] > 4096u || plan->scalar[R2_SC_NUMSPRITES] > 4096u ||
        plan->scalar[R2_SC_ANIMCOUNT] > MAXANIMS ||
        plan->scalar[R2_SC_VIEWHEIGHT] > 200u || plan->scalar[R2_SC_VIEWWIDTH] > 320u ||
        plan->scalar[R2_SC_SCALEDVIEWWIDTH] > 320u) return -1;
    return 0;
}

static int r2_v2_table_expected(uint32_t id, const r2_v2_plan_t *plan, uint32_t *bytes)
{
    switch (id) {
    case R2_T_VIEWANGLETOX: *bytes = sizeof(viewangletox); return 0;
    case R2_T_XTOVIEWANGLE: *bytes = sizeof(xtoviewangle); return 0;
    case R2_T_YSLOPE: *bytes = sizeof(yslope); return 0;
    case R2_T_DISTSCALE: *bytes = sizeof(distscale); return 0;
    case R2_T_SCREENHEIGHTARRAY: *bytes = sizeof(screenheightarray); return 0;
    case R2_T_COLUMNOFS: *bytes = sizeof(columnofs); return 0;
    case R2_T_SWITCHLIST: *bytes = sizeof(switchlist); return 0;
    case R2_T_ANIMS: *bytes = sizeof(anims); return 0;
    case R2_T_TRANSLATIONS: *bytes = sizeof(translations); return 0;
    case R2_T_SPRTEMP: *bytes = sizeof(sprtemp); return 0;
    case R2_T_TEXTUREWIDTHMASK: *bytes = plan->scalar[R2_SC_NUMTEXTURES] * 4u; return 0;
    case R2_T_TEXTUREHEIGHT: *bytes = plan->scalar[R2_SC_NUMTEXTURES] * 4u; return 0;
    case R2_T_TEXTURECOMPOSITESIZE: *bytes = plan->scalar[R2_SC_NUMTEXTURES] * 4u; return 0;
    case R2_T_TEXTURETRANSLATION: *bytes = (plan->scalar[R2_SC_NUMTEXTURES] + 1u) * 4u; return 0;
    case R2_T_FLATTRANSLATION: *bytes = (plan->scalar[R2_SC_NUMFLATS] + 1u) * 4u; return 0;
    case R2_T_SPRITEWIDTH: *bytes = plan->scalar[R2_SC_NUMSPRITELUMPS] * 4u; return 0;
    case R2_T_SPRITEOFFSET: *bytes = plan->scalar[R2_SC_NUMSPRITELUMPS] * 4u; return 0;
    case R2_T_SPRITETOPOFFSET: *bytes = plan->scalar[R2_SC_NUMSPRITELUMPS] * 4u; return 0;
    case R2_T_TEXTUREDEFS: case R2_T_TEXTURECOLUMNLUMP: case R2_T_TEXTURECOLUMNOFS:
    case R2_T_TEXTURECOMPOSITEBYTES: case R2_T_SPRITEDEFS: case R2_T_COLORMAPS:
    case R2_T_TRANSLATIONTABLEBYTES: case R2_T_SCALIGHTOFFSETS:
    case R2_T_SCALIGHTFIXEDOFFSETS: case R2_T_ZLIGHTOFFSETS: case R2_T_LIGHTSELECTORS:
        *bytes = 0xffffffffu; return 0;
    default: return -1;
    }
}

static int r2_v2_parse_count(const uint8_t *p, size_t n, uint32_t expected,
                             size_t *at)
{
    uint32_t count;
    if (!p || !at || n < 4u) return -1;
    count = r2_u32(p); if (count != expected) return -1; *at = 4u; return 0;
}

static int r2_v2_validate_variable_tables(const r2_v2_plan_t *plan)
{
    const uint8_t *p; size_t n, at, i, j, k; uint32_t width, bytes, frames;
    p = plan->table[R2_T_TEXTUREDEFS]; n = plan->table_bytes[R2_T_TEXTUREDEFS];
    if (r2_v2_parse_count(p, n, plan->scalar[R2_SC_NUMTEXTURES], &at)) return -1;
    for (i = 0; i < plan->scalar[R2_SC_NUMTEXTURES]; i++) {
        int32_t w, h, patches;
        if (n - at < 20u) return -1; at += 8u;
        w = (int32_t)r2_u32(p + at); h = (int32_t)r2_u32(p + at + 4); patches = (int32_t)r2_u32(p + at + 8); at += 12u;
        if (w <= 0 || w > 4096 || h <= 0 || h > 4096 || patches < 0 || patches > 4096 ||
            (uint64_t)patches * 12u > n - at) return -1;
        at += (size_t)patches * 12u;
    }
    if (at != n) return -1;
    for (k = 0; k < 2; k++) {
        uint32_t id = k ? R2_T_TEXTURECOLUMNOFS : R2_T_TEXTURECOLUMNLUMP;
        p = plan->table[id]; n = plan->table_bytes[id];
        if (r2_v2_parse_count(p, n, plan->scalar[R2_SC_NUMTEXTURES], &at)) return -1;
        for (i = 0; i < plan->scalar[R2_SC_NUMTEXTURES]; i++) {
            if (n - at < 8u) return -1; width = r2_u32(p + at); bytes = r2_u32(p + at + 4); at += 8u;
            if (width == 0 || width > 4096 || bytes != width * sizeof(short) || bytes > n - at) return -1;
            at += bytes;
        }
        if (at != n) return -1;
    }
    p = plan->table[R2_T_TEXTURECOMPOSITEBYTES]; n = plan->table_bytes[R2_T_TEXTURECOMPOSITEBYTES];
    if (r2_v2_parse_count(p, n, plan->scalar[R2_SC_NUMTEXTURES], &at)) return -1;
    for (i = 0; i < plan->scalar[R2_SC_NUMTEXTURES]; i++) {
        if (n - at < 4u) return -1; bytes = r2_u32(p + at); at += 4u;
        if (bytes > 4u * 1024u * 1024u || bytes > n - at) return -1; at += bytes;
    }
    if (at != n) return -1;
    p = plan->table[R2_T_SPRITEDEFS]; n = plan->table_bytes[R2_T_SPRITEDEFS];
    if (r2_v2_parse_count(p, n, plan->scalar[R2_SC_NUMSPRITES], &at)) return -1;
    for (i = 0; i < plan->scalar[R2_SC_NUMSPRITES]; i++) {
        if (n - at < 4u) return -1; frames = r2_u32(p + at); at += 4u;
        if (frames > 64u || (uint64_t)frames * 68u > n - at) return -1;
        for (j = 0; j < frames; j++) { for (k = 0; k < 17; k++) if (n - at < 4u) return -1; else at += 4u; }
    }
    if (at != n || plan->table_bytes[R2_T_COLORMAPS] < NUMCOLORMAPS * 256u ||
        plan->table_bytes[R2_T_COLORMAPS] % 256u != 0 ||
        plan->table_bytes[R2_T_TRANSLATIONTABLEBYTES] != 256u * 3u ||
        plan->table_bytes[R2_T_SCALIGHTOFFSETS] != LIGHTLEVELS * MAXLIGHTSCALE * 4u ||
        plan->table_bytes[R2_T_SCALIGHTFIXEDOFFSETS] != MAXLIGHTSCALE * 4u ||
        plan->table_bytes[R2_T_ZLIGHTOFFSETS] != LIGHTLEVELS * MAXLIGHTZ * 4u ||
        plan->table_bytes[R2_T_LIGHTSELECTORS] != 12u) return -1;
    return 0;
}

static int r2_v2_table_plan(const r2_section_t *s, const uint8_t *image,
                            r2_v2_plan_t *plan)
{
    uint8_t seen[64] = {0}; size_t at = 0; uint32_t id, n, expected; unsigned i;
    const uint8_t *p = image + s[R2_TABLES].offset;
    while (at < s[R2_TABLES].bytes) {
        if (s[R2_TABLES].bytes - at < 8u) return -1;
        id = r2_u32(p + at); n = r2_u32(p + at + 4); at += 8u;
        if (id == 0 || id >= 64 || seen[id] || n > s[R2_TABLES].bytes - at ||
            r2_v2_table_expected(id, plan, &expected)) return -1;
        if (expected != 0xffffffffu && n != expected) return -1;
        seen[id] = 1; plan->table[id] = p + at; plan->table_bytes[id] = n; at += n;
    }
    if (at != s[R2_TABLES].bytes || s[R2_TABLES].count != 29u) return -1;
    for (i = R2_T_VIEWANGLETOX; i <= R2_T_LIGHTSELECTORS; i++) if (!seen[i]) return -1;
    return r2_v2_validate_variable_tables(plan);
}

static int r2_v2_reloc_plan(const r2_section_t *s, const uint8_t *image,
                            const r2_v2_plan_t *plan)
{
    uint8_t seen[64][256] = {{0}}, zone_seen[64] = {0};
    uint32_t zone_offset[64] = {0}, zone_bytes[64] = {0};
    const uint8_t *p = image + s[R2_RELOCS].offset;
    const uint8_t *graph = image + s[R2_GRAPH].offset;
    size_t at; uint32_t field, index, region, offset, bytes, expected;
    uint32_t nv, ns, nsi, nl, ng, nss, nn, bmapw, bmaph;
    uint32_t demo_offset = 0, demo_bytes = 0, demo_p_offset = 0, demo_p_bytes = 0;
    unsigned i, j;
    if (s[R2_GRAPH].bytes < 172u) return -1;
    nv = r2_u32(graph + 32); ns = r2_u32(graph + 36); nsi = r2_u32(graph + 40);
    nl = r2_u32(graph + 44); ng = r2_u32(graph + 48); nss = r2_u32(graph + 52);
    nn = r2_u32(graph + 56); bmapw = r2_u32(graph + 156); bmaph = r2_u32(graph + 160);
    if ((uint64_t)nv * 8u > 0xffffffffu || (uint64_t)ns * 64u > 0xffffffffu ||
        (uint64_t)nsi * 20u > 0xffffffffu || (uint64_t)nl * 64u > 0xffffffffu ||
        (uint64_t)ng * 48u > 0xffffffffu || (uint64_t)nss * 16u > 0xffffffffu ||
        (uint64_t)nn * 64u > 0xffffffffu || (uint64_t)bmapw * bmaph * sizeof(mobj_t *) > 0xffffffffu) return -1;
    for (at = 0; at < s[R2_RELOCS].bytes; at += 24u) {
        field = r2_u32(p + at); index = r2_u32(p + at + 4); region = r2_u32(p + at + 8);
        offset = r2_u32(p + at + 12); bytes = r2_u32(p + at + 16);
        if (field == 0 || field >= 64 || index >= 256 || seen[field][index] ||
            r2_u32(p + at + 20) || region < R2_REGION_GRAPH || region > R2_REGION_RUNTIME) return -1;
        seen[field][index] = 1;
        if (region == R2_REGION_ZONE && ((uint64_t)offset + bytes > RI2_ZONE_BYTES)) return -1;
        if (field >= R2_PTR_VERTEXES && field <= R2_PTR_REJECT) {
            if (index || region != R2_REGION_GRAPH) return -1;
            switch (field) {
            case R2_PTR_VERTEXES: expected = nv * 8u; break;
            case R2_PTR_SECTORS: expected = ns * 64u; break;
            case R2_PTR_SIDES: expected = nsi * 20u; break;
            case R2_PTR_LINES: expected = nl * 64u; break;
            case R2_PTR_SEGS: expected = ng * 48u; break;
            case R2_PTR_SUBSECTORS: expected = nss * 16u; break;
            case R2_PTR_NODES: expected = nn * 64u; break;
            case R2_PTR_BLOCKMAPLUMP: case R2_PTR_REJECT: expected = 0; break;
            default: return -1;
            }
            if ((field == R2_PTR_BLOCKMAPLUMP && offset != R2_GRAPH_BLOCKMAP) ||
                (field == R2_PTR_REJECT && offset != R2_GRAPH_REJECT) ||
                (field != R2_PTR_BLOCKMAPLUMP && field != R2_PTR_REJECT &&
                 offset != field - R2_PTR_VERTEXES) || bytes != expected) return -1;
        } else if (field == R2_PTR_SCREENS) {
            if (index >= 4 || region != R2_REGION_FRAME || bytes != RI2_FRAME_BYTES || offset != index * RI2_FRAME_BYTES) return -1;
        } else if (field == R2_PTR_YLOOKUP) {
            if (index >= plan->scalar[R2_SC_VIEWHEIGHT] || region != R2_REGION_FRAME || bytes != RI2_FRAME_BYTES || offset != index * 320u) return -1;
        } else if (region == R2_REGION_GRAPH) {
            return -1;
        } else if (region == R2_REGION_ZONE) {
            if (index || field > R2_PTR_DEMOEND ||
                field == R2_PTR_BLOCKMAP || field == R2_PTR_DEMOEND ||
                (field >= R2_PTR_SCALIGHT && field <= R2_PTR_PLANEZLIGHT)) return -1;
            switch (field) {
            case R2_PTR_BLOCKLINKS: expected = bmapw * bmaph * (uint32_t)sizeof(mobj_t *); break;
            case R2_PTR_TEXTURES: expected = plan->scalar[R2_SC_NUMTEXTURES] * (uint32_t)sizeof(texture_t *); break;
            case R2_PTR_TEXTURECOLUMNLUMP: expected = plan->scalar[R2_SC_NUMTEXTURES] * (uint32_t)sizeof(short *); break;
            case R2_PTR_TEXTURECOLUMNOFS: expected = plan->scalar[R2_SC_NUMTEXTURES] * (uint32_t)sizeof(unsigned short *); break;
            case R2_PTR_TEXTURECOMPOSITE: expected = plan->scalar[R2_SC_NUMTEXTURES] * (uint32_t)sizeof(byte *); break;
            case R2_PTR_TEXTURECOMPOSITESIZE: expected = plan->scalar[R2_SC_NUMTEXTURES] * 4u; break;
            case R2_PTR_TEXTUREHEIGHT: expected = plan->scalar[R2_SC_NUMTEXTURES] * 4u; break;
            case R2_PTR_TEXTURETRANSLATION: expected = (plan->scalar[R2_SC_NUMTEXTURES] + 1u) * 4u; break;
            case R2_PTR_TEXTUREWIDTHMASK: expected = plan->scalar[R2_SC_NUMTEXTURES] * 4u; break;
            case R2_PTR_FLATTRANSLATION: expected = (plan->scalar[R2_SC_NUMFLATS] + 1u) * 4u; break;
            case R2_PTR_SPRITEWIDTH: case R2_PTR_SPRITEOFFSET: case R2_PTR_SPRITETOPOFFSET:
                expected = plan->scalar[R2_SC_NUMSPRITELUMPS] * 4u; break;
            case R2_PTR_SPRITES: expected = plan->scalar[R2_SC_NUMSPRITES] * (uint32_t)sizeof(spritedef_t *); break;
            case R2_PTR_COLORMAPS: case R2_PTR_TRANSLATIONTABLES: expected = 1u; break;
            case R2_PTR_DEMOBUFFER: case R2_PTR_DEMOP: expected = 0u; break;
            default: return -1;
            }
            if (field != R2_PTR_DEMOBUFFER && field != R2_PTR_DEMOP && bytes != expected) return -1;
            if (field == R2_PTR_DEMOBUFFER) { if (!bytes) return -1; demo_offset = offset; demo_bytes = bytes; }
            if (field == R2_PTR_DEMOP) { if (!bytes) return -1; demo_p_offset = offset; demo_p_bytes = bytes; }
            if (field != R2_PTR_DEMOBUFFER && field != R2_PTR_DEMOP) {
                zone_seen[field] = 1; zone_offset[field] = offset; zone_bytes[field] = bytes;
            }
        } else return -1;
    }
    if (!demo_bytes || !demo_p_bytes || demo_p_offset < demo_offset ||
        demo_p_offset - demo_offset != 13u || demo_p_bytes + 13u != demo_bytes) return -1;
    for (i = R2_PTR_VERTEXES; i <= R2_PTR_DEMOEND; i++) for (j = i + 1; j <= R2_PTR_DEMOEND; j++) {
        uint64_t a0, a1, b0, b1;
        if (!zone_seen[i] || !zone_seen[j]) continue;
        a0 = zone_offset[i]; a1 = a0 + zone_bytes[i]; b0 = zone_offset[j]; b1 = b0 + zone_bytes[j];
        if (a0 < b1 && b0 < a1) return -1;
    }
    /* Every pointer-bearing field in the v1 directory must be present once;
     * screens and ylookup are the only indexed families. */
    for (field = R2_PTR_VERTEXES; field <= R2_PTR_DEMOEND; field++) {
        if (field == R2_PTR_SCREENS || field == R2_PTR_YLOOKUP || field == R2_PTR_BLOCKMAP || field == R2_PTR_DEMOEND ||
            (field >= R2_PTR_SCALIGHT && field <= R2_PTR_PLANEZLIGHT)) continue;
        if (!seen[field][0]) return -1;
    }
    for (i = 0; i < 4; i++) if (!seen[R2_PTR_SCREENS][i]) return -1;
    for (i = 0; i < plan->scalar[R2_SC_VIEWHEIGHT]; i++) if (!seen[R2_PTR_YLOOKUP][i]) return -1;
    if (!seen[R2_PTR_DEMOBUFFER][0] || !seen[R2_PTR_DEMOP][0] ||
        s[R2_RELOCS].count != 9u + 4u + plan->scalar[R2_SC_VIEWHEIGHT] + 18u) return -1;
    return 0;
}

static int r2_v2_function_plan(const r2_section_t *s, const uint8_t *image)
{
    uint8_t seen[8] = {0}; const uint8_t *p = image + s[R2_FUNCTIONS].offset;
    size_t at; uint32_t field, id;
    if (s[R2_FUNCTIONS].count != 5u) return -1;
    for (at = 0; at < s[R2_FUNCTIONS].bytes; at += 16u) {
        field = r2_u32(p + at); id = r2_u32(p + at + 8);
        if (field == 0 || field > 5 || seen[field] || r2_u32(p + at + 4) ||
            r2_u32(p + at + 12) || !r2_function_value(id)) return -1;
        seen[field] = 1;
    }
    return seen[1] && seen[2] && seen[3] && seen[4] && seen[5] ? 0 : -1;
}

static int r2_v2_preflight(const uint8_t *image, size_t bytes,
                           r2_section_t s[5], r2_v2_plan_t *plan)
{
    memset(plan, 0, sizeof(*plan));
    if (r2_sections(image, bytes, s) || r2_v2_scalar_plan(s, image, plan) ||
        r2_v2_table_plan(s, image, plan) || r2_v2_reloc_plan(s, image, plan) ||
        r2_v2_function_plan(s, image) ||
        doom_resource_ri1_validate_graph(image + s[R2_GRAPH].offset, s[R2_GRAPH].bytes)) return -1;
    return 0;
}

static void *r2_v2_zalloc(size_t bytes)
{
    if (!bytes || bytes > 0x7fffffffu) return 0;
    return Z_Malloc((int)bytes, PU_STATIC, 0);
}

static int r2_v2_alloc_resources(const r2_v2_plan_t *plan)
{
    size_t i, j, count = plan->scalar[R2_SC_NUMTEXTURES], flats = plan->scalar[R2_SC_NUMFLATS];
    size_t lumps = plan->scalar[R2_SC_NUMSPRITELUMPS], sprites_count = plan->scalar[R2_SC_NUMSPRITES];
    const uint8_t *p; size_t n, at; uint32_t width, bytes, frames;
    void *raw;
    textures = (texture_t **)r2_v2_zalloc(count * sizeof(*textures));
    texturecolumnlump = (short **)r2_v2_zalloc(count * sizeof(*texturecolumnlump));
    texturecolumnofs = (unsigned short **)r2_v2_zalloc(count * sizeof(*texturecolumnofs));
    texturecomposite = (byte **)r2_v2_zalloc(count * sizeof(*texturecomposite));
    texturecompositesize = (int *)r2_v2_zalloc(count * sizeof(*texturecompositesize));
    textureheight = (fixed_t *)r2_v2_zalloc(count * sizeof(*textureheight));
    texturetranslation = (int *)r2_v2_zalloc((count + 1u) * sizeof(*texturetranslation));
    texturewidthmask = (int *)r2_v2_zalloc(count * sizeof(*texturewidthmask));
    flattranslation = (int *)r2_v2_zalloc((flats + 1u) * sizeof(*flattranslation));
    spritewidth = (fixed_t *)r2_v2_zalloc(lumps * sizeof(*spritewidth));
    spriteoffset = (fixed_t *)r2_v2_zalloc(lumps * sizeof(*spriteoffset));
    spritetopoffset = (fixed_t *)r2_v2_zalloc(lumps * sizeof(*spritetopoffset));
    sprites = (spritedef_t *)r2_v2_zalloc(sprites_count * sizeof(*sprites));
    blocklinks = (mobj_t **)r2_v2_zalloc((size_t)bmapwidth * (size_t)bmapheight * sizeof(*blocklinks));
    if ((count && (!textures || !texturecolumnlump || !texturecolumnofs || !texturecomposite ||
                   !texturecompositesize || !textureheight || !texturetranslation || !texturewidthmask)) ||
        !flattranslation || (lumps && (!spritewidth || !spriteoffset || !spritetopoffset)) ||
        (sprites_count && !sprites) || !blocklinks) return -1;
    R2_PHASE(7421); /* engine pointer arrays and per-map blocks allocated */

    raw = r2_v2_zalloc((size_t)plan->table_bytes[R2_T_COLORMAPS] + 255u);
    if (!raw) return -1;
    colormaps = (lighttable_t *)(((uintptr_t)raw + 255u) & ~(uintptr_t)255u);
    raw = r2_v2_zalloc(256u * 3u + 255u);
    if (!raw) return -1;
    translationtables = (byte *)(((uintptr_t)raw + 255u) & ~(uintptr_t)255u);
    R2_PHASE(7422); /* colormap and translation storage allocated */

    p = plan->table[R2_T_TEXTUREDEFS]; n = plan->table_bytes[R2_T_TEXTUREDEFS];
    if (r2_v2_parse_count(p, n, (uint32_t)count, &at)) return -1;
    for (i = 0; i < count; i++) {
        int32_t w, h, patchcount; texture_t *texture;
        if (n - at < 20u) return -1;
        texture = (texture_t *)r2_v2_zalloc(sizeof(*texture) +
            (r2_u32(p + at + 16u) ? (size_t)(r2_u32(p + at + 16u) - 1u) * sizeof(texpatch_t) : 0u));
        if (!texture) return -1;
        memcpy(texture->name, p + at, 8u); w = (int32_t)r2_u32(p + at + 8u);
        h = (int32_t)r2_u32(p + at + 12u); patchcount = (int32_t)r2_u32(p + at + 16u);
        texture->width = (short)w; texture->height = (short)h; texture->patchcount = (short)patchcount;
        at += 20u; textures[i] = texture;
        for (j = 0; j < (size_t)patchcount; j++) {
            texture->patches[j].originx = (int32_t)r2_u32(p + at);
            texture->patches[j].originy = (int32_t)r2_u32(p + at + 4u);
            texture->patches[j].patch = (int32_t)r2_u32(p + at + 8u); at += 12u;
        }
    }
    if (at != n) return -1;
    R2_PHASE(7423); /* texture definitions and patch references reconstructed */
    for (j = 0; j < 2; j++) {
        uint32_t id = j ? R2_T_TEXTURECOLUMNOFS : R2_T_TEXTURECOLUMNLUMP;
        p = plan->table[id]; n = plan->table_bytes[id];
        if (r2_v2_parse_count(p, n, (uint32_t)count, &at)) return -1;
        for (i = 0; i < count; i++) {
            if (n - at < 8u) return -1; width = r2_u32(p + at); bytes = r2_u32(p + at + 4u); at += 8u;
            if (j) texturecolumnofs[i] = (unsigned short *)r2_v2_zalloc(bytes);
            else texturecolumnlump[i] = (short *)r2_v2_zalloc(bytes);
            if (bytes && ((!j && !texturecolumnlump[i]) || (j && !texturecolumnofs[i]))) return -1;
            memcpy(j ? (void *)texturecolumnofs[i] : (void *)texturecolumnlump[i], p + at, bytes); at += bytes;
            (void)width;
        }
        if (at != n) return -1;
    }
    R2_PHASE(7424); /* texture column lookup arrays copied */
    p = plan->table[R2_T_TEXTURECOMPOSITEBYTES]; n = plan->table_bytes[R2_T_TEXTURECOMPOSITEBYTES];
    if (r2_v2_parse_count(p, n, (uint32_t)count, &at)) return -1;
    for (i = 0; i < count; i++) { bytes = r2_u32(p + at); at += 4u; if (bytes) {
        texturecomposite[i] = (byte *)r2_v2_zalloc(bytes); if (!texturecomposite[i]) return -1;
        memcpy(texturecomposite[i], p + at, bytes);
    } at += bytes; }
    if (at != n) return -1;
    R2_PHASE(7425); /* texture composites and patch cache bytes copied */
    p = plan->table[R2_T_SPRITEDEFS]; n = plan->table_bytes[R2_T_SPRITEDEFS];
    if (r2_v2_parse_count(p, n, (uint32_t)sprites_count, &at)) return -1;
    for (i = 0; i < sprites_count; i++) {
        frames = r2_u32(p + at); at += 4u; sprites[i].numframes = (int)frames;
        if (frames) { sprites[i].spriteframes = (spriteframe_t *)r2_v2_zalloc(frames * sizeof(spriteframe_t)); if (!sprites[i].spriteframes) return -1; }
        for (j = 0; j < frames; j++) {
            size_t k; sprites[i].spriteframes[j].rotate = (boolean)r2_u32(p + at); at += 4u;
            for (k = 0; k < 8; k++) { sprites[i].spriteframes[j].lump[k] = (short)(int32_t)r2_u32(p + at); at += 4u; }
            for (k = 0; k < 8; k++) { sprites[i].spriteframes[j].flip[k] = (byte)r2_u32(p + at); at += 4u; }
        }
    }
    if (at != n) return -1;
    R2_PHASE(7426); /* sprite definitions and frame caches reconstructed */
    memcpy(colormaps, plan->table[R2_T_COLORMAPS], plan->table_bytes[R2_T_COLORMAPS]);
    memcpy(translationtables, plan->table[R2_T_TRANSLATIONTABLEBYTES], 256u * 3u);
    return 0;
}

static int r2_v2_apply_lights(const r2_v2_plan_t *plan)
{
    size_t i; uint32_t off, selector; const uint8_t *p;
    p = plan->table[R2_T_SCALIGHTOFFSETS]; for (i = 0; i < LIGHTLEVELS * MAXLIGHTSCALE; i++) {
        off = r2_u32(p + i * 4u); if (off == RI2_NULL || off > plan->table_bytes[R2_T_COLORMAPS] - 256u) return -1; ((lighttable_t **)scalelight)[i] = colormaps + off;
    }
    p = plan->table[R2_T_SCALIGHTFIXEDOFFSETS]; for (i = 0; i < MAXLIGHTSCALE; i++) {
        off = r2_u32(p + i * 4u); if (off == RI2_NULL) scalelightfixed[i] = 0; else { if (off > plan->table_bytes[R2_T_COLORMAPS] - 256u) return -1; scalelightfixed[i] = colormaps + off; }
    }
    p = plan->table[R2_T_ZLIGHTOFFSETS]; for (i = 0; i < LIGHTLEVELS * MAXLIGHTZ; i++) {
        off = r2_u32(p + i * 4u); if (off > plan->table_bytes[R2_T_COLORMAPS] - 256u) return -1; ((lighttable_t **)zlight)[i] = colormaps + off;
    }
    p = plan->table[R2_T_LIGHTSELECTORS]; selector = r2_u32(p); if (selector == RI2_NULL) spritelights = 0;
    else if (selector & 0x80000000u) { if ((selector & 0x7fffffffu) >= LIGHTLEVELS) return -1; spritelights = zlight[selector & 0x7fffffffu]; }
    else if (selector & 0x40000000u) spritelights = scalelightfixed;
    else { if (selector >= LIGHTLEVELS) return -1; spritelights = scalelight[selector]; }
    selector = r2_u32(p + 4u); if (selector == RI2_NULL) walllights = 0;
    else if (selector & 0x80000000u) { if ((selector & 0x7fffffffu) >= LIGHTLEVELS) return -1; walllights = zlight[selector & 0x7fffffffu]; }
    else if (selector & 0x40000000u) walllights = scalelightfixed;
    else { if (selector >= LIGHTLEVELS) return -1; walllights = scalelight[selector]; }
    selector = r2_u32(p + 8u); if (selector == RI2_NULL) planezlight = 0;
    else if (selector & 0x80000000u) { if ((selector & 0x7fffffffu) >= LIGHTLEVELS) return -1; planezlight = zlight[selector & 0x7fffffffu]; }
    else if (selector & 0x40000000u) planezlight = scalelightfixed;
    else { if (selector >= LIGHTLEVELS) return -1; planezlight = scalelight[selector]; }
    return 0;
}

static int r2_v2_apply_table(uint32_t id, uint32_t n, const uint8_t *p, void *opaque)
{
    if (id >= R2_T_TEXTUREDEFS) return 0;
    return r2_apply_table(id, n, p, opaque);
}

/* doom-lowdetail-3: the native half of the strip-path comparison.  Prints the
 * renderer inputs the chain rebuilds for every strip, so the first divergence
 * between the two paths can be read off directly instead of inferred from
 * pixels.  The subsector census is the one that matters: R_RenderBSPNode skips
 * a subsector whose validcount already equals the global one, so a stale
 * identity left by the image import silently drops that subsector's walls,
 * flats and sprites. */
static void doom_lowdetail3_probe(subsector_t *subs, int numsubs, int vc)
{
    int i, stale = 0, x;
    unsigned long h = 1469598103934665603ul;
    for (i = 0; i < numsubs; i++) if (subs[i].validcount == vc) stale++;
    for (x = 0; x <= viewwidth; x++)
    {
        h = (h ^ (unsigned long)(uint32_t)xtoviewangle[x]) * 1099511628211ul;
        h = (h ^ (unsigned long)(uint32_t)distscale[x]) * 1099511628211ul;
    }
    for (i = 0; i < viewheight; i++)
        h = (h ^ (unsigned long)(uint32_t)yslope[i]) * 1099511628211ul;
    for (i = 0; i < FINEANGLES/2; i++)
        h = (h ^ (unsigned long)(uint32_t)viewangletox[i]) * 1099511628211ul;
    printf("PROBE validcount=%#x subs=%d stale=%d viewwidth=%d viewheight=%d "
           "centerx=%d centerxfrac=%#x centery=%d centeryfrac=%#x detailshift=%d "
           "projection=%#x viewangle=%#x viewx=%d viewy=%d viewz=%d "
           "extralight=%d fixedcolormap=%d xtoviewangle0=%#x tables=%#lx\n",
           (unsigned)vc, numsubs, stale, viewwidth, viewheight, centerx,
           (unsigned)centerxfrac, centery, (unsigned)centeryfrac, detailshift,
           (unsigned)projection, (unsigned)viewangle, (int)viewx, (int)viewy,
           (int)viewz, players[displayplayer].extralight,
           players[displayplayer].fixedcolormap, (unsigned)xtoviewangle[0], h);
    fflush(stdout);
}

static int r2_v2_commit(const r2_v2_plan_t *plan, const r2_section_t *s,
                        const uint8_t *image, const uint8_t *wad, size_t wad_bytes,
                        const uint8_t hash[32],
                        const uint8_t *state, size_t state_len, const uint8_t *controller,
                        uint8_t **state_out, size_t *state_out_len, uint8_t *mirror_out,
                        uint8_t *frame_out, uintptr_t *anchor, int run_tic)
{
    uint8_t *carrier = 0, *frame = 0, *encoded = 0, *derived = 0, *demo; size_t encoded_bytes = 0;
    int lump, demo_len, rc;
    (void)image; (void)s;
    carrier = (uint8_t *)calloc(1, 4u * 1024u * 1024u); if (!carrier) return 238;
    memset(DOOM_CTX, 0, sizeof(doom_ctx_t)); doom_ctx_init();
    if (!doom_sbf_bind_arena(carrier, 4u * 1024u * 1024u, DOOM_ZONE_BYTES)) { free(carrier); return 234; }
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0; displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init(); P_InitThinkers(); W_InitAccountImage(wad, (int)wad_bytes);
    if (doom_resource_ri1_import_graph(image + s[R2_GRAPH].offset, s[R2_GRAPH].bytes, anchor)) { free(carrier); return 242; }
    lastanim = anims + plan->scalar[R2_SC_ANIMCOUNT];
    numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    {
        int apply_rc = r2_walk((const uint8_t *)(image + s[R2_SCALARS].offset), s[R2_SCALARS].bytes, s[R2_SCALARS].count, 1, r2_apply_scalar, 0);
        if (apply_rc || r2_v2_alloc_resources(plan) ||
            r2_walk(image + s[R2_TABLES].offset, s[R2_TABLES].bytes, s[R2_TABLES].count, 0, r2_v2_apply_table, 0) ||
            r2_v2_apply_lights(plan) || r2_apply_functions(image, s)) { free(carrier); return 242; }
    }
    frame = (uint8_t *)calloc(1, 4u * RI2_FRAME_BYTES); if (!frame) { free(carrier); return 238; }
    screens[0] = frame; screens[1] = frame + RI2_FRAME_BYTES; screens[2] = frame + 2u * RI2_FRAME_BYTES; screens[3] = frame + 3u * RI2_FRAME_BYTES; screens[4] = 0;
    viewimage = screens[0]; for (rc = 0; rc < viewheight; rc++) ylookup[rc] = screens[0] + (size_t)rc * 320u;
    /* These are derived view scalars, not setup calls.  They are reconstructed
     * from the captured view dimensions and lookup table before any render. */
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 : (SCREENHEIGHT - R2_SBARHEIGHT - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0; setsizeneeded = false;
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump); if (demo_len < 17) { free(frame); free(carrier); return 234; }
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC); defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo; demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len); DoomStateSetAllocationFailure(-1);
    if (DoomStateDecode(state, state_len) || DoomControllerArmV2(controller + 24) || DoomControllerValidateCapturedDemoCursor(demo_p)) { free(frame); free(carrier); return 232; }
    /* State decoding restores gameplay fields only; reassert the derived
     * renderer fields immediately before the real tic/render boundary. */
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 : (SCREENHEIGHT - R2_SBARHEIGHT - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH;
    pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0; setsizeneeded = false;
    if (run_tic) {
        /* doom-lowdetail-3: the native strip probe.  DOOM_NATIVE_STRIP selects
         * the strip whose clip word the render runs in, so the chain's strip
         * path (image import + decoded state + clip mode 1) is exercised in
         * this process and its frame can be compared with the oracle strip for
         * strip.  DOOM_NATIVE_PROBE=1 additionally prints the renderer inputs
         * the chain rebuilds, so the two sides can be compared.  Nothing
         * changes when neither variable is set. */
        const char *probe = getenv("DOOM_NATIVE_STRIP");
        const char *report = getenv("DOOM_NATIVE_PROBE");
        if (report) doom_lowdetail3_probe(subsectors, numsubsectors, validcount);
        G_Ticker();
        if (probe) doom_render_fuzz_columns = (1u << 30) | ((unsigned long)atol(probe) << 27);
        clipangle = xtoviewangle[0]; fixedcolormap = 0; R_RenderPlayerView(&players[displayplayer]); gametic++;
        if (report) printf("PROBE after drawsegs=%d vissprites=%d subs=%d\n",
                           (int)(ds_p - drawsegs), (int)(vissprite_p - vissprites), numsubsectors);
    }
    derived = (uint8_t *)malloc(RI2_MIRROR_BYTES); if (!derived || doom_engine_ex2_fill_mirror(derived, wad, wad_bytes, hash) || DoomStateEncode(&encoded, &encoded_bytes) || encoded_bytes > RI2_STATE_MAX) { free(derived); free(encoded); free(frame); free(carrier); return 238; }
    memcpy(*state_out, encoded, encoded_bytes); *state_out_len = encoded_bytes; memcpy(mirror_out, derived, RI2_MIRROR_BYTES); memcpy(frame_out, screens[0], RI2_FRAME_BYTES);
    DoomStateFree(encoded); free(derived); free(frame); free(carrier); return 0;
}

static int doom_resource_import_initialized_common(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    const unsigned char *image, unsigned long image_len, const unsigned char *state, unsigned long state_len,
    const unsigned char *mirror, unsigned long mirror_len, const unsigned char *controller, unsigned long controller_len,
    unsigned char *state_out, unsigned long state_cap, unsigned long *state_out_len,
    unsigned char *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned char *frame_out, unsigned long frame_cap, unsigned long *frame_out_len, unsigned long *graph_anchor,
    int run_tic)
{
    r2_section_t s[5]; r2_v2_plan_t plan; uint8_t *wad = 0, hash[32]; size_t wad_bytes = 0, out_len = 0; uintptr_t anchor = 0; int rc;
    if (!image || image_len > RI2_MAX_BYTES || image_len < 12u ||
        !state || state_len < 64u || state_len > RI2_STATE_MAX || !mirror || mirror_len != RI2_MIRROR_BYTES ||
        !controller || controller_len != 64u || !state_out || !mirror_out || !frame_out || !state_out_len || !mirror_out_len || !frame_out_len ||
        state_cap < RI2_STATE_MAX || mirror_cap < RI2_MIRROR_BYTES || frame_cap < RI2_FRAME_BYTES) return 232;
    if (r2_u32(image + 8) == RI2_VERSION_V1)
        return doom_resource_import_initialized_tic_v1(wad_accounts, wad_lens, image, image_len, state, state_len, mirror, mirror_len, controller, controller_len, state_out, state_cap, state_out_len, mirror_out, mirror_cap, mirror_out_len, frame_out, frame_cap, frame_out_len, graph_anchor);
    if (r2_u32(image + 8) != RI2_VERSION || r2_v2_preflight(image, image_len, s, &plan)) return 240;
    if (doom_engine_ex2_make_wad(wad_accounts, wad_lens, &wad, &wad_bytes, hash) || memcmp(hash, r2_wad_hash, 32)) { free(wad); return 234; }
    rc = r2_v2_commit(&plan, &s, image, wad, wad_bytes, hash, state, state_len, controller, &state_out, &out_len, mirror_out, frame_out, &anchor, run_tic);
    if (rc) { free(wad); return rc; }
    *state_out_len = out_len; *mirror_out_len = RI2_MIRROR_BYTES; *frame_out_len = RI2_FRAME_BYTES; if (graph_anchor) *graph_anchor = (unsigned long)anchor;
    free(wad); return 0;
}

int doom_resource_import_initialized_tic(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    const unsigned char *image, unsigned long image_len, const unsigned char *state, unsigned long state_len,
    const unsigned char *mirror, unsigned long mirror_len, const unsigned char *controller, unsigned long controller_len,
    unsigned char *state_out, unsigned long state_cap, unsigned long *state_out_len,
    unsigned char *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned char *frame_out, unsigned long frame_cap, unsigned long *frame_out_len, unsigned long *graph_anchor)
{
    return doom_resource_import_initialized_common(
        wad_accounts, wad_lens, image, image_len, state, state_len, mirror, mirror_len,
        controller, controller_len, state_out, state_cap, state_out_len, mirror_out,
        mirror_cap, mirror_out_len, frame_out, frame_cap, frame_out_len, graph_anchor, 1);
}

/* Host-only oracle for the bounded E2R/4 seam.  It runs the same source-matched
 * reconstruction and codec as the actual tic path, but stops immediately
 * before G_Ticker/render, matching the seam's declared open boundary. */
int doom_resource_import_initialized_no_tic(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    const unsigned char *image, unsigned long image_len, const unsigned char *state, unsigned long state_len,
    const unsigned char *mirror, unsigned long mirror_len, const unsigned char *controller, unsigned long controller_len,
    unsigned char *state_out, unsigned long state_cap, unsigned long *state_out_len,
    unsigned char *mirror_out, unsigned long mirror_cap, unsigned long *mirror_out_len,
    unsigned char *frame_out, unsigned long frame_cap, unsigned long *frame_out_len, unsigned long *graph_anchor)
{
    return doom_resource_import_initialized_common(
        wad_accounts, wad_lens, image, image_len, state, state_len, mirror, mirror_len,
        controller, controller_len, state_out, state_cap, state_out_len, mirror_out,
        mirror_cap, mirror_out_len, frame_out, frame_cap, frame_out_len, graph_anchor, 0);
}

/* Native no-render oracle for DOOM-SIM/1.  The RI2 graph, WAD, zone, tables,
 * state graph, and DEMO1 bounds are installed once.  Every output slot then
 * comes from one real G_Ticker followed by the same ST1/MP1 encoders used by
 * the SBF seam.  No renderer is linked into this boundary. */
int doom_resource_import_initialized_no_render_sequence(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    const unsigned char *image, unsigned long image_len,
    const unsigned char *state, unsigned long state_len,
    const unsigned char *commands, unsigned long command_count,
    unsigned char *states_out, unsigned long state_stride, unsigned long *state_lens_out,
    unsigned char *mirrors_out, unsigned long mirror_stride, unsigned long *graph_anchor)
{
    r2_section_t s[5]; r2_v2_plan_t plan; uint8_t *wad = 0, *carrier = 0;
    uint8_t *frame = 0, *encoded = 0, *derived = 0, *demo; uint8_t hash[32];
    size_t wad_bytes = 0, encoded_bytes = 0; uintptr_t anchor = 0;
    unsigned long i; int lump, demo_len, rc;
    if (!image || image_len > RI2_MAX_BYTES || image_len < 12u ||
        !state || state_len < 64u || state_len > RI2_STATE_MAX ||
        !commands || !command_count || command_count > 128u ||
        !states_out || !state_lens_out || !mirrors_out ||
        state_stride < RI2_STATE_MAX || mirror_stride < RI2_MIRROR_BYTES)
        return 232;
    if (r2_u32(image + 8) != RI2_VERSION || r2_v2_preflight(image, image_len, s, &plan))
        return 240;
    if (doom_engine_ex2_make_wad(wad_accounts, wad_lens, &wad, &wad_bytes, hash) ||
        memcmp(hash, r2_wad_hash, 32)) { free(wad); return 234; }
    carrier = (uint8_t *)calloc(1, 4u * 1024u * 1024u); if (!carrier) { free(wad); return 238; }
    memset(DOOM_CTX, 0, sizeof(doom_ctx_t)); doom_ctx_init();
    if (!doom_sbf_bind_arena(carrier, 4u * 1024u * 1024u, DOOM_ZONE_BYTES)) { free(carrier); free(wad); return 234; }
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0; displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init(); P_InitThinkers(); W_InitAccountImage(wad, (int)wad_bytes);
    if (doom_resource_ri1_import_graph(image + s[R2_GRAPH].offset, s[R2_GRAPH].bytes, &anchor)) { free(carrier); free(wad); return 242; }
    lastanim = anims + plan.scalar[R2_SC_ANIMCOUNT]; numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    rc = r2_walk((const uint8_t *)(image + s[R2_SCALARS].offset), s[R2_SCALARS].bytes, s[R2_SCALARS].count, 1, r2_apply_scalar, 0);
    if (rc || r2_v2_alloc_resources(&plan) ||
        r2_walk(image + s[R2_TABLES].offset, s[R2_TABLES].bytes, s[R2_TABLES].count, 0, r2_v2_apply_table, 0) ||
        r2_v2_apply_lights(&plan) || r2_apply_functions(image, s)) { free(carrier); free(wad); return 242; }
    frame = (uint8_t *)calloc(1, 4u * RI2_FRAME_BYTES); if (!frame) { free(carrier); free(wad); return 238; }
    screens[0] = frame; screens[1] = frame + RI2_FRAME_BYTES; screens[2] = frame + 2u * RI2_FRAME_BYTES; screens[3] = frame + 3u * RI2_FRAME_BYTES; screens[4] = 0;
    viewimage = screens[0]; for (rc = 0; rc < viewheight; rc++) ylookup[rc] = screens[0] + (size_t)rc * 320u;
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 : (SCREENHEIGHT - R2_SBARHEIGHT - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH; pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0; setsizeneeded = false;
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump); if (demo_len < 17) { free(frame); free(carrier); free(wad); return 234; }
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC); defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo; demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len); DoomStateSetAllocationFailure(-1);
    if (DoomStateDecode(state, state_len) || DoomControllerValidateCapturedDemoCursor(demo_p)) { free(frame); free(carrier); free(wad); return 232; }
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 : (SCREENHEIGHT - R2_SBARHEIGHT - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH; pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0; setsizeneeded = false;
    for (i = 0; i < command_count; i++) {
        if (DoomControllerArmV2(commands + i * 4u) || DoomControllerValidateCapturedDemoCursor(demo_p)) { free(frame); free(carrier); free(wad); return 237; }
        G_Ticker(); gametic++;
        derived = (uint8_t *)malloc(RI2_MIRROR_BYTES);
        if (!derived || doom_engine_ex2_fill_mirror(derived, wad, wad_bytes, hash) ||
            DoomStateEncode(&encoded, &encoded_bytes) || encoded_bytes > state_stride || encoded_bytes < 64u) {
            free(derived); free(encoded); free(frame); free(carrier); free(wad); return 238;
        }
        memcpy(states_out + i * state_stride, encoded, encoded_bytes);
        memcpy(mirrors_out + i * mirror_stride, derived, RI2_MIRROR_BYTES);
        state_lens_out[i] = encoded_bytes; DoomStateFree(encoded); encoded = 0; free(derived); derived = 0;
    }
    if (graph_anchor) *graph_anchor = (unsigned long)anchor;
    free(frame); free(carrier); free(wad); return 0;
}

/*
 * DOOM-SIM/2 host session oracle.
 *
 * The sequence oracle above runs a fixed command list (<= 128) and encodes
 * after every tic.  SIM/2 needs a longer reference (the 1,000-tic transport
 * gate) and a way to publish ST1/MP1 only at chosen cursors, so this keeps
 * one host engine resident across calls.  Setup is the same no-setup RI2/2
 * import as doom_resource_import_initialized_no_render_sequence, copied
 * rather than refactored so the retained sequence oracle stays byte-for-byte
 * unchanged.  The per-tic path is the SBF step verbatim: the shared pre-tic
 * guard, DoomControllerArmV2, G_Ticker, gametic++.  Host-only; one session
 * per process (the host engine lives in the global DOOM_CTX).
 */
#include "doom_sim2_guard.h"

static uint8_t *sim2_native_wad, *sim2_native_carrier, *sim2_native_frame;
static size_t sim2_native_wad_bytes;
static uint8_t sim2_native_hash[32];
static int sim2_native_open;

void doom_resource_sim_session_close(void)
{
    free(sim2_native_frame); free(sim2_native_carrier); free(sim2_native_wad);
    sim2_native_frame = 0; sim2_native_carrier = 0; sim2_native_wad = 0;
    sim2_native_wad_bytes = 0; sim2_native_open = 0;
}

int doom_resource_sim_session_open(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    const unsigned char *image, unsigned long image_len,
    const unsigned char *state, unsigned long state_len)
{
    r2_section_t s[5]; r2_v2_plan_t plan; uint8_t *demo;
    uintptr_t anchor = 0; int lump, demo_len, rc;
    doom_resource_sim_session_close();
    if (!image || image_len > RI2_MAX_BYTES || image_len < 12u ||
        !state || state_len < 64u || state_len > RI2_STATE_MAX)
        return 232;
    if (r2_u32(image + 8) != RI2_VERSION || r2_v2_preflight(image, image_len, s, &plan))
        return 240;
    if (doom_engine_ex2_make_wad(wad_accounts, wad_lens, &sim2_native_wad,
                                 &sim2_native_wad_bytes, sim2_native_hash) ||
        memcmp(sim2_native_hash, r2_wad_hash, 32)) { doom_resource_sim_session_close(); return 234; }
    sim2_native_carrier = (uint8_t *)calloc(1, 4u * 1024u * 1024u);
    if (!sim2_native_carrier) { doom_resource_sim_session_close(); return 238; }
    memset(DOOM_CTX, 0, sizeof(doom_ctx_t)); doom_ctx_init();
    if (!doom_sbf_bind_arena(sim2_native_carrier, 4u * 1024u * 1024u, DOOM_ZONE_BYTES)) { doom_resource_sim_session_close(); return 234; }
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0; displayplayer = 0; myargc = 0; myargv = 0;
    Z_Init(); P_InitThinkers(); W_InitAccountImage(sim2_native_wad, (int)sim2_native_wad_bytes);
    if (doom_resource_ri1_import_graph(image + s[R2_GRAPH].offset, s[R2_GRAPH].bytes, &anchor)) { doom_resource_sim_session_close(); return 242; }
    lastanim = anims + plan.scalar[R2_SC_ANIMCOUNT]; numlinespecials = 0;
    for (rc = 0; rc < DOOM_CTX->numlines && numlinespecials < MAXLINEANIMS; rc++)
        if (lines[rc].special == 48) linespeciallist[numlinespecials++] = &lines[rc];
    rc = r2_walk((const uint8_t *)(image + s[R2_SCALARS].offset), s[R2_SCALARS].bytes, s[R2_SCALARS].count, 1, r2_apply_scalar, 0);
    if (rc || r2_v2_alloc_resources(&plan) ||
        r2_walk(image + s[R2_TABLES].offset, s[R2_TABLES].bytes, s[R2_TABLES].count, 0, r2_v2_apply_table, 0) ||
        r2_v2_apply_lights(&plan) || r2_apply_functions(image, s)) { doom_resource_sim_session_close(); return 242; }
    sim2_native_frame = (uint8_t *)calloc(1, 4u * RI2_FRAME_BYTES);
    if (!sim2_native_frame) { doom_resource_sim_session_close(); return 238; }
    screens[0] = sim2_native_frame; screens[1] = sim2_native_frame + RI2_FRAME_BYTES;
    screens[2] = sim2_native_frame + 2u * RI2_FRAME_BYTES; screens[3] = sim2_native_frame + 3u * RI2_FRAME_BYTES; screens[4] = 0;
    viewimage = screens[0]; for (rc = 0; rc < viewheight; rc++) ylookup[rc] = screens[0] + (size_t)rc * 320u;
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 : (SCREENHEIGHT - R2_SBARHEIGHT - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH; pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0; setsizeneeded = false;
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump); if (demo_len < 17) { doom_resource_sim_session_close(); return 234; }
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC); defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo; demoend = demo + demo_len;
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len); DoomStateSetAllocationFailure(-1);
    if (DoomStateDecode(state, state_len) || DoomControllerValidateCapturedDemoCursor(demo_p)) { doom_resource_sim_session_close(); return 232; }
    viewwindowx = (SCREENWIDTH - scaledviewwidth) >> 1;
    viewwindowy = scaledviewwidth == SCREENWIDTH ? 0 : (SCREENHEIGHT - R2_SBARHEIGHT - viewheight) >> 1;
    pspritescale = FRACUNIT * viewwidth / SCREENWIDTH; pspriteiscale = FRACUNIT * SCREENWIDTH / viewwidth;
    clipangle = xtoviewangle[0]; viewactive = true; fixedcolormap = 0; setsizeneeded = false;
    sim2_native_open = 1;
    return 0;
}

/* Same contract as doom_sbf_sim2_step: 0 ran, 1..5 halted before G_Ticker,
 * 232 refused command, 237 arm failure, 238 the tic ran without consuming the
 * armed command. */
int doom_resource_sim_session_step(const unsigned char *command)
{
    int halt;
    if (!sim2_native_open) return 232;
    halt = doom_sim2_pretic_guard(command);
    if (halt) return halt;
    if (DoomControllerArmV2(command)) return 237;
    G_Ticker();
    if (!DoomControllerConsumed()) return 238;
    gametic++;
    return 0;
}

/* The whole player view at the resident gametic, row-major 320x200, the same
 * bytes the eight chain strips reassemble.  Host-only; the render cost path
 * is untouched, this calls the same R_RenderPlayerView the strips call. */
int doom_resource_sim_session_frame(unsigned char *out, unsigned long cap)
{
    if (!sim2_native_open || !out || cap < RI2_FRAME_BYTES) return 232;
    R_RenderPlayerView(&players[displayplayer]);
    memcpy(out, screens[0], RI2_FRAME_BYTES);
    return 0;
}

/* Same outputs as doom_sbf_sim2_publish: MP1 derived from the live thinker
 * graph, ST1 encoded, the ST1 tail up to state_cap zeroed. */
int doom_resource_sim_session_publish(
    unsigned char *state_out, unsigned long state_cap, unsigned long *state_len_out,
    unsigned char *mirror_out, unsigned long mirror_cap)
{
    uint8_t *encoded = 0; size_t encoded_bytes = 0;
    if (!sim2_native_open || !state_out || !state_len_out || !mirror_out ||
        state_cap < RI2_STATE_MAX || mirror_cap < RI2_MIRROR_BYTES) return 232;
    if (doom_engine_ex2_fill_mirror(mirror_out, sim2_native_wad, sim2_native_wad_bytes, sim2_native_hash))
        return 233;
    if (DoomStateEncode(&encoded, &encoded_bytes) || encoded_bytes > state_cap || encoded_bytes < 64u) {
        DoomStateFree(encoded); return 238;
    }
    memcpy(state_out, encoded, encoded_bytes);
    memset(state_out + encoded_bytes, 0, state_cap - encoded_bytes);
    *state_len_out = encoded_bytes;
    DoomStateFree(encoded);
    return 0;
}

/* Engine-level probe fields the host gate records per tic (not committed). */
int doom_resource_sim_session_probe(long out[6])
{
    if (!sim2_native_open || !out) return 232;
    out[0] = gametic; out[1] = gamestate; out[2] = gameaction;
    out[3] = players[0].playerstate; out[4] = players[0].health;
    out[5] = (long)(demo_p - demobuffer);
    return 0;
}
