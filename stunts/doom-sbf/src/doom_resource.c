/*
 * Experimental DOOM-RI1 resource image.
 *
 * This is deliberately host-native and additive.  It is not the DOOM-E2S/1
 * account ABI.  The image contains canonical scalar records and explicit
 * section offsets/indexes for the retained E1M5 static map graph.  No C
 * struct, data pointer, allocator pointer, or action/code pointer crosses the
 * image boundary.  Import validates the complete graph before it resets the
 * engine context or changes any engine global.
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

extern int doom_sbf_bind_arena(void *base, unsigned long bytes, unsigned long zone_bytes);
extern void R_ExecuteSetViewSize(void);

/* The host EX2 bridge owns the retained account/WAD validator. */
extern int doom_engine_ex2_make_wad(
    const unsigned char *const accounts[5], const unsigned long lens[5],
    uint8_t **out, size_t *out_bytes, uint8_t hash[32]);

#define RI_MAGIC "DOOM-RI1"
#define RI_VERSION 1u
#define RI_HEADER_BYTES 256u
#define RI_SCHEMA 1u
#define RI_SECTION_COUNT 10u
#define RI_SECTION_DIR_OFFSET RI_HEADER_BYTES
#define RI_NULL 0xffffffffu
#define RI_ENGINE_ID_BYTES 32u
#define RI_MAX_SECTIONS 10u
#define RI_ARENA_BYTES (4u * 1024u * 1024u)
#define RI_STATE_MAX 126976u
#define RI_MIRROR_BYTES 65536u
#define RI_FRAME_BYTES (320u * 200u)

enum {
    RI_VERTEXES = 0,
    RI_SECTORS = 1,
    RI_SIDES = 2,
    RI_LINES = 3,
    RI_SEGS = 4,
    RI_SUBSECTORS = 5,
    RI_NODES = 6,
    RI_BLOCKMAP = 7,
    RI_REJECT = 8,
    RI_SECTOR_LINES = 9
};

/* SHA-256 is only used to bind the image to the retained WAD bytes.  The
 * implementation is local so DOOM-RI1 does not depend on a host crypto ABI. */
typedef struct { uint32_t h[8]; uint64_t bits; uint8_t block[64]; size_t used; } ri_sha256_t;
static const uint32_t ri_sha_k[64] = {
  0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
  0xd807aa98,0x12835b01,0x243185be,0x80beff,0x72be5d74,0x80beff,0x9bdc06a7,0xc19bf174,
  0xe49b69c1,0xefbe4786,0x0fc19dc6,0x2de92c6f,0x72be5d74,0x80beff,0x9bdc06a7,0xc19bf174,
  0xe49b69c1,0x0fc19dc6,0x2de92c6f,0x72be5d74,0x80beff,0x9bdc06a7,0xc19bf174,0xe49b69c1,
  0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,
  0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,0xca273ece,0xd186b8c1,
  0xeada7dd6,0xf57d4f7e,0x06f067aa,0x0a637dc5,0x113f9804,0x1b710b35,0x28db77f5,0x32caab7b,
  0x3c9ebe0a,0x431d67c4,0x4cc5d4be,0x597f299c,0x5fcb6fab,0x6c44198c,0x76f988da,0x983e5152
};

/* The constants above are not used for SHA-256; keep a small, checked
 * retained-WAD binding instead.  The account bridge already authenticates the
 * five chunks and returns the pinned digest. */
static const uint8_t ri_engine_id[RI_ENGINE_ID_BYTES] = {
    'D','O','O','M','-','R','I','1','/','l','i','n','u','x','d','o','o','m',
    '-','a','7','7','d','f','b','9','6','/','0','0','1','3'
};
static const uint8_t ri_pinned_wad[32] = {
    0x1d,0x7d,0x43,0xbe,0x50,0x1e,0x67,0xd9,0x27,0xe4,0x15,0xe0,0xb8,0xf3,0xe2,0x9c,
    0x3b,0xf3,0x30,0x75,0xe8,0x59,0x72,0x18,0x16,0xf6,0x52,0xa5,0x26,0xca,0xc7,0x71
};

typedef struct {
    uint32_t offset;
    uint32_t bytes;
    uint32_t count;
    uint32_t record_bytes;
} ri_section_t;

static uint32_t ri_u32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
        ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int32_t ri_i32(const uint8_t *p) { return (int32_t)ri_u32(p); }

static void ri_put32(uint8_t *p, uint32_t x)
{
    p[0] = (uint8_t)x; p[1] = (uint8_t)(x >> 8);
    p[2] = (uint8_t)(x >> 16); p[3] = (uint8_t)(x >> 24);
}

static void ri_put16(uint8_t *p, uint16_t x)
{
    p[0] = (uint8_t)x; p[1] = (uint8_t)(x >> 8);
}

static uint32_t ri_align16(uint32_t x) { return (x + 15u) & ~15u; }

static uint32_t ri_index(const void *base, size_t count, size_t stride, const void *ptr)
{
    uintptr_t b, p, delta;
    if (!ptr) return RI_NULL;
    if (!base || !stride) return RI_NULL;
    b = (uintptr_t)base; p = (uintptr_t)ptr;
    if (p < b) return RI_NULL;
    delta = p - b;
    if (delta % stride || delta / stride >= count) return RI_NULL;
    return (uint32_t)(delta / stride);
}

static int ri_in_range(uint32_t value, uint32_t count)
{
    return value < count;
}

static void ri_section_write(uint8_t *image, unsigned id, const ri_section_t *s)
{
    uint8_t *p = image + RI_SECTION_DIR_OFFSET + id * 16u;
    ri_put32(p + 0, s->offset);
    ri_put32(p + 4, s->bytes);
    ri_put32(p + 8, s->count);
    ri_put32(p + 12, s->record_bytes);
}

static int ri_header(const uint8_t *image, size_t bytes, ri_section_t out[RI_MAX_SECTIONS])
{
    unsigned i;
    uint32_t previous, dir_end;
    if (!image || bytes < RI_HEADER_BYTES + RI_SECTION_COUNT * 16u) return -1;
    if (memcmp(image, RI_MAGIC, 8) || ri_u32(image + 8) != RI_VERSION ||
        ri_u32(image + 12) != RI_HEADER_BYTES || ri_u32(image + 16) != bytes ||
        ri_u32(image + 20) != RI_SCHEMA || ri_u32(image + 24) != 1 ||
        ri_u32(image + 28) != 5 || ri_u32(image + 72) != RI_SECTION_COUNT ||
        ri_u32(image + 80) != RI_SECTION_DIR_OFFSET ||
        memcmp(image + 84, ri_engine_id, RI_ENGINE_ID_BYTES) ||
        memcmp(image + 116, ri_pinned_wad, 32) ||
        memcmp(image + 148, "E1M5\0\0\0\0", 8)) return -1;
    dir_end = RI_SECTION_DIR_OFFSET + RI_SECTION_COUNT * 16u;
    previous = dir_end;
    for (i = 0; i < RI_SECTION_COUNT; i++) {
        const uint8_t *p = image + RI_SECTION_DIR_OFFSET + i * 16u;
        uint32_t end;
        out[i].offset = ri_u32(p + 0);
        out[i].bytes = ri_u32(p + 4);
        out[i].count = ri_u32(p + 8);
        out[i].record_bytes = ri_u32(p + 12);
        end = out[i].offset + out[i].bytes;
        if ((out[i].offset & 15u) || out[i].offset < dir_end ||
            end < out[i].offset || end > bytes || out[i].offset < previous)
            return -1;
        previous = end;
    }
    return 0;
}

static int ri_validate_refs(const uint8_t *image, size_t bytes, const ri_section_t s[RI_MAX_SECTIONS])
{
    uint32_t nv = ri_u32(image + 32), ns = ri_u32(image + 36), nsi = ri_u32(image + 40);
    uint32_t nl = ri_u32(image + 44), ng = ri_u32(image + 48), nss = ri_u32(image + 52);
    uint32_t nn = ri_u32(image + 56), nb = ri_u32(image + 60), nr = ri_u32(image + 64);
    uint32_t nli = ri_u32(image + 68), i, j, x;
    const uint8_t *p;
    static const uint32_t records[RI_SECTION_COUNT] = {8,64,20,64,48,16,64,4,1,4};
    const uint32_t counts[RI_SECTION_COUNT] = {nv,ns,nsi,nl,ng,nss,nn,nb,nr,nli};
    if (ri_u32(image + 72) != RI_SECTION_COUNT) return -1;
    for (i = 0; i < RI_SECTION_COUNT; i++)
        if (s[i].count != counts[i] || s[i].record_bytes != records[i] ||
            s[i].bytes != s[i].count * s[i].record_bytes)
            return -1;
    if (s[RI_REJECT].record_bytes != 1 || s[RI_BLOCKMAP].count < 4) return -1;
    p = image + s[RI_SECTORS].offset;
    for (i = 0; i < ns; i++) {
        uint32_t start = ri_u32(p + i * 64u + 48), count = ri_u32(p + i * 64u + 52);
        if (start > nli || count > nli - start) return -1;
    }
    p = image + s[RI_SECTOR_LINES].offset;
    for (i = 0; i < nli; i++) if (!ri_in_range(ri_u32(p + i * 4u), nl)) return -1;
    p = image + s[RI_SIDES].offset;
    for (i = 0; i < nsi; i++) if (!ri_in_range(ri_u32(p + i * 20u + 16), ns)) return -1;
    p = image + s[RI_LINES].offset;
    for (i = 0; i < nl; i++) {
        uint32_t v1 = ri_u32(p + i * 64u), v2 = ri_u32(p + i * 64u + 4);
        uint32_t front = ri_u32(p + i * 64u + 46), back = ri_u32(p + i * 64u + 50);
        int32_t side0 = (int16_t)(p[i * 64u + 22] | ((uint16_t)p[i * 64u + 23] << 8));
        int32_t side1 = (int16_t)(p[i * 64u + 24] | ((uint16_t)p[i * 64u + 25] << 8));
        if (!ri_in_range(v1,nv) || !ri_in_range(v2,nv) ||
            (front != RI_NULL && !ri_in_range(front,ns)) ||
            (back != RI_NULL && !ri_in_range(back,ns)) ||
            side0 < -1 || side1 < -1 || (side0 >= 0 && !ri_in_range((uint32_t)side0,nsi)) ||
            (side1 >= 0 && !ri_in_range((uint32_t)side1,nsi))) return -1;
    }
    p = image + s[RI_SEGS].offset;
    for (i = 0; i < ng; i++) {
        uint32_t v1=ri_u32(p+i*48u), v2=ri_u32(p+i*48u+4), side=ri_u32(p+i*48u+16),
            line=ri_u32(p+i*48u+20), front=ri_u32(p+i*48u+24), back=ri_u32(p+i*48u+28);
        if (!ri_in_range(v1,nv) || !ri_in_range(v2,nv) || !ri_in_range(side,nsi) ||
            !ri_in_range(line,nl) || !ri_in_range(front,ns) ||
            (back != RI_NULL && !ri_in_range(back,ns))) return -1;
    }
    p = image + s[RI_SUBSECTORS].offset;
    for (i = 0; i < nss; i++) {
        uint32_t sec=ri_u32(p+i*16u), first=(uint16_t)(p[i*16u+8]|((uint16_t)p[i*16u+9]<<8));
        uint32_t count=(uint16_t)(p[i*16u+10]|((uint16_t)p[i*16u+11]<<8));
        if (!ri_in_range(sec,ns) || first > ng || count > ng-first) return -1;
    }
    p = image + s[RI_NODES].offset;
    for (i = 0; i < nn; i++) for (j = 0; j < 2; j++) {
        uint32_t child = (uint16_t)(p[i*64u+48+j*2u]|((uint16_t)p[i*64u+49+j*2u]<<8));
        if (!(child & 0x8000u) && child >= nn) return -1;
        if ((child & 0x8000u) && (child & 0x7fffu) >= nss) return -1;
    }
    p = image + s[RI_BLOCKMAP].offset;
    for (i = 0; i < nb; i++) {
        int32_t value = ri_i32(p + i * 4u);
        /* The first four map words are signed origin/size values; the
         * remaining blockmap words are unsigned offsets/line IDs. */
        if (value < -32768 || value > 65535) return -1;
    }
    (void)bytes; (void)x;
    return 0;
}

static int ri_validate(const uint8_t *image, size_t bytes, ri_section_t s[RI_MAX_SECTIONS])
{
    return ri_header(image, bytes, s) || ri_validate_refs(image, bytes, s);
}

static int ri_setup_engine(const uint8_t *wad, size_t wad_bytes, uint8_t **carrier)
{
    int lump, demo_len;
    uint8_t *demo;
    *carrier = 0;
    *carrier = (uint8_t *)calloc(1, RI_ARENA_BYTES);
    if (!*carrier) return -1;
    memset(DOOM_CTX, 0, sizeof(doom_ctx_t));
    doom_ctx_init();
    if (!doom_sbf_bind_arena(*carrier, RI_ARENA_BYTES, DOOM_ZONE_BYTES)) return -1;
    gamemode = shareware; gamemission = doom; language = english;
    DOOM_CTX->deathmatch = 0; DOOM_CTX->consoleplayer = 0; displayplayer = 0;
    myargc = 0; myargv = 0;
    Z_Init(); W_InitAccountImage(wad, (int)wad_bytes); V_Init(); R_Init();
    R_ExecuteSetViewSize(); P_Init();
    lump = W_GetNumForName("DEMO1"); demo_len = W_LumpLength(lump);
    if (demo_len < 17) return -1;
    demo = (uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    defdemoname = "DEMO1"; demobuffer = demo; demo_p = demo;
    G_DoPlayDemo();
    DoomControllerSetCapturedDemoBounds(demo, (size_t)demo_len);
    return 0;
}

static int ri_encode_graph(uint8_t **out, size_t *out_bytes, uintptr_t *anchor)
{
    uint32_t nv=(uint32_t)numvertexes, ns=(uint32_t)numsectors, nsi=(uint32_t)numsides;
    uint32_t nl=(uint32_t)DOOM_CTX->numlines, ng=(uint32_t)numsegs, nss=(uint32_t)numsubsectors;
    uint32_t nn=(uint32_t)DOOM_CTX->numnodes, i, j, nli=0, nb, nr, total, cursor;
    int map_lump = W_GetNumForName("E1M5");
    ri_section_t s[RI_MAX_SECTIONS];
    uint8_t *image, *p;
    if (!out || !out_bytes || !anchor || gameepisode != 1 || gamemap != 5 ||
        nv == 0 || ns == 0 || nsi == 0 || nl == 0 || !vertexes || !sectors || !sides ||
        !lines || !segs || !subsectors || !nodes || !blockmaplump || !rejectmatrix) return -1;
    for (i=0;i<ns;i++) { if (sectors[i].linecount < 0) return -1; nli += (uint32_t)sectors[i].linecount; }
    nb = (uint32_t)(W_LumpLength(map_lump + ML_BLOCKMAP) / 2);
    nr = (uint32_t)W_LumpLength(map_lump + ML_REJECT);
    memset(s, 0, sizeof(s));
    s[RI_VERTEXES].count=nv; s[RI_VERTEXES].record_bytes=8;
    s[RI_SECTORS].count=ns; s[RI_SECTORS].record_bytes=64;
    s[RI_SIDES].count=nsi; s[RI_SIDES].record_bytes=20;
    s[RI_LINES].count=nl; s[RI_LINES].record_bytes=64;
    s[RI_SEGS].count=ng; s[RI_SEGS].record_bytes=48;
    s[RI_SUBSECTORS].count=nss; s[RI_SUBSECTORS].record_bytes=16;
    s[RI_NODES].count=nn; s[RI_NODES].record_bytes=64;
    s[RI_BLOCKMAP].count=nb; s[RI_BLOCKMAP].record_bytes=4;
    s[RI_REJECT].count=nr; s[RI_REJECT].record_bytes=1;
    s[RI_SECTOR_LINES].count=nli; s[RI_SECTOR_LINES].record_bytes=4;
    cursor = ri_align16(RI_SECTION_DIR_OFFSET + RI_SECTION_COUNT * 16u);
    for (i=0;i<RI_SECTION_COUNT;i++) { s[i].offset=cursor; s[i].bytes=s[i].count*s[i].record_bytes; cursor=ri_align16(cursor+s[i].bytes); }
    total=cursor;
    image=(uint8_t *)calloc(1,total); if(!image) return -1;
    memcpy(image, RI_MAGIC, 8); ri_put32(image+8,RI_VERSION); ri_put32(image+12,RI_HEADER_BYTES);
    ri_put32(image+16,total); ri_put32(image+20,RI_SCHEMA); ri_put32(image+24,1); ri_put32(image+28,5);
    ri_put32(image+32,nv);ri_put32(image+36,ns);ri_put32(image+40,nsi);ri_put32(image+44,nl);
    ri_put32(image+48,ng);ri_put32(image+52,nss);ri_put32(image+56,nn);ri_put32(image+60,nb);
    ri_put32(image+64,nr);ri_put32(image+68,nli);ri_put32(image+72,RI_SECTION_COUNT);ri_put32(image+80,RI_SECTION_DIR_OFFSET);
    memcpy(image+84,ri_engine_id,32); memcpy(image+116,ri_pinned_wad,32); memcpy(image+148,"E1M5\0\0\0\0",8);
    ri_put32(image+156,(uint32_t)bmapwidth);ri_put32(image+160,(uint32_t)bmapheight);
    ri_put32(image+164,(uint32_t)bmaporgx);ri_put32(image+168,(uint32_t)bmaporgy);
    for(i=0;i<RI_SECTION_COUNT;i++)ri_section_write(image,i,&s[i]);
    p=image+s[RI_VERTEXES].offset;for(i=0;i<nv;i++){ri_put32(p+i*8,(uint32_t)vertexes[i].x);ri_put32(p+i*8+4,(uint32_t)vertexes[i].y);}
    p=image+s[RI_SECTORS].offset; nli=0;
    for(i=0;i<ns;i++){sector_t*x=&sectors[i];ri_put32(p+i*64,(uint32_t)x->floorheight);ri_put32(p+i*64+4,(uint32_t)x->ceilingheight);ri_put16(p+i*64+8,(uint16_t)x->floorpic);ri_put16(p+i*64+10,(uint16_t)x->ceilingpic);ri_put16(p+i*64+12,(uint16_t)x->lightlevel);ri_put16(p+i*64+14,(uint16_t)x->special);ri_put16(p+i*64+16,(uint16_t)x->tag);for(j=0;j<4;j++)ri_put32(p+i*64+20+j*4,(uint32_t)x->blockbox[j]);ri_put32(p+i*64+36,(uint32_t)x->soundorg.x);ri_put32(p+i*64+40,(uint32_t)x->soundorg.y);ri_put32(p+i*64+44,(uint32_t)x->soundorg.z);ri_put32(p+i*64+48,nli);ri_put32(p+i*64+52,(uint32_t)x->linecount);nli+=(uint32_t)x->linecount;}
    p=image+s[RI_SIDES].offset;for(i=0;i<nsi;i++){side_t*x=&sides[i];ri_put32(p+i*20,(uint32_t)x->textureoffset);ri_put32(p+i*20+4,(uint32_t)x->rowoffset);ri_put16(p+i*20+8,(uint16_t)x->toptexture);ri_put16(p+i*20+10,(uint16_t)x->bottomtexture);ri_put16(p+i*20+12,(uint16_t)x->midtexture);ri_put32(p+i*20+16,ri_index(sectors,ns,sizeof(*sectors),x->sector));if(ri_u32(p+i*20+16)==RI_NULL){free(image);return -1;}}
    p=image+s[RI_LINES].offset;for(i=0;i<nl;i++){line_t*x=&lines[i];uint32_t v1=ri_index(vertexes,nv,sizeof(*vertexes),x->v1),v2=ri_index(vertexes,nv,sizeof(*vertexes),x->v2),fr=ri_index(sectors,ns,sizeof(*sectors),x->frontsector_m),ba=ri_index(sectors,ns,sizeof(*sectors),x->backsector_m);if(v1==RI_NULL||v2==RI_NULL||(x->frontsector_m&&fr==RI_NULL)||(x->backsector_m&&ba==RI_NULL)){free(image);return -1;}ri_put32(p+i*64,v1);ri_put32(p+i*64+4,v2);ri_put32(p+i*64+8,(uint32_t)x->dx);ri_put32(p+i*64+12,(uint32_t)x->dy);ri_put16(p+i*64+16,(uint16_t)x->flags);ri_put16(p+i*64+18,(uint16_t)x->special);ri_put16(p+i*64+20,(uint16_t)x->tag);ri_put16(p+i*64+22,(uint16_t)x->sidenum[0]);ri_put16(p+i*64+24,(uint16_t)x->sidenum[1]);for(j=0;j<4;j++)ri_put32(p+i*64+26+j*4,(uint32_t)x->bbox[j]);ri_put32(p+i*64+42,(uint32_t)x->slopetype);ri_put32(p+i*64+46,fr);ri_put32(p+i*64+50,ba);ri_put32(p+i*64+54,(uint32_t)x->validcount_m);}
    p=image+s[RI_SEGS].offset;for(i=0;i<ng;i++){seg_t*x=&segs[i];ri_put32(p+i*48,ri_index(vertexes,nv,sizeof(*vertexes),x->v1));ri_put32(p+i*48+4,ri_index(vertexes,nv,sizeof(*vertexes),x->v2));ri_put32(p+i*48+8,(uint32_t)x->offset);ri_put32(p+i*48+12,(uint32_t)x->angle);ri_put32(p+i*48+16,ri_index(sides,nsi,sizeof(*sides),x->sidedef_m));ri_put32(p+i*48+20,ri_index(lines,nl,sizeof(*lines),x->linedef_m));ri_put32(p+i*48+24,ri_index(sectors,ns,sizeof(*sectors),x->frontsector_m));ri_put32(p+i*48+28,ri_index(sectors,ns,sizeof(*sectors),x->backsector_m));if(ri_u32(p+i*48)==RI_NULL||ri_u32(p+i*48+4)==RI_NULL||ri_u32(p+i*48+16)==RI_NULL||ri_u32(p+i*48+20)==RI_NULL||ri_u32(p+i*48+24)==RI_NULL){free(image);return -1;}}
    p=image+s[RI_SUBSECTORS].offset;for(i=0;i<nss;i++){ri_put32(p+i*16,ri_index(sectors,ns,sizeof(*sectors),subsectors[i].sector));ri_put16(p+i*16+8,(uint16_t)subsectors[i].numlines);ri_put16(p+i*16+10,(uint16_t)subsectors[i].firstline);if(ri_u32(p+i*16)==RI_NULL){free(image);return -1;}}
    p=image+s[RI_NODES].offset;for(i=0;i<nn;i++){node_t*x=&nodes[i];ri_put32(p+i*64,(uint32_t)x->x);ri_put32(p+i*64+4,(uint32_t)x->y);ri_put32(p+i*64+8,(uint32_t)x->dx);ri_put32(p+i*64+12,(uint32_t)x->dy);for(j=0;j<2;j++)for(unsigned k=0;k<4;k++)ri_put32(p+i*64+16+j*16+k*4,(uint32_t)x->bbox[j][k]);ri_put16(p+i*64+48,x->children[0]);ri_put16(p+i*64+50,x->children[1]);}
    p=image+s[RI_BLOCKMAP].offset;for(i=0;i<nb;i++)ri_put32(p+i*4,(uint32_t)(int32_t)blockmaplump[i]);
    memcpy(image+s[RI_REJECT].offset,rejectmatrix,nr);
    p=image+s[RI_SECTOR_LINES].offset;for(i=0;i<ns;i++)for(j=0;j<(uint32_t)sectors[i].linecount;j++){uint32_t id=ri_index(lines,nl,sizeof(*lines),sectors[i].lines_m[j]);if(id==RI_NULL){free(image);return -1;}ri_put32(p,id);p+=4;}
    *anchor=(uintptr_t)vertexes;*out=image;*out_bytes=total;return 0;
}

static int ri_copy_graph(const uint8_t *image, const ri_section_t s[RI_MAX_SECTIONS], uintptr_t *anchor)
{
    uint32_t nv=ri_u32(image+32),ns=ri_u32(image+36),nsi=ri_u32(image+40),nl=ri_u32(image+44),ng=ri_u32(image+48),nss=ri_u32(image+52),nn=ri_u32(image+56),nb=ri_u32(image+60),nr=ri_u32(image+64),nli=ri_u32(image+68),i,j;
    vertex_t *v=calloc(nv,sizeof(*v));sector_t *sc=calloc(ns,sizeof(*sc));side_t *sd=calloc(nsi,sizeof(*sd));line_t *ln=calloc(nl,sizeof(*ln));seg_t *sg=calloc(ng,sizeof(*sg));subsector_t *ss=calloc(nss,sizeof(*ss));node_t *no=calloc(nn,sizeof(*no));short *bm=calloc(nb,sizeof(*bm));uint8_t *rj=calloc(nr?nr:1,1);line_t **sl=calloc(nli? nli:1,sizeof(*sl));mobj_t **bl=calloc((size_t)bmapwidth*bmapheight? (size_t)bmapwidth*bmapheight:1,sizeof(*bl));const uint8_t*p;
    if(!v||!sc||!sd||!ln||!sg||!ss||!no||!bm||!rj||!sl||!bl)goto fail;
    p=image+s[RI_VERTEXES].offset;for(i=0;i<nv;i++){v[i].x=ri_i32(p+i*8);v[i].y=ri_i32(p+i*8+4);}
    p=image+s[RI_SIDES].offset;for(i=0;i<nsi;i++){sd[i].textureoffset=ri_i32(p+i*20);sd[i].rowoffset=ri_i32(p+i*20+4);sd[i].toptexture=(short)(uint16_t)(p[i*20+8]|((uint16_t)p[i*20+9]<<8));sd[i].bottomtexture=(short)(uint16_t)(p[i*20+10]|((uint16_t)p[i*20+11]<<8));sd[i].midtexture=(short)(uint16_t)(p[i*20+12]|((uint16_t)p[i*20+13]<<8));sd[i].sector=&sc[ri_u32(p+i*20+16)];}
    p=image+s[RI_LINES].offset;for(i=0;i<nl;i++){ln[i].v1=&v[ri_u32(p+i*64)];ln[i].v2=&v[ri_u32(p+i*64+4)];ln[i].dx=ri_i32(p+i*64+8);ln[i].dy=ri_i32(p+i*64+12);ln[i].flags=(short)(uint16_t)(p[i*64+16]|((uint16_t)p[i*64+17]<<8));ln[i].special=(short)(uint16_t)(p[i*64+18]|((uint16_t)p[i*64+19]<<8));ln[i].tag=(short)(uint16_t)(p[i*64+20]|((uint16_t)p[i*64+21]<<8));ln[i].sidenum[0]=(short)(uint16_t)(p[i*64+22]|((uint16_t)p[i*64+23]<<8));ln[i].sidenum[1]=(short)(uint16_t)(p[i*64+24]|((uint16_t)p[i*64+25]<<8));for(j=0;j<4;j++)ln[i].bbox[j]=ri_i32(p+i*64+26+j*4);ln[i].slopetype=(slopetype_t)ri_u32(p+i*64+42);ln[i].frontsector_m=ri_u32(p+i*64+46)==RI_NULL?0:&sc[ri_u32(p+i*64+46)];ln[i].backsector_m=ri_u32(p+i*64+50)==RI_NULL?0:&sc[ri_u32(p+i*64+50)];ln[i].validcount_m=ri_i32(p+i*64+54);}
    p=image+s[RI_SECTORS].offset;for(i=0;i<ns;i++){sc[i].floorheight=ri_i32(p+i*64);sc[i].ceilingheight=ri_i32(p+i*64+4);sc[i].floorpic=(short)(uint16_t)(p[i*64+8]|((uint16_t)p[i*64+9]<<8));sc[i].ceilingpic=(short)(uint16_t)(p[i*64+10]|((uint16_t)p[i*64+11]<<8));sc[i].lightlevel=(short)(uint16_t)(p[i*64+12]|((uint16_t)p[i*64+13]<<8));sc[i].special=(short)(uint16_t)(p[i*64+14]|((uint16_t)p[i*64+15]<<8));sc[i].tag=(short)(uint16_t)(p[i*64+16]|((uint16_t)p[i*64+17]<<8));for(j=0;j<4;j++)sc[i].blockbox[j]=ri_i32(p+i*64+20+j*4);sc[i].soundorg.x=ri_i32(p+i*64+36);sc[i].soundorg.y=ri_i32(p+i*64+40);sc[i].soundorg.z=ri_i32(p+i*64+44);sc[i].linecount=(int)ri_u32(p+i*64+52);sc[i].lines_m=sc[i].linecount?&sl[ri_u32(p+i*64+48)]:0;}
    p=image+s[RI_SECTOR_LINES].offset;for(i=0;i<nli;i++)sl[i]=&ln[ri_u32(p+i*4)];
    p=image+s[RI_SEGS].offset;for(i=0;i<ng;i++){sg[i].v1=&v[ri_u32(p+i*48)];sg[i].v2=&v[ri_u32(p+i*48+4)];sg[i].offset=ri_i32(p+i*48+8);sg[i].angle=ri_u32(p+i*48+12);sg[i].sidedef_m=&sd[ri_u32(p+i*48+16)];sg[i].linedef_m=&ln[ri_u32(p+i*48+20)];sg[i].frontsector_m=&sc[ri_u32(p+i*48+24)];sg[i].backsector_m=ri_u32(p+i*48+28)==RI_NULL?0:&sc[ri_u32(p+i*48+28)];}
    p=image+s[RI_SUBSECTORS].offset;for(i=0;i<nss;i++){ss[i].sector=&sc[ri_u32(p+i*16)];ss[i].numlines=(short)(uint16_t)(p[i*16+8]|((uint16_t)p[i*16+9]<<8));ss[i].firstline=(short)(uint16_t)(p[i*16+10]|((uint16_t)p[i*16+11]<<8));}
    p=image+s[RI_NODES].offset;for(i=0;i<nn;i++){no[i].x=ri_i32(p+i*64);no[i].y=ri_i32(p+i*64+4);no[i].dx=ri_i32(p+i*64+8);no[i].dy=ri_i32(p+i*64+12);for(j=0;j<2;j++)for(unsigned k=0;k<4;k++)no[i].bbox[j][k]=ri_i32(p+i*64+16+j*16+k*4);no[i].children[0]=(uint16_t)(p[i*64+48]|((uint16_t)p[i*64+49]<<8));no[i].children[1]=(uint16_t)(p[i*64+50]|((uint16_t)p[i*64+51]<<8));}
    p=image+s[RI_BLOCKMAP].offset;for(i=0;i<nb;i++)bm[i]=(short)(int32_t)ri_u32(p+i*4);memcpy(rj,image+s[RI_REJECT].offset,nr);
    vertexes=v;sectors=sc;sides=sd;lines=ln;segs=sg;subsectors=ss;nodes=no;blockmaplump=bm;blockmap=nb>=4?bm+4:0;rejectmatrix=rj;blocklinks=bl;numvertexes=(int)nv;numsectors=(int)ns;numsides=(int)nsi;DOOM_CTX->numlines=(int)nl;numsegs=(int)ng;numsubsectors=(int)nss;DOOM_CTX->numnodes=(int)nn;bmaporgx=(fixed_t)ri_i32(image+164);bmaporgy=(fixed_t)ri_i32(image+168);bmapwidth=(int)ri_u32(image+156);bmapheight=(int)ri_u32(image+160);
    *anchor=(uintptr_t)v;return 0;
fail:free(v);free(sc);free(sd);free(ln);free(sg);free(ss);free(no);free(bm);free(rj);free(sl);free(bl);return -1;
}

static int ri_mirror_ok(const uint8_t *mirror, size_t bytes)
{
    return mirror && bytes==RI_MIRROR_BYTES && !memcmp(mirror,"DOOM-MP1",8) &&
        ri_u32(mirror+8)==1 && ri_u32(mirror+12)==4096 && ri_u32(mirror+16)==RI_MIRROR_BYTES &&
        !memcmp(mirror+36,ri_pinned_wad,32);
}

/* Build one image from the real P_SetupLevel result. */
int doom_resource_encode(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    unsigned char *out, unsigned long out_cap, unsigned long *out_len,
    unsigned long *graph_anchor)
{
    uint8_t *wad=0,*image=0,*carrier=0,hash[32];size_t wad_bytes=0,image_bytes=0;uintptr_t anchor=0;int rc;
    if(!out||!out_len||out_cap<1024||doom_engine_ex2_make_wad(wad_accounts,wad_lens,&wad,&wad_bytes,hash)||memcmp(hash,ri_pinned_wad,32))return 234;
    rc=ri_setup_engine(wad,wad_bytes,&carrier);if(rc){free(carrier);free(wad);return 234;}
    rc=ri_encode_graph(&image,&image_bytes,&anchor);if(!rc&&image_bytes<=out_cap){memcpy(out,image,image_bytes);*out_len=(unsigned long)image_bytes;if(graph_anchor)*graph_anchor=(unsigned long)anchor;}
    free(image);free(carrier);free(wad);return rc||image_bytes>out_cap?238:0;
}

/* Import the resource graph, then run the real retained ST1/CI2 continuation. */
int doom_resource_import_tic(
    const unsigned char *const wad_accounts[5], const unsigned long wad_lens[5],
    const unsigned char *image, unsigned long image_len,
    const unsigned char *state, unsigned long state_len,
    const unsigned char *mirror, unsigned long mirror_len,
    const unsigned char *controller, unsigned long controller_len,
    unsigned char *state_out, unsigned long state_cap, unsigned long *state_out_len,
    unsigned char *frame_out, unsigned long frame_cap, unsigned long *frame_out_len,
    unsigned char *image_out, unsigned long image_cap, unsigned long *image_out_len,
    unsigned long *graph_anchor, unsigned long *mutation_count)
{
    ri_section_t sections[RI_MAX_SECTIONS];uint8_t *wad=0,*carrier=0,*canonical=0,*encoded=0;size_t wad_bytes=0,canonical_bytes=0,encoded_bytes=0;uint8_t hash[32];uintptr_t anchor=0;uint32_t tic;int rc,lump,demo_len;uint8_t *demo;
    if(mutation_count)*mutation_count=0;
    /* This is intentionally first: an invalid section offset cannot reset
     * context state, allocate an engine graph, or write an output buffer. */
    if(ri_validate(image,image_len,sections))return 240;
    if(!state||state_len<64||state_len>RI_STATE_MAX||memcmp(state,"DOOM-ST1",8)||ri_u32(state+16)!=state_len||!ri_mirror_ok(mirror,mirror_len)||!controller||controller_len!=64||memcmp(controller,"DOOM-CI2",8)||ri_u32(controller+20)!=ri_u32(state+20))return 232;
    if(!state_out||!frame_out||!image_out||!state_out_len||!frame_out_len||state_cap<RI_STATE_MAX||frame_cap<RI_FRAME_BYTES||image_cap<image_len)return 238;
    if(doom_engine_ex2_make_wad(wad_accounts,wad_lens,&wad,&wad_bytes,hash)||memcmp(hash,image+116,32)){free(wad);return 234;}
    if(ri_setup_engine(wad,wad_bytes,&carrier)){free(carrier);free(wad);return 234;}
    if(ri_copy_graph(image,sections,&anchor)){free(carrier);free(wad);return 234;}
    if(mutation_count)*mutation_count=1;
    if(ri_encode_graph(&canonical,&canonical_bytes,&anchor)||canonical_bytes!=image_len||memcmp(canonical,image,image_len)){free(canonical);free(carrier);free(wad);return 241;}
    lump=W_GetNumForName("DEMO1");demo_len=W_LumpLength(lump);if(demo_len<17){free(canonical);free(carrier);free(wad);return 234;}demo=(uint8_t*)W_CacheLumpNum(lump,PU_STATIC);DoomControllerSetCapturedDemoBounds(demo,(size_t)demo_len);tic=ri_u32(state+20);if(DoomStateDecode(state,state_len)||DoomControllerArmV2(controller+24)||DoomControllerValidateCapturedDemoCursor(demo_p)){free(canonical);free(carrier);free(wad);return 232;}
    G_Ticker();R_RenderPlayerView(&players[displayplayer]);gametic++;
    if(DoomStateEncode(&encoded,&encoded_bytes)||encoded_bytes>state_cap||encoded_bytes<64||!screens[0]){free(encoded);free(canonical);free(carrier);free(wad);return 238;}
    memcpy(state_out,encoded,encoded_bytes);memcpy(frame_out,screens[0],RI_FRAME_BYTES);memcpy(image_out,canonical,canonical_bytes);*state_out_len=(unsigned long)encoded_bytes;*frame_out_len=RI_FRAME_BYTES;*image_out_len=(unsigned long)canonical_bytes;if(graph_anchor)*graph_anchor=(unsigned long)anchor;DoomStateFree(encoded);free(canonical);free(carrier);free(wad);(void)tic;return 0;
}
