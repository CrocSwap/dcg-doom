use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

const ABI_DESCRIPTOR: &str = "DOOM-EX2/v2|header=256|little-endian|operation=TIC/2|schema=2|roles=12|state=ST1/v0|mirror=MP1/v1|controller=CI2/v1|frame=FB1/64000|role-map=00..0b";

fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn rust_array(bytes: &[u8; 32]) -> String {
    bytes
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn json_string(value: &str) -> String {
    let mut escaped = String::from("\"");
    for byte in value.bytes() {
        match byte {
            b'\\' => escaped.push_str("\\\\"),
            b'\"' => escaped.push_str("\\\""),
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            0x00..=0x1f => write!(&mut escaped, "\\u{byte:04x}").unwrap(),
            _ => escaped.push(byte as char),
        }
    }
    escaped.push('"');
    escaped
}

fn command_version(path: &Path) -> String {
    match Command::new(path).arg("--version").output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            text.trim().to_owned()
        }
        Err(error) => format!("unavailable: {error}"),
    }
}

fn env_value(name: &str, fallback: &str) -> String {
    env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn build_metadata(target: &str, is_sbf: bool) -> String {
    let mut compiler_probe = cc::Build::new();
    if is_sbf {
        let cc = env::var("CC").expect("ctx-engine SBF build needs CC from cargo-build-sbf");
        compiler_probe.compiler(cc);
    }
    let compiler = compiler_probe.get_compiler();
    let cc_path = compiler.path().to_string_lossy().into_owned();
    let cc_version = command_version(compiler.path());
    let rustc_path = PathBuf::from(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    let cargo_path = PathBuf::from(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    let ar_path = PathBuf::from(env::var_os("AR").unwrap_or_else(|| "ar".into()));
    let profile = env_value("PROFILE", "unknown");
    let opt_level = env_value("OPT_LEVEL", "unknown");
    let debug = env_value("DEBUG", "unknown");
    let debug_assertions = env::var("DEBUG_ASSERTIONS").unwrap_or_else(|_| {
        if profile == "release" {
            "false".to_owned()
        } else {
            "unknown".to_owned()
        }
    });
    let overflow_checks = env_value("CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS", "false");
    let lto = env_value("CARGO_PROFILE_RELEASE_LTO", "true");
    let codegen_units = env_value("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1");
    format!(
        "{{\n  \"target\": {},\n  \"rustc\": {{\"path\": {}, \"version\": {}}},\n  \"cargo\": {{\"path\": {}, \"version\": {}}},\n  \"c_compiler\": {{\"path\": {}, \"version\": {}}},\n  \"archiver\": {{\"path\": {}, \"version\": {}}},\n  \"profile\": {{\"name\": {}, \"opt_level\": {}, \"debug\": {}, \"debug_assertions\": {}, \"overflow_checks\": {}, \"lto\": {}, \"codegen_units\": {}}}\n}}",
        json_string(target),
        json_string(&rustc_path.to_string_lossy()),
        json_string(&command_version(&rustc_path)),
        json_string(&cargo_path.to_string_lossy()),
        json_string(&command_version(&cargo_path)),
        json_string(&cc_path),
        json_string(&cc_version),
        json_string(&ar_path.to_string_lossy()),
        json_string(&command_version(&ar_path)),
        json_string(&profile),
        json_string(&opt_level),
        json_string(&debug),
        json_string(&debug_assertions),
        json_string(&overflow_checks),
        json_string(&lto),
        json_string(&codegen_units),
    )
}

fn collect_repo_inputs(
    root: &Path,
    dir: &Path,
    prefix: &str,
    extensions: &[&str],
    out: &mut Vec<(String, PathBuf)>,
) {
    let mut children = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("cannot read local build inputs {}: {err}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    for path in children {
        if path.is_dir() {
            collect_repo_inputs(root, &path, prefix, extensions, out);
        } else if path
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|extension| extensions.contains(&extension))
        {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((format!("{prefix}/{rel}"), path));
        }
    }
}

fn collect_inputs(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut children = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("cannot read external Doom source {}: {err}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    for path in children {
        if path.is_dir() {
            collect_inputs(root, &path, out);
        } else if matches!(
            path.extension().and_then(|x| x.to_str()),
            Some("c" | "h" | "inc")
        ) {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((format!("external/{rel}"), path));
        }
    }
}

fn git_commit(tree: &Path) -> String {
    let output = Command::new("git")
        .args(["-C", tree.to_str().unwrap(), "rev-parse", "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        _ => "unavailable".to_owned(),
    }
}

fn source_manifest(
    crate_root: &Path,
    tree: Option<&Path>,
    target: &str,
    is_sbf: bool,
    common_flags: &[&str],
    metadata: &str,
) -> String {
    let mut inputs = Vec::<(String, PathBuf)>::new();
    for name in ["build.rs", "Cargo.toml", "Cargo.lock"] {
        inputs.push((
            format!("repo/stunts/doom-sbf/{name}"),
            crate_root.join(name),
        ));
    }
    collect_repo_inputs(
        crate_root,
        &crate_root.join("src"),
        "repo/stunts/doom-sbf",
        &["c", "h", "inc", "rs"],
        &mut inputs,
    );
    collect_repo_inputs(
        crate_root,
        &crate_root.join("sbfinc"),
        "repo/stunts/doom-sbf",
        &["h"],
        &mut inputs,
    );
    for name in [
        "driver/Cargo.toml",
        "driver/Cargo.lock",
        "driver/src/bin/ex2_native.rs",
        "../doom-host/build.sh",
        "../doom-host/tools/doom_controller_producer.py",
        "../doom-host/tools/doom_account_image.py",
    ] {
        let path = crate_root.join(name);
        let display = name.strip_prefix("../").unwrap_or(name);
        let manifest_name = if name.starts_with("../") {
            format!("repo/stunts/{display}")
        } else {
            format!("repo/stunts/doom-sbf/{name}")
        };
        inputs.push((manifest_name, path));
    }
    let patch_dir = crate_root.join("../doom-host/patches");
    let mut patches = fs::read_dir(&patch_dir)
        .unwrap_or_else(|err| {
            panic!(
                "cannot read Doom patch directory {}: {err}",
                patch_dir.display()
            )
        })
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|x| x.to_str()) == Some("patch"))
        .collect::<Vec<_>>();
    patches.sort();
    for path in patches {
        let name = path.file_name().unwrap().to_string_lossy();
        inputs.push((format!("repo/stunts/doom-host/patches/{name}"), path));
    }
    let upstream_commit = tree
        .map(git_commit)
        .unwrap_or_else(|| "not-enabled".to_owned());
    if let Some(tree) = tree {
        collect_inputs(tree, tree, &mut inputs);
    }
    inputs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut manifest = String::new();
    writeln!(&mut manifest, "{{").unwrap();
    writeln!(
        &mut manifest,
        "  \"schema\": \"DOOM-EX2-NATIVE-SOURCE-MANIFEST/1\","
    )
    .unwrap();
    writeln!(&mut manifest, "  \"target\": \"{target}\",").unwrap();
    writeln!(&mut manifest, "  \"sbf\": {is_sbf},").unwrap();
    writeln!(
        &mut manifest,
        "  \"upstream_commit\": \"{upstream_commit}\","
    )
    .unwrap();
    writeln!(&mut manifest, "  \"abi_descriptor\": \"{ABI_DESCRIPTOR}\",").unwrap();
    writeln!(&mut manifest, "  \"build_metadata\": {metadata},").unwrap();
    writeln!(&mut manifest, "  \"compiler_flags\": [").unwrap();
    for (index, flag) in common_flags.iter().enumerate() {
        writeln!(
            &mut manifest,
            "    \"{}\"{}",
            flag,
            if index + 1 == common_flags.len() {
                ""
            } else {
                ","
            }
        )
        .unwrap();
    }
    writeln!(&mut manifest, "  ],").unwrap();
    writeln!(&mut manifest, "  \"inputs\": [").unwrap();
    for (index, (name, path)) in inputs.iter().enumerate() {
        let bytes = fs::read(path)
            .unwrap_or_else(|err| panic!("cannot read build input {}: {err}", path.display()));
        let sha = hex(&digest(&bytes));
        writeln!(
            &mut manifest,
            "    {{\"path\": \"{name}\", \"bytes\": {}, \"sha256\": \"{sha}\"}}{}",
            bytes.len(),
            if index + 1 == inputs.len() { "" } else { "," }
        )
        .unwrap();
        println!("cargo:rerun-if-changed={}", path.display());
    }
    writeln!(&mut manifest, "  ]").unwrap();
    writeln!(&mut manifest, "}}\n").unwrap();
    manifest
}

fn write_identity(
    crate_root: &Path,
    out_dir: &Path,
    tree: Option<&Path>,
    target: &str,
    is_sbf: bool,
    common_flags: &[&str],
) {
    let metadata = build_metadata(target, is_sbf);
    let manifest = source_manifest(crate_root, tree, target, is_sbf, common_flags, &metadata);
    let abi_id = digest(format!("DOOM-EX2-ABI-ID\0{ABI_DESCRIPTOR}").as_bytes());
    let mut image_input = b"DOOM-EX2-IMAGE-ID\0".to_vec();
    image_input.extend_from_slice(manifest.as_bytes());
    let image_id = digest(&image_input);
    let manifest_id = hex(&digest(manifest.as_bytes()));
    fs::write(out_dir.join("doom_ex2_source_manifest.json"), &manifest)
        .expect("write Doom EX2 source manifest");
    let generated = format!(
        "pub const IMAGE_ID: [u8; 32] = [{}];\n\
         pub const ABI_ID: [u8; 32] = [{}];\n\
         pub const SOURCE_MANIFEST_SHA256: &str = \"{manifest_id}\";\n\
         pub const ABI_DESCRIPTOR: &str = {ABI_DESCRIPTOR:?};\n\
         pub const BUILD_SOURCE_MANIFEST: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/doom_ex2_source_manifest.json\"));\n",
        rust_array(&image_id), rust_array(&abi_id),
    );
    fs::write(out_dir.join("doom_ex2_build_identity.rs"), generated)
        .expect("write Doom EX2 build identity");
}

fn main() {
    println!("cargo:rerun-if-changed=src/doom_hotloops.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_platform.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_ctx.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_engine.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_ex2.c");

    let target = env::var("TARGET").expect("Cargo supplies TARGET to build scripts");
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").expect("Cargo supplies OUT_DIR to build scripts"));
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_STATIC_WRITE_PROBE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_ZERO_BSS_PROBE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_CTX_ENGINE");
    println!("cargo:rerun-if-env-changed=DOOM_SCRATCH");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=AR");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=CARGO");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=OPT_LEVEL");
    println!("cargo:rerun-if-env-changed=DEBUG");
    println!("cargo:rerun-if-env-changed=DEBUG_ASSERTIONS");

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
    } else {
        write_identity(
            &std::path::PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()),
            &out_dir,
            None,
            &target,
            false,
            &[],
        );
    }
}

/// Fold the 41-TU patch-0007 engine, its generated initializer, and the
/// context-mode platform layer into the program image.
///
/// The engine sources live outside git (fetched by stunts/doom-host/build.sh
/// into $DOOM_SCRATCH with the ordered host patches applied), so this resolves
/// the tree from the environment and fails loudly when patch 0007 is missing.
/// Three archives.  The engine TUs use the bare-name macros; doom_ctx_init.c
/// and doom_sbf_ctx.c spell DOOM_CTX->field explicitly and compile with
/// -DDOOM_CTX_NO_MACROS; doom_sbf_platform.c compiles WITHOUT -include
/// doom_ctx.h, because the engine headers pull in sbfinc/stdio.h's variadic
/// printf declarations and SBF cannot define a variadic function.
fn compile_ctx_engine(target: &str) {
    let manifest = std::path::PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let scratch = env::var("DOOM_SCRATCH").expect(
        "ctx-engine needs DOOM_SCRATCH=<dir> with the build.sh layout \
         (doom-src/linuxdoom-1.10 with the ordered host patches)",
    );
    let scratch_path = std::path::PathBuf::from(&scratch);
    let nested_tree = scratch_path.join("doom-src/linuxdoom-1.10");
    let direct_tree = scratch_path.join("linuxdoom-1.10");
    let tree = if nested_tree.is_dir() {
        nested_tree
    } else {
        direct_tree
    };
    let ctx_h = tree.join("headless/doom_ctx.h");
    assert!(
        ctx_h.is_file(),
        "ctx-engine needs patch 0007 in {}/doom-src or {}/linuxdoom-1.10 \
         (missing headless/doom_ctx.h); run DOOM_SCRATCH=<dir> \
         sh stunts/doom-host/build.sh first",
        scratch,
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
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").expect("Cargo supplies OUT_DIR to build scripts"));
    write_identity(&manifest, &out_dir, Some(&tree), target, is_sbf, common);
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

    // EX2 is deliberately native-only.  It reuses the retained host state
    // and account codecs and therefore must never enter an SBF image.  The
    // C bridge is linked only for the host ctx-engine feature; SBF receives
    // the explicit refusal symbol in doom_sbf_engine.c instead.
    if !is_sbf {
        let mut native = cc::Build::new();
        for f in common {
            native.flag(f);
        }
        native
            .include(&tree)
            .include(tree.join("headless"))
            .flag("-include")
            .flag("doom_ctx.h")
            .file(tree.join("headless/doom_state.c"))
            .file(tree.join("headless/doom_controller.c"))
            .file(manifest.join("src/doom_sbf_ex2.c"));
        native.compile("doom_engine_ex2_native");
    }
}
