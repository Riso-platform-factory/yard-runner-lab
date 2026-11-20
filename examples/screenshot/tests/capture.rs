//! Runs the tool as `cargo run -p screenshot` does. Both tests need a GPU
//! (Metal on macOS, a Vulkan or GL driver on Linux), so they are ignored by
//! default: `cargo test -p screenshot -- --ignored`.

use std::process::{Command, Output};

fn run(out: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_screenshot"))
        .args([
            "--out",
            out.to_str().unwrap(),
            "--width",
            "320",
            "--height",
            "200",
        ])
        .env(
            yard::startup::PRODUCT.lifecycle_variable(),
            "info,screenshot=debug",
        )
        .output()
        .expect("the tool runs")
}

#[test]
#[ignore = "needs a GPU"]
fn writes_the_window_at_the_requested_size() {
    let folder = tempfile::tempdir().unwrap();
    let out = folder.path().join("window.png");
    let output = run(&out);
    let trace = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{trace}");
    let image = image::open(&out).expect("the PNG decodes").into_rgba8();
    assert_eq!(image.dimensions(), (320, 200));
    let first = image.get_pixel(0, 0);
    assert!(
        image.pixels().any(|pixel| pixel != first),
        "the PNG is a single colour"
    );
    assert!(
        trace.contains("✓ [app-startup > screenshot-capture{width=320, height=200"),
        "{trace}"
    );
    assert!(trace.contains("] written"), "{trace}");
    assert!(!trace.contains("ERROR"), "{trace}");
}

#[test]
#[ignore = "needs a GPU"]
fn a_failed_write_is_an_error_and_exit_code_1() {
    let folder = tempfile::tempdir().unwrap();
    let out = folder.path().join("missing").join("window.png");
    let output = run(&out);
    let trace = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{trace}");
    assert!(
        trace.contains("ERROR [app-startup > screenshot-capture{width=320, height=200"),
        "{trace}"
    );
    assert!(trace.contains("writing the PNG failed"), "{trace}");
    assert!(!out.exists());
}
