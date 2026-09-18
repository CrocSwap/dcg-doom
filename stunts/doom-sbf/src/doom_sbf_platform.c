/*
 * SBF platform layer for the patch-0007 linuxdoom-1.10 headless cut.
 *
 * This file replaces stunts/doom-host/.../headless/doom_stubs.c for the
 * sbf-solana-solana target.  It supplies:
 *
 *   - the subsystem stubs the headless cut already removed (sound, menu, HUD,
 *     intermission, finale, automap, netgame, savegame),
 *   - the freestanding libc subset the engine actually calls,
 *   - the freestanding libc subset the engine actually calls.
 *
 * (I_ZoneBase, the arena, the error latch, the ticcmd scratch, the
 * constant-address guard and the write/read probe live in doom_sbf_ctx.c,
 * which is the only platform file that sees doom_ctx.h: this one must NOT
 * be compiled with -include doom_ctx.h, because the engine headers pull in
 * sbfinc/stdio.h's variadic printf declarations, and SBF cannot define a
 * variadic function.)
 *
 * Zero-static-data design: this translation unit defines NO mutable global
 * and no const-pointer table.  stdout / stderr / stdin are const NULL
 * pointers (.rodata): in-cut engine code only reads them (sbfinc/stdio.h
 * declares them const to match).  Stub globals nothing in the 41-TU cut
 * references (devparm, drone, inhelpscreens, mousemoved, snd volumes,
 * debugfile, errno) are deleted, not moved.
 *
 * Nothing here opens a file, allocates from the program heap, or keeps state
 * outside the context account.  The file-descriptor entry points exist only
 * because w_wad.c still references them on the non-account path; they are
 * fail-closed and are never reached once the account loader is bound.
 *
 * No Doom source is copied into this file.  See ../LICENSE-NOTE.md.
 *
 * Engine init/load_level/tic/render are NOT implemented here: tan() (called
 * once, r_main.c R_InitTextureMapping) and the other init-only paths abort
 * through doom_sbf_panic so a future caller fails loudly instead of running
 * on stub state.
 */

#include <stddef.h>

/* Local twin of sbfinc/stdio.h's FILE: including that header would also pull
 * its variadic printf declarations into conflict with the fixed-arity
 * definitions below (SBF cannot define a variadic function, only call one).
 * Both spell an incomplete-struct pointer, so the stranded const-NULL
 * definitions link against engine references. */
typedef struct _DOOM_SBF_FILE DOOM_SBF_FILE;

typedef unsigned char byte_t;
typedef unsigned long u64_t;
typedef long i64_t;

/* Aborts the whole instruction.  Supplied by the Rust side of the program so
 * that a Doom I_Error surfaces as a program error rather than as committed
 * half-built account state. */
void doom_sbf_panic(void);

/* ------------------------------------------------------------------ */
/* Doom platform entry points                                          */
/* ------------------------------------------------------------------ */

/* Doom's own headers are not included here at all (the declarations below
 * match linuxdoom-1.10's definitions exactly; the context-mode entry points
 * that need doom_ctx.h live in doom_sbf_ctx.c). */

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

DOOM_SBF_FILE *const stdout = 0;
DOOM_SBF_FILE *const stderr = 0;
DOOM_SBF_FILE *const stdin = 0;

/* Native (host) feature builds: the engine TUs include sbfinc's shims (the
 * -I order shadows the system headers), so every sbfinc-declared name the
 * engine references must resolve here even natively.  That is ONLY the
 * const-NULL stdio pointers below (in-cut engine code merely reads them,
 * and the host libc has no sbfinc-typed `stderr` symbol to offer).  Every
 * other libc-named definition (getenv, tan, the silent printf family, the
 * fail-closed descriptors) is SBF-only: natively the real libc serves, so a
 * stub can neither swallow test output nor lie to a native caller.  The SBF
 * image has no libc: there these are the only definitions.  (The same split
 * holds in doom_sbf_ctx.c for malloc/calloc/realloc/free/exit/abort.) */
#ifdef DOOM_SBF_BUILD
/* No environment inside a transaction. */
char *getenv(const char *name) { (void)name; return (char *)0; }


/* Engine init is not implemented in this brief (no init/load_level/tic/
 * render): tan() has exactly one in-cut call site (r_main.c
 * R_InitTextureMapping) and must fail loudly if init is ever attempted. */
double tan(double x) { (void)x; doom_sbf_panic(); return 0.0; }

/* Diagnostic output has no destination inside a transaction.  These are
 * present because the engine calls them on its progress and error paths;
 * they deliberately produce nothing rather than logging, so that CU cost does
 * not depend on how chatty a code path is.  SBF cannot define a variadic
 * function, only call one; callers pass extra arguments that these bodies
 * ignore, which is safe because none of them reads a va_list. */
int printf(const char *fmt) { (void)fmt; return 0; }
int fprintf(DOOM_SBF_FILE *f, const char *fmt) { (void)f; (void)fmt; return 0; }
int sprintf(char *dst, const char *fmt) { (void)fmt; dst[0] = 0; return 0; }
int snprintf(char *dst, size_t n, const char *fmt) { (void)fmt; if (n) dst[0] = 0; return 0; }
int sscanf(const char *s, const char *fmt) { (void)s; (void)fmt; return 0; }
int puts(const char *s) { (void)s; return 0; }
int fputs(const char *s, DOOM_SBF_FILE *f) { (void)s; (void)f; return 0; }
int putchar(int c) { return c; }
int fflush(DOOM_SBF_FILE *f) { (void)f; return 0; }

/* Fail-closed file descriptors.  The account loader is the only supported
 * WAD source on SBF; if the engine ever reaches these, the caller has bound
 * the wrong source and the instruction must not silently produce state. */
DOOM_SBF_FILE *fopen(const char *p, const char *m) { (void)p; (void)m; return (DOOM_SBF_FILE *)0; }
int fclose(DOOM_SBF_FILE *f) { (void)f; return -1; }
size_t fread(void *d, size_t s, size_t n, DOOM_SBF_FILE *f) { (void)d; (void)s; (void)n; (void)f; return 0; }
size_t fwrite(const void *d, size_t s, size_t n, DOOM_SBF_FILE *f) { (void)d; (void)s; (void)n; (void)f; return 0; }
int fseek(DOOM_SBF_FILE *f, long o, int w) { (void)f; (void)o; (void)w; return -1; }
long ftell(DOOM_SBF_FILE *f) { (void)f; return -1; }
int open(const char *p, int f) { (void)p; (void)f; return -1; }
int close(int fd) { (void)fd; return -1; }
long read(int fd, void *b, size_t n) { (void)fd; (void)b; (void)n; return -1; }
long write(int fd, const void *b, size_t n) { (void)fd; (void)b; (void)n; return -1; }
long lseek(int fd, long o, int w) { (void)fd; (void)o; (void)w; return -1; }
int access(const char *p, int m) { (void)p; (void)m; return -1; }
struct doom_sbf_stat { long st_size; };
int fstat(int fd, struct doom_sbf_stat *st) { (void)fd; (void)st; return -1; }
int stat(const char *p, struct doom_sbf_stat *st) { (void)p; (void)st; return -1; }

#endif /* DOOM_SBF_BUILD */
