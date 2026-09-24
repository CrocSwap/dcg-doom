#!/usr/bin/env python3
"""Generate Doom patch 0007: relocate mutable engine globals into doom_ctx_t.

Every mutable engine global becomes a field of one context struct `doom_ctx_t`.
On SBF the context pointer is bound per instruction to the current account-data
mapping, allowing account-relative native pointers to be rebased when an account
moves.  Immutable tables become const (.rodata).  Dead debug globals are
removed.

Usage: mkctx.py <linuxdoom-1.10-dir>   (0001-0006 already applied)
Edits the tree in place and writes headless/doom_ctx.h + doom_ctx_init.c.

Method (see docs/experiments/doom-step-5-ctx-constant-2026-09-18.md):
- The 411-symbol worklist comes from llvm-readelf over the 41-TU SBF link
  (doom_ctx_spec.json pins each definition's TU/line/type; every slice is
  verified against the live tree before editing, so drift fails loudly).
- Names with NO struct-member/param/local collision get a shared
  `#define N (DOOM_CTX->N)` (rule A).
- Names colliding with a struct member either rename the MEMBER (rule D,
  member accesses are syntactically unambiguous `x->N`) or, where the bare
  uses are fewer, replace bare uses with `(DOOM_CTX->N)` and define nothing
  (rule C).  Both directions fail LOUD (compile error), never silent.
- Shadowing locals/params are renamed (rule R).
"""

import json
import os
import re
import sys

TUS = ("doomdef doomstat dstrings d_items g_game m_argv m_bbox m_fixed m_random "
       "m_swap tables info z_zone w_wad v_video p_ceilng p_doors p_enemy p_floor "
       "p_inter p_lights p_map p_maputl p_mobj p_plats p_pspr p_setup p_sight "
       "p_spec p_switch p_telept p_tick p_user r_bsp r_data r_draw r_main r_plane "
       "r_segs r_sky r_things").split()

# Initialized tables proven (object-text write scan outside #if 0, including
# member-level compound assignment, plus ASan backstop) to be never written
# at runtime: become const (.rodata).  states/mobjinfo look immutable but
# the -fast/nightmare skill path writes states[].tics and mobjinfo[].speed,
# so they stay mutable context fields (measured, not assumed).
CONST = {"finesine", "finetangent", "tantoangle",
         "gammatable", "sprnames", "alphSwitchList", "animdefs", "rndtable",
         "weaponinfo", "fuzzoffset", "checkcoord", "endmsg", "pars", "cpars",
         "opposite", "xspeed", "yspeed", "diags", "clipammo", "maxammo",
         "TRACEANGLE"}

# Dead debug globals (W_Profile writes waddump.txt via file I/O; its only
# caller is commented out): removed with the function.
DELETE = {"info", "profilecount"}

# Rule C: struct member of the same name exists, so no #define; bare uses
# become (DOOM_CTX->N) explicitly.
C_NAMES = {"angleturn", "bottomtexture", "consoleplayer", "deathmatch",
           "linecount", "midtexture", "numlines", "numnodes", "sidemove",
           "soundtarget", "toptexture"}

# Rule D: rename the struct MEMBER old->new (member accesses are unambiguous
# `x->N`), keep the global #define.
D_RENAMES = {"backsector": "backsector_m", "frontsector": "frontsector_m",
             "columnofs": "columnofs_m", "consistancy": "consistancy_m",
             "curline": "curline_m", "drawsegs": "drawsegs_m",
             "extralight": "extralight_m", "fixedcolormap": "fixedcolormap_m",
             "forwardmove": "forwardmove_m", "linedef": "linedef_m",
             "lines": "lines_m", "maskedtexturecol": "maskedtexturecol_m",
             "numlumps": "numlumps_m", "numsegs": "numsegs_m",
             "sidedef": "sidedef_m", "validcount": "validcount_m",
             "viewheight": "viewheight_m", "viewz": "viewz_m",
             "vissprites": "vissprites_m"}

# Rule R: shadowing locals/params renamed (verified to never use the global
# of the same name in that function).
R_RENAMES = [
    # (file, function, old, new)
    ("p_maputl.c", "P_LineOpening", "linedef", "openline"),
    ("p_setup.c", "P_LoadSegs", "linedef", "ldnum"),
    ("p_sight.c", "P_CrossSubsector", "opentop", "sighttop"),
    ("p_sight.c", "P_CrossSubsector", "openbottom", "sightbottom"),
]

# File-scoped renames for headless-driver locals colliding with context
# macros (AST-verified: no engine-global use of that name in the file).
FILE_RENAMES = [
    # (file, old, new)
    ("headless/doom_headless.c", "states", "statehashes"),
]

# Platform-layer (doom_sbf_platform.c, ours) mutable state that must also
# leave the image: fields + defines.  Each entry is (C type, field name,
# array suffix, initializer or None).  Stub globals Nothing in the
# 41-TU cut references (devparm, drone, inhelpscreens, mousemoved, snd
# volumes, debugfile, errno) are deleted, not moved; FILE stderr/stdout/
# stdin become const NULL (.rodata) since in-cut engine code only reads
# them (sbfinc/stdio.h declares them const to match).
PLATFORM_KEEP = [
    ("unsigned char *", "arena_base", "", None),
    ("unsigned long", "arena_bytes", "", None),
    ("unsigned long", "arena_used", "", None),
    ("unsigned long", "arena_zone_bytes", "", None),
    ("int", "arena_exhausted", "", None),
    ("unsigned long", "arena_high_water", "", None),
    ("int", "mb_used", "", "PLATFORM_MB_USED"),
    ("unsigned char", "empty_ticcmd", "[8]", None),
    ("int", "nomonsters", "", None),
    ("int", "respawnparm", "", None),
    ("int", "fastparm", "", None),
    ("int", "singletics", "", None),
    ("int", "menuactive", "", None),
    ("int", "automapactive", "", None),
    ("int", "screenblocks", "", "11"),
    ("int", "detailLevel", "", None),
    ("int", "mouseSensitivity", "", "5"),
    # Host default was GS_DEMOSCREEN (SBF stub had 0/GS_LEVEL, but the SBF
    # side never ran engine code; the retained goldens pin GS_DEMOSCREEN).
    ("int", "wipegamestate", "", "GS_DEMOSCREEN"),
    ("int", "maketic", "", None),
    ("int", "ticdup", "", "1"),
    ("unsigned char *", "save_p", "", None),
    ("unsigned char", "netcmds", "[4 * 128 * 8]", None),
    ("char *", "player_names", "[4]",
     '{"player 1", "player 2", "player 3", "player 4"}'),
    ("int", "doom_error_latched", "", None),
    ("int", "doom_sbf_demo_done", "", None),
    ("unsigned int", "doom_render_fuzz_columns", "", None),
]

# Engine headers doom_ctx.h includes for member types (iterated against the
# compiler; the TU's own later includes become no-ops via guards).
CTX_INCLUDES = ["doomtype.h", "doomdata.h", "doomdef.h", "d_think.h",
                "d_player.h", "d_ticcmd.h", "d_items.h", "info.h", "tables.h",
                "m_fixed.h", "r_defs.h", "p_mobj.h", "p_tick.h", "p_spec.h",
                "p_setup.h", "p_local.h", "w_wad.h", "z_zone.h", "v_video.h",
                "g_game.h", "d_event.h", "d_net.h", "m_swap.h", "r_main.h",
                "r_data.h", "r_draw.h", "r_plane.h", "dstrings.h", "sounds.h"]

DOOM_ZONE_BYTES = 3 * 1024 * 1024
# Pinned first-account data address (M1021); the instruction guard refuses
# any other mapping.  Overridden on the mkctx command line after measuring
# the real layout with the final image.
DOOM_CTX_ADDR_DEFAULT = "0x400000060UL"

HERE = os.path.dirname(os.path.abspath(__file__))


def word_re(name):
    return re.compile(r"(?<![_a-zA-Z0-9])" + re.escape(name) + r"(?![_a-zA-Z0-9])")


def split_toplevel(text, sep):
    """Split text at sep (a single char) ignoring (), {}, [] nesting."""
    parts, depth, cur = [], 0, ""
    pairs = {"(": ")", "[": "]", "{": "}"}
    closers = set(pairs.values())
    stack = []
    i = 0
    while i < len(text):
        c = text[i]
        if c in pairs:
            stack.append(pairs[c])
            depth += 1
        elif stack and c == stack[-1]:
            stack.pop()
            depth -= 1
        if c == sep and depth == 0:
            parts.append(cur)
            cur = ""
        else:
            cur += c
        i += 1
    parts.append(cur)
    return parts


def load_tree(src):
    files = {}
    for tu in TUS:
        p = os.path.join(src, tu + ".c")
        files[tu + ".c"] = open(p, errors="replace").read().splitlines()
    for h in sorted(os.listdir(src)):
        if h.endswith(".h"):
            files[h] = open(os.path.join(src, h),
                            errors="replace").read().splitlines()
    for h in sorted(os.listdir(os.path.join(src, "headless"))):
        if h.endswith(".c") or h.endswith(".h"):
            files["headless/" + h] = open(
                os.path.join(src, "headless", h),
                errors="replace").read().splitlines()
    return files


def split_comment(line):
    """Split trailing // comment (outside strings) from code."""
    q = False
    for k, c in enumerate(line):
        if c == '"' and (k == 0 or line[k - 1] != "\\"):
            q = not q
        if c == "/" and not q and line[k:k + 2] == "//":
            return line[:k], line[k:]
    return line, ""


def slice_def(lines, rec, name):
    """Return (decl_head, init_text_or_None) for a definition record."""
    start = rec["line"] - 1
    end = (rec.get("endline") or rec["line"]) - 1
    assert 0 <= start <= end < len(lines), (name, start, end)
    body = "\n".join(lines[start:end + 1])
    assert word_re(name).search(body), ("name missing in slice", name, body[:120])
    code, _comment = split_comment(lines[end])
    assert code.strip().endswith(";"), ("def does not end with ;", name,
                                        code.strip()[-60:])
    lines[end] = code.rstrip()
    body = "\n".join(lines[start:end + 1])
    # Strip one trailing ; and split declarator from initializer.
    core = body.rstrip()
    assert core.endswith(";")
    core = core[:-1]
    chunks = split_toplevel(core, "=")
    if len(chunks) == 1:
        return core.strip(), None
    head, init = chunks[0], "=".join(chunks[1:])
    return head.strip(), init.strip()


def field_of(head, name, new_name=None):
    """Turn a declarator head into a struct field (drop leading storage)."""
    text = re.sub(r"^\s*(static|extern)\s+", "", head).strip()
    if new_name and new_name != name:
        text = word_re(name).sub(new_name, text, count=1)
    return text + ";"


def const_of(head, name):
    """Add const to a definition head, preserving use-site types.

    `T n[N]` -> `const T n[N]`; `T *n[N]` -> `T *const n[N]`.
    """
    m = word_re(name).search(head)
    assert m, ("const head lacks name", head[:100])
    before = head[:m.start()]
    if "*" in before:
        return head[:m.start()] + "const " + head[m.start():]
    stripped = head.lstrip()
    indent = head[:len(head) - len(stripped)]
    return indent + "const " + stripped


def strip_strings_comments(line):
    line = re.sub(r"//.*$", "", line)
    line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
    return line


def is_member_decl(line, name):
    """Heuristic: a struct-member declaration of `name` (also one-line)."""
    if "(" in line or "=" in line or line.lstrip().startswith("#"):
        return False
    m = re.search(r"(^|[{};])\s*[A-Za-z_][\w\s\*]*?\b" + re.escape(name) +
                  r"\b\s*(\[[^\]]*\])?\s*;", line)
    if not m:
        return False
    seg = m.group(0)
    if re.search(r"\b(return|case|goto|sizeof|if|while|for|switch|else|do|"
                 r"typedef|extern|static)\b", seg):
        return False
    return True


def find_extern_type(all_lines, name):
    """Collect distinct `extern` declaration bodies for a platform name."""
    found = []
    for fname, lines in sorted(all_lines.items()):
        for line in lines:
            code, _ = split_comment(line)
            s = code.strip()
            if not re.match(r"extern\s", s) or not s.endswith(";"):
                continue
            if "(" in s and not re.search(r"\(\s*\*\s*" + re.escape(name) +
                                          r"\s*\)", s):
                continue  # function declaration, not our variable
            if not word_re(name).search(s):
                continue
            body = re.sub(r"^extern\s+", "", s)[:-1].strip()
            found.append((fname, body))
    return found


class Ctx:
    def __init__(self, src, addr):
        self.src = src
        self.addr = addr
        self.spec = json.load(open(os.path.join(HERE, "doom_ctx_spec.json")))
        self.files = load_tree(src)
        self.fields = []      # (field_name, field_decl)
        self.defines = []     # (macro_name, field_name)
        self.init_scalar = []  # (field, init_text)
        self.init_array = []   # (field, [elements])
        self.inits_tail = []   # verbatim extra init lines (platform)
        self.deletions = {}    # fname -> [(start, end)] 0-based inclusive
        self.log = []

    def emit(self, msg):
        self.log.append(msg)

    def delete_range(self, fname, start, end):
        self.deletions.setdefault(fname, []).append((start, end))

    def apply_deletions(self):
        for fname, ranges in self.deletions.items():
            lines = self.files[fname]
            for start, end in sorted(ranges, reverse=True):
                assert 0 <= start <= end < len(lines), (fname, start, end)
                del lines[start:end + 1]

    # -- pass 1: definitions -------------------------------------------
    def pass_definitions(self):
        self.done = set()
        for name in sorted(self.spec):
            if name in self.done:
                continue
            rec = self.spec[name]
            if "." in name:
                self.local_static(name, rec)
                self.done.add(name)
                continue
            if rec.get("line") is None:
                continue  # consumed via its multi-declarator statement
            fname = rec["tu"] + ".c"
            lines = self.files[fname]
            head, init = slice_def(lines, rec, name)
            # Multi-declarator statement: split into per-name fields.
            chunks = split_toplevel(head, ",")
            if len(chunks) > 1:
                self.multi_declarator(fname, rec, name, chunks, init)
                continue
            if name in DELETE:
                self.delete_range(fname, rec["line"] - 1,
                                  (rec.get("endline") or rec["line"]) - 1)
                self.emit("delete dead %s %s:%d" % (name, fname, rec["line"]))
                continue
            if name in CONST:
                assert init is not None, ("const without init", name)
                new_head = const_of(head, name)
                assert "\n" not in new_head, ("multiline const head", name)
                first = lines[rec["line"] - 1]
                assert head in first, ("head not on first line", name, first)
                lines[rec["line"] - 1] = first.replace(head, new_head, 1)
                self.emit("const %s %s:%d" % (name, fname, rec["line"]))
                continue
            # Context-bound global.
            field = field_of(head, name)
            assert field.count(";") == 1 and field.endswith(";")
            if init is None:
                pass
            elif re.match(r"^\s*\{", init):
                # Array initializer: keep a const copy in .rodata and memcpy
                # it in (uniform for scalar and struct elements).
                m = word_re(name).search(field)
                assert m, ("array field lacks name", field)
                etype = field[:m.start()].strip()
                suffix = field[m.end():].strip()
                assert suffix.startswith("[") and suffix.endswith(";"), \
                    (name, field)
                suffix = suffix[:-1]
                self.init_array.append((name, etype, suffix, init.strip()))
            else:
                self.init_scalar.append((name, init))
            self.fields.append((name, field))
            if name in C_NAMES:
                pass  # no #define; bare uses rewritten explicitly
            else:
                self.defines.append((name, name))
            self.delete_range(fname, rec["line"] - 1,
                              (rec.get("endline") or rec["line"]) - 1)
            self.emit("ctx %s %s:%d" % (name, fname, rec["line"]))
            self.done.add(name)
        # Every line-less global must have been consumed by its statement.
        for name in sorted(self.spec):
            rec = self.spec[name]
            if "." not in name and rec.get("line") is None:
                assert name in self.done, ("unconsumed collaborator", name)

    def multi_declarator(self, fname, rec, name, chunks, init):
        assert init is None, ("multi-declarator with init", name)
        lines = self.files[fname]
        base = None
        for k, chunk in enumerate(chunks):
            names = [n for n in self.spec
                     if "." not in n and self.spec[n]["tu"] == rec["tu"]
                     and word_re(n).search(chunk)]
            assert len(names) == 1, (fname, chunk[:80], names)
            n = names[0]
            assert self.spec[n].get("line") is None or n == name, (n, name)
            if k == 0:
                m = word_re(n).search(chunk)
                base = chunk[:m.start()]
                field = chunk.strip() + ";"
            else:
                field = (base + " " + chunk.strip()).strip() + ";"
            assert n not in C_NAMES and n not in CONST and n not in DELETE, \
                ("special name in multi-declarator", n)
            self.fields.append((n, field))
            self.defines.append((n, n))
            self.done.add(n)
            self.emit("ctx-multi %s %s" % (n, fname))
        self.delete_range(fname, rec["line"] - 1,
                          (rec.get("endline") or rec["line"]) - 1)

    def local_static(self, name, rec):
        var = name.partition(".")[2]
        fname = rec["tu"] + ".c"
        lines = self.files[fname]
        if rec.get("line") is None:
            idx = self.find_local_decl(fname, name.partition(".")[0], var)
        else:
            idx = rec["line"] - 1
        assert word_re(var).search(lines[idx]), (name, lines[idx][:100])
        head, init = slice_def(lines, {"line": idx + 1,
                                       "endline": idx + 1}, var)
        if init is not None:
            assert init.strip() == "0", ("local static nonzero init", name)
            init = None
        self.fields.append((var, field_of(head, var)))
        self.defines.append((var, var))
        self.delete_range(fname, idx, idx)
        self.emit("ctx-local %s %s:%d" % (name, fname, idx + 1))

    def find_local_decl(self, fname, func, var):
        lines = self.files[fname]
        cands = [i for i, l in enumerate(lines)
                 if re.search(r"\b" + re.escape(func) + r"\s*\(", l)]
        assert cands, ("func not found", fname, func)
        # Definition = the candidate followed by an opening brace.
        start = None
        for i in cands:
            for j in range(i, min(i + 4, len(lines))):
                if lines[j].strip().startswith("{"):
                    start = i
                    break
            if start is not None:
                break
        assert start is not None, ("func body not found", fname, func)
        depth = 0
        end = start
        for i in range(start, len(lines)):
            for _k, ch in enumerate(lines[i]):
                if ch == "{":
                    depth += 1
                elif ch == "}":
                    depth -= 1
            if depth == 0 and i > start:
                end = i
                break
        for i in range(start, end + 1):
            if re.search(r"^\s*static\b.*\b" + re.escape(var) + r"\b",
                         lines[i]):
                return i
        raise AssertionError(("local decl not found", fname, func, var))

    # -- pass 2: extern declarations -----------------------------------
    def pass_externs(self):
        moved = {n for n in self.spec if "." not in n and n not in DELETE}
        moved |= {f for _, f, _, _ in PLATFORM_KEEP}
        for fname, lines in self.files.items():
            keep = []
            for i, line in enumerate(lines):
                code, _ = split_comment(line)
                s = code.strip()
                hit = None
                if re.match(r"extern\s", s) and s.endswith(";"):
                    for n in sorted(moved):
                        if not word_re(n).search(s):
                            continue
                        if "(" in s and not re.search(
                                r"\(\s*\*\s*" + re.escape(n) + r"\s*\)", s):
                            continue  # function declaration
                        hit = n
                        break
                if hit is None:
                    keep.append(line)
                    continue
                if hit in CONST:
                    new = const_of(s[:-1].strip(), hit) + ";"
                    keep.append(line.replace(s, new, 1))
                    self.emit("const-extern %s %s:%d" % (hit, fname, i + 1))
                else:
                    self.emit("del-extern %s %s:%d: %s"
                              % (hit, fname, i + 1, s[:90]))
            self.files[fname] = keep
        # Any surviving multi-line extern mentioning a moved name is manual.
        for fname, lines in self.files.items():
            for i, line in enumerate(lines):
                code, _ = split_comment(line)
                s = code.strip()
                if re.match(r"extern\s", s) and not s.endswith(";"):
                    for n in sorted(moved):
                        if word_re(n).search(s):
                            raise AssertionError(
                                ("multiline extern", fname, i + 1, s[:100]))

    # -- pass 3: rule C bare-use replacement ---------------------------
    def pass_rule_c(self):
        for n in sorted(C_NAMES):
            for fname, lines in self.files.items():
                for i, line in enumerate(lines):
                    if not word_re(n).search(line):
                        continue
                    code, _ = split_comment(line)
                    if fname.endswith(".h") and is_member_decl(code, n):
                        continue
                    new = self.replace_bare(line, n,
                                            "(DOOM_CTX->%s)" % n)
                    if new != line:
                        lines[i] = new
                        self.emit("C %s %s:%d" % (n, fname, i + 1))

    @staticmethod
    def replace_bare(line, name, repl, skip_call=False):
        # Split // comment tail (outside strings) from processing.
        code, tail = line, ""
        q = False
        for k, c in enumerate(line):
            if c == '"' and (k == 0 or line[k - 1] != "\\"):
                q = not q
            if c == "/" and not q and line[k:k + 2] == "//":
                code, tail = line[:k], line[k:]
                break
        res = ""
        k = 0
        while k < len(code):
            c = code[k]
            if c == '"':
                res += c
                k += 1
                while k < len(code) and code[k] != '"':
                    res += code[k]
                    if code[k] == "\\" and k + 1 < len(code):
                        res += code[k + 1]
                        k += 1
                    k += 1
                if k < len(code):
                    res += code[k]
                    k += 1
                continue
            mm = word_re(name).match(code, k)
            if mm and k > 0 and code[k - 1] in ".>":
                mm = None
            if mm and skip_call and re.match(r"\s*\(", code[mm.end():]):
                mm = None
            if mm:
                res += repl
                k = mm.end()
            else:
                res += c
                k += 1
        return res + tail

    # -- pass 4: rule D member renames ---------------------------------
    def pass_rule_d(self):
        for old, new in sorted(D_RENAMES.items()):
            # New member name must not occur anywhere in scope.
            for fname, lines in self.files.items():
                assert not any(word_re(new).search(l) for l in lines), \
                    ("rename target exists", old, new, fname)
            for fname, lines in self.files.items():
                for i, line in enumerate(lines):
                    if not word_re(old).search(line):
                        continue
                    code = strip_strings_comments(line)
                    changed = False
                    # Member accesses: x->N, x.N (spaces allowed).
                    new_line, nsub = re.subn(
                        r"(\.\s*|->\s*)" + re.escape(old) + r"\b",
                        r"\g<1>" + new, line)
                    if nsub:
                        line, changed = new_line, True
                    # Member declarations (headers and local structs).
                    if is_member_decl(code, old):
                        line2, nsub2 = re.subn(
                            r"(?<![_a-zA-Z0-9])" + re.escape(old) +
                            r"(?![_a-zA-Z0-9])", new, line)
                        assert nsub2 == 1, (fname, i + 1, line[:100])
                        line, changed = line2, True
                    if changed:
                        lines[i] = line
                        self.emit("D %s->%s %s:%d" % (old, new, fname, i + 1))

    # -- pass 4b: vldoor_e close/open collide with POSIX once p_spec.h
    # rides the forced include into unistd/fcntl TUs: rename the enumerators
    # (16 in-cut use sites + the definition; real open()/close() calls and
    # strings are left alone).
    def pass_enum(self):
        renames = (("close", "doorclose"), ("open", "dooropen"))
        for old, new in renames:
            for fname, lines in self.files.items():
                assert not any(word_re(new).search(l) for l in lines), \
                    ("enum rename target exists", new, fname)
        for old, new in renames:
            for fname, lines in self.files.items():
                for i, line in enumerate(lines):
                    if not word_re(old).search(line):
                        continue
                    if line.strip().startswith("#"):
                        continue  # preprocessor: #undef close stays
                    new_line = self.replace_bare(line, old, new,
                                                 skip_call=True)
                    if new_line != line:
                        lines[i] = new_line
                        self.emit("E %s->%s %s:%d" % (old, new, fname, i + 1))

    # -- pass 5: rule R local/param renames ----------------------------
    def pass_rule_r(self):
        for fname, func, old, new in R_RENAMES:
            assert not any(word_re(new).search(l)
                           for l in self.files[fname]), (fname, new)
            lines = self.files[fname]
            start = next(i for i, l in enumerate(lines)
                         if re.search(r"\b" + re.escape(func) +
                                      r"\s*\(", l))
            brace = next(i for i in range(start, len(lines))
                         if lines[i].strip().startswith("{"))
            depth = 0
            end = brace
            for i in range(brace, len(lines)):
                depth += lines[i].count("{") - lines[i].count("}")
                if depth == 0:
                    end = i
                    break
            self.emit("R %s %s:%d-%d" % (old, fname, start + 1, end + 1))
            for i in list(range(start, end + 1)):
                if word_re(old).search(lines[i]):
                    # Member accesses (x->old) keep the member name.
                    lines[i] = self.replace_bare(lines[i], old, new)
        # P_LineOpening prototype lives in p_local.h: rename there too.
        pl = self.files["p_local.h"]
        for i, line in enumerate(pl):
            if "P_LineOpening" in line and word_re("linedef").search(line):
                pl[i] = word_re("linedef").sub("openline", line)
                self.emit("R proto p_local.h:%d" % (i + 1))
        for fname, old, new in FILE_RENAMES:
            assert not any(word_re(new).search(l)
                           for l in self.files[fname]), (fname, new)
            for i, line in enumerate(self.files[fname]):
                if word_re(old).search(line):
                    self.files[fname][i] = self.replace_bare(line, old, new)
            self.emit("R-file %s %s->%s" % (fname, old, new))

    # -- pass 6: platform fields + W_Profile + host stubs ---------------
    def pass_platform(self):
        for ctype, field, suffix, init in PLATFORM_KEEP:
            bodies = find_extern_type(self.files, field)
            if bodies:
                types = {b for _, b in bodies}
                assert len(types) == 1, (field, bodies)
                declared = list(types)[0]
                self.emit("platform %s declared as: %s" % (field, declared))
                # The header's own spelling is authoritative for the field.
                self.fields.append((field, declared + ";"))
            else:
                self.emit("platform %s: no header extern, using %s %s%s"
                          % (field, ctype, field, suffix))
                self.fields.append((field, "%s %s%s;" % (ctype, field, suffix)))
            self.defines.append((field, field))
            if init is None:
                continue
            if init == "PLATFORM_MB_USED":
                self.inits_tail.append("#ifdef DOOM_SBF_BUILD")
                self.inits_tail.append("    DOOM_CTX->mb_used = 3;")
                self.inits_tail.append("#else")
                self.inits_tail.append("    DOOM_CTX->mb_used = 64;")
                self.inits_tail.append("#endif")
            elif suffix:
                if "*" in ctype:
                    # Array of pointers (player_names): a static const
                    # table of string addresses needs .data for its
                    # relocations, which the SBF loader refuses.  Assign
                    # each element at runtime instead; string literals
                    # themselves stay in .rodata.
                    elems = [e.strip() for e in init.strip("{}").split(",")]
                    for i, e in enumerate(elems):
                        self.inits_tail.append(
                            "DOOM_CTX->%s[%d] = %s;" % (field, i, e))
                else:
                    self.inits_tail.append(
                        "{ static const %s doom_ctx_init_%s%s = %s;"
                        % (ctype, field, suffix, init))
                    self.inits_tail.append(
                        "  memcpy(DOOM_CTX->%s, doom_ctx_init_%s, sizeof(DOOM_CTX->%s)); }"
                        % (field, field, field))
            else:
                self.inits_tail.append("DOOM_CTX->%s = %s;" % (field, init))
        # ticcmd scratch for I_BaseTiccmd, shared by both platform layers.
        # No #define: the two I_BaseTiccmd bodies use DOOM_CTX->emptycmd.
        self.fields.append(("emptycmd", "ticcmd_t emptycmd;"))

    # -- pass 1b: hoist TU-local types/macros needed for field sizes ----
    # Each entry: (src, end_regex, start_regex_or_None, dst_header).
    # start None = single line.  Types are found by end marker, scanning
    # back to the opening typedef.
    HOISTS = [
        ("z_zone.c", r"^\} memzone_t;$", r"^typedef struct$", "z_zone.h"),
        ("r_data.c", r"^\} texpatch_t;$", r"^typedef struct", "r_data.h"),
        ("r_data.c", r"^\} texture_t;$", r"^typedef struct", "r_data.h"),
        ("r_bsp.c", r"^\} cliprange_t;$", r"^typedef\s+struct$", "r_defs.h"),
        ("r_bsp.c", r"^#define MAXSEGS\s", None, "r_defs.h"),
        ("r_plane.c", r"^#define MAXVISPLANES\s", None, "r_defs.h"),
        ("r_plane.c", r"^#define MAXOPENINGS\s", None, "r_defs.h"),
        ("r_draw.c", r"^#define MAXWIDTH\s", None, "r_defs.h"),
        ("r_draw.c", r"^#define MAXHEIGHT\s", None, "r_defs.h"),
        ("g_game.c", r"^#define\s+BODYQUESIZE\s", None, "g_game.h"),
        ("g_game.c", r"^#define NUMKEYS\s", None, "d_event.h"),
        ("p_spec.c", r"^#define MAXLINEANIMS\s", None, "p_spec.h"),
        ("p_spec.c", r"^\} anim_t;$", r"^typedef struct$", "p_spec.h"),
        ("p_spec.c", r"^#define MAXANIMS\s", None, "p_spec.h"),
        ("p_setup.c", r"^#define MAX_DEATHMATCH_STARTS\s", None, "g_game.h"),
        ("p_enemy.c", r"^#define MAXSPECIALCROSS\s", None, "p_local.h"),
    ]

    def pass_hoist(self):
        for src, end_re, start_re, dst in self.HOISTS:
            lines = self.files[src]
            ends = [i for i, l in enumerate(lines) if re.match(end_re, l)]
            assert len(ends) == 1, (src, end_re, ends)
            end = ends[0]
            if start_re is None:
                start = end
            else:
                start = next(i for i in range(end, -1, -1)
                             if re.match(start_re, lines[i]))
            block = lines[start:end + 1]
            del lines[start:end + 1]
            h = self.files[dst]
            guards = [i for i, l in enumerate(h)
                      if re.match(r"^#endif\b", l.strip())]
            assert guards, ("no guard", dst)
            at = guards[-1]
            ins = ["/* Patch 0007: hoisted from %s for doom_ctx_t. */"
                   % src] + block
            h[at:at] = ins
            self.emit("hoist %s:%d-%d -> %s" % (src, start + 1, end + 1, dst))
        # Second copy of the shared macro.
        lines = self.files["p_map.c"]
        hits = [i for i, l in enumerate(lines)
                if re.match(r"^#define MAXSPECIALCROSS\s", l)]
        assert len(hits) == 1, hits
        del lines[hits[0]:hits[0] + 1]
        self.emit("hoist p_map.c MAXSPECIALCROSS deleted (kept in p_local.h)")

    def pass_wprofile(self):
        lines = self.files["w_wad.c"]
        start = next(i for i, l in enumerate(lines)
                     if re.search(r"\bW_Profile\s*\(void\)", l))
        brace = next(i for i in range(start, len(lines))
                     if lines[i].strip().startswith("{"))
        depth = 0
        for i in range(brace, len(lines)):
            depth += lines[i].count("{") - lines[i].count("}")
            if depth == 0:
                end = i
                break
        del lines[start:end + 1]
        self.emit("delete W_Profile w_wad.c:%d-%d" % (start + 1, end + 1))

    STUB_TYPE_WORDS = {"static", "extern", "const", "unsigned", "signed",
                       "void", "char", "short", "int", "long", "float",
                       "double", "boolean", "byte", "fixed_t", "angle_t",
                       "ticcmd_t", "gamestate_t"}

    @classmethod
    def stub_declared(cls, line, drop):
        """Name declared (not merely used) by this definition line."""
        s = line.strip()
        if "(" in s or s.startswith("/*") or s.startswith("*"):
            return None
        toks = re.findall(r"[A-Za-z_]\w*|\*", s)
        i = 0
        while i < len(toks) and toks[i] in cls.STUB_TYPE_WORDS:
            i += 1
        while i < len(toks) and toks[i] == "*":
            i += 1
        if i < len(toks) and toks[i] in drop:
            return toks[i]
        return None

    def pass_stubs(self):
        # Host definitions now owned by the context (dead ones too).
        drop = ["mb_used", "inhelpscreens", "menuactive", "automapactive",
                "viewactive", "paused", "usergame", "snd_SfxVolume",
                "snd_MusicVolume", "mousemoved", "wipegamestate",
                "mouseSensitivity", "detailLevel", "screenblocks", "maketic",
                "netcmds", "ticdup", "save_p", "player_names", "emptycmd",
                "nomonsters", "respawnparm", "fastparm", "singletics",
                "doom_render_fuzz_columns"]
        for fname in [f for f in self.files
                      if f.startswith("headless/") and f.endswith(".c")]:
            out = []
            for i, line in enumerate(self.files[fname]):
                hit = self.stub_declared(line, drop)
                if hit:
                    self.emit("del-stub %s %s:%d" % (hit, fname, i + 1))
                    continue
                out.append(line)
            self.files[fname] = out
        lines = self.files["headless/doom_stubs.c"]
        # Zone comes from the context now, capped at the measured floor.
        text = "\n".join(lines)
        old_heap = "int I_GetHeapSize(void){return mb_used*1024*1024;}"
        new_heap = ("int I_GetHeapSize(void){int want=mb_used*1024*1024;"
                    "return want<DOOM_ZONE_BYTES?want:DOOM_ZONE_BYTES;}")
        assert old_heap in text
        text = text.replace(old_heap, new_heap, 1)
        old_base = ("byte *I_ZoneBase(int *size)"
                    "{*size=mb_used*1024*1024;return calloc(1,*size);}")
        new_base = ("byte *I_ZoneBase(int *size){int want=mb_used*1024*1024;"
                    "unsigned i;byte *z;\n"
                    "if(want>DOOM_ZONE_BYTES)want=DOOM_ZONE_BYTES;\n"
                    "*size=want;z=DOOM_CTX->zone.bytes;\n"
                    "for(i=0;i<(unsigned)want;i++)z[i]=0;\nreturn z;}")
        assert old_base in text
        text = text.replace(old_base, new_base, 1)
        old_tick = "ticcmd_t *I_BaseTiccmd(void){return &emptycmd;}"
        new_tick = ("ticcmd_t *I_BaseTiccmd(void)"
                    "{return &DOOM_CTX->emptycmd;}")
        assert old_tick in text
        text = text.replace(old_tick, new_tick, 1)
        self.files["headless/doom_stubs.c"] = text.split("\n")
        self.emit("stubs zone/base/ticcmd rewritten")

    # -- emission ----------------------------------------------------
    def emit_headers(self):
        names = {n for n, _ in self.fields}
        assert "zone" not in names and "doom_ctx_probe_cell" not in names
        h = []
        h.append("/* Patch 0007: every mutable engine global lives in doom_ctx_t,")
        h.append(" * reached through DOOM_CTX.  On the host it is a real backing;")
        h.append(" * on SBF doom_ctx_guard binds it to the current account mapping.")
        h.append(" * Generated by stunts/doom-host/tools/")
        h.append(" * mkctx.py: do not hand-edit. */")
        h.append("#ifndef DOOM_CTX_H")
        h.append("#define DOOM_CTX_H")
        for inc in CTX_INCLUDES:
            h.append('#include "%s"' % inc)
        h.append("")
        h.append("/* Measured zone floor: 2 MiB fails the DEMO1 replay, 3 MiB")
        h.append(" * completes all 5,026 tics with the golden stream (M1006). */")
        h.append("#define DOOM_ZONE_BYTES %d" % DOOM_ZONE_BYTES)
        h.append("")
        h.append("typedef struct doom_ctx_s {")
        h.append("    unsigned long doom_ctx_probe_cell;")
        for _, field in self.fields:
            h.append("    " + field)
        h.append("    /* memzone_t needs pointer alignment; a plain byte array")
        h.append("     * inherits only offset alignment from the struct layout, so")
        h.append("     * the union pins 8-byte alignment on host and SBF alike. */")
        h.append("    union { void *align_me; unsigned char bytes[DOOM_ZONE_BYTES]; } zone;")
        h.append("} doom_ctx_t;")
        h.append("")
        h.append("void doom_ctx_init(void);")
        h.append("unsigned long doom_ctx_size(void);")
        h.append("")
        h.append("#ifdef DOOM_SBF_BUILD")
        h.append("extern doom_ctx_t *doom_ctx_active;")
        h.append("#define DOOM_CTX (doom_ctx_active)")
        h.append("#else")
        h.append("extern doom_ctx_t doom_ctx_host_backing;")
        h.append("#define DOOM_CTX (&doom_ctx_host_backing)")
        h.append("#endif")
        h.append("")
        h.append("/* Per-global access macros.  Files that spell DOOM_CTX->FIELD")
        h.append(" * explicitly (doom_ctx_init.c, the SBF platform layer) compile")
        h.append(" * with -DDOOM_CTX_NO_MACROS and use the explicit form; with the")
        h.append(" * macros active, DOOM_CTX->FIELD would re-expand FIELD. */")
        h.append("#ifndef DOOM_CTX_NO_MACROS")
        for macro, field in self.defines:
            h.append("#define %s (DOOM_CTX->%s)" % (macro, field))
        h.append("#endif")
        h.append("")
        h.append("#endif /* DOOM_CTX_H */")
        self.header_text = "\n".join(h) + "\n"
        c = []
        c.append("/* Patch 0007 runtime initializers: replay of every relocated")
        c.append(" * nonzero initializer, applied once before any engine use")
        c.append(" * (host: constructor; SBF: future init instruction).  Generated")
        c.append(" * by mkctx.py: do not hand-edit. */")
        c.append('#include "doom_ctx.h"')
        c.append("#include <string.h>")
        c.append("")
        # info.c declares the states action functions itself (no header);
        # the states const-copy below needs the same declarations.
        c.append("/* Action-function prototypes, mirrored from info.c. */")
        for line in self.files["info.c"]:
            if re.match(r"^void\s+A_\w+\(\);?\s*$", line):
                c.append(line.strip())
        c.append("")
        c.append("#ifndef DOOM_SBF_BUILD")
        c.append("doom_ctx_t doom_ctx_host_backing;")
        c.append("#endif")
        c.append("")
        c.append("unsigned long doom_ctx_size(void)")
        c.append("{")
        c.append("    return (unsigned long)sizeof(doom_ctx_t);")
        c.append("}")
        c.append("")
        c.append("void doom_ctx_init(void)")
        c.append("{")
        fnames = [n for n, _ in self.fields]
        for field, init in self.init_scalar:
            # This TU compiles with DOOM_CTX_NO_MACROS: spell member
            # references in init expressions explicitly.
            for other in fnames:
                init = self.replace_bare(init, other,
                                         "(DOOM_CTX->%s)" % other)
            c.append("    DOOM_CTX->%s = %s;" % (field, init))
        for field, etype, suffix, init in self.init_array:
            c.append("    { static const %s doom_ctx_init_%s%s = %s;"
                     % (etype, field, suffix, init))
            c.append("      memcpy(DOOM_CTX->%s, doom_ctx_init_%s, "
                     "sizeof(DOOM_CTX->%s)); }" % (field, field, field))
        for line in self.inits_tail:
            c.append("    " + line if not line.startswith("#") else line)
        c.append("}")
        c.append("")
        c.append("#ifndef DOOM_SBF_BUILD")
        c.append("__attribute__((constructor)) static void doom_ctx_auto_init(void)")
        c.append("{")
        c.append("    doom_ctx_init();")
        c.append("}")
        c.append("#endif")
        self.init_text = "\n".join(c) + "\n"

    def pass_makefile(self):
        with open(os.path.join(self.src, "headless/Makefile")) as fh:
            lines = fh.read().splitlines()
        text = "\n".join(lines)
        assert "doom_ctx_init.o" not in text
        # -include lives in CPPFLAGS (not CFLAGS): sanitizer runs override
        # CFLAGS explicitly and must keep the forced context header.
        old_cpp = "CPPFLAGS ?= -I.."
        assert old_cpp in text, "CPPFLAGS line changed"
        text = text.replace(old_cpp, "CPPFLAGS ?= -I.. -I. -include doom_ctx.h",
                            1)
        text = text.replace(
            "OBJS = $(patsubst ../%.c,%.o,$(SOURCES)) doom_headless.o doom_state.o doom_account.o doom_render.o doom_stubs.o",
            "OBJS = $(patsubst ../%.c,%.o,$(SOURCES)) doom_headless.o doom_state.o doom_account.o doom_render.o doom_stubs.o doom_ctx_init.o",
            1)
        text += ("\n\ndoom_ctx_init.o: doom_ctx_init.c doom_ctx.h\n"
                 "\t$(CC) $(CPPFLAGS) $(CFLAGS) -DDOOM_CTX_NO_MACROS "
                 "-c $< -o $@\n")
        self.files["headless/Makefile"] = text.split("\n")
        self.emit("makefile updated")

    # -- verification --------------------------------------------------
    def verify(self):
        leftovers = []
        for n in sorted(C_NAMES):
            for fname, lines in self.files.items():
                for i, line in enumerate(lines):
                    if not word_re(n).search(line):
                        continue
                    code, _ = split_comment(line)
                    if fname.endswith(".h") and is_member_decl(code, n):
                        continue
                    code = strip_strings_comments(line)
                    # Allow member accesses and the init-file macro uses.
                    tmp = re.sub(r"[.>]\s*" + re.escape(n) + r"\b", "", code)
                    if word_re(n).search(tmp):
                        leftovers.append((n, fname, i + 1, line.strip()[:100]))
        for item in leftovers:
            print("LEFTOVER-C %s %s:%d %s" % item, flush=True)
        assert not leftovers, "%d rule-C leftovers" % len(leftovers)
        for old in sorted(D_RENAMES):
            for fname, lines in self.files.items():
                for i, line in enumerate(lines):
                    code = strip_strings_comments(line)
                    if re.search(r"(\.\s*|->\s*)" + re.escape(old) + r"\b",
                                 code):
                        raise AssertionError(
                            ("member access survives", old, fname, i + 1))
        self.emit("verify ok")

    def write_out(self):
        for fname, lines in self.files.items():
            if fname == "headless/Makefile":
                path = os.path.join(self.src, "headless/Makefile")
            elif "/" in fname:
                path = os.path.join(self.src, fname)
            elif fname.endswith(".h"):
                path = os.path.join(self.src, fname)
            else:
                path = os.path.join(self.src, fname)
            with open(path, "w", errors="replace") as fh:
                fh.write("\n".join(lines) + "\n")
        with open(os.path.join(self.src, "headless/doom_ctx.h"), "w") as fh:
            fh.write(self.header_text)
        with open(os.path.join(self.src, "headless/doom_ctx_init.c"),
                  "w") as fh:
            fh.write(self.init_text)


def main():
    src = sys.argv[1]
    addr = sys.argv[2] if len(sys.argv) > 2 else DOOM_CTX_ADDR_DEFAULT
    ctx = Ctx(src, addr)
    ctx.pass_definitions()
    ctx.pass_platform()
    ctx.apply_deletions()
    ctx.pass_hoist()
    ctx.pass_wprofile()
    ctx.pass_externs()
    ctx.pass_rule_r()
    ctx.pass_enum()
    ctx.pass_rule_c()
    ctx.pass_rule_d()
    ctx.pass_stubs()
    ctx.emit_headers()
    ctx.pass_makefile()
    ctx.verify()
    ctx.write_out()
    print("fields=%d defines=%d init_scalar=%d init_array=%d" % (
        len(ctx.fields), len(ctx.defines), len(ctx.init_scalar),
        len(ctx.init_array)))
    for line in ctx.log:
        print(line)


if __name__ == "__main__":
    main()




