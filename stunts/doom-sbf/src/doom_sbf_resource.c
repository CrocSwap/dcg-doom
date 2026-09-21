/*
 * DOOM-E2R/1 resource-account continuation.
 *
 * The account carries the captured DOOM-RI1 scalar graph, not a native C
 * object graph.  This file validates section bounds/references before touching
 * DOOM_CTX, allocates fresh runtime objects from the current invocation's
 * zone, and rebuilds every resource pointer from an image index.  No pointer
 * or mutable static is read from the resource account.
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
#include "p_local.h"
#include "r_defs.h"
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
extern int doom_sbf_fill_pool(uint8_t *pool, const uint8_t wad_hash[32]);
extern void R_ExecuteSetViewSize(void);

#define E2R_REFUSE_STATE 232
#define E2R_REFUSE_POOL 233
#define E2R_REFUSE_RESOURCE 234
#define E2R_REFUSE_IMAGE 235
#define E2R_REFUSE_CONTROLLER 237
#define E2R_REFUSE_OUTPUT 238
#define E2R_REFUSE_ARENA 239

#define RI_HEADER_BYTES 256u
#define RI_SECTION_COUNT 10u
#define RI_SECTION_DIR_OFFSET 256u
#define RI_NULL 0xffffffffu
#define RI_STATE_MAX 126976u
#define RI_MIRROR_BYTES 65536u
#define RI_POOL_HEADER_BYTES 36u
#define RI_POOL_SLOT_BYTES 128u
#define RI_POOL_SLOTS 384u
#define RI_WAD_BYTES 4196020u
#define RI_WAD_HASH_OFFSET 116u
#define RI_LOW_ARENA_BYTES (1024u * 1024u)
#define RI_ZONE_BYTES (3u * 1024u * 1024u)

enum {
    RI_VERTEXES = 0, RI_SECTORS = 1, RI_SIDES = 2, RI_LINES = 3,
    RI_SEGS = 4, RI_SUBSECTORS = 5, RI_NODES = 6, RI_BLOCKMAP = 7,
    RI_REJECT = 8, RI_SECTOR_LINES = 9
};

typedef struct {
    uint32_t offset, bytes, count, record_bytes;
} e2r_section_t;

static const uint8_t e2r_engine_id[32] = {
    'D','O','O','M','-','R','I','1','/','l','i','n','u','x','d','o',
    'o','m','-','a','7','7','d','f','b','9','6','/','0','0','1','3'
};
static const uint8_t e2r_wad_hash[32] = {
    0x1d,0x7d,0x43,0xbe,0x50,0x1e,0x67,0xd9,0x27,0xe4,0x15,0xe0,0xb8,0xf3,0xe2,0x9c,
    0x3b,0xf3,0x30,0x75,0xe8,0x59,0x72,0x18,0x16,0xf6,0x52,0xa5,0x26,0xca,0xc7,0x71
};

static uint32_t e2r_u32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
        ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int32_t e2r_i32(const uint8_t *p) { return (int32_t)e2r_u32(p); }

static uint16_t e2r_u16(const uint8_t *p)
{
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static int e2r_in(uint32_t value, uint32_t count) { return value < count; }

static int e2r_zero(const uint8_t *p, size_t n)
{
    size_t i;
    for (i = 0; i < n; i++) if (p[i]) return 0;
    return 1;
}

static int e2r_header(const uint8_t *image, size_t bytes,
                      e2r_section_t out[RI_SECTION_COUNT])
{
    static const uint32_t records[RI_SECTION_COUNT] = { 8,64,20,64,48,16,64,4,1,4 };
    unsigned i;
    uint32_t previous, dir_end;
    if (!image || bytes < RI_HEADER_BYTES + RI_SECTION_COUNT * 16u ||
        memcmp(image, "DOOM-RI1", 8) || e2r_u32(image + 8) != 1 ||
        e2r_u32(image + 12) != RI_HEADER_BYTES || e2r_u32(image + 16) != bytes ||
        e2r_u32(image + 20) != 1 || e2r_u32(image + 24) != 1 ||
        e2r_u32(image + 28) != 5 || e2r_u32(image + 72) != RI_SECTION_COUNT ||
        e2r_u32(image + 80) != RI_SECTION_DIR_OFFSET ||
        memcmp(image + 84, e2r_engine_id, 32) || memcmp(image + RI_WAD_HASH_OFFSET, e2r_wad_hash, 32) ||
        memcmp(image + 148, "E1M5\0\0\0\0", 8)) return -1;
    dir_end = RI_SECTION_DIR_OFFSET + RI_SECTION_COUNT * 16u;
    previous = dir_end;
    for (i = 0; i < RI_SECTION_COUNT; i++) {
        const uint8_t *p = image + RI_SECTION_DIR_OFFSET + i * 16u;
        uint32_t end;
        out[i].offset = e2r_u32(p);
        out[i].bytes = e2r_u32(p + 4);
        out[i].count = e2r_u32(p + 8);
        out[i].record_bytes = e2r_u32(p + 12);
        if (out[i].record_bytes != records[i] ||
            out[i].count > 0xffffffffu / records[i] ||
            out[i].bytes != out[i].count * records[i]) return -1;
        end = out[i].offset + out[i].bytes;
        if ((out[i].offset & 15u) || out[i].offset < dir_end ||
            end < out[i].offset || end > bytes || out[i].offset < previous) return -1;
        previous = end;
    }
    return 0;
}

static int e2r_refs(const uint8_t *image, const e2r_section_t s[RI_SECTION_COUNT])
{
    static const uint32_t records[RI_SECTION_COUNT] = { 8,64,20,64,48,16,64,4,1,4 };
    uint32_t nv=e2r_u32(image+32), ns=e2r_u32(image+36), nsi=e2r_u32(image+40);
    uint32_t nl=e2r_u32(image+44), ng=e2r_u32(image+48), nss=e2r_u32(image+52);
    uint32_t nn=e2r_u32(image+56), nb=e2r_u32(image+60), nr=e2r_u32(image+64);
    uint32_t nli=e2r_u32(image+68), i, j;
    const uint32_t counts[RI_SECTION_COUNT] = { nv,ns,nsi,nl,ng,nss,nn,nb,nr,nli };
    const uint8_t *p;
    for (i=0;i<RI_SECTION_COUNT;i++)
        if (s[i].count != counts[i] || s[i].record_bytes != records[i] ||
            s[i].bytes != s[i].count * s[i].record_bytes) return -1;
    if (s[RI_REJECT].record_bytes != 1 || s[RI_BLOCKMAP].count < 4) return -1;
    p=image+s[RI_SECTORS].offset;
    for (i=0;i<ns;i++) {
        uint32_t start=e2r_u32(p+i*64u+48), count=e2r_u32(p+i*64u+52);
        if (start>nli || count>nli-start) return -1;
    }
    p=image+s[RI_SECTOR_LINES].offset;
    for(i=0;i<nli;i++) if(!e2r_in(e2r_u32(p+i*4u),nl)) return -1;
    p=image+s[RI_SIDES].offset;
    for(i=0;i<nsi;i++) if(!e2r_in(e2r_u32(p+i*20u+16),ns)) return -1;
    p=image+s[RI_LINES].offset;
    for(i=0;i<nl;i++) {
        uint32_t v1=e2r_u32(p+i*64u), v2=e2r_u32(p+i*64u+4);
        uint32_t front=e2r_u32(p+i*64u+46), back=e2r_u32(p+i*64u+50);
        int32_t side0=(int16_t)e2r_u16(p+i*64u+22), side1=(int16_t)e2r_u16(p+i*64u+24);
        if(!e2r_in(v1,nv)||!e2r_in(v2,nv)||
           (front!=RI_NULL&&!e2r_in(front,ns))||(back!=RI_NULL&&!e2r_in(back,ns))||
           side0 < -1 || side1 < -1 || (side0 >= 0 && !e2r_in((uint32_t)side0,nsi)) ||
           (side1 >= 0 && !e2r_in((uint32_t)side1,nsi))) return -1;
    }
    p=image+s[RI_SEGS].offset;
    for(i=0;i<ng;i++) {
        uint32_t v1=e2r_u32(p+i*48u),v2=e2r_u32(p+i*48u+4),side=e2r_u32(p+i*48u+16);
        uint32_t line=e2r_u32(p+i*48u+20),front=e2r_u32(p+i*48u+24),back=e2r_u32(p+i*48u+28);
        if(!e2r_in(v1,nv)||!e2r_in(v2,nv)||!e2r_in(side,nsi)||!e2r_in(line,nl)||
           !e2r_in(front,ns)||(back!=RI_NULL&&!e2r_in(back,ns))) return -1;
    }
    p=image+s[RI_SUBSECTORS].offset;
    for(i=0;i<nss;i++) {
        uint32_t sec=e2r_u32(p+i*16u), first=e2r_u16(p+i*16u+8), count=e2r_u16(p+i*16u+10);
        if(!e2r_in(sec,ns)||first>ng||count>ng-first) return -1;
    }
    p=image+s[RI_NODES].offset;
    for(i=0;i<nn;i++) for(j=0;j<2;j++) {
        uint32_t child=e2r_u16(p+i*64u+48+j*2u);
        if(!(child&0x8000u)&&child>=nn) return -1;
        if((child&0x8000u)&&(child&0x7fffu)>=nss) return -1;
    }
    p=image+s[RI_BLOCKMAP].offset;
    for(i=0;i<nb;i++) if(e2r_i32(p+i*4u)<-32768||e2r_i32(p+i*4u)>65535) return -1;
    if (!e2r_u32(image+156) || !e2r_u32(image+160) ||
        (uint64_t)e2r_u32(image+156) * (uint64_t)e2r_u32(image+160) > 65536u) return -1;
    (void)nr;
    return 0;
}

static int e2r_validate(const uint8_t *image, size_t bytes,
                        e2r_section_t sections[RI_SECTION_COUNT])
{
    return e2r_header(image, bytes, sections) || e2r_refs(image, sections);
}

/* Import all ten RI1 sections into the fresh level zone. */
static int e2r_import_graph(const uint8_t *image, const e2r_section_t s[RI_SECTION_COUNT])
{
    uint32_t nv=e2r_u32(image+32), ns=e2r_u32(image+36), nsi=e2r_u32(image+40);
    uint32_t nl=e2r_u32(image+44), ng=e2r_u32(image+48), nss=e2r_u32(image+52);
    uint32_t nn=e2r_u32(image+56), nb=e2r_u32(image+60), nr=e2r_u32(image+64);
    uint32_t nli=e2r_u32(image+68), i, j;
    vertex_t *v; sector_t *sc; side_t *sd; line_t *ln; seg_t *sg;
    subsector_t *ss; node_t *no; short *bm; uint8_t *rj; line_t **sl;
    mobj_t **bl; const uint8_t *p;
#define RS_ALLOC(type,count) (type *)Z_Malloc((int)((count) * sizeof(type)), PU_LEVEL, 0)
    v=RS_ALLOC(vertex_t,nv); sc=RS_ALLOC(sector_t,ns); sd=RS_ALLOC(side_t,nsi);
    ln=RS_ALLOC(line_t,nl); sg=RS_ALLOC(seg_t,ng); ss=RS_ALLOC(subsector_t,nss);
    no=RS_ALLOC(node_t,nn); bm=RS_ALLOC(short,nb); rj=RS_ALLOC(uint8_t,nr?nr:1);
    sl=RS_ALLOC(line_t *,nli?nli:1);
    bl=RS_ALLOC(mobj_t *,(size_t)e2r_u32(image+156)*e2r_u32(image+160));
    if(!v||!sc||!sd||!ln||!sg||!ss||!no||!bm||!rj||!sl||!bl) return -1;
    memset(v,0,nv*sizeof(*v)); memset(sc,0,ns*sizeof(*sc)); memset(sd,0,nsi*sizeof(*sd));
    memset(ln,0,nl*sizeof(*ln)); memset(sg,0,ng*sizeof(*sg)); memset(ss,0,nss*sizeof(*ss));
    memset(no,0,nn*sizeof(*no)); memset(bm,0,nb*sizeof(*bm)); memset(rj,0,(nr?nr:1));
    memset(sl,0,(nli?nli:1)*sizeof(*sl));
    memset(bl,0,(size_t)e2r_u32(image+156)*e2r_u32(image+160)*sizeof(*bl));
    p=image+s[RI_VERTEXES].offset; for(i=0;i<nv;i++){v[i].x=e2r_i32(p+i*8);v[i].y=e2r_i32(p+i*8+4);}
    p=image+s[RI_SIDES].offset; for(i=0;i<nsi;i++){sd[i].textureoffset=e2r_i32(p+i*20);sd[i].rowoffset=e2r_i32(p+i*20+4);sd[i].toptexture=(short)e2r_u16(p+i*20+8);sd[i].bottomtexture=(short)e2r_u16(p+i*20+10);sd[i].midtexture=(short)e2r_u16(p+i*20+12);sd[i].sector=&sc[e2r_u32(p+i*20+16)];}
    p=image+s[RI_LINES].offset; for(i=0;i<nl;i++){ln[i].v1=&v[e2r_u32(p+i*64)];ln[i].v2=&v[e2r_u32(p+i*64+4)];ln[i].dx=e2r_i32(p+i*64+8);ln[i].dy=e2r_i32(p+i*64+12);ln[i].flags=(short)e2r_u16(p+i*64+16);ln[i].special=(short)e2r_u16(p+i*64+18);ln[i].tag=(short)e2r_u16(p+i*64+20);ln[i].sidenum[0]=(short)e2r_u16(p+i*64+22);ln[i].sidenum[1]=(short)e2r_u16(p+i*64+24);for(j=0;j<4;j++)ln[i].bbox[j]=e2r_i32(p+i*64+26+j*4);ln[i].slopetype=(slopetype_t)e2r_u32(p+i*64+42);ln[i].frontsector_m=e2r_u32(p+i*64+46)==RI_NULL?0:&sc[e2r_u32(p+i*64+46)];ln[i].backsector_m=e2r_u32(p+i*64+50)==RI_NULL?0:&sc[e2r_u32(p+i*64+50)];ln[i].validcount_m=e2r_i32(p+i*64+54);}
    p=image+s[RI_SECTORS].offset; for(i=0;i<ns;i++){sc[i].floorheight=e2r_i32(p+i*64);sc[i].ceilingheight=e2r_i32(p+i*64+4);sc[i].floorpic=(short)e2r_u16(p+i*64+8);sc[i].ceilingpic=(short)e2r_u16(p+i*64+10);sc[i].lightlevel=(short)e2r_u16(p+i*64+12);sc[i].special=(short)e2r_u16(p+i*64+14);sc[i].tag=(short)e2r_u16(p+i*64+16);for(j=0;j<4;j++)sc[i].blockbox[j]=e2r_i32(p+i*64+20+j*4);sc[i].soundorg.x=e2r_i32(p+i*64+36);sc[i].soundorg.y=e2r_i32(p+i*64+40);sc[i].soundorg.z=e2r_i32(p+i*64+44);sc[i].lines_m=nli?&sl[e2r_u32(p+i*64+48)]:0;sc[i].linecount=(int)e2r_u32(p+i*64+52);}
    p=image+s[RI_SECTOR_LINES].offset; for(i=0;i<nli;i++) sl[i]=&ln[e2r_u32(p+i*4)];
    p=image+s[RI_SEGS].offset; for(i=0;i<ng;i++){sg[i].v1=&v[e2r_u32(p+i*48)];sg[i].v2=&v[e2r_u32(p+i*48+4)];sg[i].offset=e2r_i32(p+i*48+8);sg[i].angle=e2r_u32(p+i*48+12);sg[i].sidedef_m=&sd[e2r_u32(p+i*48+16)];sg[i].linedef_m=&ln[e2r_u32(p+i*48+20)];sg[i].frontsector_m=&sc[e2r_u32(p+i*48+24)];sg[i].backsector_m=e2r_u32(p+i*48+28)==RI_NULL?0:&sc[e2r_u32(p+i*48+28)];}
    p=image+s[RI_SUBSECTORS].offset; for(i=0;i<nss;i++){ss[i].sector=&sc[e2r_u32(p+i*16)];ss[i].numlines=(short)e2r_u16(p+i*16+8);ss[i].firstline=(short)e2r_u16(p+i*16+10);}
    p=image+s[RI_NODES].offset; for(i=0;i<nn;i++){no[i].x=e2r_i32(p+i*64);no[i].y=e2r_i32(p+i*64+4);no[i].dx=e2r_i32(p+i*64+8);no[i].dy=e2r_i32(p+i*64+12);for(j=0;j<2;j++){for(unsigned k=0;k<4;k++)no[i].bbox[j][k]=e2r_i32(p+i*64+16+j*16+k*4);no[i].children[j]=e2r_u16(p+i*64+48+j*2);}}
    p=image+s[RI_BLOCKMAP].offset; for(i=0;i<nb;i++)bm[i]=(short)e2r_i32(p+i*4); memcpy(rj,image+s[RI_REJECT].offset,nr);
    vertexes=v; sectors=sc; sides=sd; lines=ln; segs=sg; subsectors=ss; nodes=no; blockmaplump=bm; blockmap=nb>=4?bm+4:0; rejectmatrix=rj; blocklinks=bl;
    numvertexes=(int)nv; numsectors=(int)ns; numsides=(int)nsi; DOOM_CTX->numlines=(int)nl; numsegs=(int)ng; numsubsectors=(int)nss; DOOM_CTX->numnodes=(int)nn;
    bmaporgx=(fixed_t)e2r_i32(image+164); bmaporgy=(fixed_t)e2r_i32(image+168); bmapwidth=(int)e2r_u32(image+156); bmapheight=(int)e2r_u32(image+160);
#undef RS_ALLOC
    return 0;
}

static int e2r_state_shape(const uint8_t *state, size_t bytes)
{
    return state && bytes>=64 && bytes<=RI_STATE_MAX && !memcmp(state,"DOOM-ST1",8) &&
        e2r_u32(state+8)==0 && e2r_u32(state+12)==64 && (size_t)e2r_u32(state+16)==bytes;
}

static int e2r_mirror_shape(const uint8_t *mirror, size_t bytes, const uint8_t wad_hash[32])
{
    return mirror && bytes==RI_MIRROR_BYTES && !memcmp(mirror,"DOOM-MP1",8) &&
        e2r_u32(mirror+8)==1 && e2r_u32(mirror+12)==4096 && e2r_u32(mirror+16)==RI_MIRROR_BYTES &&
        e2r_u32(mirror+20)==160 && e2r_u32(mirror+24)==384 && e2r_u32(mirror+28)<=384 &&
        e2r_zero(mirror+68,4096-68) && !memcmp(mirror+36,wad_hash,32);
}

static int e2r_controller_shape(const uint8_t *controller, size_t bytes, uint32_t expected_tic)
{
    return controller && bytes==64 && !memcmp(controller,"DOOM-CI2",8) && e2r_u32(controller+8)==1 &&
        e2r_u32(controller+12)==28 && e2r_u32(controller+16)==64 && e2r_u32(controller+20)==expected_tic &&
        e2r_zero(controller+28,36);
}

int doom_engine_ex2_resource_sbf_tic_entry(
    const void *context, const uint8_t *resource, unsigned long resource_bytes,
    const uint8_t *wad_image, unsigned long wad_bytes, void *bootstrap, unsigned long bootstrap_bytes,
    const uint8_t *state, unsigned long state_len, const uint8_t *mirror, unsigned long mirror_len,
    const uint8_t *controller, unsigned long controller_len, uint8_t *state_out, unsigned long state_cap,
    unsigned long *state_out_len, uint8_t *mirror_out, unsigned long mirror_cap,
    unsigned long *mirror_out_len, unsigned long *arena_high_water_out)
{
    e2r_section_t sections[RI_SECTION_COUNT]; uint8_t *low_arena, *encoded=0, *derived=0, *demo;
    uint8_t wad_hash[32]; size_t encoded_bytes=0; uint32_t input_tic; int lump, demo_len;
    if (doom_ctx_guard(context)) return 199;
    if (!resource || resource_bytes < RI_HEADER_BYTES ||
        !wad_image || wad_bytes != RI_WAD_BYTES || !bootstrap || bootstrap_bytes < RI_LOW_ARENA_BYTES ||
        !e2r_state_shape(state,state_len) || !mirror || mirror_len != RI_MIRROR_BYTES ||
        !controller || controller_len != 64 || !state_out || !state_out_len || !mirror_out ||
        !mirror_out_len || state_cap < RI_STATE_MAX || mirror_cap < RI_MIRROR_BYTES) return E2R_REFUSE_RESOURCE;
    if (e2r_validate(resource,resource_bytes,sections)) return E2R_REFUSE_IMAGE;
    input_tic=e2r_u32(state+20);
    if(!e2r_controller_shape(controller,controller_len,input_tic)) return E2R_REFUSE_CONTROLLER;
    memcpy(wad_hash,resource+RI_WAD_HASH_OFFSET,32);
    if(!e2r_mirror_shape(mirror,mirror_len,wad_hash)) return E2R_REFUSE_POOL;
    memset((void *)DOOM_CTX,0,sizeof(doom_ctx_t)); doom_ctx_init();
    if(!doom_sbf_bind_arena(bootstrap,bootstrap_bytes,RI_ZONE_BYTES)) return E2R_REFUSE_ARENA;
    low_arena=(uint8_t *)calloc(1,RI_LOW_ARENA_BYTES); if(!low_arena) return E2R_REFUSE_RESOURCE;
    if(!doom_sbf_bind_arena(low_arena,RI_LOW_ARENA_BYTES,RI_ZONE_BYTES)) return E2R_REFUSE_ARENA;
    gamemode=shareware; gamemission=doom; language=english; DOOM_CTX->deathmatch=0; DOOM_CTX->consoleplayer=0; displayplayer=0; myargc=0; myargv=0;
    Z_Init(); W_InitAccountImage(wad_image,(int)wad_bytes); V_Init(); R_Init(); R_ExecuteSetViewSize(); P_Init();
    lump=W_GetNumForName("DEMO1"); demo_len=W_LumpLength(lump); if(demo_len<17) return E2R_REFUSE_RESOURCE;
    demo=(uint8_t *)W_CacheLumpNum(lump,PU_STATIC); defdemoname="DEMO1"; demobuffer=demo; demo_p=demo; G_DoPlayDemo();
    DoomControllerSetCapturedDemoBounds(demo,(size_t)demo_len);
    if(e2r_import_graph(resource,sections)) return E2R_REFUSE_IMAGE;
    if(DoomStateDecode(state,(size_t)state_len)) return E2R_REFUSE_STATE;
    if(DoomControllerArmV2(controller+24)) return E2R_REFUSE_CONTROLLER;
    derived=(uint8_t *)malloc(RI_MIRROR_BYTES); if(!derived || doom_sbf_fill_pool(derived,wad_hash) || memcmp(derived,mirror,RI_MIRROR_BYTES)) return E2R_REFUSE_POOL;
    if(DoomControllerValidateCapturedDemoCursor(demo_p)) return E2R_REFUSE_CONTROLLER;
    G_Ticker(); gametic++;
    if(doom_sbf_arena_exhausted()) return E2R_REFUSE_ARENA;
    if(DoomStateEncode(&encoded,&encoded_bytes)||encoded_bytes>RI_STATE_MAX||encoded_bytes>state_cap||encoded_bytes<64) return E2R_REFUSE_OUTPUT;
    if(doom_sbf_fill_pool(derived,wad_hash)) return E2R_REFUSE_POOL;
    memcpy(state_out,encoded,encoded_bytes); memcpy(mirror_out,derived,RI_MIRROR_BYTES); *state_out_len=encoded_bytes; *mirror_out_len=RI_MIRROR_BYTES;
    if(arena_high_water_out) *arena_high_water_out=doom_sbf_arena_high_water();
    return 0;
}
