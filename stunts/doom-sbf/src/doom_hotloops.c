/*
 * Narrow C extraction for the Doom-on-SBF measurement program.
 *
 * The column and span loop bodies below are adapted from
 * linuxdoom-1.10/r_draw.c; FixedMul/FixedDiv and R_PointOnSide are adapted
 * from linuxdoom-1.10/m_fixed.c and linuxdoom-1.10/r_main.c respectively.
 * The adapters replace Doom globals (dc_*, ds_*, ylookup, and node_t) with
 * explicit pointers and small structs suitable for an SBF entrypoint.  They
 * retain the original fixed-point expressions and do/while loop bodies.
 * See ../LICENSE-NOTE.md for source and licence information.
 */

typedef signed int i32;
typedef unsigned int u32;
typedef signed long long i64;
typedef unsigned long long u64;
typedef unsigned char byte;

#define FRACBITS 16
#define SCREENWIDTH 320
#define SCREENHEIGHT 200
#define FRAME_BYTES (SCREENWIDTH * SCREENHEIGHT)
#define STATE_COLUMN 64
#define STATE_COLORMAP (STATE_COLUMN + 128)
#define STATE_FLAT (STATE_COLORMAP + 256)

typedef i32 fixed_t;

typedef struct {
    fixed_t x;
    fixed_t y;
    fixed_t dx;
    fixed_t dy;
} doom_node_t;

/* linuxdoom-1.10/m_fixed.c: the exact 64-bit FixedMul formulation. */
static fixed_t FixedMul(fixed_t a, fixed_t b) {
    return (fixed_t)(((i64)a * (i64)b) >> FRACBITS);
}

/* Inputs in this spike stay in the non-overflow domain.  The integer
 * FixedDiv2 below is the fixed-point implementation preserved in the
 * #if 0 branch of linuxdoom-1.10/m_fixed.c; it avoids adding an SBF soft-f64
 * runtime merely to express an operation whose inputs and result are i32.
 */
static fixed_t FixedDiv2(fixed_t a, fixed_t b) {
    return (fixed_t)(((i64)a << FRACBITS) / (i64)b);
}

static i32 doom_abs(i32 value) {
    return value < 0 ? -value : value;
}

/* linuxdoom-1.10/m_fixed.c, with local abs and the fixed-point FixedDiv2. */
static fixed_t FixedDiv(fixed_t a, fixed_t b) {
    if ((doom_abs(a) >> 14) >= doom_abs(b)) {
        return (a ^ b) < 0 ? (i32)0x80000000 : 0x7fffffff;
    }
    return FixedDiv2(a, b);
}

/* linuxdoom-1.10/r_main.c R_PointOnSide, with node_t made local. */
static int R_PointOnSide(fixed_t x, fixed_t y, doom_node_t *node) {
    fixed_t dx;
    fixed_t dy;
    fixed_t left;
    fixed_t right;

    if (!node->dx) {
        if (x <= node->x) {
            return node->dy > 0;
        }
        return node->dy < 0;
    }
    if (!node->dy) {
        if (y <= node->y) {
            return node->dx < 0;
        }
        return node->dx > 0;
    }

    dx = x - node->x;
    dy = y - node->y;

    if ((node->dy ^ node->dx ^ dx ^ dy) & (i32)0x80000000) {
        if ((node->dy ^ dx) & (i32)0x80000000) {
            return 1;
        }
        return 0;
    }

    left = FixedMul(node->dy >> FRACBITS, dx);
    right = FixedMul(dy, node->dx >> FRACBITS);

    if (right < left) {
        return 0;
    }
    return 1;
}

static doom_node_t synthetic_node(u32 index, const byte *state) {
    /* A deterministic 256-node binary tree, derived from account-resident
     * state so the compiler cannot replace the traversal with constants. */
    i32 bias = (i32)state[32 + (index & 31)] - 128;
    doom_node_t node;
    node.x = ((i32)(index & 15) - 8) * 0x10000 + (bias << 8);
    node.y = ((i32)((index >> 4) & 15) - 8) * 0x10000 - (bias << 7);
    node.dx = ((i32)((index * 17) & 31) - 15) * 0x10000;
    node.dy = ((i32)((index * 29) & 31) - 15) * 0x10000;
    if (!node.dx) {
        node.dx = 0x10000;
    }
    if (!node.dy) {
        node.dy = -0x10000;
    }
    return node;
}

/* linuxdoom-1.10/r_draw.c R_DrawColumn's hot-loop shape. */
void doom_draw_columns(byte *framebuffer, byte *state) {
    i32 dc_x;
    const byte *dc_source = state + STATE_COLUMN;
    const byte *dc_colormap = state + STATE_COLORMAP;

    for (dc_x = 0; dc_x < SCREENWIDTH; ++dc_x) {
        i32 count;
        byte *dest;
        fixed_t frac;
        fixed_t fracstep;
        const i32 dc_yl = 0;
        const i32 dc_yh = SCREENHEIGHT - 1;
        const fixed_t dc_iscale = 0x18000 + ((dc_x & 7) << 12);
        const fixed_t dc_texturemid = ((dc_x & 127) - 64) << FRACBITS;
        const i32 centery = SCREENHEIGHT / 2;

        count = dc_yh - dc_yl;
        dest = framebuffer + dc_yl * SCREENWIDTH + dc_x;
        fracstep = dc_iscale;
        frac = dc_texturemid + (dc_yl - centery) * fracstep;

        do {
            *dest = dc_colormap[dc_source[(frac >> FRACBITS) & 127]];
            dest += SCREENWIDTH;
            frac += fracstep;
        } while (count--);
    }
}

/* linuxdoom-1.10/r_draw.c R_DrawSpan's hot-loop shape. */
void doom_draw_spans(byte *framebuffer, byte *state) {
    i32 ds_y;
    const byte *ds_source = state + STATE_FLAT;
    const byte *ds_colormap = state + STATE_COLORMAP;

    for (ds_y = 0; ds_y < SCREENHEIGHT; ++ds_y) {
        fixed_t xfrac;
        fixed_t yfrac;
        byte *dest;
        i32 count;
        i32 spot;
        const i32 ds_x1 = 0;
        const i32 ds_x2 = SCREENWIDTH - 1;
        const fixed_t ds_xfrac = ((ds_y * 9) & 63) << FRACBITS;
        const fixed_t ds_yfrac = ((ds_y * 5) & 63) << FRACBITS;
        const fixed_t ds_xstep = 0x17000;
        const fixed_t ds_ystep = 0x0b000;

        xfrac = ds_xfrac;
        yfrac = ds_yfrac;
        dest = framebuffer + ds_y * SCREENWIDTH + ds_x1;
        count = ds_x2 - ds_x1;

        do {
            spot = ((yfrac >> (16 - 6)) & (63 * 64)) + ((xfrac >> 16) & 63);
            *dest++ = ds_colormap[ds_source[spot]];
            xfrac += ds_xstep;
            yfrac += ds_ystep;
        } while (count--);
    }
}

u64 doom_bsp_descent(byte *state) {
    u64 checksum = 0;
    u32 column;

    for (column = 0; column < SCREENWIDTH; ++column) {
        fixed_t x = ((i32)column - 160) << FRACBITS;
        fixed_t y = ((i32)((column * 13) & 255) - 128) << FRACBITS;
        u32 node_index = 0;
        while (node_index < 256) {
            doom_node_t node = synthetic_node(node_index, state);
            int side = R_PointOnSide(x, y, &node);
            checksum += (u64)(node_index + (u32)side);
            node_index = node_index * 2 + 1 + (u32)side;
            x += 0x0311;
            y -= 0x0217;
        }
    }
    return checksum;
}

u64 doom_fixed_mul_only(byte *state) {
    u64 checksum = 0;
    u32 i;
    for (i = 0; i < 10000; ++i) {
        fixed_t a = 0x10000 + (i32)((i * 97 + state[16]) & 0x7fff);
        fixed_t b = 0x08000 + (i32)((i * 53 + state[17]) & 0x3fff);
        checksum += (u32)FixedMul(a, b);
    }
    return checksum;
}

u64 doom_fixed_div_only(byte *state) {
    u64 checksum = 0;
    u32 i;
    for (i = 0; i < 10000; ++i) {
        fixed_t a = 0x10000 + (i32)((i * 97 + state[18]) & 0x7fff);
        fixed_t b = 0x08000 + (i32)((i * 53 + state[19]) & 0x3fff);
        checksum += (u32)FixedDiv(a, b);
    }
    return checksum;
}

u64 doom_fixed_mul_div(byte *state) {
    u64 checksum = 0;
    u32 i;
    for (i = 0; i < 10000; ++i) {
        fixed_t a = 0x10000 + (i32)((i * 97 + state[20]) & 0x7fff);
        fixed_t b = 0x08000 + (i32)((i * 53 + state[21]) & 0x3fff);
        checksum += (u32)FixedMul(a, b);
        checksum ^= (u32)FixedDiv(a, b);
    }
    return checksum;
}

void doom_write_strip(byte *strip, u32 bytes, const byte *state) {
    u32 i;
    byte seed = state[8];
    for (i = 0; i < bytes; ++i) {
        strip[i] = (byte)(seed + (byte)(i * 37));
    }
}

/* Writability probe for the engine port.  linuxdoom-1.10 keeps its live state
 * in ordinary C globals, which land in the program image's .data/.bss.  This
 * probe writes one such global and reads it back, so whether an SBF program
 * may hold mutable static state is settled by measurement rather than by
 * assumption.  It is not part of any Doom code path. */
#ifdef DOOM_STATIC_WRITE_PROBE
static u64 doom_probe_cell = 0x5a5a5a5a5a5a5a5aull;

u64 doom_probe_static_write(u64 value) {
    doom_probe_cell ^= value;
    return doom_probe_cell;
}
#endif

/* Same measurement as doom_probe_static_write, but the cell carries no
 * initializer, so it lands in .bss (zero-fill-on-load) rather than .data
 * (nonzero image content).  Whether the SBF loader's "no writable static
 * data" refusal is specific to a nonzero .data cell or also covers a
 * zero-initialized .bss cell was not measured by M1006's static-write-probe,
 * and it decides whether a single lone pointer (e.g. a base/context pointer
 * left un-initialized until the platform layer sets it at instruction entry)
 * could ever be a global rather than a threaded parameter. */
#ifdef DOOM_ZERO_BSS_PROBE
static u64 doom_probe_bss_cell;

u64 doom_probe_zero_bss_write(u64 value) {
    doom_probe_bss_cell ^= value;
    return doom_probe_bss_cell;
}
#endif
