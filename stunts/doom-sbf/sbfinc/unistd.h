#ifndef DOOM_SBF_UNISTD_H
#define DOOM_SBF_UNISTD_H
#include <stddef.h>
typedef long ssize_t;
int open(const char *, int, ...);
int close(int);
ssize_t read(int, void *, size_t);
ssize_t write(int, const void *, size_t);
long lseek(int, long, int);
int access(const char *, int);
#endif
