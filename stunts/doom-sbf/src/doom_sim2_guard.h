/*
 * DOOM-SIM/2 pre-tic guard, shared verbatim by the SBF step
 * (doom_sbf_resource_ri2.c) and the host session oracle (doom_resource_ri2.c).
 *
 * A staged tic runs only from a steady in-level state.  Any transition this
 * no-render profile cannot serve halts the simulation BEFORE G_Ticker runs,
 * so the tic that would start it is never executed:
 *
 *   - a pending gameaction (ga_loadlevel, ga_savegame, ga_completed, ...)
 *     would run G_DoLoadLevel/P_SetupLevel, G_DoSaveGame (R_FillBackScreen,
 *     V_DrawPatch into screens[1]) or the intermission stubs;
 *   - a PST_REBORN player makes G_Ticker call G_DoReborn -> level reload;
 *   - gamestate != GS_LEVEL means the intermission/finale stubs own the tic;
 *   - paused makes P_Ticker a no-op (unreachable with BT_SPECIAL refused, but
 *     an imported ST1 could carry it);
 *   - in the versioned legacy SIM2INPT/2 profile only, an exhausted captured
 *     DEMO1 cursor (0x80 marker) ends the context's lifetime. SIM2INPT/3
 *     accepts the committed staged command stream independently of demo_p.
 *
 * Commands carrying BT_SPECIAL (buttons & 0x80: pause, save-game) are refused
 * as malformed input, not halted: the deposit boundary already refuses them.
 *
 * A dead player pressing BT_USE is a restart request, refused with its own
 * code rather than swallowed.  Vanilla single-player has no use-to-respawn:
 * G_Ticker only calls G_DoReborn for a PST_REBORN player, and in single player
 * G_DoReborn sets gameaction = ga_loadlevel, a full level reload this profile
 * cannot serve.  So the press cannot resurrect a run in progress, and without
 * this clause the staged tic would be consumed and committed as a no-op: the
 * slot would sit dead until its buffer filled, and the caller would see a
 * consumed command rather than the request.  Refusing commits nothing, leaves
 * the meta un-halted, and names the request, so the answer is SIM/2 RESET/9
 * (return the context to its committed start state).  A live player pressing
 * BT_USE is ordinary input and is untouched.
 *
 * Return: 0 = run, 1..5 = halt reason, 232 = refused command, 295 = restart
 * requested.
 */
#ifndef DOOM_SIM2_GUARD_H
#define DOOM_SIM2_GUARD_H

#define DOOM_SIM2_HALT_GAMESTATE 1
#define DOOM_SIM2_HALT_GAMEACTION 2
#define DOOM_SIM2_HALT_REBORN 3
#define DOOM_SIM2_HALT_PAUSED 4
#define DOOM_SIM2_HALT_DEMO_END 5
#define DOOM_SIM2_REFUSE_COMMAND 232
/* These mirror the pinned Doom d_event.h buttoncode_t values. */
#define DOOM_SIM2_BT_ATTACK 0x01u
#define DOOM_SIM2_BT_USE 0x02u
#define DOOM_SIM2_BT_CHANGE 0x04u
#define DOOM_SIM2_BT_SPECIAL 0x80u
/* A dead player asked to restart.  Not a halt: the tic does not run, nothing
 * is committed, and the meta keeps whatever halt state it had. */
#define DOOM_SIM2_REFUSE_RESTART 295

static int doom_sim2_pretic_guard(const uint8_t *command)
{
    int i;
    if (!command || (command[3] & DOOM_SIM2_BT_SPECIAL))
        return DOOM_SIM2_REFUSE_COMMAND;
    if (gamestate != GS_LEVEL) return DOOM_SIM2_HALT_GAMESTATE;
    if (gameaction != ga_nothing) return DOOM_SIM2_HALT_GAMEACTION;
    if (paused) return DOOM_SIM2_HALT_PAUSED;
    if (command[3] & DOOM_SIM2_BT_USE)
        for (i = 0; i < MAXPLAYERS; i++)
            if (playeringame[i] && players[i].playerstate == PST_DEAD)
                return DOOM_SIM2_REFUSE_RESTART;
    for (i = 0; i < MAXPLAYERS; i++)
        if (playeringame[i] && players[i].playerstate == PST_REBORN)
            return DOOM_SIM2_HALT_REBORN;
#ifdef DOOM_SIM2_LEGACY_DEMO_CAP
    if (DoomControllerValidateCapturedDemoCursor(demo_p))
        return DOOM_SIM2_HALT_DEMO_END;
#endif
    return 0;
}

#endif
