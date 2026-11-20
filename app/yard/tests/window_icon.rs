//! The window icon that `build.rs` writes for Linux and FreeBSD builds.

#[allow(dead_code)]
#[path = "../build.rs"]
mod build;

use std::path::Path;

#[test]
fn only_linux_and_freebsd_targets_need_the_window_icon() {
    assert!(build::needs_window_icon("linux"));
    assert!(build::needs_window_icon("freebsd"));
    for target_os in ["macos", "windows", "ios", "android"] {
        assert!(!build::needs_window_icon(target_os), "{target_os}");
    }
}

#[test]
fn an_icon_without_a_png_extension_is_decoded_as_a_png() {
    let folder = tempfile::tempdir().unwrap();
    let icon = folder.path().join("app-icon");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/yard_build_config/tests/configs/plain/icon.png"),
        &icon,
    )
    .unwrap();
    let out = folder.path().join("window_icon.png");
    build::write_window_icon(&icon, &out);
    let written = image::open(&out).expect("the window icon is a PNG");
    assert_eq!((written.width(), written.height()), (256, 256));
}
