//! Probe host linkage for the real GPIO backend.
//!
//! `libgpiod-sys` performs bindgen + `system-deps` linking. This script only
//! validates the target and (on Linux) that pkg-config can see libgpiod 2.x,
//! so missing headers fail with a service-specific message instead of a
//! bindgen traceback. Link flags are left to `libgpiod-sys`.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=SYSTEM_DEPS_LIBGPIOD_NO_PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=SYSTEM_DEPS_LIBGPIOD_SEARCH_NATIVE");
    println!("cargo:rerun-if-env-changed=SYSTEM_DEPS_LIBGPIOD_LIB");
    println!("cargo:rerun-if-env-changed=SYSTEM_DEPS_LIBGPIOD_INCLUDE");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        println!(
            "cargo:warning=real libgpiod backend is Linux-only; compiling without libgpiod-sys"
        );
        return;
    }

    if std::env::var_os("SYSTEM_DEPS_LIBGPIOD_NO_PKG_CONFIG").is_some() {
        // Caller is pointing libgpiod-sys at a custom include/lib tree.
        return;
    }

    match pkg_config::Config::new()
        .atleast_version("2")
        .cargo_metadata(false)
        .probe("libgpiod")
    {
        Ok(_) => {}
        Err(err) => {
            panic!(
                "Linux builds need libgpiod 2.x (pkg-config package `libgpiod` >= 2.0). \
                 Install the development package (for example `libgpiod-dev`) or set \
                 SYSTEM_DEPS_LIBGPIOD_NO_PKG_CONFIG=1 and SYSTEM_DEPS_LIBGPIOD_{{SEARCH_NATIVE,LIB,INCLUDE}} \
                 as documented by libgpiod-sys. pkg-config error: {err}"
            );
        }
    }
}
