// Neo modification (2026-10-04): mandatory, fail-closed no-TTS native linkage.
// Historical build script retained in build-before-default.rs.
mod neo_asr;

use std::{env, error::Error, path::PathBuf};

fn main() {
    if let Err(error) = try_main() {
        panic!("{error}; prepare native inputs with tools/build_sherpa_asr.py (--help lists the required stages); no legacy download fallback");
    }
}

fn try_main() -> Result<(), Box<dyn Error>> {
    for name in [
        "SHERPA_ONNX_LIB_DIR",
        "SHERPA_ONNX_ARCHIVE_DIR",
        "DOCS_RS",
        "NEO_SHERPA_ASR_ONLY",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if let Some(value) = env::var_os("NEO_SHERPA_ASR_ONLY") {
        if value != "1" {
            return Err(
                "NEO_SHERPA_ASR_ONLY must be unset or exactly 1; no-TTS cannot be disabled".into(),
            );
        }
    }
    if env::var("TARGET")? != "x86_64-pc-windows-msvc"
        || env::var_os("CARGO_FEATURE_SHARED").is_some()
        || env::var_os("DOCS_RS").is_some()
        || env::var_os("SHERPA_ONNX_ARCHIVE_DIR").is_some()
    {
        return Err("ASR-only requires Windows x64 static, no DOCS_RS/archive override".into());
    }
    let path = match env::var_os("SHERPA_ONNX_LIB_DIR") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("Missing crate directory")?)
            .join("../../target/sherpa-asr/native/install/lib"),
    };
    if !path.is_absolute() || !path.is_dir() {
        return Err(
            "ASR-only requires an existing native cache or absolute SHERPA_ONNX_LIB_DIR".into(),
        );
    }
    let path = path.canonicalize()?;
    neo_asr::validate(&path)?;
    println!("cargo:rustc-link-search=native={}", path.display());
    for lib in neo_asr::LIBS {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    Ok(())
}
