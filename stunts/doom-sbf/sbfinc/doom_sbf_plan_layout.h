#ifndef DOOM_SBF_PLAN_LAYOUT_H
#define DOOM_SBF_PLAN_LAYOUT_H

/* The bootstrap carrier is split into an active bump-arena prefix and a
 * reserved tail. Keep fixed budgets for the two persistent plans so each
 * translation unit can prove its own object fits without a cross-TU sizeof. */
#define RI2_SPLIT_PLAN_RESERVE (256u * 1024u)
#define RI2_SPLIT_DECODER_PLAN_RESERVE (192u * 1024u)
#define RI2_SPLIT_RI2_PLAN_RESERVE (64u * 1024u)

_Static_assert(
    RI2_SPLIT_DECODER_PLAN_RESERVE + RI2_SPLIT_RI2_PLAN_RESERVE ==
        RI2_SPLIT_PLAN_RESERVE,
    "RI2 split plan budgets must exactly fill the reserved tail");

#endif
