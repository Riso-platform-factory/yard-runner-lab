#![cfg(all(feature = "dev", windows))]
//! `cargo xtask bundle windows` for real: it builds yard with the `plain`
//! config, makes the installer with Inno Setup and the zip with Windows'
//! `tar.exe`, and the zip holds `Lab/Lab.exe`. It takes minutes and needs
//! Inno Setup, so it is ignored by default: `cargo test -p xtask --test
//! bundle_windows_end_to_end -- --ignored`.

#[allow(dead_code)]
mod support;

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

#[test]
#[ignore = "builds yard in release and runs Inno Setup"]
fn bundles_plain() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let stdout = succeeds(
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["bundle", "windows", "--config"])
            .arg(under_test("plain"))
            .arg("--out")
            .arg(&out)
            .current_dir(xtask::repository()),
    );
    let lines: Vec<&str> = stdout.lines().collect();
    for step in ["stages", "build", "icon", "bundle", "installer", "zip"] {
        assert!(
            lines.contains(&step),
            "the step {step} is printed: {stdout}"
        );
    }
    let arch = std::env::consts::ARCH;
    let setup = out.join(format!("Lab-0.1.0-{arch}-setup.exe"));
    let zip = out.join(format!("Lab-0.1.0-windows-{arch}.zip"));
    assert!(setup.is_file(), "{}", setup.display());
    assert!(zip.is_file(), "{}", zip.display());
    for path in [&setup, &zip] {
        let at = lines
            .iter()
            .position(|line| *line == path.to_str().unwrap())
            .unwrap_or_else(|| panic!("{} is printed: {stdout}", path.display()));
        assert!(
            at > 0 && lines[at - 1].starts_with("sha256 "),
            "its sha256 comes before it: {stdout}"
        );
    }
    let tar = std::path::Path::new(&std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("tar.exe");
    let listed = succeeds(Command::new(tar).arg("-tf").arg(&zip));
    assert!(
        listed.lines().any(|line| line == "Lab/Lab.exe"),
        "the zip holds Lab/Lab.exe: {listed}"
    );
}
