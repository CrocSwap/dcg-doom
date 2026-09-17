/*
 * SBF platform layer for the patched linuxdoom-1.10 headless cut.
 *
 * This file replaces stunts/doom-host/.../headless/doom_stubs.c for the
 * sbf-solana-solana target.  It supplies:
 *
 *   - the subsystem stubs the headless cut already removed (sound, menu, HUD,
 *     intermission, finale, automap, netgame, savegame),
 *   - the freestanding libc subset the engine actually calls, and
 *   - I_ZoneBase, which hands Doom's zone allocator a caller-supplied arena
 *     instead of calloc.  On SBF that arena is a writable account's data, so
 *     every dynamic Doom allocation lands in account state rather than in the
 *     32 KiB program heap.
 *
 * Nothing here opens a file, allocates from the program heap, or keeps state
 * that outlives the arena.  The file-descriptor entry points exist only
 * because w_wad.c still references them on the non-account path; they are
 * fail-closed and are never reached once the account loader is bound.
 *
 * No Doom source is copied into this file.  See ../LICENSE-NOTE.md.
 */

#include <stddef.h>

typedef unsigned char byte_t;
typedef unsigned long u64_t;
typedef long i64_t;

/* Aborts the whole instruction.  Supplied by the Rust side of the program so
 * that a Doom I_Error surfaces as a program error rather than as committed
 * half-built account state. */
void doom_sbf_panic(void);

/* ------------------------------------------------------------------ */
/* Arena                                                               */
/* ------------------------------------------------------------------ */

/* Set by doom_sbf_bind_arena before any engine call.  A single bump pointer
 * serves I_ZoneBase, I_AllocLow and malloc; the engine never frees any of
 * them, so a bump allocator is a complete implementation rather than a
 * simplification.  Z_Free/Z_Malloc operate inside the zone Doom carves out of
 * this arena and are unaffected. */
static byte_t *arena_base;
static u64_t arena_bytes;
static u64_t arena_used;
static u64_t arena_zone_bytes;
static int arena_exhausted;

/* The furthest the bump pointer ever reached, reported as the heap
 * high-water receipt. */
static u64_t arena_high_water;

void doom_sbf_bind_arena(void *base, u64_t bytes, u64_t zone_bytes)
{
    arena_base = (byte_t *)base;
    arena_bytes = bytes;
    arena_used = 0;
    arena_zone_bytes = zone_bytes;
    arena_exhausted = 0;
    arena_high_water = 0;
}

u64_t doom_sbf_arena_high_water(void)
{
    return arena_high_water;
}

int doom_sbf_arena_exhausted(void)
{
    return arena_exhausted;
}

static void *arena_take(u64_t bytes)
{
    u64_t start = (arena_used + 15u) & ~(u64_t)15u;
    if (!arena_base || bytes > arena_bytes || start > arena_bytes - bytes)
    {
	arena_exhausted = 1;
	return (void *)0;
    }
    arena_used = start + bytes;
    if (arena_used > arena_high_water)
	arena_high_water = arena_used;
    return arena_base + start;
}

/* ------------------------------------------------------------------ */
/* Doom platform entry points                                          */
/* ------------------------------------------------------------------ */

/* Doom's own headers are not included here: this file is deliberately
 * independent of the engine tree so that the engine sources stay out of Git.
 * The declarations below match linuxdoom-1.10's definitions exactly. */

/* Measured zone floor: 2 MiB fails the DEMO1 replay, 3 MiB completes all
 * 5,026 tics with the golden stream.  See the experiment note. */
int mb_used = 3;

int I_GetHeapSize(void)
{
    return (int)arena_zone_bytes;
}

byte_t *I_ZoneBase(int *size)
{
    byte_t *zone = (byte_t *)arena_take(arena_zone_bytes);
    u64_t i;
    if (!zone)
    {
	*size = 0;
	return (byte_t *)0;
    }
    /* calloc semantics: the host stub zeroes the zone, and Doom's
     * Z_Init/Z_Malloc residue repairs assume that. */
    for (i = 0; i < arena_zone_bytes; i++)
	zone[i] = 0;
    *size = (int)arena_zone_bytes;
    return zone;
}

byte_t *I_AllocLow(int length)
{
    byte_t *p = (byte_t *)arena_take((u64_t)(length < 0 ? 0 : length));
    int i;
    if (!p)
	return p;
    for (i = 0; i < length; i++)
	p[i] = 0;
    return p;
}

/* A transaction has no wall clock.  Doom's headless cut uses I_GetTime only
 * for the frame pacing the replay path does not run. */
int I_GetTime(void) { return 0; }

void I_Init(void) {}
void I_Quit(void) { doom_sbf_panic(); }
void I_WaitVBL(int count) { (void)count; }
void I_BeginRead(void) {}
void I_EndRead(void) {}
void I_Tactile(int on, int off, int total) { (void)on; (void)off; (void)total; }
void I_StartFrame(void) {}
void I_StartTic(void) {}
void I_InitGraphics(void) {}
void I_ShutdownGraphics(void) {}
void I_FinishUpdate(void) {}
void I_SetPalette(byte_t *palette) { (void)palette; }
void I_UpdateNoBlit(void) {}
void I_ReadScreen(byte_t *scr) { (void)scr; }

/* I_Error is Doom's fail-closed exit.  On SBF it must abort the instruction:
 * a program that kept running after I_Error would commit half-built state. */
static int doom_error_latched;
int doom_sbf_error_latched(void) { return doom_error_latched; }

/* SBF cannot define a variadic function, only call one.  Doom's headers
 * declare I_Error and the printf family as variadic and callers pass extra
 * arguments; these definitions accept the first argument and ignore the rest,
 * which is safe because none of them reads a va_list. */
void I_Error(char *error)
{
    (void)error;
    doom_error_latched = 1;
    doom_sbf_panic();
}

/* ticcmd_t is 8 bytes in the LP64 headless build (patch 0001). */
static byte_t empty_ticcmd[8];
void *I_BaseTiccmd(void) { return (void *)empty_ticcmd; }

/* ------------------------------------------------------------------ */
/* Globals the removed subsystems owned                                */
/* ------------------------------------------------------------------ */

int devparm, nomonsters, respawnparm, fastparm, singletics, drone;
int inhelpscreens, menuactive, automapactive;
/* viewactive, paused and usergame are defined by g_game.c; the host stub
 * relied on tentative-definition merging, which ld.lld does not do. */
int mousemoved;
int snd_SfxVolume, snd_MusicVolume;
int wipegamestate = 0;
int mouseSensitivity = 5;
int detailLevel = 0;
int screenblocks = 11;
int maketic;
int ticdup = 1;
byte_t *save_p;
/* MAXPLAYERS(4) * BACKUPTICS(128) * sizeof(ticcmd_t)(8) */
byte_t netcmds[4 * 128 * 8];
char *player_names[4] = { "player 1", "player 2", "player 3", "player 4" };
void *debugfile;
unsigned int doom_render_fuzz_columns;

/* ------------------------------------------------------------------ */
/* Removed subsystems                                                  */
/* ------------------------------------------------------------------ */

void S_Init(int sfx, int music) { (void)sfx; (void)music; }
void S_Start(void) {}
void S_StartSound(void *origin, int sfx_id) { (void)origin; (void)sfx_id; }
void S_StopSound(void *origin) { (void)origin; }
void S_UpdateSounds(void *listener) { (void)listener; }
void S_ChangeMusic(int id, int looping) { (void)id; (void)looping; }
void S_StopMusic(void) {}
void S_PauseSound(void) {}
void S_ResumeSound(void) {}

void M_LoadDefaults(void) {}
void M_SaveDefaults(void) {}
void M_Init(void) {}
int M_Responder(void *ev) { (void)ev; return 0; }
void M_StartControlPanel(void) {}
void M_Drawer(void) {}
void M_Ticker(void) {}
int M_WriteFile(const char *name, void *src, int len) { (void)name; (void)src; (void)len; return 0; }
int M_ReadFile(const char *name, byte_t **buf) { (void)name; (void)buf; return 0; }
void M_ScreenShot(void) {}

void HU_Init(void) {}
void HU_Start(void) {}
void HU_Stop(void) {}
void HU_Ticker(void) {}
void HU_Drawer(void) {}
void HU_Erase(void) {}
int HU_Responder(void *ev) { (void)ev; return 0; }
char HU_dequeueChatChar(void) { return 0; }

void ST_Init(void) {}
void ST_Start(void) {}
void ST_Stop(void) {}
void ST_Ticker(void) {}
void ST_Drawer(int full, int redraw) { (void)full; (void)redraw; }
int ST_Responder(void *ev) { (void)ev; return 0; }

void WI_Start(void *wb) { (void)wb; }
void WI_Ticker(void) {}
void WI_Drawer(void) {}
void F_StartFinale(void) {}
void F_Ticker(void) {}
void F_Drawer(void) {}
int F_Responder(void *ev) { (void)ev; return 0; }
void AM_Start(void) {}
void AM_Stop(void) {}
void AM_Ticker(void) {}
void AM_Drawer(void) {}
int AM_Responder(void *ev) { (void)ev; return 0; }

int doom_sbf_demo_done;
void D_AdvanceDemo(void) { doom_sbf_demo_done = 1; }
void D_QuitNetGame(void) {}
void D_PostEvent(void *ev) { (void)ev; }
void D_ProcessEvents(void) {}
void D_PageTicker(void) {}
void NetUpdate(void) {}

void P_ArchivePlayers(void) {} void P_UnArchivePlayers(void) {}
void P_ArchiveWorld(void) {} void P_UnArchiveWorld(void) {}
void P_ArchiveThinkers(void) {} void P_UnArchiveThinkers(void) {}
void P_ArchiveSpecials(void) {} void P_UnArchiveSpecials(void) {}
void P_ArchiveRNG(void) {} void P_UnArchiveRNG(void) {}
void P_ArchiveMap(void) {} void P_UnArchiveMap(void) {}

/* ------------------------------------------------------------------ */
/* Freestanding libc subset                                            */
/* ------------------------------------------------------------------ */

void *memset(void *dst, int value, size_t n)
{
    byte_t *p = (byte_t *)dst;
    size_t i;
    for (i = 0; i < n; i++)
	p[i] = (byte_t)value;
    return dst;
}

void *memcpy(void *dst, const void *src, size_t n)
{
    byte_t *d = (byte_t *)dst;
    const byte_t *s = (const byte_t *)src;
    size_t i;
    for (i = 0; i < n; i++)
	d[i] = s[i];
    return dst;
}

void *memmove(void *dst, const void *src, size_t n)
{
    byte_t *d = (byte_t *)dst;
    const byte_t *s = (const byte_t *)src;
    size_t i;
    if (d == s || !n)
	return dst;
    if (d < s)
	for (i = 0; i < n; i++)
	    d[i] = s[i];
    else
	for (i = n; i-- > 0; )
	    d[i] = s[i];
    return dst;
}

int memcmp(const void *a, const void *b, size_t n)
{
    const byte_t *p = (const byte_t *)a;
    const byte_t *q = (const byte_t *)b;
    size_t i;
    for (i = 0; i < n; i++)
	if (p[i] != q[i])
	    return p[i] < q[i] ? -1 : 1;
    return 0;
}

size_t strlen(const char *s)
{
    size_t n = 0;
    while (s[n])
	n++;
    return n;
}

char *strcpy(char *dst, const char *src)
{
    size_t i = 0;
    while ((dst[i] = src[i]) != 0)
	i++;
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n)
{
    size_t i = 0;
    while (i < n && src[i])
    {
	dst[i] = src[i];
	i++;
    }
    while (i < n)
	dst[i++] = 0;
    return dst;
}

char *strcat(char *dst, const char *src)
{
    strcpy(dst + strlen(dst), src);
    return dst;
}

int strcmp(const char *a, const char *b)
{
    size_t i = 0;
    while (a[i] && a[i] == b[i])
	i++;
    return (int)(unsigned char)a[i] - (int)(unsigned char)b[i];
}

int strncmp(const char *a, const char *b, size_t n)
{
    size_t i = 0;
    while (i < n && a[i] && a[i] == b[i])
	i++;
    if (i == n)
	return 0;
    return (int)(unsigned char)a[i] - (int)(unsigned char)b[i];
}

int toupper(int c) { return (c >= 'a' && c <= 'z') ? c - 'a' + 'A' : c; }
int tolower(int c) { return (c >= 'A' && c <= 'Z') ? c - 'A' + 'a' : c; }
int isspace(int c) { return c == ' ' || (c >= 9 && c <= 13); }
int isdigit(int c) { return c >= '0' && c <= '9'; }
int isalpha(int c) { return (c | 32) >= 'a' && (c | 32) <= 'z'; }

int strcasecmp(const char *a, const char *b)
{
    size_t i = 0;
    while (a[i] && tolower((unsigned char)a[i]) == tolower((unsigned char)b[i]))
	i++;
    return tolower((unsigned char)a[i]) - tolower((unsigned char)b[i]);
}

int strncasecmp(const char *a, const char *b, size_t n)
{
    size_t i = 0;
    while (i < n && a[i] && tolower((unsigned char)a[i]) == tolower((unsigned char)b[i]))
	i++;
    if (i == n)
	return 0;
    return tolower((unsigned char)a[i]) - tolower((unsigned char)b[i]);
}

char *strchr(const char *s, int c)
{
    for (;; s++)
    {
	if (*s == (char)c)
	    return (char *)s;
	if (!*s)
	    return (char *)0;
    }
}

char *strrchr(const char *s, int c)
{
    const char *last = (const char *)0;
    for (;; s++)
    {
	if (*s == (char)c)
	    last = s;
	if (!*s)
	    return (char *)last;
    }
}

char *strstr(const char *hay, const char *needle)
{
    size_t n = strlen(needle);
    if (!n)
	return (char *)hay;
    for (; *hay; hay++)
	if (!strncmp(hay, needle, n))
	    return (char *)hay;
    return (char *)0;
}

int abs(int v) { return v < 0 ? -v : v; }

int atoi(const char *s)
{
    int sign = 1, value = 0;
    while (isspace((unsigned char)*s))
	s++;
    if (*s == '-') { sign = -1; s++; }
    else if (*s == '+') s++;
    while (isdigit((unsigned char)*s))
	value = value * 10 + (*s++ - '0');
    return sign * value;
}

void *malloc(size_t n) { return arena_take((u64_t)n); }
void *calloc(size_t n, size_t size)
{
    u64_t total = (u64_t)n * (u64_t)size;
    byte_t *p = (byte_t *)arena_take(total);
    u64_t i;
    if (p)
	for (i = 0; i < total; i++)
	    p[i] = 0;
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

/* Diagnostic output has no destination inside a transaction.  These are
 * present because the engine calls them on its progress and error paths;
 * they deliberately produce nothing rather than logging, so that CU cost does
 * not depend on how chatty a code path is. */
void *stdout;
void *stderr;
void *stdin;
int printf(const char *fmt) { (void)fmt; return 0; }
int fprintf(void *f, const char *fmt) { (void)f; (void)fmt; return 0; }
int sprintf(char *dst, const char *fmt) { (void)fmt; dst[0] = 0; return 0; }
int snprintf(char *dst, size_t n, const char *fmt) { (void)fmt; if (n) dst[0] = 0; return 0; }
int sscanf(const char *s, const char *fmt) { (void)s; (void)fmt; return 0; }
int puts(const char *s) { (void)s; return 0; }
int fputs(const char *s, void *f) { (void)s; (void)f; return 0; }
int putchar(int c) { return c; }
int fflush(void *f) { (void)f; return 0; }

/* Fail-closed file descriptors.  The account loader is the only supported
 * WAD source on SBF; if the engine ever reaches these, the caller has bound
 * the wrong source and the instruction must not silently produce state. */
void *fopen(const char *p, const char *m) { (void)p; (void)m; return (void *)0; }
int fclose(void *f) { (void)f; return -1; }
size_t fread(void *d, size_t s, size_t n, void *f) { (void)d; (void)s; (void)n; (void)f; return 0; }
size_t fwrite(const void *d, size_t s, size_t n, void *f) { (void)d; (void)s; (void)n; (void)f; return 0; }
int fseek(void *f, long o, int w) { (void)f; (void)o; (void)w; return -1; }
long ftell(void *f) { (void)f; return -1; }
int open(const char *p, int f) { (void)p; (void)f; return -1; }
int close(int fd) { (void)fd; return -1; }
long read(int fd, void *b, size_t n) { (void)fd; (void)b; (void)n; return -1; }
long write(int fd, const void *b, size_t n) { (void)fd; (void)b; (void)n; return -1; }
long lseek(int fd, long o, int w) { (void)fd; (void)o; (void)w; return -1; }
int access(const char *p, int m) { (void)p; (void)m; return -1; }
struct doom_sbf_stat { long st_size; };
int fstat(int fd, struct doom_sbf_stat *st) { (void)fd; (void)st; return -1; }
int stat(const char *p, struct doom_sbf_stat *st) { (void)p; (void)st; return -1; }
int errno;

/* SBF has 64-bit multiply and divide instructions, but the backend still
 * emits calls to these helpers for signed 64-bit division in the fixed-point
 * paths.  Implementing them here keeps the image free of compiler-rt. */
i64_t __divdi3(i64_t a, i64_t b)
{
    int negate = 0;
    u64_t ua, ub, q;
    if (a < 0) { ua = (u64_t)(-(a + 1)) + 1u; negate ^= 1; } else ua = (u64_t)a;
    if (b < 0) { ub = (u64_t)(-(b + 1)) + 1u; negate ^= 1; } else ub = (u64_t)b;
    q = ub ? ua / ub : 0u;
    return negate ? -(i64_t)q : (i64_t)q;
}

i64_t __moddi3(i64_t a, i64_t b)
{
    int negate = 0;
    u64_t ua, ub, r;
    if (a < 0) { ua = (u64_t)(-(a + 1)) + 1u; negate = 1; } else ua = (u64_t)a;
    if (b < 0) ub = (u64_t)(-(b + 1)) + 1u; else ub = (u64_t)b;
    r = ub ? ua % ub : 0u;
    return negate ? -(i64_t)r : (i64_t)r;
}
