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
const SBF_ABI_DESCRIPTOR: &str = "DOOM-E2S/1|instruction=24|operation=TIC/2|accounts=9|context=9734128|wad=5x1048576|state=126976|mirror=65536|controller=64|continuation=G_Ticker|frame=none";
const RESOURCE_SBF_ABI_DESCRIPTOR: &str = "DOOM-E2R/1|instruction=32|operation=TIC/2|accounts=10|resource=DOOM-RI1/1:197664|context=9734128|wad=5x1048576|state=126976|mirror=65536|controller=64|continuation=resource-import+G_Ticker|frame=none";
const RESOURCE_RI2_SBF_ABI_DESCRIPTOR: &str = "DOOM-E2R/2|instruction=32|operation=TIC/2|accounts=11|resource=DOOM-RI2/2:334384|context=9734128|wad=5x1048576|state=126976|mirror=65536|controller=64|frame=64000|continuation=initialized-resource-import+G_Ticker+R_RenderPlayerView";

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
    // cargo-build-sbf warns that the crate's simultaneous `cdylib` + `lib`
    // types disable LTO. Record the effective SBF setting rather than the
    // profile request so the emitted identity cannot overclaim optimization.
    let lto = if is_sbf {
        "false".to_owned()
    } else {
        env_value("CARGO_PROFILE_RELEASE_LTO", "true")
    };
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
    writeln!(
        &mut manifest,
        "  \"sbf_abi_descriptor\": \"{SBF_ABI_DESCRIPTOR}\","
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"resource_sbf_abi_descriptor\": \"{RESOURCE_SBF_ABI_DESCRIPTOR}\","
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"resource_ri2_sbf_abi_descriptor\": \"{RESOURCE_RI2_SBF_ABI_DESCRIPTOR}\","
    )
    .unwrap();
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
    let sbf_abi_id = digest(format!("DOOM-E2S-ABI-ID\0{SBF_ABI_DESCRIPTOR}").as_bytes());
    let resource_sbf_abi_id = digest(
        format!("DOOM-E2R-ABI-ID\0{RESOURCE_SBF_ABI_DESCRIPTOR}").as_bytes(),
    );
    let resource_ri2_sbf_abi_id = digest(
        format!("DOOM-E2R2-ABI-ID\0{RESOURCE_RI2_SBF_ABI_DESCRIPTOR}").as_bytes(),
    );
    let mut image_input = b"DOOM-EX2-IMAGE-ID\0".to_vec();
    image_input.extend_from_slice(manifest.as_bytes());
    let image_id = digest(&image_input);
    let manifest_id = hex(&digest(manifest.as_bytes()));
    fs::write(out_dir.join("doom_ex2_source_manifest.json"), &manifest)
        .expect("write Doom EX2 source manifest");
    let generated = format!(
        "pub const IMAGE_ID: [u8; 32] = [{}];\n\
         pub const ABI_ID: [u8; 32] = [{}];\n\
         pub const SBF_ABI_ID: [u8; 32] = [{}];\n\
         pub const RESOURCE_SBF_ABI_ID: [u8; 32] = [{}];\n\
         pub const RESOURCE_RI2_SBF_ABI_ID: [u8; 32] = [{}];\n\
         pub const SOURCE_MANIFEST_SHA256: &str = \"{manifest_id}\";\n\
         pub const ABI_DESCRIPTOR: &str = {ABI_DESCRIPTOR:?};\n\
         pub const SBF_ABI_DESCRIPTOR: &str = {SBF_ABI_DESCRIPTOR:?};\n\
         pub const RESOURCE_SBF_ABI_DESCRIPTOR: &str = {RESOURCE_SBF_ABI_DESCRIPTOR:?};\n\
         pub const RESOURCE_RI2_SBF_ABI_DESCRIPTOR: &str = {RESOURCE_RI2_SBF_ABI_DESCRIPTOR:?};\n\
         pub const BUILD_SOURCE_MANIFEST: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/doom_ex2_source_manifest.json\"));\n",
        rust_array(&image_id),
        rust_array(&abi_id),
        rust_array(&sbf_abi_id),
        rust_array(&resource_sbf_abi_id),
        rust_array(&resource_ri2_sbf_abi_id),
    );
    fs::write(out_dir.join("doom_ex2_build_identity.rs"), generated)
        .expect("write Doom EX2 build identity");
}

fn write_sbf_codec_source(
    out_dir: &Path,
    tree: &Path,
    relative: &str,
    needle: &str,
    replacement: &str,
    name: &str,
) -> PathBuf {
    let source_path = tree.join(relative);
    let source = fs::read_to_string(&source_path).unwrap_or_else(|err| {
        panic!(
            "cannot read SBF codec source {}: {err}",
            source_path.display()
        )
    });
    assert_eq!(
        source.matches(needle).count(),
        1,
        "SBF codec rewrite must match exactly one source block in {relative}"
    );
    let rewritten = source.replacen(needle, replacement, 1);
    let generated = out_dir.join(name);
    fs::write(&generated, rewritten).unwrap_or_else(|err| {
        panic!(
            "cannot write generated SBF codec {}: {err}",
            generated.display()
        )
    });
    println!("cargo:rerun-if-changed={}", source_path.display());
    generated
}

/// Generate only the source-matched RI2/2 preflight and canonical table
/// importer for SBF.  The native file also contains the producer and the
/// setup-based predecessor reader; those sections are intentionally absent
/// from the deployable image.  The generated translation unit appends the
/// explicit no-setup SBF entry from doom_sbf_resource_ri2.c.
fn write_sbf_ri2_source(out_dir: &Path, manifest: &Path) -> PathBuf {
    let native = manifest.join("src/doom_resource_ri2.c");
    let source = fs::read_to_string(&native).expect("read source-matched RI2 codec");
    let take = |start: &str, end: &str| -> String {
        let begin = source.find(start).expect("RI2 SBF source start marker");
        let finish = source[begin..]
            .find(end)
            .map(|offset| begin + offset)
            .expect("RI2 SBF source end marker");
        source[begin..finish].to_owned()
    };
    let mut generated = String::from(
        "/* generated from src/doom_resource_ri2.c; do not edit */\n",
    );
    generated.push_str(&take(
        "/*\n * DOOM-RI2/2 initialized-resource capture",
        "#define R2_SCALAR",
    ));
    generated.push_str(&take("static int r2_apply_scalar", "static int r2_apply_relocs"));
    generated.push_str(&take(
        "typedef struct {\n    uint32_t scalar[64];",
        "static int r2_v2_commit",
    ));
    // The canonical relocation preflight uses a 64x256 duplicate detector.
    // Keep that bounded scratch in the caller-provided bootstrap plan rather
    // than the 4 KiB SBF stack.  This is storage-only; validation order and
    // predicates remain source-matched.
    let plan_decl = "    uint32_t table_bytes[64];\n} r2_v2_plan_t;";
    assert_eq!(generated.matches(plan_decl).count(), 1);
    generated = generated.replace(
        plan_decl,
        "    uint32_t table_bytes[64];\n    uint8_t reloc_seen[64][256];\n} r2_v2_plan_t;",
    );
    let reloc_seen = "    uint8_t seen[64][256] = {{0}}, zone_seen[64] = {0};";
    assert_eq!(generated.matches(reloc_seen).count(), 1);
    generated = generated.replace(
        reloc_seen,
        "    uint8_t (*seen)[256] = plan->reloc_seen, zone_seen[64] = {0};\n    memset(seen, 0, sizeof(plan->reloc_seen));",
    );
    generated.push_str("#include \"doom_sbf_resource_ri2.c\"\n");
    let generated_path = out_dir.join("doom_sbf_resource_ri2_generated.c");
    fs::write(&generated_path, generated).expect("write generated SBF RI2 codec");
    println!("cargo:rerun-if-changed={}", native.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("src/doom_sbf_resource_ri2.c").display()
    );
    generated_path
}

fn main() {
    println!("cargo:rerun-if-changed=src/doom_hotloops.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_platform.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_ctx.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_engine.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_ex2.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_resource.c");
    println!("cargo:rerun-if-changed=src/doom_resource.c");
    println!("cargo:rerun-if-changed=src/doom_resource_ri2.c");
    println!("cargo:rerun-if-changed=src/doom_sbf_resource_ri2.c");

    let target = env::var("TARGET").expect("Cargo supplies TARGET to build scripts");
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").expect("Cargo supplies OUT_DIR to build scripts"));
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_STATIC_WRITE_PROBE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_ZERO_BSS_PROBE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_CTX_ENGINE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_DIAGNOSTIC_PROFILE");
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
    let diagnostic_profile = is_sbf && env::var("CARGO_FEATURE_DIAGNOSTIC_PROFILE").is_ok();
    let out_dir =
        PathBuf::from(env::var("OUT_DIR").expect("Cargo supplies OUT_DIR to build scripts"));
    let identity_flags: Vec<&str> = if diagnostic_profile {
        common
            .iter()
            .copied()
            .chain(["-DDOOM_SBF_PHASE_PROFILE"])
            .collect()
    } else {
        common.to_vec()
    };
    write_identity(
        &manifest,
        &out_dir,
        Some(&tree),
        target,
        is_sbf,
        &identity_flags,
    );
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
    if diagnostic_profile {
        platform.define("DOOM_SBF_PHASE_PROFILE", None);
    }
    for tu in ENGINE_TUS {
        engine.file(tree.join(format!("{tu}.c")));
    }
    engine.flag("-include").flag("doom_ctx.h");
    ctx.flag("-include")
        .flag("doom_ctx.h")
        .flag("-DDOOM_CTX_NO_MACROS");
    let ctx_init = if is_sbf {
        let original = tree.join("headless/doom_ctx_init.c");
        let mut source = fs::read_to_string(&original)
            .unwrap_or_else(|err| panic!("read SBF ctx initializer {}: {err}", original.display()));
        if diagnostic_profile {
            source = format!(
                "extern void sol_log_64_(unsigned long, unsigned long, unsigned long, unsigned long, unsigned long);\nextern unsigned long sol_remaining_compute_units(void);\n{source}"
            );
        }
        source = format!(
            "static void doom_sbf_init_copy(void *dst, const void *src, unsigned long n) {{ unsigned long i; unsigned char *d = (unsigned char *)dst; const unsigned char *s = (const unsigned char *)src; for (i = 0; i < n; i++) d[i] = s[i]; }}\n{source}"
        );
        for (needle, phase) in [
            ("memcpy(DOOM_CTX->angleturn, doom_ctx_init_angleturn, sizeof(DOOM_CTX->angleturn));", 0x64u64),
            ("memcpy(DOOM_CTX->forwardmove, doom_ctx_init_forwardmove, sizeof(DOOM_CTX->forwardmove));", 0x65u64),
            ("memcpy(DOOM_CTX->mobjinfo, doom_ctx_init_mobjinfo, sizeof(DOOM_CTX->mobjinfo));", 0x66u64),
            ("memcpy(DOOM_CTX->sidemove, doom_ctx_init_sidemove, sizeof(DOOM_CTX->sidemove));", 0x67u64),
            ("memcpy(DOOM_CTX->states, doom_ctx_init_states, sizeof(DOOM_CTX->states));", 0x68u64),
        ] {
            assert_eq!(source.matches(needle).count(), 1, "SBF ctx initializer rewrite must match once");
            let call = match needle {
                "memcpy(DOOM_CTX->angleturn, doom_ctx_init_angleturn, sizeof(DOOM_CTX->angleturn));" => "doom_sbf_init_copy(DOOM_CTX->angleturn, doom_ctx_init_angleturn, sizeof(DOOM_CTX->angleturn));",
                "memcpy(DOOM_CTX->forwardmove, doom_ctx_init_forwardmove, sizeof(DOOM_CTX->forwardmove));" => "doom_sbf_init_copy(DOOM_CTX->forwardmove, doom_ctx_init_forwardmove, sizeof(DOOM_CTX->forwardmove));",
                "memcpy(DOOM_CTX->mobjinfo, doom_ctx_init_mobjinfo, sizeof(DOOM_CTX->mobjinfo));" => "doom_sbf_init_copy(DOOM_CTX->mobjinfo, doom_ctx_init_mobjinfo, sizeof(DOOM_CTX->mobjinfo));",
                "memcpy(DOOM_CTX->sidemove, doom_ctx_init_sidemove, sizeof(DOOM_CTX->sidemove));" => "doom_sbf_init_copy(DOOM_CTX->sidemove, doom_ctx_init_sidemove, sizeof(DOOM_CTX->sidemove));",
                "memcpy(DOOM_CTX->states, doom_ctx_init_states, sizeof(DOOM_CTX->states));" => "doom_sbf_init_copy(DOOM_CTX->states, doom_ctx_init_states, sizeof(DOOM_CTX->states));",
                _ => unreachable!(),
            };
            let replacement = if diagnostic_profile {
                format!("{call} sol_log_64_(0x444f4f4d494e4954, {phase}, sol_remaining_compute_units(), 0, 0);")
            } else {
                call.to_owned()
            };
            source = source.replacen(needle, &replacement, 1);
        }
        let generated = out_dir.join(if diagnostic_profile {
            "doom_ctx_init_diagnostic.c"
        } else {
            "doom_ctx_init_sbf.c"
        });
        fs::write(&generated, source)
            .unwrap_or_else(|err| panic!("write SBF ctx initializer {}: {err}", generated.display()));
        generated
    } else {
        tree.join("headless/doom_ctx_init.c")
    };
    ctx.file(ctx_init)
        .file(manifest.join("src/doom_sbf_ctx.c"))
        .file(manifest.join("src/doom_sbf_engine.c"));
    platform.file(manifest.join("src/doom_sbf_platform.c"));
    engine.compile("doom_engine_ctx");
    platform.compile("doom_sbf_platform");
    ctx.compile("doom_engine_ctx_state");

    if is_sbf {
        // The state codec is the retained pointer-free ST1 implementation.
        // Its host allocation-failure counters are test controls only; on
        // SBF they occupy existing context fields so the image carries no
        // writable static state.  The controller's host globals are likewise
        // relocated into context fields by an exact, source-checked rewrite.
        let state = write_sbf_codec_source(
            &out_dir,
            &tree,
            "headless/doom_state.c",
            "static long ds_alloc_fail_after = -1;\nstatic long ds_alloc_calls;",
            "#ifdef DOOM_SBF_BUILD\n#undef doom_render_fuzz_columns\n#define ds_alloc_fail_after DOOM_CTX->doom_ctx_probe_cell\n#define ds_alloc_calls DOOM_CTX->doom_render_fuzz_columns\n#else\nstatic long ds_alloc_fail_after = -1;\nstatic long ds_alloc_calls;\n#endif",
            "doom_state_sbf.c",
        );
        // The retained decoder keeps a large fixed plan on the C stack. SBF
        // permits only a 4 KiB frame, so move that plan into the bounded Doom
        // allocator for the SBF codec copy. This is a mechanical rewrite of
        // the existing decoder body; host ST1 behavior remains unchanged.
        let mut state_source =
            fs::read_to_string(&state).expect("read generated SBF state source");
        // libc realloc cannot express the old allocation length, and the
        // monotonic SBF arena places the larger replacement immediately after
        // the old block.  The source codec only needs its live prefix, so the
        // SBF copy uses malloc plus an exact-length memcpy.  Host/native
        // builds retain the source's libc realloc path and byte behavior.
        let state_grow_needle = "static void ds_need(ds_buf_t *b, size_t n) { uint8_t *q; size_t cap; if (b->bad) return; if (n <= b->cap-b->used) return; cap=b->cap?b->cap:4096; while (n>cap-b->used) { if (cap>(size_t)-1/2) { b->bad=1; return; } cap*=2; } q=realloc(b->p,cap); if(!q){b->bad=1;return;} b->p=q;b->cap=cap; }";
        let state_grow_replacement = "static void ds_need(ds_buf_t *b, size_t n) { uint8_t *q; size_t cap; if (b->bad) return; if (n <= b->cap-b->used) return; cap=b->cap?b->cap:4096; while (n>cap-b->used) { if (cap>(size_t)-1/2) { b->bad=1; return; } cap*=2; } q=malloc(cap); if(q && b->p && b->used) memcpy(q,b->p,b->used); if(!q){b->bad=1;return;} b->p=q;b->cap=cap; }";
        assert_eq!(state_source.matches(state_grow_needle).count(), 1, "SBF state grow rewrite must match once");
        state_source = state_source.replacen(state_grow_needle, state_grow_replacement, 1);
        if diagnostic_profile {
            let marker = "extern void doom_sbf_phase_marker(unsigned long);\n";
            state_source = format!("{marker}{state_source}");
            // Split the source-matched header gate into observable checks so
            // a refusal identifies the ABI/global mismatch rather than only
            // reporting the decoder's aggregate -1.
            let preflight_probe =
                "    memset(p, 0, sizeof(*p));\n    if (!image || image_bytes < DS_HEADER_BYTES";
            assert_eq!(
                state_source.matches(preflight_probe).count(),
                1,
                "diagnostic decoder header probe must match once"
            );
            let preflight_checks = "    memset(p, 0, sizeof(*p));
    if (!image || image_bytes < DS_HEADER_BYTES) { doom_sbf_phase_marker(361); return -1; }
    if (image_bytes > DS_MAX_IMAGE_BYTES || image_bytes > 0xffffffffU) { doom_sbf_phase_marker(362); return -1; }
    if (memcmp(image, \"DOOM-ST1\", 8)) { doom_sbf_phase_marker(363); return -1; }
    if (ds_at32(image + 8) != profile || ds_at32(image + 12) != DS_HEADER_BYTES || ds_at32(image + 16) != (uint32_t)image_bytes) { doom_sbf_phase_marker(364); return -1; }
    if (ds_at32(image + 36) != (uint32_t)numsectors) { doom_sbf_phase_marker(365); return -1; }
    if (ds_at32(image + 40) != (uint32_t)(DOOM_CTX->numlines)) { doom_sbf_phase_marker(366); return -1; }
    if (ds_at32(image + 44) != (uint32_t)numsides) { doom_sbf_phase_marker(367); return -1; }
    if (ds_at32(image + 48) != (uint32_t)numflats) { doom_sbf_phase_marker(368); return -1; }
    if (ds_at32(image + 52) != (uint32_t)numtextures) { doom_sbf_phase_marker(369); return -1; }
    if (image[60] || image[61] || image[62] || image[63]) { doom_sbf_phase_marker(370); return -1; }
    if (!image || image_bytes < DS_HEADER_BYTES
";
            state_source = state_source.replacen(preflight_probe, preflight_checks, 1);
        }
        let profile_start = state_source
            .find("static int ds_decode_profile")
            .expect("find generated SBF decoder");
        let profile_end = profile_start
            + state_source[profile_start..]
                .find("\nint DoomStateDecode")
                .expect("find generated SBF decoder end");
        let mut profile = state_source[profile_start..profile_end].to_owned();
        let plan_decl = "ds_plan_t p; ds_read_t r; unsigned i, j, mi=0, si=0; memset(&p,0,sizeof(p));";
        assert_eq!(
            profile.matches(plan_decl).count(),
            1,
            "SBF decoder plan rewrite must match exactly one stack plan"
        );
        profile = profile.replacen(
            plan_decl,
            "ds_plan_t *p = ds_alloc_bytes(sizeof(*p), 1); ds_read_t r; unsigned i, j, mi=0, si=0; if (!p) return -1; memset(p,0,sizeof(*p));",
            1,
        );
        profile = profile.replace("p.", "p->");
        profile = profile.replace(
            "ds_preflight(image,image_bytes,&p,profile)",
            "ds_preflight(image,image_bytes,p,profile)",
        );
        profile = profile.replace("ds_plan_prepare(&p)", "ds_plan_prepare(p)");
        profile = profile.replace("ds_plan_release(&p, 1);", "ds_plan_release(p, 1); free(p);");
        profile = profile.replace("ds_commit_plan(&p,", "ds_commit_plan(p,");
        profile = profile.replace("ds_plan_release(&p, 0);", "ds_plan_release(p, 0); free(p);");
        assert!(
            !profile.contains("ds_plan_t p;"),
            "SBF decoder plan rewrite left a stack plan reference"
        );
        if diagnostic_profile {
            let plan_alloc_probe = "if (!p) return -1; memset(p,0,sizeof(*p));";
            assert_eq!(
                profile.matches(plan_alloc_probe).count(),
                1,
                "diagnostic decoder plan-allocation probe must match once"
            );
            profile = profile.replacen(
                plan_alloc_probe,
                "if (!p) { doom_sbf_phase_marker(371); return -1; } memset(p,0,sizeof(*p));",
                1,
            );
            // Keep the decoder refusal localized without changing the normal
            // image. These markers distinguish preflight, plan allocation,
            // full parse, and commit failures in the source-matched SBF copy.
            let preflight =
                "if (ds_preflight(image,image_bytes,p,profile) || ds_plan_prepare(p)) return -1;";
            assert_eq!(
                profile.matches(preflight).count(),
                1,
                "diagnostic decoder preflight rewrite must match once"
            );
            profile = profile.replacen(
                preflight,
                "if (ds_preflight(image,image_bytes,p,profile)) { doom_sbf_phase_marker(351); return -1; } doom_sbf_phase_marker(352); if (ds_plan_prepare(p)) { doom_sbf_phase_marker(353); return -1; } doom_sbf_phase_marker(354);",
                1,
            );
            let parse_fail =
                "if (r.bad || r.used != r.cap || mi != p->nmo || si != p->nsp)";
            assert_eq!(
                profile.matches(parse_fail).count(),
                1,
                "diagnostic decoder parse rewrite must match once"
            );
            profile = profile.replacen(
                parse_fail,
                "doom_sbf_phase_marker(355);\n    if (r.bad || r.used != r.cap || mi != p->nmo || si != p->nsp)",
                1,
            );
            let parse_fail_tail =
                "ds_plan_release(p, 1); free(p); return -1;\n    }\n    ds_commit_plan(p, ds_at32(image + 24));";
            assert_eq!(
                profile.matches(parse_fail_tail).count(),
                1,
                "diagnostic decoder parse-failure tail must match once"
            );
            profile = profile.replacen(
                parse_fail_tail,
                "ds_plan_release(p, 1); free(p); return -1;\n    }\n    doom_sbf_phase_marker(356);\n    ds_commit_plan(p, ds_at32(image + 24));",
                1,
            );
            let commit_tail = "ds_plan_release(p, 0); free(p);\n    return 0;";
            assert_eq!(
                profile.matches(commit_tail).count(),
                1,
                "diagnostic decoder commit tail must match once"
            );
            profile = profile.replacen(
                commit_tail,
                "ds_plan_release(p, 0); free(p);\n    doom_sbf_phase_marker(357);\n    return 0;",
                1,
            );
        }
        state_source.replace_range(profile_start..profile_end, &profile);
        fs::write(&state, state_source).expect("write SBF state source");
        let controller = write_sbf_codec_source(
            &out_dir,
            &tree,
            "headless/doom_controller.c",
            "static int controller_armed;\nstatic int controller_v2_armed;\nstatic uint8_t controller_command[4];\nstatic const uint8_t *captured_demo_begin;\nstatic const uint8_t *captured_demo_end;",
            "#endif\n#ifdef DOOM_SBF_BUILD\n#undef doom_sbf_demo_done\n#undef doom_error_latched\n#undef demobuffer\n#undef demoend\n#define controller_armed DOOM_CTX->doom_sbf_demo_done\n#define controller_v2_armed DOOM_CTX->doom_error_latched\n#define controller_command ((uint8_t *)&DOOM_CTX->doom_ctx_probe_cell)\n#define captured_demo_begin DOOM_CTX->demobuffer\n#define captured_demo_end DOOM_CTX->demoend\n#else\nstatic int controller_armed;\nstatic int controller_v2_armed;\nstatic uint8_t controller_command[4];\nstatic const uint8_t *captured_demo_begin;\nstatic const uint8_t *captured_demo_end;\n#endif",
            "doom_controller_sbf.c",
        );
        // The retained controller source also contains the host filesystem
        // CI1/CI2 reader. The SBF entry receives already-validated CI2 bytes
        // from an account, so keep only the shared in-engine arm/apply path;
        // compiling fopen/fread/fgetc here would create a false libc
        // dependency and hide the actual engine boundary behind a shim.
        let mut controller_source = fs::read_to_string(&controller)
            .expect("read generated SBF controller source");
        let controller_host_needle = "static int read_account(";
        assert_eq!(
            controller_source.matches(controller_host_needle).count(),
            1,
            "SBF controller host-reader rewrite must match exactly one block"
        );
        controller_source = controller_source.replacen(
            controller_host_needle,
            "#ifndef DOOM_SBF_BUILD\nstatic int read_account(",
            1,
        );
        assert_eq!(
            controller_source.matches("sizeof(controller_command)").count(),
            2,
            "SBF controller command-size rewrite must match both copies"
        );
        controller_source = controller_source.replace("sizeof(controller_command)", "4u");
        fs::write(&controller, controller_source).expect("write SBF controller source");
        let mut codec = cc::Build::new();
        for f in common {
            codec.flag(f);
        }
        codec
            .include(&tree)
            .include(tree.join("headless"))
            .include(manifest.join("sbfinc"))
            .include(manifest.join("src"))
            .flag("-include")
            .flag("doom_ctx.h")
            .flag("-DDOOM_SBF_BUILD")
            .flag("-ffreestanding")
            .flag("-fno-stack-protector")
            .flag("-fno-builtin")
            .flag("-fno-asynchronous-unwind-tables")
            .flag("-fno-unwind-tables")
            .compiler(env::var("CC").expect("ctx-engine SBF build needs CC"))
            .archiver(env::var("AR").expect("ctx-engine SBF build needs AR"))
            .file(state)
            .file(controller)
            .file(manifest.join("src/doom_sbf_account_pool.c"))
            .file(manifest.join("src/doom_sbf_ex2.c"))
            .file(manifest.join("src/doom_sbf_resource.c"));
        let ri2_source = write_sbf_ri2_source(&out_dir, &manifest);
        codec.file(ri2_source);
        if diagnostic_profile {
            codec.define("DOOM_SBF_PHASE_PROFILE", None);
        }
        codec.compile("doom_engine_ctx_codec");
        println!(
            "cargo:rerun-if-changed={}",
            manifest.join("src/doom_sbf_account_pool.c").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            manifest.join("src/doom_sbf_ex2.c").display()
        );
    } else {
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
            .file(manifest.join("src/doom_sbf_ex2.c"))
            .file(manifest.join("src/doom_sbf_resource.c"))
            .file(manifest.join("src/doom_resource.c"))
            .file(manifest.join("src/doom_resource_ri2.c"));
        native.compile("doom_engine_ex2_native");
    }
}
