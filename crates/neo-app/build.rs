//! Embed the Windows application icon into `neo.exe`.
//!
//! The `.res` file is generated once with `rc.exe` from the tracked
//! `resources/icons/neo.ico`; linking it avoids a build-time dependency on the
//! Windows SDK being present on every developer machine.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../resources/icons/neo.res");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    let resource = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../resources/icons/neo.res");
    println!("cargo:rustc-link-arg={}", resource.display());
}
