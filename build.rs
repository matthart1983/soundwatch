//! Embeds `Info.plist` into the executable's `__TEXT,__info_plist` section.
//!
//! This is how a command-line tool carries the keys TCC requires — a bundle is
//! the usual vehicle and a TUI cannot live in one. `ld` places the section
//! before the ad-hoc signature is applied, so the signature covers it and the
//! binary stays valid.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this");
    let plist = PathBuf::from(manifest).join("Info.plist");

    // Bins only: the test harness links its own executable and does not need
    // (and cannot use) a TCC identity.
    println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
}
