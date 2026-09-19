use std::env;

/// The 41 patch-0007 engine translation units (mkctx TUS): the whole engine
/// cut that runs on the host goldens, compiled for SBF with the context
/// macros on.
const ENGINE_TUS: &[&str] = &[
    "doomdef", "doomstat", "dstrings", "d_items", "g_game", "m_argv", "m_bbox", "m_fixed",
    "m_random", "m_swap", "tables", "info", "z_zone", "w_wad", "v_video", "p_ceilng", "p_doors",
    "p_enemy", "p_floor", "p_inter", "p_lights", "p_map", "p_maputl", "p_mobj", "p_plats",
    "p_pspr", "p_setup", "p_sight", "p_spec", "p_switch", "p_telept", "p_tick", "p_user", "r_bsp",
    "r_data", "r_draw", "r_main", "r_plane", "r_segs", "r_sky", "r_things",
];

fn main() {
    println!("cargo:rerun-if-changed=src/doom_hotloops.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_platform.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_ctx.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_engine.c");

    let target = env::var("TARGET").expect("Cargo supplies TARGET to build scripts");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_STATIC_WRITE_PROBE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_ZERO_BSS_PROBE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_CTX_ENGINE");
    println!("cargo:rerun-if-env-changed=DOOM_SCRATCH");

    let mut build = cc::Build::new();
    build.file("src/doom_hotloops.c");
    if env::var("CARGO_FEATURE_STATIC_WRITE_PROBE").is_ok() {
        build.define("DOOM_STATIC_WRITE_PROBE", None);
    }
    if env::var("CARGO_FEATURE_ZERO_BSS_PROBE").is_ok() {
        build.define("DOOM_ZERO_BSS_PROBE", None);
    }

    if target == "sbf-solana-solana" {
        // Do not let cc discover the host compiler: the C object has to be
        // SBF, then rustc/ld.lld fold it into the program image.
        build
            .compiler("/Users/colkitt/.cache/solana/v1.51/platform-tools/llvm/bin/clang")
            .archiver("/Users/colkitt/.cache/solana/v1.51/platform-tools/llvm/bin/llvm-ar")
            .target("sbf-solana-solana")
            .flag("--target=sbf-solana-solana")
            .flag("-O3")
            .flag("-ffreestanding")
            .flag("-fno-stack-protector")
            .flag("-fno-builtin")
            .flag("-fno-asynchronous-unwind-tables")
            .flag("-fno-unwind-tables");
    }

    build.compile("doom_hotloops");

    if env::var("CARGO_FEATURE_CTX_ENGINE").is_ok() {
        compile_ctx_engine(&target);
    }
}

/// Fold the 41-TU patch-0007 engine, its generated initializer, and the
/// context-mode platform layer into the program image.
///
/// The engine sources live outside git (fetched by stunts/doom-host/build.sh
/// into $DOOM_SCRATCH with patches 0001-0007 applied), so this resolves the
/// tree from the environment and fails loudly when patch 0007 is missing.
/// Three archives.  The engine TUs use the bare-name macros; doom_ctx_init.c
/// and doom_sbf_ctx.c spell DOOM_CTX->field explicitly and compile with
/// -DDOOM_CTX_NO_MACROS; doom_sbf_platform.c compiles WITHOUT -include
/// doom_ctx.h, because the engine headers pull in sbfinc/stdio.h's variadic
/// printf declarations and SBF cannot define a variadic function.
fn compile_ctx_engine(target: &str) {
    let manifest = std::path::PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let scratch = env::var("DOOM_SCRATCH").expect(
        "ctx-engine needs DOOM_SCRATCH=<dir> with the build.sh layout \
         (doom-src/linuxdoom-1.10 at patches 0001-0007)",
    );
    let tree = std::path::PathBuf::from(&scratch).join("doom-src/linuxdoom-1.10");
    let ctx_h = tree.join("headless/doom_ctx.h");
    assert!(
        ctx_h.is_file(),
        "ctx-engine needs patch 0007 in {}/doom-src (missing headless/doom_ctx.h); \
         run DOOM_SCRATCH=<dir> sh stunts/doom-host/build.sh first",
        scratch
    );
    println!("cargo:rerun-if-changed={}", ctx_h.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("src/doom_sbf_platform.c").display()
    );

    // Host parity first (the flags the golden-verified host build uses),
    // then the freestanding SBF backend flags the hotloop build uses.
    // DOOM_SBF_BUILD is SBF-only: host feature builds (driver link, crate
    // tests) keep the real host backing so the objects stay honest.
    let common: &[&str] = &[
        "-O2",
        "-std=gnu89",
        "-DNORMALUNIX",
        "-DHEADLESS_DOOM",
        "-Wno-deprecated-non-prototype",
        "-Wno-implicit-int",
        "-Wno-int-conversion",
        "-Wno-incompatible-pointer-types",
        "-Wno-format-security",
    ];
    // NB: cargo-build-sbf v1.51 reports TARGET as `sbpf-solana-solana`; the
    // older `sbf-solana-solana` spelling is kept so the same build.rs works
    // under both.  (The pre-existing hotloop block above still checks only
    // the old spelling and rides on env CC by luck; it is deliberately
    // untouched -- changing its codegen would silently shift the retained
    // hotloop CU measurements.)
    let is_sbf = target == "sbf-solana-solana" || target == "sbpf-solana-solana";
    let mut engine = cc::Build::new();
    let mut ctx = cc::Build::new();
    let mut platform = cc::Build::new();
    for b in [&mut engine, &mut ctx, &mut platform] {
        for f in common {
            b.flag(f);
        }
        b.include(&tree)
            .include(tree.join("headless"))
            .include(manifest.join("sbfinc"));
        if is_sbf {
            // cargo-build-sbf exports CC/AR pointing at its platform-tools
            // clang, which defaults to the BPF target (no --target flag
            // needed, and cc-rs 1.2.58 cannot parse the sbf triple anyway).
            // Reading them from the environment keeps this working across
            // installs instead of hardcoding one toolchain path.
            let cc = env::var("CC").expect(
                "ctx-engine SBF build needs CC from cargo-build-sbf \
                 (build with cargo-build-sbf, not plain cargo build)",
            );
            let ar = env::var("AR").expect("ctx-engine SBF build needs AR from cargo-build-sbf");
            b.compiler(cc).archiver(ar);
            b.flag("-DDOOM_SBF_BUILD")
                .flag("-ffreestanding")
                .flag("-fno-stack-protector")
                .flag("-fno-builtin")
                .flag("-fno-asynchronous-unwind-tables")
                .flag("-fno-unwind-tables");
        }
    }
    for tu in ENGINE_TUS {
        engine.file(tree.join(format!("{tu}.c")));
    }
    engine.flag("-include").flag("doom_ctx.h");
    ctx.flag("-include")
        .flag("doom_ctx.h")
        .flag("-DDOOM_CTX_NO_MACROS")
        .file(tree.join("headless/doom_ctx_init.c"))
        .file(manifest.join("src/doom_sbf_ctx.c"))
        .file(manifest.join("src/doom_sbf_engine.c"));
    platform.file(manifest.join("src/doom_sbf_platform.c"));
    engine.compile("doom_engine_ctx");
    ctx.compile("doom_engine_ctx_state");
    platform.compile("doom_sbf_platform");
}
