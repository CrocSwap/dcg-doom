use std::env;

fn main() {
    println!("cargo:rerun-if-changed=src/doom_hotloops.c");

    let target = env::var("TARGET").expect("Cargo supplies TARGET to build scripts");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_STATIC_WRITE_PROBE");

    let mut build = cc::Build::new();
    build.file("src/doom_hotloops.c");
    if env::var("CARGO_FEATURE_STATIC_WRITE_PROBE").is_ok() {
        build.define("DOOM_STATIC_WRITE_PROBE", None);
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
}
