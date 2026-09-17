#ifndef DOOM_SBF_SYS_STAT_H
#define DOOM_SBF_SYS_STAT_H
struct stat { long st_size; };
int fstat(int, struct stat *);
int stat(const char *, struct stat *);
#endif
