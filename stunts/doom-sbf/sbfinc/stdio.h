/* Freestanding declaration-only shim for the SBF engine build.  Doom's
 * headless cut only ever *calls* these; the SBF link supplies fail-closed
 * bodies (see doom_sbf_libc.c).  No hosted stdio exists on SBF. */
#ifndef DOOM_SBF_STDIO_H
#define DOOM_SBF_STDIO_H
#include <stddef.h>
typedef struct _DOOM_SBF_FILE FILE;
/* Const NULL: in-cut engine code only reads these (never assigns them), so
 * the SBF link supplies them from .rodata instead of .bss.  A TU that
 * assigns one fails to compile here rather than growing writable static. */
extern FILE *const stdout;
extern FILE *const stderr;
extern FILE *const stdin;
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
int printf(const char *, ...);
int fprintf(FILE *, const char *, ...);
int sprintf(char *, const char *, ...);
int snprintf(char *, size_t, const char *, ...);
int sscanf(const char *, const char *, ...);
int puts(const char *);
int fputs(const char *, FILE *);
int putchar(int);
int fflush(FILE *);
FILE *fopen(const char *, const char *);
int fclose(FILE *);
size_t fread(void *, size_t, size_t, FILE *);
size_t fwrite(const void *, size_t, size_t, FILE *);
int fseek(FILE *, long, int);
long ftell(FILE *);
#endif
