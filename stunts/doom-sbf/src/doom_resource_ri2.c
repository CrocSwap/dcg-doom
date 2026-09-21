/*
 * DOOM-RI2/1 initialized-resource capture and native consumer.
 *
 * RI1/E2R1 is frozen in doom_resource.c.  RI2 adds an explicit, additive
 * capture of the source-matched Z/W/V/R/view/P/demo initialization boundary.
 * It contains only named scalar/table records, symbolic callback IDs, and
 * region-relative pointer relocations.  Native addresses, C function
 * addresses, allocator headers, and unspecified memory are never serialized.
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
#include "r_draw.h"
#include "r_things.h"
#include "v_video.h"
#include "w_wad.h"
#include "z_zone.h"
#include "doom_state.h"
#include "doom_controller.h"

extern int doom_engine_ex2_make_wad(
    const unsigned char *const accounts[5], const unsigned long lens[5],
    uint8_t **out, size_t *out_bytes, uint8_t hash[32]);
extern int doom_engine_ex2_fill_mirror(
    uint8_t *out, const uint8_t *wad, size_t wad_bytes, const uint8_t hash[32]);
extern int doom_resource_ri1_setup(const uint8_t *wad, size_t wad_bytes, uint8_t **carrier);
extern int doom_resource_ri1_encode_graph(uint8_t **out, size_t *out_bytes, uintptr_t *anchor);
extern int doom_resource_ri1_import_graph(const uint8_t *image, size_t image_bytes,
                                          uintptr_t *anchor);

#define RI2_MAGIC "DOOM-RI2"
#define RI2_VERSION 1u
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
    R2_SC_PRECACHE, R2_SC_DEMOPLAYBACK
};

enum {
    R2_T_VIEWANGLETOX = 1, R2_T_XTOVIEWANGLE, R2_T_YSLOPE,
    R2_T_DISTSCALE, R2_T_SCREENHEIGHTARRAY, R2_T_COLUMNOFS,
    R2_T_SWITCHLIST, R2_T_ANIMS, R2_T_TRANSLATIONS,
    R2_T_TEXTUREWIDTHMASK, R2_T_TEXTUREHEIGHT, R2_T_TEXTURECOMPOSITESIZE,
    R2_T_TEXTURETRANSLATION, R2_T_FLATTRANSLATION,
    R2_T_SPRITEWIDTH, R2_T_SPRITEOFFSET, R2_T_SPRITETOPOFFSET,
    R2_T_SPRTEMP
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

static int r2_sections(const uint8_t *image, size_t bytes, r2_section_t out[5])
{
    uint32_t previous, directory_end, end; unsigned i;
    if (!image || bytes < RI2_HEADER_BYTES + RI2_SECTION_COUNT * 16u ||
        memcmp(image, RI2_MAGIC, 8) || r2_u32(image + 8) != RI2_VERSION ||
        r2_u32(image + 12) != RI2_HEADER_BYTES || r2_u32(image + 16) != bytes ||
        r2_u32(image + 20) != RI2_SCHEMA || r2_u32(image + 24) != 1 ||
        r2_u32(image + 28) != 5 || r2_u32(image + 32) != RI2_SECTION_COUNT ||
        r2_u32(image + 36) != RI2_SECTION_DIR_OFFSET ||
        memcmp(image + 40, r2_engine_id, RI2_ENGINE_ID_BYTES) ||
        memcmp(image + 72, r2_wad_hash, 32) || memcmp(image + 104, "E1M5\0\0\0\0", 8)) return -1;
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
    R2_SCALAR(demoplayback, R2_SC_DEMOPLAYBACK);
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
    memcpy(image + 104, "E1M5\0\0\0\0", 8); r2_put32(image + 112, scalars->count);
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

int doom_resource_import_initialized_tic(
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
    if (!image || image_len > RI2_MAX_BYTES || r2_sections(image, image_len, s) ||
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
