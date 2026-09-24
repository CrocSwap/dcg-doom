use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/native_archiver.rs"]
mod native_archiver;
use native_archiver::archiver_version;

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
const RESOURCE_RI3_SBF_ABI_DESCRIPTOR: &str = "DOOM-RC3/1|instruction=32|operation=TIC/2|accounts=11|resource=DOOM-RI2/2:334384|context=9734128|wad=5x1048576|state=126976|mirror=65536|controller=64|frame=64000|continuation=initialized-resource-import+G_Ticker+R_RenderPlayerView|render=diagnostic-strip40-modes1-full-scene2-solidseg-clipped";
const RENDER_SNAPSHOT_ABI_DESCRIPTOR: &str = "DOOM-RSN/2|operations=INIT/2+SNAPSHOT/2+RENDER/1|init=16:accounts=payer+meta+snapshot+system|snapshot=52:accounts=meta+ST1+MP1+snapshot|snapshot-PDA(DOOM-RENDER-SNAPSHOT/2,context):192704|render=52:accounts=scratch+RI2+WADx5+snapshot+strip|scratch=9734128|strip=8128|header=192|payload=ST1:126976+MP1:65536|wad=MP1[36..68]|resource-abi=RI2|root=SIM2(ST1,MP1-used)|hash=sha256|advance=unchanged";
const NATIVE_PROVENANCE_PROFILE: &str =
    "DOOM-NATIVE-PROVENANCE/sweep_native+fresh_resume_probe/ctx-engine-host-release/2";
const NATIVE_SOURCE_MANIFEST_SCHEMA: &str = "DOOM-EX2-NATIVE-SOURCE-MANIFEST/3";

const NATIVE_ALLOWED_CARGO_FEATURES: &[&str] = &["ctx-engine"];
const NATIVE_RUST_MODULE_CLOSURE: &[(&str, &[&str])] = &[
    (
        "sweep_native",
        &[
            "driver/src/bin/sweep_native.rs",
            "driver/src/bin/continuation_split_sbf.rs",
            "driver/src/sweep_common.rs",
            "src/lib.rs",
            "src/ex2.rs",
            "src/continuation_split_sbf.rs",
            "src/resource.rs",
            "src/resource_sbf.rs",
            "src/simulation.rs",
            "src/sim2.rs",
            "src/stage.rs",
        ],
    ),
    (
        "fresh_resume_probe",
        &[
            "driver/src/bin/fresh_resume_probe.rs",
            "driver/src/bin/continuation_split_sbf.rs",
            "driver/src/sweep_common.rs",
            "src/lib.rs",
            "src/ex2.rs",
            "src/continuation_split_sbf.rs",
            "src/resource.rs",
            "src/resource_sbf.rs",
            "src/simulation.rs",
            "src/sim2.rs",
            "src/stage.rs",
        ],
    ),
];
const HOST_COMMON_FLAGS: &[&str] = &[
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
const HOST_CONTEXT_REPO_FILES: &[&str] = &["src/doom_sbf_ctx.c", "src/doom_sbf_engine.c"];
const HOST_PLATFORM_REPO_FILES: &[&str] = &["src/doom_sbf_platform.c"];
const HOST_NATIVE_CODEC_UPSTREAM_FILES: &[&str] =
    &["headless/doom_state.c", "headless/doom_controller.c"];
const HOST_NATIVE_CODEC_REPO_FILES: &[&str] = &[
    "src/doom_sbf_ex2.c",
    "src/doom_sbf_resource.c",
    "src/doom_resource.c",
    "src/doom_resource_ri2.c",
];
const NATIVE_BUILD_METADATA_FILES: &[&str] = &[
    "repo/stunts/doom-sbf/build.rs",
    "repo/stunts/doom-sbf/Cargo.toml",
    "repo/stunts/doom-sbf/Cargo.lock",
    "repo/stunts/doom-sbf/driver/Cargo.toml",
    "repo/stunts/doom-sbf/driver/Cargo.lock",
];

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

fn command_version(path: &Path, required: bool) -> String {
    match Command::new(path).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            let text = text.trim().to_owned();
            if required && text.is_empty() {
                panic!(
                    "tool {} returned an empty successful --version output",
                    path.display()
                );
            }
            text
        }
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr);
            if required {
                panic!(
                    "tool {} --version failed ({}): {}",
                    path.display(),
                    output.status,
                    detail.trim()
                );
            }
            format!("failed: {}", output.status)
        }
        Err(error) if required => panic!("tool {} is unavailable: {error}", path.display()),
        Err(error) => format!("unavailable: {error}"),
    }
}

fn env_value(name: &str, fallback: &str) -> String {
    env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn normalized_env(name: &str) -> String {
    env::var(name)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalized_encoded_env(name: &str) -> Vec<String> {
    // Cargo sets CARGO_ENCODED_RUSTFLAGS to "" when there are no flags; that
    // is zero flags, not one empty flag (matches doom_native_expected_profile.py).
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .to_string_lossy()
                .split('\x1f')
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn json_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn json_static_array(values: &[&str]) -> String {
    let owned = values
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    json_string_array(&owned)
}

fn host_translation_units() -> Vec<String> {
    let mut files = vec!["repo/stunts/doom-sbf/src/doom_hotloops.c".to_owned()];
    files.extend(ENGINE_TUS.iter().map(|tu| format!("external/{tu}.c")));
    files.push("external/headless/doom_ctx_init.c".to_owned());
    files.extend(
        HOST_CONTEXT_REPO_FILES
            .iter()
            .map(|path| format!("repo/stunts/doom-sbf/{path}")),
    );
    files.extend(
        HOST_PLATFORM_REPO_FILES
            .iter()
            .map(|path| format!("repo/stunts/doom-sbf/{path}")),
    );
    files.extend(
        HOST_NATIVE_CODEC_UPSTREAM_FILES
            .iter()
            .map(|path| format!("external/{path}")),
    );
    files.extend(
        HOST_NATIVE_CODEC_REPO_FILES
            .iter()
            .map(|path| format!("repo/stunts/doom-sbf/{path}")),
    );
    files
}

fn json_tool(path: &Path, version: &str) -> String {
    format!(
        "{{\"path\": {}, \"version\": {}}}",
        json_string(&path.to_string_lossy()),
        json_string(version)
    )
}

fn json_archiver(path: &Path, capability: &str, bytes: u64, sha256: &str) -> String {
    format!(
        "{{\"path\": {}, \"version\": {}, \"bytes\": {}, \"sha256\": {}}}",
        json_string(&path.to_string_lossy()),
        json_string(capability),
        bytes,
        json_string(sha256),
    )
}

fn canonical_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|error| {
            panic!(
                "cannot canonicalize provenance path {}: {error}",
                path.display()
            )
        })
        .to_string_lossy()
        .into_owned()
}

fn resolve_executable(path: &Path) -> PathBuf {
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join(path))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| path.to_owned())
    };
    if !candidate.is_file() {
        panic!("cannot resolve selected executable {}", path.display());
    }
    if candidate.is_absolute() {
        candidate
    } else {
        env::current_dir()
            .expect("current directory is available")
            .join(candidate)
    }
}

fn host_group_json(
    name: &str,
    archive_output: &str,
    files: &[String],
    flags: &[String],
    defines: &[String],
    forced_includes: &[String],
    include_root_keys: &[String],
    include_roots: &[String],
    compiler: &str,
    compiler_version: &str,
    archiver: &str,
    archiver_version: &str,
    archiver_bytes: u64,
    archiver_sha256: &str,
) -> String {
    format!(
        "{{\"name\": {}, \"archive_output\": {}, \"files\": {}, \"flags\": {}, \"defines\": {}, \"forced_includes\": {}, \"include_root_keys\": {}, \"include_roots\": {}, \"compiler\": {}, \"archiver\": {}}}",
        json_string(name),
        json_string(archive_output),
        json_string_array(files),
        json_string_array(flags),
        json_string_array(defines),
        json_string_array(forced_includes),
        json_string_array(include_root_keys),
        json_string_array(include_roots),
        json_tool(Path::new(compiler), compiler_version),
        json_archiver(Path::new(archiver), archiver_version, archiver_bytes, archiver_sha256),
    )
}

fn host_groups_metadata(
    crate_root: &Path,
    tree: &Path,
    compiler: &Path,
    compiler_version: &str,
    archiver: &Path,
    archiver_version: &str,
    archiver_bytes: u64,
    archiver_sha256: &str,
) -> (String, String, String) {
    let upstream = canonical_path(tree);
    let upstream_headless = canonical_path(&tree.join("headless"));
    let repo_sbfinc = canonical_path(&crate_root.join("sbfinc"));
    let compiler = compiler.to_string_lossy();
    let archiver = archiver.to_string_lossy();
    let common = HOST_COMMON_FLAGS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    let hotloop_defines = Vec::new();
    let mut engine_flags = common.clone();
    engine_flags.extend(["-include", "doom_ctx.h"].into_iter().map(str::to_owned));
    let mut context_flags = engine_flags.clone();
    context_flags.push("-DDOOM_CTX_NO_MACROS".to_owned());
    let platform_flags = common.clone();
    let native_codec_flags = engine_flags.clone();
    let engine_files = ENGINE_TUS
        .iter()
        .map(|tu| format!("external/{tu}.c"))
        .collect::<Vec<_>>();
    let mut context_files = vec!["external/headless/doom_ctx_init.c".to_owned()];
    context_files.extend(
        HOST_CONTEXT_REPO_FILES
            .iter()
            .map(|path| format!("repo/stunts/doom-sbf/{path}")),
    );
    let platform_files = HOST_PLATFORM_REPO_FILES
        .iter()
        .map(|path| format!("repo/stunts/doom-sbf/{path}"))
        .collect::<Vec<_>>();
    let native_codec_files = HOST_NATIVE_CODEC_UPSTREAM_FILES
        .iter()
        .map(|path| format!("external/{path}"))
        .chain(
            HOST_NATIVE_CODEC_REPO_FILES
                .iter()
                .map(|path| format!("repo/stunts/doom-sbf/{path}")),
        )
        .collect::<Vec<_>>();
    let roots_engine = [
        upstream.clone(),
        upstream_headless.clone(),
        repo_sbfinc.clone(),
    ]
    .into_iter()
    .collect::<Vec<_>>();
    let roots_native = [upstream.clone(), upstream_headless.clone()]
        .into_iter()
        .collect::<Vec<_>>();
    let groups = [
        host_group_json(
            "hotloop",
            "libdoom_hotloops.a",
            &["repo/stunts/doom-sbf/src/doom_hotloops.c".to_owned()],
            &[],
            &hotloop_defines,
            &[],
            &[],
            &[],
            &compiler,
            compiler_version,
            &archiver,
            archiver_version,
            archiver_bytes,
            archiver_sha256,
        ),
        host_group_json(
            "engine",
            "libdoom_engine_ctx.a",
            &engine_files,
            &engine_flags,
            &["NORMALUNIX".to_owned(), "HEADLESS_DOOM".to_owned()],
            &["doom_ctx.h".to_owned()],
            &[
                "upstream".to_owned(),
                "upstream/headless".to_owned(),
                "repo/sbfinc".to_owned(),
            ],
            &roots_engine,
            &compiler,
            compiler_version,
            &archiver,
            archiver_version,
            archiver_bytes,
            archiver_sha256,
        ),
        host_group_json(
            "context",
            "libdoom_engine_ctx_state.a",
            &context_files,
            &context_flags,
            &[
                "NORMALUNIX".to_owned(),
                "HEADLESS_DOOM".to_owned(),
                "DOOM_CTX_NO_MACROS".to_owned(),
            ],
            &["doom_ctx.h".to_owned()],
            &[
                "upstream".to_owned(),
                "upstream/headless".to_owned(),
                "repo/sbfinc".to_owned(),
            ],
            &roots_engine,
            &compiler,
            compiler_version,
            &archiver,
            archiver_version,
            archiver_bytes,
            archiver_sha256,
        ),
        host_group_json(
            "platform",
            "libdoom_sbf_platform.a",
            &platform_files,
            &platform_flags,
            &["NORMALUNIX".to_owned(), "HEADLESS_DOOM".to_owned()],
            &[],
            &[
                "upstream".to_owned(),
                "upstream/headless".to_owned(),
                "repo/sbfinc".to_owned(),
            ],
            &roots_engine,
            &compiler,
            compiler_version,
            &archiver,
            archiver_version,
            archiver_bytes,
            archiver_sha256,
        ),
        host_group_json(
            "nativecodec",
            "libdoom_engine_ex2_native.a",
            &native_codec_files,
            &native_codec_flags,
            &["NORMALUNIX".to_owned(), "HEADLESS_DOOM".to_owned()],
            &["doom_ctx.h".to_owned()],
            &["upstream".to_owned(), "upstream/headless".to_owned()],
            &roots_native,
            &compiler,
            compiler_version,
            &archiver,
            archiver_version,
            archiver_bytes,
            archiver_sha256,
        ),
    ]
    .join(", ");
    let header = tree.join("headless/doom_ctx.h");
    let header_bytes = fs::read(&header).unwrap_or_else(|error| {
        panic!(
            "cannot read host provenance header {}: {error}",
            header.display()
        )
    });
    let header_root = format!(
        "{{\"path\": \"external/headless/doom_ctx.h\", \"absolute_path\": {}, \"bytes\": {}, \"sha256\": {}}}",
        json_string(&canonical_path(&header)),
        header_bytes.len(),
        json_string(&hex(&digest(&header_bytes)))
    );
    let translation_units = host_translation_units();
    (
        format!("[{}]", groups),
        header_root,
        json_string_array(&translation_units),
    )
}

fn build_metadata(crate_root: &Path, tree: Option<&Path>, target: &str, is_sbf: bool) -> String {
    let mut compiler_probe = cc::Build::new();
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let ar = env::var("AR").unwrap_or_else(|_| "ar".to_owned());
    compiler_probe.compiler(cc).archiver(ar);
    let compiler = compiler_probe.get_compiler();
    let compiler_path = resolve_executable(compiler.path());
    let cc_path = compiler_path.to_string_lossy().into_owned();
    let strict = !is_sbf;
    let cc_version = command_version(&compiler_path, strict);
    let archiver_command = compiler_probe.get_archiver();
    let ar_path = resolve_executable(Path::new(archiver_command.get_program()));
    let rustc_path = resolve_executable(&PathBuf::from(
        env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
    ));
    let cargo_path = resolve_executable(&PathBuf::from(
        env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
    ));
    let linker_path = env::var_os("RUSTC_LINKER")
        .or_else(|| {
            let key = format!(
                "CARGO_TARGET_{}_LINKER",
                target.replace('-', "_").to_ascii_uppercase()
            );
            env::var_os(key)
        })
        .map(PathBuf::from)
        .unwrap_or_else(|| compiler_path.clone());
    let linker_path = resolve_executable(&linker_path);
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
    let enabled_features = [
        ("ctx-engine", "CARGO_FEATURE_CTX_ENGINE"),
        ("static-write-probe", "CARGO_FEATURE_STATIC_WRITE_PROBE"),
        (
            "account-address-probe",
            "CARGO_FEATURE_ACCOUNT_ADDRESS_PROBE",
        ),
        ("zero-bss-probe", "CARGO_FEATURE_ZERO_BSS_PROBE"),
        ("diagnostic-profile", "CARGO_FEATURE_DIAGNOSTIC_PROFILE"),
        ("sim-transport", "CARGO_FEATURE_SIM_TRANSPORT"),
        (
            "render-clip-measurement",
            "CARGO_FEATURE_RENDER_CLIP_MEASUREMENT",
        ),
        ("metered-sweep", "CARGO_FEATURE_METERED_SWEEP"),
    ]
    .into_iter()
    .filter_map(|(feature, variable)| env::var_os(variable).map(|_| feature.to_owned()))
    .collect::<Vec<_>>();
    if strict
        && enabled_features
            != NATIVE_ALLOWED_CARGO_FEATURES
                .iter()
                .map(|v| (*v).to_owned())
                .collect::<Vec<_>>()
    {
        panic!(
            "native host provenance requires exactly Cargo features {:?}, got {:?}",
            NATIVE_ALLOWED_CARGO_FEATURES, enabled_features
        );
    }
    if strict && (profile != "release" || debug != "false" || debug_assertions != "false") {
        panic!(
            "native host provenance requires release/no-debug settings, got profile={profile} debug={debug} debug_assertions={debug_assertions}"
        );
    }
    let rustflags = normalized_env("RUSTFLAGS");
    let encoded_rustflags = normalized_encoded_env("CARGO_ENCODED_RUSTFLAGS");
    let rustc_wrapper = env::var_os("RUSTC_WRAPPER")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rustc_workspace_wrapper = env::var_os("RUSTC_WORKSPACE_WRAPPER")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    if strict && (!rustc_wrapper.is_empty() || !rustc_workspace_wrapper.is_empty()) {
        panic!("native host provenance requires empty RUSTC_WRAPPER and RUSTC_WORKSPACE_WRAPPER");
    }
    let cflags = normalized_env("CFLAGS");
    let cppflags = normalized_env("CPPFLAGS");
    let c_include_path = normalized_env("C_INCLUDE_PATH");
    let cargo_features = json_string_array(&enabled_features);
    let linker_version = command_version(&linker_path, strict);
    let rustc_version = command_version(&rustc_path, strict);
    let cargo_version = command_version(&cargo_path, strict);
    let ar_version = archiver_version(&ar_path, strict);
    let ar_bytes = fs::metadata(&ar_path)
        .unwrap_or_else(|error| {
            panic!(
                "cannot stat selected archiver {}: {error}",
                ar_path.display()
            )
        })
        .len();
    let ar_sha256 = hex(&digest(&fs::read(&ar_path).unwrap_or_else(|error| {
        panic!(
            "cannot read selected archiver {}: {error}",
            ar_path.display()
        )
    })));
    let hotloop_defines = if env::var_os("CARGO_FEATURE_STATIC_WRITE_PROBE").is_some() {
        vec!["DOOM_STATIC_WRITE_PROBE".to_owned()]
    } else if env::var_os("CARGO_FEATURE_ZERO_BSS_PROBE").is_some() {
        vec!["DOOM_ZERO_BSS_PROBE".to_owned()]
    } else {
        Vec::new()
    };
    let common_flags = HOST_COMMON_FLAGS.to_vec();
    let engine_flags = common_flags
        .iter()
        .map(|v| (*v).to_owned())
        .collect::<Vec<_>>();
    let hotloop_flags = Vec::<String>::new();
    let host_c_defines = vec!["NORMALUNIX".to_owned(), "HEADLESS_DOOM".to_owned()];
    let module_closure = NATIVE_RUST_MODULE_CLOSURE
        .iter()
        .map(|(binary, paths)| format!("{}: {}", binary, paths.join(",")))
        .collect::<Vec<_>>();
    let (host_groups, host_header_root, host_translation_units) = match tree {
        Some(tree) if !is_sbf => host_groups_metadata(
            crate_root,
            tree,
            &compiler_path,
            &cc_version,
            &ar_path,
            &ar_version,
            ar_bytes,
            &ar_sha256,
        ),
        _ => ("[]".to_owned(), "null".to_owned(), "[]".to_owned()),
    };
    format!(
        "{{\n  \"target\": {},\n  \"rustc\": {{\"path\": {}, \"version\": {}}},\n  \"cargo\": {{\"path\": {}, \"version\": {}}},\n  \"c_compiler\": {{\"path\": {}, \"version\": {}}},\n  \"linker\": {{\"path\": {}, \"version\": {}}},\n  \"archiver\": {},\n  \"profile\": {{\"name\": {}, \"opt_level\": {}, \"debug\": {}, \"debug_assertions\": {}, \"overflow_checks\": {}, \"lto\": {}, \"codegen_units\": {}}},\n  \"cargo_features\": {},\n  \"rust\": {{\"rustflags\": {}, \"encoded_rustflags\": {}, \"rustc_wrapper\": {}, \"rustc_workspace_wrapper\": {}}},\n  \"c_environment\": {{\"cflags\": {}, \"cppflags\": {}, \"c_include_path\": {}}},\n  \"c_compilation\": {{\"hotloop_flags\": {}, \"hotloop_defines\": {}, \"engine_flags\": {}, \"engine_defines\": {}, \"groups\": {}, \"group_scope\": \"exact-host-cc-build-contract/1\", \"header_root\": {}, \"translation_units\": {}, \"translation_unit_scope\": \"exact-host-build-inputs/1\"}},\n  \"rust_module_closure\": {},\n  \"effective_linker_source\": \"RUSTC_LINKER-or-target-linker-or-cc-rs-compiler\"\n}}",
        json_string(target),
        json_string(&rustc_path.to_string_lossy()),
        json_string(&rustc_version),
        json_string(&cargo_path.to_string_lossy()),
        json_string(&cargo_version),
        json_string(&cc_path),
        json_string(&cc_version),
        json_string(&linker_path.to_string_lossy()),
        json_string(&linker_version),
        json_archiver(&ar_path, &ar_version, ar_bytes, &ar_sha256),
        json_string(&profile),
        json_string(&opt_level),
        json_string(&debug),
        json_string(&debug_assertions),
        json_string(&overflow_checks),
        json_string(&lto),
        json_string(&codegen_units),
        cargo_features,
        json_string(&rustflags),
        json_string_array(&encoded_rustflags),
        json_string(&rustc_wrapper),
        json_string(&rustc_workspace_wrapper),
        json_string(&cflags),
        json_string(&cppflags),
        json_string(&c_include_path),
        json_string_array(&hotloop_flags),
        json_string_array(&hotloop_defines),
        json_string_array(&engine_flags),
        json_string_array(&host_c_defines),
        host_groups,
        host_header_root,
        host_translation_units,
        json_string_array(&module_closure),
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

fn git_query(tree: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(tree.to_str().expect("git tree path is UTF-8"))
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("cannot run git {:?}: {err}", args));
    if !output.status.success() {
        panic!(
            "git {:?} failed in {}: {}",
            args,
            tree.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn input_root(inputs: &[(String, PathBuf)]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"DOOM-NATIVE-SOURCE-INPUT-ROOT/1\0");
    for (name, path) in inputs {
        let bytes = fs::read(path)
            .unwrap_or_else(|err| panic!("cannot read build input {}: {err}", path.display()));
        let sha = hex(&digest(&bytes));
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(bytes.len().to_string().as_bytes());
        hasher.update([0]);
        hasher.update(sha.as_bytes());
        hasher.update([b'\n']);
    }
    hex(&hasher.finalize())
}

fn add_input(inputs: &mut Vec<(String, PathBuf)>, name: String, path: PathBuf) {
    if inputs.iter().any(|(existing, _)| existing == &name) {
        panic!("duplicate embedded build input path {name}");
    }
    if !path.is_file() {
        panic!("missing embedded build input {name}: {}", path.display());
    }
    inputs.push((name, path));
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
    let repo_root = crate_root
        .join("../..")
        .canonicalize()
        .expect("repository root for Doom provenance");
    let compile_commit = git_query(&repo_root, &["rev-parse", "HEAD"]);
    let compile_tree = git_query(&repo_root, &["rev-parse", "HEAD^{tree}"]);
    let compile_status = git_query(
        &repo_root,
        &["status", "--porcelain", "--untracked-files=all"],
    );
    // A native provenance image must be compiled from the exact clean source
    // commit it records.  This is deliberately a build-time refusal: reading
    // HEAD in a later probe would only describe the caller's checkout, not the
    // sources which produced the binary.
    if tree.is_some() && !compile_status.is_empty() {
        panic!(
            "native Doom provenance build requires a clean repository; dirty paths:\n{compile_status}"
        );
    }
    for name in ["build.rs", "Cargo.toml", "Cargo.lock"] {
        add_input(
            &mut inputs,
            format!("repo/stunts/doom-sbf/{name}"),
            crate_root.join(name),
        );
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
        "driver/src/bin/sweep_native.rs",
        "driver/src/bin/fresh_resume_probe.rs",
        "driver/src/bin/continuation_split_sbf.rs",
        "driver/src/sweep_common.rs",
        "../doom-host/build.sh",
        "../doom-host/tools/doom_controller_producer.py",
        "../doom-host/tools/doom_account_image.py",
        "../../scripts/doom_native_provenance_descriptor.py",
        "../../scripts/doom_native_expected_profile.py",
        "../../scripts/doom_native_provenance_acceptance.sh",
        "doom-native-host-profile-v2.json",
    ] {
        let path = crate_root.join(name);
        let manifest_name = if let Some(display) = name.strip_prefix("../../") {
            format!("repo/{display}")
        } else if let Some(display) = name.strip_prefix("../") {
            format!("repo/stunts/{display}")
        } else {
            format!("repo/stunts/doom-sbf/{name}")
        };
        add_input(&mut inputs, manifest_name, path);
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
        add_input(
            &mut inputs,
            format!("repo/stunts/doom-host/patches/{name}"),
            path,
        );
    }
    let upstream_commit = tree
        .map(git_commit)
        .unwrap_or_else(|| "not-enabled".to_owned());
    if let Some(tree) = tree {
        collect_inputs(tree, tree, &mut inputs);
    }
    inputs.sort_by(|a, b| a.0.cmp(&b.0));
    let input_root = input_root(&inputs);
    let enabled_features = [
        ("ctx-engine", "CARGO_FEATURE_CTX_ENGINE"),
        ("static-write-probe", "CARGO_FEATURE_STATIC_WRITE_PROBE"),
        (
            "account-address-probe",
            "CARGO_FEATURE_ACCOUNT_ADDRESS_PROBE",
        ),
        ("zero-bss-probe", "CARGO_FEATURE_ZERO_BSS_PROBE"),
        ("diagnostic-profile", "CARGO_FEATURE_DIAGNOSTIC_PROFILE"),
        ("sim-transport", "CARGO_FEATURE_SIM_TRANSPORT"),
        ("metered-sweep", "CARGO_FEATURE_METERED_SWEEP"),
    ]
    .into_iter()
    .filter_map(|(feature, variable)| env::var_os(variable).map(|_| feature.to_owned()))
    .collect::<Vec<_>>();
    let header_inputs = inputs
        .iter()
        .filter(|(name, _)| name.ends_with(".h") || name.ends_with(".inc"))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let c_translation_units = tree.map(|_| host_translation_units()).unwrap_or_default();
    let source_profile = if tree.is_some() && !is_sbf {
        NATIVE_PROVENANCE_PROFILE
    } else if tree.is_some() {
        "DOOM-SBF/ctx-engine/1"
    } else {
        "DOOM-EX2/legacy/1"
    };

    let mut manifest = String::new();
    writeln!(&mut manifest, "{{").unwrap();
    writeln!(
        &mut manifest,
        "  \"schema\": {},",
        json_string(NATIVE_SOURCE_MANIFEST_SCHEMA)
    )
    .unwrap();
    writeln!(&mut manifest, "  \"target\": \"{target}\",").unwrap();
    writeln!(&mut manifest, "  \"sbf\": {is_sbf},").unwrap();
    writeln!(
        &mut manifest,
        "  \"source_profile\": {},",
        json_string(source_profile)
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"allowed_cargo_features\": {},",
        json_static_array(NATIVE_ALLOWED_CARGO_FEATURES)
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"enabled_cargo_features\": {},",
        json_string_array(&enabled_features)
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"input_scope\": \"conservative-source-envelope\","
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"rust_module_closure\": {{\"sweep_native\": {}, \"fresh_resume_probe\": {}}},",
        json_string_array(
            &NATIVE_RUST_MODULE_CLOSURE[0]
                .1
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        ),
        json_string_array(
            &NATIVE_RUST_MODULE_CLOSURE[1]
                .1
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        )
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"c_translation_units\": {},",
        json_string_array(&c_translation_units)
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"c_translation_units_scope\": \"conservative-source-envelope\","
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"c_headers\": {{\"scope\": \"conservative-source-envelope\", \"paths\": {}, \"header_root\": {}}},",
        json_string_array(&header_inputs),
        json_string(if tree.is_some() {
            "external/headless/doom_ctx.h"
        } else {
            "none"
        })
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"build_scripts_and_locks\": {},",
        json_static_array(NATIVE_BUILD_METADATA_FILES)
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"binary_targets\": [\"sweep_native\", \"fresh_resume_probe\"],"
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"compile_source_commit\": {},",
        json_string(&compile_commit)
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"compile_source_tree\": {},",
        json_string(&compile_tree)
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"compile_source_dirty\": {},",
        !compile_status.is_empty()
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"upstream_commit\": \"{upstream_commit}\","
    )
    .unwrap();
    // The reproducible wrapper exports the git tree of `stunts/` at the
    // built commit; any other build records that it was not recorded.
    let source_tree = env::var("DOOM_SBF_SOURCE_TREE").unwrap_or_else(|_| "unrecorded".to_owned());
    writeln!(
        &mut manifest,
        "  \"source_tree\": {},",
        json_string(&source_tree)
    )
    .unwrap();
    let profile = if env::var("CARGO_FEATURE_RENDER_CLIP_MEASUREMENT").is_ok() {
        "render-clip-measurement"
    } else if env::var("CARGO_FEATURE_SIM_TRANSPORT").is_ok() {
        "sim-transport"
    } else {
        "legacy"
    };
    writeln!(&mut manifest, "  \"profile\": \"{profile}\",").unwrap();
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
    writeln!(
        &mut manifest,
        "  \"resource_ri3_sbf_abi_descriptor\": \"{RESOURCE_RI3_SBF_ABI_DESCRIPTOR}\","
    )
    .unwrap();
    writeln!(
        &mut manifest,
        "  \"render_snapshot_abi_descriptor\": \"{RENDER_SNAPSHOT_ABI_DESCRIPTOR}\","
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
    writeln!(&mut manifest, "  \"input_count\": {},", inputs.len()).unwrap();
    writeln!(
        &mut manifest,
        "  \"input_root\": {},",
        json_string(&input_root)
    )
    .unwrap();
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
    let metadata = build_metadata(crate_root, tree, target, is_sbf);
    let manifest = source_manifest(crate_root, tree, target, is_sbf, common_flags, &metadata);
    let abi_id = digest(format!("DOOM-EX2-ABI-ID\0{ABI_DESCRIPTOR}").as_bytes());
    let sbf_abi_id = digest(format!("DOOM-E2S-ABI-ID\0{SBF_ABI_DESCRIPTOR}").as_bytes());
    let resource_sbf_abi_id =
        digest(format!("DOOM-E2R-ABI-ID\0{RESOURCE_SBF_ABI_DESCRIPTOR}").as_bytes());
    let resource_ri2_sbf_abi_id =
        digest(format!("DOOM-E2R2-ABI-ID\0{RESOURCE_RI2_SBF_ABI_DESCRIPTOR}").as_bytes());
    let resource_ri3_sbf_abi_id =
        digest(format!("DOOM-E2R3-ABI-ID\0{RESOURCE_RI3_SBF_ABI_DESCRIPTOR}").as_bytes());
    let render_snapshot_abi_id = digest(
        format!("DOOM-RENDER-SNAPSHOT-ABI-ID\0{RENDER_SNAPSHOT_ABI_DESCRIPTOR}").as_bytes(),
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
         pub const RESOURCE_RI3_SBF_ABI_ID: [u8; 32] = [{}];\n\
         pub const RENDER_SNAPSHOT_ABI_ID: [u8; 32] = [{}];\n\
         pub const SOURCE_MANIFEST_SHA256: &str = \"{manifest_id}\";\n\
         pub const ABI_DESCRIPTOR: &str = {ABI_DESCRIPTOR:?};\n\
         pub const SBF_ABI_DESCRIPTOR: &str = {SBF_ABI_DESCRIPTOR:?};\n\
         pub const RESOURCE_SBF_ABI_DESCRIPTOR: &str = {RESOURCE_SBF_ABI_DESCRIPTOR:?};\n\
         pub const RESOURCE_RI2_SBF_ABI_DESCRIPTOR: &str = {RESOURCE_RI2_SBF_ABI_DESCRIPTOR:?};\n\
         pub const RESOURCE_RI3_SBF_ABI_DESCRIPTOR: &str = {RESOURCE_RI3_SBF_ABI_DESCRIPTOR:?};\n\
         pub const RENDER_SNAPSHOT_ABI_DESCRIPTOR: &str = {RENDER_SNAPSHOT_ABI_DESCRIPTOR:?};\n\
         pub const BUILD_SOURCE_MANIFEST: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/doom_ex2_source_manifest.json\"));\n",
        rust_array(&image_id),
        rust_array(&abi_id),
        rust_array(&sbf_abi_id),
        rust_array(&resource_sbf_abi_id),
        rust_array(&resource_ri2_sbf_abi_id),
        rust_array(&resource_ri3_sbf_abi_id),
        rust_array(&render_snapshot_abi_id),
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
        "/* generated from src/doom_resource_ri2.c; do not edit */\n#include \"doom_sbf_plan_layout.h\"\n",
    );
    generated.push_str(&take(
        "/*\n * DOOM-RI2/2 initialized-resource capture",
        "#define R2_SCALAR",
    ));
    generated.push_str(&take(
        "static int r2_apply_scalar",
        "static int r2_apply_relocs",
    ));
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
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("sbfinc/doom_sbf_plan_layout.h").display()
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
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_SIM_TRANSPORT");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_RENDER_CLIP_MEASUREMENT");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_LEGACY_5120_BUFFER");
    println!("cargo:rerun-if-env-changed=DOOM_SBF_SOURCE_TREE");
    println!("cargo:rerun-if-env-changed=DOOM_SCRATCH");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=AR");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=CARGO");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=OPT_LEVEL");
    println!("cargo:rerun-if-env-changed=DEBUG");
    println!("cargo:rerun-if-env-changed=DEBUG_ASSERTIONS");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=RUSTC_WRAPPER");
    println!("cargo:rerun-if-env-changed=RUSTC_WORKSPACE_WRAPPER");
    println!("cargo:rerun-if-env-changed=CFLAGS");
    println!("cargo:rerun-if-env-changed=CPPFLAGS");
    println!("cargo:rerun-if-env-changed=C_INCLUDE_PATH");
    println!("cargo:rerun-if-env-changed=RUSTC_LINKER");
    println!("cargo:rerun-if-env-changed=CARGO_PROFILE_RELEASE_LTO");
    println!("cargo:rerun-if-env-changed=CARGO_PROFILE_RELEASE_CODEGEN_UNITS");
    println!("cargo:rerun-if-env-changed=CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS");

    let mut build = cc::Build::new();
    build.file("src/doom_hotloops.c");
    let host_cc = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let host_ar = env::var("AR").unwrap_or_else(|_| "ar".to_owned());
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
    } else {
        build.compiler(host_cc).archiver(host_ar);
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
    let common: &[&str] = HOST_COMMON_FLAGS;
    // NB: cargo-build-sbf v1.51 reports TARGET as `sbpf-solana-solana`; the
    // older `sbf-solana-solana` spelling is kept so the same build.rs works
    // under both.  (The pre-existing hotloop block above still checks only
    // the old spelling and rides on env CC by luck; it is deliberately
    // untouched -- changing its codegen would silently shift the retained
    // hotloop CU measurements.)
    let is_sbf = target == "sbf-solana-solana" || target == "sbpf-solana-solana";
    let diagnostic_profile = is_sbf && env::var("CARGO_FEATURE_DIAGNOSTIC_PROFILE").is_ok();
    let legacy_5120_buffer = env::var("CARGO_FEATURE_LEGACY_5120_BUFFER").is_ok();
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
    let selected_cc = env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let selected_ar = env::var("AR").unwrap_or_else(|_| "ar".to_owned());
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
            if env::var("CC").is_err() || env::var("AR").is_err() {
                panic!(
                    "ctx-engine SBF build needs CC and AR from cargo-build-sbf \
                     (build with cargo-build-sbf, not plain cargo build)"
                );
            }
            b.flag("-DDOOM_SBF_BUILD")
                .flag("-ffreestanding")
                .flag("-fno-stack-protector")
                .flag("-fno-builtin")
                .flag("-fno-asynchronous-unwind-tables")
                .flag("-fno-unwind-tables");
        }
        b.compiler(selected_cc.clone())
            .archiver(selected_ar.clone());
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
            "static void doom_sbf_init_copy(void *dst, const void *src, unsigned long n) {{ unsigned char *d = (unsigned char *)dst; const unsigned char *s = (const unsigned char *)src; while (n >= 8u) {{ d[0] = s[0]; d[1] = s[1]; d[2] = s[2]; d[3] = s[3]; d[4] = s[4]; d[5] = s[5]; d[6] = s[6]; d[7] = s[7]; d += 8; s += 8; n -= 8; }} while (n--) *d++ = *s++; }}\n{source}"
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
        fs::write(&generated, source).unwrap_or_else(|err| {
            panic!("write SBF ctx initializer {}: {err}", generated.display())
        });
        generated
    } else {
        tree.join("headless/doom_ctx_init.c")
    };
    ctx.file(ctx_init);
    for relative in HOST_CONTEXT_REPO_FILES {
        ctx.file(manifest.join(relative));
    }
    for relative in HOST_PLATFORM_REPO_FILES {
        platform.file(manifest.join(relative));
    }
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
        let mut state_source = fs::read_to_string(&state).expect("read generated SBF state source");
        state_source = format!("#include \"doom_sbf_plan_layout.h\"\n{state_source}");
        // libc realloc cannot express the old allocation length, and the
        // monotonic SBF arena places the larger replacement immediately after
        // the old block.  The source codec only needs its live prefix, so the
        // SBF copy uses malloc plus an exact-length memcpy.  Host/native
        // builds retain the source's libc realloc path and byte behavior.
        let state_grow_needle = "static void ds_need(ds_buf_t *b, size_t n) { uint8_t *q; size_t cap; if (b->bad) return; if (n <= b->cap-b->used) return; cap=b->cap?b->cap:4096; while (n>cap-b->used) { if (cap>(size_t)-1/2) { b->bad=1; return; } cap*=2; } q=realloc(b->p,cap); if(!q){b->bad=1;return;} b->p=q;b->cap=cap; }";
        let state_grow_replacement = "static void ds_need(ds_buf_t *b, size_t n) { uint8_t *q; size_t cap; if (b->bad) return; if (n <= b->cap-b->used) return; cap=b->cap?b->cap:4096; while (n>cap-b->used) { if (cap>(size_t)-1/2) { b->bad=1; return; } cap*=2; } q=malloc(cap); if(q && b->p && b->used) memcpy(q,b->p,b->used); if(!q){b->bad=1;return;} b->p=q;b->cap=cap; }";
        assert_eq!(
            state_source.matches(state_grow_needle).count(),
            1,
            "SBF state grow rewrite must match once"
        );
        state_source = state_source.replacen(state_grow_needle, state_grow_replacement, 1);
        // The SBF entry has a fixed ST1 output ceiling.  Reserve that exact
        // bounded space once, then use checked fixed-width readers/writers.
        // The wire bytes and refusal behavior stay the same, but the old
        // byte-at-a-time helpers made every 16/32-bit field pay nested bounds
        // checks.  This is intentionally SBF-only; the host codec remains the
        // source oracle and keeps its original allocator behavior.
        let state_need_fast = "static void ds_need(ds_buf_t *b, size_t n) { uint8_t *q; size_t cap; if (b->bad) return; if (n <= b->cap-b->used) return; cap=b->cap?b->cap:4096; while (n>cap-b->used) { if (cap>(size_t)-1/2) { b->bad=1; return; } cap*=2; } q=malloc(cap); if(q && b->p && b->used) memcpy(q,b->p,b->used); if(!q){b->bad=1;return;} b->p=q;b->cap=cap; }";
        assert_eq!(
            state_source.matches(state_need_fast).count(),
            1,
            "SBF state allocator rewrite must match once"
        );
        state_source = state_source.replacen(
            state_need_fast,
            "static void ds_need(ds_buf_t *b, size_t n) { uint8_t *q; if (b->bad) return; if (n <= b->cap-b->used) return; if (b->p || n > 126976u) { b->bad=1; return; } q=malloc(126976u); if(!q){b->bad=1;return;} b->p=q; b->cap=126976u; }",
            1,
        );
        let state_writer_needle = "static void ds_u8(ds_buf_t *b, uint8_t x) { ds_need(b,1);if(!b->bad)b->p[b->used++]=x; }\nstatic void ds_u16(ds_buf_t *b, uint16_t x) { ds_u8(b,(uint8_t)x);ds_u8(b,(uint8_t)(x>>8)); }\nstatic void ds_u32(ds_buf_t *b, uint32_t x) { ds_u16(b,(uint16_t)x);ds_u16(b,(uint16_t)(x>>16)); }";
        assert_eq!(
            state_source.matches(state_writer_needle).count(),
            1,
            "SBF state writer rewrite must match once"
        );
        state_source = state_source.replacen(
            state_writer_needle,
            // Each writer loads the cursor and base once and stores the new
            // cursor once.  `b->p[b->used++]` forced a reload of both after
            // every byte store (a uint8_t store may alias the buffer
            // header), which dominated the SBF encoder (measured, DOOM-SIM/2
            // profile).  Same bytes, same order, same refusal (`bad`) rule.
            "static void ds_u8(ds_buf_t *b, uint8_t x) { size_t u = b->used; if (b->bad || u >= b->cap) { b->bad=1; return; } b->p[u]=x; b->used=u+1u; } static void ds_u16(ds_buf_t *b, uint16_t x) { size_t u = b->used; uint8_t *q; if (b->bad || b->cap-u < 2u) { b->bad=1; return; } q=b->p+u; q[0]=(uint8_t)x; q[1]=(uint8_t)(x>>8); b->used=u+2u; } static void ds_u32(ds_buf_t *b, uint32_t x) { size_t u = b->used; uint8_t *q; if (b->bad || b->cap-u < 4u) { b->bad=1; return; } q=b->p+u; q[0]=(uint8_t)x; q[1]=(uint8_t)(x>>8); q[2]=(uint8_t)(x>>16); q[3]=(uint8_t)(x>>24); b->used=u+4u; }",
            1,
        );
        let state_reader_needle = "static uint8_t ds_r8(ds_read_t *r) { if(r->bad||r->used==r->cap){r->bad=1;return 0;}return r->p[r->used++]; }\nstatic uint16_t ds_r16(ds_read_t *r) { uint16_t a=ds_r8(r),b=ds_r8(r);return (uint16_t)(a|(b<<8)); }\nstatic uint32_t ds_r32(ds_read_t *r) { uint32_t a=ds_r16(r),b=ds_r16(r);return a|(b<<16); }\nstatic int32_t ds_ri32(ds_read_t *r) { return (int32_t)ds_r32(r); }";
        assert_eq!(
            state_source.matches(state_reader_needle).count(),
            1,
            "SBF state reader rewrite must match once"
        );
        state_source = state_source.replacen(
            state_reader_needle,
            "static uint8_t ds_r8(ds_read_t *r) { if(r->bad||r->used>=r->cap){r->bad=1;return 0;}return r->p[r->used++]; } static uint16_t ds_r16(ds_read_t *r) { uint16_t a,b; if(r->bad||r->cap-r->used<2u){r->bad=1;return 0;} a=r->p[r->used++]; b=r->p[r->used++]; return (uint16_t)(a|(b<<8)); } static uint32_t ds_r32(ds_read_t *r) { uint32_t a,b,c,d; if(r->bad||r->cap-r->used<4u){r->bad=1;return 0;} a=r->p[r->used++]; b=r->p[r->used++]; c=r->p[r->used++]; d=r->p[r->used++]; return a|(b<<8)|(c<<16)|(d<<24); } static int32_t ds_ri32(ds_read_t *r) { return (int32_t)ds_r32(r); }",
            1,
        );
        // The retained encoder assigns stable IDs by scanning all mobjs for
        // each link.  In the SBF copy only, temporarily index the same mobj
        // order in the existing validcount field and restore it before return.
        // ST1 emits that field as the canonical zero, so the wire bytes do not
        // change and the context is not left with the temporary index.
        let state_mobj_id_needle = "static uint32_t ds_mobj_id(mobj_t **m, unsigned n, mobj_t *x) { unsigned i; if(!x)return 0;for(i=0;i<n;i++)if(m[i]==x)return i+1;return 0; }";
        assert_eq!(
            state_source.matches(state_mobj_id_needle).count(),
            1,
            "SBF mobj index rewrite must match once"
        );
        state_source = state_source.replacen(
            state_mobj_id_needle,
            "static uint32_t ds_mobj_id(mobj_t **m, unsigned n, mobj_t *x) { unsigned value; (void)m; if(!x)return 0; value=(unsigned)x->validcount_m; return value && value<=n ? value : 0; } static void ds_mobj_index(mobj_t **m, unsigned n, int32_t *saved) { unsigned i; for(i=0;i<n;i++){saved[i]=m[i]->validcount_m;m[i]->validcount_m=(int32_t)(i+1);} } static void ds_mobj_restore(mobj_t **m, unsigned n, const int32_t *saved) { unsigned i; for(i=0;i<n;i++)m[i]->validcount_m=saved[i]; }",
            1,
        );
        let state_encode_decl = "ds_buf_t b; thinker_t *t; mobj_t **m; unsigned nm=0, ns=0, nr=0, nceil=0, nplat=0, i; int k; memset(&b,0,sizeof(b));";
        assert_eq!(
            state_source.matches(state_encode_decl).count(),
            1,
            "SBF encoder declaration rewrite must match once"
        );
        state_source = state_source.replacen(
            state_encode_decl,
            "ds_buf_t b; thinker_t *t; mobj_t **m; int32_t *mobj_validcount=0; unsigned nm=0, ns=0, nr=0, nceil=0, nplat=0, i; int k; memset(&b,0,sizeof(b));",
            1,
        );
        let state_encode_mobj = "m=nm?calloc(nm,sizeof(*m)):0;if(nm&&!m){free(b.p);return -1;}i=0;for(t=thinkercap.next;t!=&thinkercap;t=t->next)if(ds_kind(t)==DS_MOBJ)m[i++]=(mobj_t*)t;";
        assert_eq!(
            state_source.matches(state_encode_mobj).count(),
            1,
            "SBF encoder mobj setup rewrite must match once"
        );
        state_source = state_source.replacen(
            state_encode_mobj,
            "m=nm?calloc(nm,sizeof(*m)):0;if(nm&&!m){free(b.p);return -1;}mobj_validcount=nm?malloc(nm*sizeof(*mobj_validcount)):0;if(nm&&!mobj_validcount){free(m);free(b.p);return -1;}i=0;for(t=thinkercap.next;t!=&thinkercap;t=t->next)if(ds_kind(t)==DS_MOBJ)m[i++]=(mobj_t*)t;if(nm)ds_mobj_index(m,nm,mobj_validcount);",
            1,
        );
        let state_encode_cleanup =
            "free(m);if(b.bad||b.used>DS_MAX_IMAGE_BYTES){free(b.p);return -1;}";
        assert_eq!(
            state_source.matches(state_encode_cleanup).count(),
            1,
            "SBF encoder cleanup rewrite must match once"
        );
        state_source = state_source.replacen(
            state_encode_cleanup,
            "if(nm)ds_mobj_restore(m,nm,mobj_validcount);free(mobj_validcount);free(m);if(b.bad||b.used>DS_MAX_IMAGE_BYTES){free(b.p);return -1;}",
            1,
        );
        if diagnostic_profile {
            let marker = "extern void doom_sbf_phase_marker(unsigned long);\n";
            state_source = format!("{marker}{state_source}");
            // Encoder section markers (950-957) for the DOOM-SIM/2 compute
            // profile: globals, booleans, players+bodyque, sectors, lines,
            // translations+items+buttons, thinkers, header.
            for (needle, phase) in [
                ("    ds_i32(&b,gameskill);", 950u32),
                ("    ds_u8(&b,(uint8_t)respawnmonsters);", 951),
                ("    for(i=0;i<MAXPLAYERS;i++)ds_player(&b,&players[i],m,nm);", 952),
                ("    for(i=0;i<(unsigned)numsectors;i++){sector_t*x=&sectors[i];", 953),
                ("    for(i=0;i<(unsigned)(DOOM_CTX->numlines);i++){line_t*x=&lines[i];", 954),
                ("    for(i=0;i<=(unsigned)numflats;i++)", 955),
                ("    for(t=thinkercap.next;t!=&thinkercap;t=t->next){if(t->function.acv==(actionf_v)-1)continue;k=ds_kind(t);if(k==DS_MOBJ)", 956),
                ("    memcpy(b.p,\"DOOM-ST1\",8);", 957),
            ] {
                assert_eq!(state_source.matches(needle).count(), 1, "encoder marker needle must match once");
                state_source = state_source.replacen(
                    needle,
                    &format!("    doom_sbf_phase_marker({phase});{}", &needle[4..]),
                    1,
                );
            }
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
        let plan_decl =
            "ds_plan_t p; ds_read_t r; unsigned i, j, mi=0, si=0; memset(&p,0,sizeof(p));";
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
            let parse_fail = "if (r.bad || r.used != r.cap || mi != p->nmo || si != p->nsp)";
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
        // E2R/4 reuses the exact source-matched decoder body, but keeps its
        // validated plan in the caller-bound arena so the preflight,
        // allocation, and parse/commit portions can be authenticated as
        // separate transactions.  E2R/3 continues to call DoomStateDecode
        // and therefore retains its prior behavior.
        let split_body_start = profile
            .find("\n    memset(&r,0,sizeof(r));")
            .expect("find generated decoder parse body");
        let split_body = &profile[split_body_start..];
        let split_decoder = format!(
            "static ds_plan_t *doom_state_split_plan(void) {{\n    if (!DOOM_CTX->arena_base || DOOM_CTX->arena_bytes <= sizeof(ds_plan_t) + 4096u) return 0;\n    return (ds_plan_t *)((uint8_t *)DOOM_CTX->arena_base + DOOM_CTX->arena_bytes - sizeof(ds_plan_t) - 4096u);\n}}\n\nint DoomStateDecodeSplitPreflight(const uint8_t *image, size_t image_bytes) {{\n    ds_plan_t *p = doom_state_split_plan();\n    if (!p) return -1;\n    memset(p, 0, sizeof(*p));\n    return ds_preflight(image, image_bytes, p, DS_IMPORT_PROFILE_V0);\n}}\n\nint DoomStateDecodeSplitPrepare(void) {{\n    ds_plan_t *p = doom_state_split_plan();\n    return p ? ds_plan_prepare(p) : -1;\n}}\n\nstatic int ds_decode_split_parse(const uint8_t *image, size_t image_bytes, unsigned profile) {{\n    ds_plan_t *p = doom_state_split_plan();\n    ds_read_t r; unsigned i, j, mi = 0, si = 0;\n    if (!p) return -1;{}\n\nint DoomStateDecodeSplitParse(const uint8_t *image, size_t image_bytes) {{\n    return ds_decode_split_parse(image, image_bytes, DS_IMPORT_PROFILE_V0);\n}}\n",
            split_body
        );
        let old_split_plan = "static ds_plan_t *doom_state_split_plan(void) {\n    if (!DOOM_CTX->arena_base || DOOM_CTX->arena_bytes <= sizeof(ds_plan_t) + 4096u) return 0;\n    return (ds_plan_t *)((uint8_t *)DOOM_CTX->arena_base + DOOM_CTX->arena_bytes - sizeof(ds_plan_t) - 4096u);\n}";
        let new_split_plan = "_Static_assert(sizeof(ds_plan_t) <= RI2_SPLIT_DECODER_PLAN_RESERVE, \"decoder plan exceeds its reserved tail slice\");\nstatic ds_plan_t *doom_state_split_plan(void) {\n    if (!arena_base || arena_bytes == 0) return 0;\n    /* arena_bytes ends exactly at the decoder slice; the RI2 plan occupies\n     * the final 64 KiB of the 256 KiB reserved tail. */\n    return (ds_plan_t *)((uint8_t *)arena_base + arena_bytes);\n}";
        assert_eq!(split_decoder.matches(old_split_plan).count(), 1);
        let split_decoder = split_decoder.replace(old_split_plan, new_split_plan);
        state_source.push_str(&split_decoder);
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
        let mut controller_source =
            fs::read_to_string(&controller).expect("read generated SBF controller source");
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
            controller_source
                .matches("sizeof(controller_command)")
                .count(),
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
            .compiler(selected_cc.clone())
            .archiver(selected_ar.clone())
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
        if legacy_5120_buffer {
            codec.define("DOOM_SIM2_LEGACY_DEMO_CAP", None);
        } else if env::var_os("CARGO_FEATURE_SIM_TRANSPORT").is_some() {
            codec.define("DOOM_SIM2_UNBOUNDED_INPUT", None);
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
            .compiler(selected_cc)
            .archiver(selected_ar);
        for relative in HOST_NATIVE_CODEC_UPSTREAM_FILES {
            native.file(tree.join(relative));
        }
        for relative in HOST_NATIVE_CODEC_REPO_FILES {
            native.file(manifest.join(relative));
        }
        if legacy_5120_buffer {
            native.define("DOOM_SIM2_LEGACY_DEMO_CAP", None);
        } else if env::var_os("CARGO_FEATURE_CTX_ENGINE").is_some() {
            native.define("DOOM_SIM2_UNBOUNDED_INPUT", None);
        }
        native.compile("doom_engine_ex2_native");
    }
}
