//! The window icon that `build.rs` writes for Linux and FreeBSD builds, and
//! the icon and the resource script of the `.exe` for Windows builds.

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

#[test]
fn the_windows_icon_holds_one_png_frame_for_each_size_windows_uses() {
    let folder = tempfile::tempdir().unwrap();
    let icon = folder.path().join("app.ico");
    build::write_windows_icon(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/yard_build_config/tests/configs/plain/icon.png"),
        &icon,
    )
    .unwrap();
    let bytes = std::fs::read(&icon).unwrap();
    // ICONDIR: reserved 0, type 1 (icon), then the count of entries.
    assert_eq!(bytes[..4], [0, 0, 1, 0]);
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut sizes = Vec::new();
    for entry in bytes[6..6 + 16 * count].chunks(16) {
        // A width or height of 0 means 256.
        let side = |byte: u8| if byte == 0 { 256 } else { u32::from(byte) };
        let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        let frame = image::load_from_memory_with_format(
            &bytes[offset..offset + length],
            image::ImageFormat::Png,
        )
        .expect("each frame is a PNG");
        assert_eq!(
            (frame.width(), frame.height()),
            (side(entry[0]), side(entry[1]))
        );
        sizes.push(frame.width());
    }
    assert_eq!(sizes, build::WINDOWS_ICON_SIZES);
    // image's own ICO reader takes it too, and reads the largest frame.
    let largest = image::open(&icon).expect("the icon is an ICO");
    assert_eq!((largest.width(), largest.height()), (256, 256));
}

#[test]
fn an_icon_that_is_not_a_png_is_an_error_naming_the_file() {
    let folder = tempfile::tempdir().unwrap();
    let icon = folder.path().join("broken.png");
    std::fs::write(&icon, b"not a png").unwrap();
    let error = build::write_windows_icon(&icon, &folder.path().join("app.ico")).unwrap_err();
    assert!(error.contains("broken.png"), "{error}");
}

#[test]
fn the_windows_resource_names_icon_one_and_the_product_and_its_version() {
    let script = build::windows_resource(
        Path::new(r"C:\out\app.ico"),
        "Lab \"Notes\" Pro",
        "1.2.3-beta.4",
    )
    .unwrap();
    for line in [
        "#pragma code_page(65001)",
        r#"1 ICON "C:\\out\\app.ico""#,
        "1 VERSIONINFO",
        "FILEVERSION 1,2,3,0",
        "PRODUCTVERSION 1,2,3,0",
        r#"        VALUE "Translation", 0x409, 1200"#,
        r#"        BLOCK "040904B0""#,
        r#"            VALUE "FileDescription", "Lab ""Notes"" Pro""#,
        r#"            VALUE "FileVersion", "1.2.3-beta.4""#,
        r#"            VALUE "ProductName", "Lab ""Notes"" Pro""#,
        r#"            VALUE "ProductVersion", "1.2.3-beta.4""#,
    ] {
        assert!(
            script.lines().any(|got| got == line),
            "{line:?} in\n{script}"
        );
    }
    // The config has no company, so the resource names none.
    assert!(!script.contains("CompanyName"), "{script}");
}

#[test]
fn the_version_numbers_are_the_core_version_then_zero() {
    for (version, numbers) in [
        ("1.2.3", "1,2,3,0"),
        ("1.2.3+42", "1,2,3,0"),
        ("1.2.3-beta.4", "1,2,3,0"),
        ("1.2.3.4", "1,2,3,0"),
        ("7", "7,0,0,0"),
        ("1.2", "1,2,0,0"),
        ("1.2.65535", "1,2,65535,0"),
    ] {
        assert_eq!(
            build::version_numbers(version).unwrap(),
            numbers,
            "{version}"
        );
    }
}

#[test]
fn a_version_number_over_65535_is_an_error_saying_so() {
    for version in ["1.2.65536", "70000.0.0", "1.99999999999999999999.0"] {
        let error = build::version_numbers(version).unwrap_err();
        assert!(error.contains("65535"), "{version}: {error}");
        assert!(error.contains(version), "{version}: {error}");
        assert!(build::windows_resource(Path::new("app.ico"), "Lab", version).is_err());
    }
}
