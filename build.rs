//! Embeds the icon and version information into review.exe on Windows.

fn main() {
    println!("cargo:rerun-if-changed=assets/windows/review.rc");
    println!("cargo:rerun-if-env-changed=REVIEW_WINDOWS_RES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Cross builds pass a resource file precompiled with `zig rc` (see scripts/cross-build.sh).
    if let Ok(res) = std::env::var("REVIEW_WINDOWS_RES") {
        println!("cargo:rustc-link-arg-bins={res}");
        return;
    }
    // Native Windows builds compile it with the toolchain's resource compiler; without one the
    // exe simply has no embedded icon.
    let _ = embed_resource::compile("assets/windows/review.rc", embed_resource::NONE).manifest_optional();
}
