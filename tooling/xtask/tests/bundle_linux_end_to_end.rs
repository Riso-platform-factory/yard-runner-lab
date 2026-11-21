#![cfg(all(feature = "dev", target_os = "linux"))]
//! `cargo xtask bundle linux` for real: it builds yard with the `plain`
//! config, archives the package, and the unpacked archive holds the layout.
//! It takes minutes, so it is ignored by default: `cargo test -p xtask --test
//! bundle_linux_end_to_end -- --ignored`.

#[allow(dead_code)]
mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use support::{tempdir, under_test};

fn succeeds(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
#[ignore = "builds yard in release"]
fn bundles_plain_and_unpacks() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let stdout = succeeds(
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["bundle", "linux", "--config"])
            .arg(under_test("plain"))
            .arg("--out")
            .arg(&out)
            .current_dir(xtask::repository()),
    );
    let lines: Vec<&str> = stdout.lines().collect();
    for step in ["stages", "build", "icon", "bundle", "archive"] {
        assert!(
            lines.contains(&step),
            "the step {step} is printed: {stdout}"
        );
    }
    let archive = out.join(format!("lab-0.1.0-linux-{}.tar.gz", std::env::consts::ARCH));
    assert_eq!(lines.last(), Some(&archive.to_str().unwrap()), "{stdout}");
    let digest = lines
        .iter()
        .find_map(|line| line.strip_prefix("sha256 "))
        .unwrap_or_else(|| panic!("no sha256 line: {stdout}"));
    let computed = succeeds(Command::new("sha256sum").arg(&archive));
    assert_eq!(computed.split_whitespace().next(), Some(digest));
    assert!(out.join("lab.app/bin/lab").is_file(), "the unpacked app");

    let unpacked = temp.path().join("unpacked");
    std::fs::create_dir(&unpacked).unwrap();
    succeeds(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&unpacked),
    );
    assert_eq!(
        std::fs::read_dir(&unpacked)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        ["lab.app"]
    );
    let app = unpacked.join("lab.app");
    let binary = app.join("bin/lab");
    assert_eq!(mode(&binary), 0o755);
    assert_eq!(&std::fs::read(&binary).unwrap()[..4], b"\x7fELF");
    let sections = succeeds(Command::new("readelf").arg("-S").arg(&binary));
    assert!(
        !sections.contains(".debug_info"),
        "the binary has no debug data"
    );
    assert_eq!(mode(&app.join("install.sh")), 0o755);
    let desktop = app.join("share/applications/test.yard.lab.desktop");
    assert_eq!(mode(&desktop), 0o644);
    assert!(
        std::fs::read_to_string(&desktop)
            .unwrap()
            .contains("MimeType=x-scheme-handler/yardlab;\n")
    );
    assert!(
        app.join("share/icons/hicolor/512x512/apps/test.yard.lab.png")
            .is_file()
    );
    assert!(
        !app.join("share/icons/hicolor/1024x1024").exists(),
        "plain's icon is 512"
    );
}
