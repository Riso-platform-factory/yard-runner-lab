#![cfg(all(feature = "dev", unix))]
//! The `install.sh` that `bundle linux` writes, really run, with `HOME` and
//! `XDG_DATA_HOME` in a temporary folder and fake `update-desktop-database`
//! and `ldd` on `PATH` that record their arguments: it installs the app, its
//! command, and its menu entry with absolute paths; a second install
//! replaces the first; and a failed copy leaves the first in place.

#[allow(dead_code)]
mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use support::{FakeRunner, tempdir, under_test};
use xtask::{Interrupt, LinuxArgs, bundle_linux};

/// A temporary folder with a package of `plain` in `out`, a home, a data
/// folder, and fake tools.
struct Setup {
    _temp: support::TempDir,
    app: PathBuf,
    home: PathBuf,
    data: PathBuf,
    fake: PathBuf,
}

impl Setup {
    /// A home without characters that need quoting.
    fn new(dev: bool, env: &[&str], binary: &[u8]) -> Self {
        Self::with_home("home", dev, env, binary)
    }

    /// `home` is the home's folder name; `binary` is the app the fake build
    /// reports.
    fn with_home(home: &str, dev: bool, env: &[&str], binary: &[u8]) -> Self {
        let temp = tempdir();
        let home = temp.path().join(home);
        let data = temp.path().join("data");
        let fake = temp.path().join("fake");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&fake).unwrap();
        let record = |tool: &str, then: &str| {
            let script = fake.join(tool);
            let log = temp.path().join(format!("{tool}.log"));
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n{then}",
                    log.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        record("update-desktop-database", "");
        record(
            "ldd",
            "printf '\\tlinux-vdso.so.1 (0x0)\\n\\tlibmissing.so.3 => not found\\n'\n",
        );

        let mut runner = FakeRunner::new(temp.path());
        std::fs::write(&runner.binary, binary).unwrap();
        let options = LinuxArgs {
            config: Some(under_test("plain")),
            out: Some(temp.path().join("out")),
            dev,
            env: env.iter().map(|pair| pair.to_string()).collect(),
        };
        let bundled = bundle_linux(&options, "linux", &Interrupt::default(), &mut runner).unwrap();
        Self {
            app: bundled.app,
            home,
            data,
            fake,
            _temp: temp,
        }
    }

    fn local(&self) -> PathBuf {
        self.home.join(".local")
    }

    fn installed(&self) -> PathBuf {
        self.local().join("lab.app")
    }

    fn log(&self, tool: &str) -> String {
        std::fs::read_to_string(self.fake.with_file_name(format!("{tool}.log"))).unwrap_or_default()
    }

    /// Runs `install.sh` with `sh <path>`, only these variables set, and
    /// `PATH` starting with `first` (when given), then the fake tools.
    fn install(&self, first: Option<&Path>, data: Option<&Path>) -> Output {
        let mut path = String::new();
        if let Some(first) = first {
            path.push_str(&format!("{}:", first.display()));
        }
        path.push_str(&format!("{}:/usr/bin:/bin", self.fake.display()));
        let mut command = Command::new("/bin/sh");
        command
            .arg(self.app.join("install.sh"))
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path);
        if let Some(data) = data {
            command.env("XDG_DATA_HOME", data);
        }
        command.output().unwrap()
    }
}

/// A folder beside the home with `tool`, a script with this text.
fn tool(setup: &Setup, tool: &str, text: &str) -> PathBuf {
    let folder = setup.home.with_file_name(format!("fake-{tool}"));
    std::fs::create_dir_all(&folder).unwrap();
    let script = folder.join(tool);
    std::fs::write(&script, text).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    folder
}

/// Installs once, marks the install, and changes the package, so a second
/// install can be compared with the first.
fn installed_once(setup: &Setup) -> Vec<(String, u32, Vec<u8>)> {
    succeeded(&setup.install(None, Some(&setup.data)));
    std::fs::write(setup.installed().join("first"), b"first").unwrap();
    std::fs::write(setup.app.join("second"), b"second").unwrap();
    tree(&setup.installed())
}

/// The first install, its link, and nothing else in `~/.local`.
fn first_install_kept(setup: &Setup, before: &[(String, u32, Vec<u8>)]) {
    assert_eq!(
        tree(&setup.installed()),
        before,
        "the first install is intact"
    );
    assert_eq!(
        entries(&setup.local()),
        ["bin", "lab.app"],
        "no new or old bundle is left beside it"
    );
    assert_eq!(
        std::fs::read(setup.local().join("bin/lab")).unwrap(),
        b"the yard binary",
        "the command still starts the app"
    );
}

fn succeeded(output: &Output) -> (String, String) {
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert!(
        output.status.success(),
        "install.sh failed:\n{stdout}\n{stderr}"
    );
    (stdout, stderr)
}

fn entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Every file below `folder`, relative to it, with its mode and contents.
fn tree(folder: &Path) -> Vec<(String, u32, Vec<u8>)> {
    fn walk(root: &Path, folder: &Path, files: &mut Vec<(String, u32, Vec<u8>)>) {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(folder, folder, &mut files);
    files.sort();
    files
}

#[test]
fn install_puts_the_app_its_command_and_its_menu_entry_in_place() {
    // A home whose path needs quoting and escaping in `Exec`.
    let setup = Setup::with_home("a 100% $home", false, &[], b"the yard binary");
    let (stdout, stderr) = succeeded(&setup.install(None, Some(&setup.data)));

    let installed = setup.installed();
    assert_eq!(tree(&installed), tree(&setup.app), "the bundle, copied");
    assert_eq!(
        entries(&setup.local()),
        ["bin", "lab.app"],
        "no temporary folder stays"
    );
    let link = setup.local().join("bin/lab");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        installed.join("bin/lab")
    );
    assert_eq!(std::fs::read(&link).unwrap(), b"the yard binary");

    let applications = setup.data.join("applications");
    assert_eq!(entries(&applications), ["test.yard.lab.desktop"]);
    let binary = installed.join("bin/lab").display().to_string();
    let exec = binary.replace('$', "\\\\$").replace('%', "%%");
    assert_eq!(
        std::fs::read_to_string(applications.join("test.yard.lab.desktop")).unwrap(),
        format!(
            "[Desktop Entry]\nVersion=1.0\nType=Application\nName=Lab\n\
             Exec=\"{exec}\" %U\nTryExec={binary}\nIcon={}\nStartupNotify=true\n\
             MimeType=x-scheme-handler/yardlab;\n",
            installed
                .join("share/icons/hicolor/512x512/apps/test.yard.lab.png")
                .display()
        ),
        "Exec, TryExec, and Icon are absolute"
    );

    assert_eq!(
        setup.log("update-desktop-database"),
        format!("{}\n", applications.display())
    );
    assert_eq!(setup.log("ldd"), format!("{binary}\n"));
    assert!(
        stderr.contains("warning: this system lacks libraries Lab needs:\n    libmissing.so.3\n"),
        "the missing library is a warning: {stderr}"
    );
    assert!(!stderr.contains("linux-vdso"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "install.sh: warning: {binary} holds a space or a %: the command {} works, but the \
             menu entry and xdg-open may not start Lab from this path.\n",
            link.display()
        )),
        "a path that menus may not start is a warning: {stderr}"
    );
    assert!(
        stdout.contains(&format!("Installed Lab in {}\n", installed.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "{} is not on PATH",
            setup.local().join("bin").display()
        )),
        "{stdout}"
    );
}

#[test]
fn a_second_install_replaces_the_first() {
    let setup = Setup::new(false, &[], b"the yard binary");
    succeeded(&setup.install(None, Some(&setup.data)));
    std::fs::write(setup.installed().join("first"), b"first").unwrap();
    std::fs::write(setup.app.join("second"), b"second").unwrap();

    // From inside the unpacked bundle, this time.
    let output = Command::new("./install.sh")
        .current_dir(&setup.app)
        .env_clear()
        .env("HOME", &setup.home)
        .env("XDG_DATA_HOME", &setup.data)
        .env("PATH", format!("{}:/usr/bin:/bin", setup.fake.display()))
        .output()
        .unwrap();
    let (_, stderr) = succeeded(&output);
    assert!(
        !stderr.contains("holds a space or a %"),
        "a plain home gives no path warning: {stderr}"
    );
    assert!(
        !setup.installed().join("first").exists(),
        "the first is gone"
    );
    assert_eq!(tree(&setup.installed()), tree(&setup.app));
    assert_eq!(entries(&setup.local()), ["bin", "lab.app"]);
    assert_eq!(
        std::fs::read_link(setup.local().join("bin/lab")).unwrap(),
        setup.installed().join("bin/lab")
    );
}

#[test]
fn a_failed_copy_leaves_the_first_install_in_place() {
    let setup = Setup::new(false, &[], b"the yard binary");
    succeeded(&setup.install(None, Some(&setup.data)));
    std::fs::write(setup.installed().join("first"), b"first").unwrap();
    let before = tree(&setup.installed());
    std::fs::write(setup.app.join("second"), b"second").unwrap();

    // A `cp` that copies everything, then fails.
    let failing = setup.home.with_file_name("failing");
    std::fs::create_dir_all(&failing).unwrap();
    let cp = failing.join("cp");
    std::fs::write(
        &cp,
        "#!/bin/sh\n/bin/cp \"$@\"\necho 'cp failed on purpose' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&cp, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = setup.install(Some(&failing), Some(&setup.data));
    assert!(!output.status.success(), "install.sh fails");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cp failed on purpose\n",
        "it stops at the copy"
    );

    assert_eq!(
        tree(&setup.installed()),
        before,
        "the first install is intact"
    );
    assert_eq!(
        entries(&setup.local()),
        ["bin", "lab.app"],
        "the partial copy is removed"
    );
}

#[test]
fn without_xdg_data_home_the_menu_entry_goes_to_local_share() {
    let setup = Setup::new(false, &[], b"the yard binary");
    // A relative XDG_DATA_HOME is ignored, as the XDG spec says.
    for data in [None, Some(Path::new("relative"))] {
        succeeded(&setup.install(None, data));
        assert_eq!(
            entries(&setup.local().join("share/applications")),
            ["test.yard.lab.desktop"],
            "{data:?}"
        );
        assert!(!setup.data.exists());
    }
}

#[test]
fn a_dev_bundle_runs_dev_through_the_installed_command() {
    let setup = Setup::new(
        true,
        &["YARD_HOME=/tmp/a b"],
        b"#!/bin/sh\necho \"args: $*\"\necho \"home: $YARD_HOME\"\n",
    );
    let bin = setup.local().join("bin");
    let (stdout, _) = succeeded(&setup.install(Some(&bin), Some(&setup.data)));
    assert!(
        stdout.contains("Start it from the applications menu, or with: lab\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("not on PATH"), "{stdout}");
    assert_eq!(
        setup.log("ldd"),
        format!("{}\n", setup.installed().join("libexec/lab").display()),
        "ldd checks the real binary"
    );

    let output = Command::new(bin.join("lab"))
        .arg("yardlab://x")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "args: dev yardlab://x\nhome: /tmp/a b\n"
    );
}

#[test]
fn a_failure_after_the_rename_puts_the_first_install_back() {
    let setup = Setup::new(false, &[], b"the yard binary");
    let before = installed_once(&setup);
    // The applications folder cannot be made: it is a file.
    let data = setup.home.with_file_name("data-file");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("applications"), b"a file").unwrap();
    let output = setup.install(None, Some(&data));
    assert!(!output.status.success(), "install.sh fails");
    first_install_kept(&setup, &before);
}

#[test]
fn a_failed_menu_entry_leaves_no_temporary_file() {
    for (name, text) in [
        // The write fails before the rename.
        ("chmod", "#!/bin/sh\nexit 1\n"),
        // The rename of the entry fails; the bundle's renames go through.
        (
            "mv",
            "#!/bin/sh\ncase \"$1\" in *.desktop.*) exit 1 ;; esac\nexec /bin/mv \"$@\"\n",
        ),
    ] {
        let setup = Setup::new(false, &[], b"the yard binary");
        let before = installed_once(&setup);
        let applications = setup.data.join("applications");
        let entry = std::fs::read(applications.join("test.yard.lab.desktop")).unwrap();
        let output = setup.install(Some(&tool(&setup, name, text)), Some(&setup.data));
        assert!(!output.status.success(), "{name}: install.sh fails");
        assert_eq!(
            entries(&applications),
            ["test.yard.lab.desktop"],
            "{name}: no temporary file is left"
        );
        assert_eq!(
            std::fs::read(applications.join("test.yard.lab.desktop")).unwrap(),
            entry,
            "{name}: the first entry is kept"
        );
        first_install_kept(&setup, &before);
    }
}

#[test]
fn a_dev_bundle_starts_even_with_its_own_path() {
    let setup = Setup::new(
        true,
        &["PATH=/nonexistent"],
        b"#!/bin/sh\necho \"args: $*\"\necho \"path: $PATH\"\n",
    );
    succeeded(&setup.install(None, Some(&setup.data)));
    let output = Command::new(setup.local().join("bin/lab"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "args: dev\npath: /nonexistent\n"
    );
}

#[test]
fn exec_is_quoted_only_when_the_path_needs_it() {
    for (home, quoted) in [("home", false), ("100%", false), ("a home", true)] {
        let setup = Setup::with_home(home, false, &[], b"the yard binary");
        succeeded(&setup.install(None, Some(&setup.data)));
        let binary = setup.installed().join("bin/lab").display().to_string();
        let program = binary.replace('%', "%%");
        let expected = if quoted {
            format!("Exec=\"{program}\" %U")
        } else {
            format!("Exec={program} %U")
        };
        let entry =
            std::fs::read_to_string(setup.data.join("applications/test.yard.lab.desktop")).unwrap();
        assert!(
            entry.lines().any(|line| line == expected),
            "{home:?}: {expected:?} in\n{entry}"
        );
    }
}
