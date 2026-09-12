# Doom source note

`src/doom_hotloops.c` contains only the measurement functions, adapted from
the following files in id Software's `DOOM` repository at
`linuxdoom-1.10/`:

- `r_draw.c`: `R_DrawColumn` and `R_DrawSpan` loop bodies;
- `r_main.c`: `R_PointOnSide`;
- `m_fixed.c`: `FixedMul`, `FixedDiv`, and the integer `FixedDiv2` expression
  retained in that source file's disabled C alternative.

Copyright (C) 1993–1996 id Software, Inc.; current repository copyright
notice: Copyright (c) ZeniMax Media Inc.  The upstream repository's
`README.TXT` states: **Licensed under the GNU General Public License 2.0.**

The full Doom source tree and game data are deliberately not vendored here.
The adaptations make globals explicit account-backed pointers, replace Doom's
error path with bounded spike inputs, and use the integer `FixedDiv2` variant
so the SBF image does not require a floating-point runtime.  This spike is not
a Doom distribution or a playable port.
