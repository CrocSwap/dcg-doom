#ifndef DOOM_SBF_STDLIB_H
#define DOOM_SBF_STDLIB_H
#include <stddef.h>
void *malloc(size_t);
void *calloc(size_t, size_t);
void *realloc(void *, size_t);
void free(void *);
void exit(int);
void abort(void);
int atoi(const char *);
long atol(const char *);
int abs(int);
char *getenv(const char *);
int system(const char *);
#endif
