#![cfg(feature = "dev")]
//! `bundle windows` lays out the installer's files and the zip from each
//! config under test, on any system, with the commands it would run recorded
//! instead of run, and a fake Inno Setup compiler: the staged files, every
//! line of the `.iss` against the config, the mutex the app creates, the
//! registry keys, a review bundle that runs `dev`, and the icon's sizes; and
//! what breaks a rule is refused before anything runs.

#[allow(dead_code)]
mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use support::{DIGEST, FakeRunner, config_in, tempdir, under_test};
use xtask::{Failure, Interrupt, WindowsArgs, WindowsBundled, WindowsHost, bundle_windows};

fn options(config: &Path, out: &Path) -> WindowsArgs {
    WindowsArgs {
        config: Some(config.to_owned()),
        out: Some(out.to_owned()),
        ..WindowsArgs::default()
    }
}

/// A Windows host whose `PATH` is a folder of `temp` holding a fake
/// `ISCC.exe`.
fn host(temp: &Path) -> WindowsHost {
    let inno = temp.join("inno");
    std::fs::create_dir_all(&inno).unwrap();
    std::fs::write(inno.join("ISCC.exe"), b"").unwrap();
    WindowsHost {
        system: "windows".into(),
        path: Some(inno.into_os_string()),
        ..WindowsHost::default()
    }
}

fn bundle(
    options: &WindowsArgs,
    temp: &Path,
    runner: &mut FakeRunner,
) -> Result<WindowsBundled, Failure> {
    bundle_windows(options, &host(temp), &Interrupt::default(), runner)
}

fn entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Every file below `folder`, relative to it, with `/` between the parts.
fn files(folder: &Path) -> Vec<String> {
    fn walk(root: &Path, folder: &Path, files: &mut Vec<String>) {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .display()
                        .to_string()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut files = Vec::new();
    walk(folder, folder, &mut files);
    files.sort();
    files
}

/// What the staging folder held when Inno Setup was run.
#[derive(Debug, Default, Clone)]
struct Staged {
    script: PathBuf,
    files: Vec<String>,
    iss: String,
    ico: Vec<u8>,
    exe: Vec<u8>,
}

/// Keeps what the staging folder holds as `ISCC` starts.
fn watch_staging(runner: &mut FakeRunner, name: &str) -> Arc<Mutex<Staged>> {
    let staged = Arc::new(Mutex::new(Staged::default()));
    let seen = staged.clone();
    let name = name.to_owned();
    runner.on_command = Some(Box::new(move |command| {
        if command.program == "ISCC" {
            let script = PathBuf::from(command.args.last().unwrap());
            let folder = script.parent().unwrap();
            *seen.lock().unwrap() = Staged {
                files: files(folder),
                iss: std::fs::read_to_string(&script).unwrap(),
                ico: std::fs::read(folder.join(format!("{name}.ico"))).unwrap(),
                exe: std::fs::read(folder.join(&name).join(format!("{name}.exe"))).unwrap(),
                script,
            };
        }
    }));
    staged
}

/// The sizes of an ICO's frames, read from its directory.
fn ico_sizes(ico: &[u8]) -> Vec<(u32, u32)> {
    assert_eq!(&ico[..4], [0, 0, 1, 0], "an icon file");
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    (0..count)
        .map(|index| {
            let entry = &ico[6 + index * 16..];
            let side = |byte: u8| if byte == 0 { 256 } else { byte as u32 };
            (side(entry[0]), side(entry[1]))
        })
        .collect()
}

/// The `.iss` lines a config gives, with or without `--dev`.
fn expected_lines(
    name: &str,
    version: &str,
    identifier: &str,
    scheme: &str,
    dev: bool,
) -> Vec<String> {
    let arch = std::env::consts::ARCH;
    let (parameters, run_parameters, argument) = if dev {
        ("; Parameters: \"dev\"", "Parameters: \"dev\"; ", "dev ")
    } else {
        ("", "", "")
    };
    let mut lines = vec![
        "[Setup]".to_owned(),
        format!("AppId={identifier}"),
        format!("AppName={name}"),
        format!("AppVersion={version}"),
        format!("AppVerName={name} {version}"),
        format!("DefaultDirName={{localappdata}}\\Programs\\{name}"),
        format!("DefaultGroupName={name}"),
        "DisableProgramGroupPage=yes".to_owned(),
        "PrivilegesRequired=lowest".to_owned(),
        "OutputDir=.".to_owned(),
        format!("OutputBaseFilename={name}-{version}-{arch}-setup"),
        format!("SetupIconFile={name}.ico"),
        format!("UninstallDisplayIcon={{app}}\\{name}.exe"),
        format!("AppMutex={identifier}-instance"),
        "CloseApplications=yes".to_owned(),
        "Compression=lzma2".to_owned(),
        "SolidCompression=yes".to_owned(),
        "WizardStyle=modern".to_owned(),
        "[Files]".to_owned(),
        format!("Source: \"{name}\\{name}.exe\"; DestDir: \"{{app}}\"; Flags: ignoreversion"),
        "[Icons]".to_owned(),
        format!(
            "Name: \"{{userprograms}}\\{name}\"; Filename: \"{{app}}\\{name}.exe\"; AppUserModelID: \"{identifier}\"{parameters}"
        ),
        "[Registry]".to_owned(),
        format!(
            "Root: HKCU; Subkey: \"Software\\Classes\\{scheme}\"; ValueType: string; ValueName: \"\"; ValueData: \"URL:{name}\"; Flags: uninsdeletekey"
        ),
        format!(
            "Root: HKCU; Subkey: \"Software\\Classes\\{scheme}\"; ValueType: string; ValueName: \"URL Protocol\"; ValueData: \"\""
        ),
        format!(
            "Root: HKCU; Subkey: \"Software\\Classes\\{scheme}\\DefaultIcon\"; ValueType: string; ValueName: \"\"; ValueData: \"\"\"{{app}}\\{name}.exe\"\",0\""
        ),
        format!(
            "Root: HKCU; Subkey: \"Software\\Classes\\{scheme}\\shell\\open\\command\"; ValueType: string; ValueName: \"\"; ValueData: \"\"\"{{app}}\\{name}.exe\"\" {argument}\"\"%1\"\"\""
        ),
        "[Run]".to_owned(),
        format!(
            "Filename: \"{{app}}\\{name}.exe\"; {run_parameters}Description: \"Launch {name}\"; Flags: nowait postinstall skipifsilent"
        ),
    ];
    let architecture = match arch {
        "x86_64" => Some("x64compatible"),
        "aarch64" => Some("arm64"),
        _ => None,
    };
    if let Some(architecture) = architecture {
        lines.push(format!("ArchitecturesAllowed={architecture}"));
        lines.push(format!("ArchitecturesInstallIn64BitMode={architecture}"));
    }
    lines
}

/// The lines of an `.iss` that are neither comments nor empty, sorted.
fn settings(iss: &str) -> Vec<String> {
    let mut lines: Vec<String> = iss
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with(';'))
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

fn sorted(mut lines: Vec<String>) -> Vec<String> {
    lines.sort();
    lines
}

#[test]
fn each_config_lays_out_its_installer_and_zip() {
    let arch = std::env::consts::ARCH;
    for (folder, name, version, identifier, scheme) in [
        ("plain", "Lab", "0.1.0", "test.yard.lab", "yardlab"),
        (
            "spaced",
            "Lab Notes Pro",
            "0.2.0",
            "test.yard.labnotes",
            "yardlabnotes",
        ),
        (
            "other",
            "Other Lab",
            "1.0.0",
            "test.yard.other",
            "yardother",
        ),
    ] {
        let temp = tempdir();
        let out = temp.path().join("out");
        let config = under_test(folder);
        let mut runner = FakeRunner::new(temp.path());
        let staged = watch_staging(&mut runner, name);
        let bundled = bundle(&options(&config, &out), temp.path(), &mut runner).unwrap();

        let setup = format!("{name}-{version}-{arch}-setup.exe");
        let zip = format!("{name}-{version}-windows-{arch}.zip");
        assert_eq!(
            bundled,
            WindowsBundled {
                installer: (out.join(&setup), DIGEST.to_owned()),
                zip: (out.join(&zip), DIGEST.to_owned()),
            }
        );
        assert_eq!(
            entries(&out),
            sorted(vec![setup.clone(), zip.clone()]),
            "{folder}: only the installer and the zip stay"
        );
        assert_eq!(std::fs::read(out.join(&setup)).unwrap(), b"setup");

        let staged = staged.lock().unwrap().clone();
        assert_eq!(
            staged.files,
            sorted(vec![
                format!("{name}.ico"),
                format!("{name}.iss"),
                format!("{name}/{name}.exe"),
            ]),
            "{folder}: the staged files"
        );
        assert_eq!(
            staged.exe, b"the yard binary",
            "{folder}: the app is the reported binary"
        );
        assert_eq!(
            settings(&staged.iss),
            sorted(expected_lines(name, version, identifier, scheme, false)),
            "{folder}: every line of the .iss:\n{}",
            staged.iss
        );
        assert!(
            staged
                .iss
                .lines()
                .any(|line| line == format!("AppMutex={}", yard_product::instance_name(identifier))),
            "{folder}: the installer names the app's mutex"
        );
        assert_eq!(
            ico_sizes(&staged.ico),
            [16, 24, 32, 48, 64, 256].map(|size| (size, size)),
            "{folder}: the installer's icon"
        );

        assert_eq!(
            runner.programs(),
            ["cargo", "cargo", "ISCC", "certutil", "tar", "certutil"],
            "{folder}: the commands in order"
        );
        let stages = &runner.commands[0];
        assert_eq!(stages.args, ["test", "-p", "yard", "--test", "stages"]);
        let build = &runner.commands[1];
        assert_eq!(
            build.args,
            [
                "build",
                "--release",
                "-p",
                "yard",
                "--bin",
                "yard",
                "--no-default-features",
                "--message-format=json-render-diagnostics"
            ]
        );
        let set: Vec<(&str, &str)> = build
            .envs
            .iter()
            .filter_map(|(key, value)| Some((key.as_str(), value.as_deref()?)))
            .collect();
        assert_eq!(set, [("PRODUCT_CONFIG", config.to_str().unwrap())]);
        assert!(
            build
                .envs
                .iter()
                .any(|(key, value)| key == "WINDOWS_CERTIFICATE" && value.is_none()),
            "{folder}: no secret reaches cargo"
        );

        let iscc = &runner.commands[2];
        let staging = staged.script.parent().unwrap();
        assert_eq!(
            iscc.args,
            ["/Q".to_owned(), staged.script.display().to_string()]
        );
        assert_eq!(staged.script, staging.join(format!("{name}.iss")));
        assert_eq!(
            staging.parent(),
            Some(out.as_path()),
            "{folder}: staged inside out"
        );
        assert_eq!(
            runner.commands[3].args,
            [
                "-hashfile".to_owned(),
                staging.join(&setup).display().to_string(),
                "SHA256".to_owned()
            ]
        );
        let tar = &runner.commands[4];
        assert_eq!(
            tar.args,
            [
                "-a".to_owned(),
                "-c".to_owned(),
                "-f".to_owned(),
                staging.join("release.zip").display().to_string(),
                "-C".to_owned(),
                staging.display().to_string(),
                name.to_owned(),
            ],
            "{folder}: the zip holds <Name>/<Name>.exe"
        );
    }
}

#[test]
fn a_dev_bundle_runs_dev_from_its_shortcut_and_its_url_command() {
    let temp = tempdir();
    let mut runner = FakeRunner::new(temp.path());
    let staged = watch_staging(&mut runner, "Other Lab");
    let options = WindowsArgs {
        dev: true,
        ..options(&under_test("other"), &temp.path().join("out"))
    };
    bundle(&options, temp.path(), &mut runner).unwrap();
    let iss = staged.lock().unwrap().iss.clone();
    assert_eq!(
        settings(&iss),
        sorted(expected_lines(
            "Other Lab",
            "1.0.0",
            "test.yard.other",
            "yardother",
            true
        )),
        "{iss}"
    );
    assert!(
        !runner.commands[1]
            .args
            .iter()
            .any(|arg| arg == "--no-default-features"),
        "a review bundle builds with dev"
    );
}

#[test]
fn braces_in_the_name_are_doubled_where_inno_reads_constants() {
    let temp = tempdir();
    let config = config_in(
        temp.path(),
        "Lab {Notes}",
        "0.1.0",
        "test.yard.braces",
        "yardbraces",
    );
    let mut runner = FakeRunner::new(temp.path());
    let staged = watch_staging(&mut runner, "Lab {Notes}");
    bundle(
        &options(&config, &temp.path().join("out")),
        temp.path(),
        &mut runner,
    )
    .unwrap();
    let iss = staged.lock().unwrap().iss.clone();
    let arch = std::env::consts::ARCH;
    for line in [
        "AppName=Lab {{Notes}".to_owned(),
        "DefaultDirName={localappdata}\\Programs\\Lab {{Notes}".to_owned(),
        "UninstallDisplayIcon={app}\\Lab {{Notes}.exe".to_owned(),
        // Read when compiling, where Inno Setup reads no constants.
        format!("OutputBaseFilename=Lab {{Notes}}-0.1.0-{arch}-setup"),
        "SetupIconFile=Lab {Notes}.ico".to_owned(),
        "Source: \"Lab {Notes}\\Lab {Notes}.exe\"; DestDir: \"{app}\"; Flags: ignoreversion"
            .to_owned(),
    ] {
        assert!(iss.lines().any(|l| l == line), "{line:?} in:\n{iss}");
    }
}

#[test]
fn the_signing_table_is_not_signed_yet_and_the_run_goes_on() {
    let temp = tempdir();
    let mut runner = FakeRunner::new(temp.path());
    let bundled = bundle(
        &options(&under_test("signed"), &temp.path().join("out")),
        temp.path(),
        &mut runner,
    )
    .unwrap();
    assert!(bundled.installer.0.is_file());
    assert!(
        !runner.programs().contains(&"signtool"),
        "{:?}",
        runner.programs()
    );
}

#[test]
fn inno_setup_is_found_in_program_files_when_not_on_path() {
    let temp = tempdir();
    let program_files = temp.path().join("Program Files (x86)");
    let inno = program_files.join("Inno Setup 6");
    std::fs::create_dir_all(&inno).unwrap();
    std::fs::write(inno.join("ISCC.exe"), b"").unwrap();
    let host = WindowsHost {
        system: "windows".into(),
        path: Some(temp.path().join("empty").into_os_string()),
        program_files_x86: Some(program_files.into_os_string()),
        system_root: Some("C:\\Windows".into()),
    };
    let mut runner = FakeRunner::new(temp.path());
    bundle_windows(
        &options(&under_test("plain"), &temp.path().join("out")),
        &host,
        &Interrupt::default(),
        &mut runner,
    )
    .unwrap();
    assert_eq!(
        runner.commands[2].program, "ISCC",
        "{:?}",
        runner.commands[2]
    );
    assert!(
        runner.commands[4].program.ends_with("tar"),
        "Windows' own tar.exe: {:?}",
        runner.commands[4]
    );
}

#[test]
fn no_inno_setup_is_refused_before_building() {
    let temp = tempdir();
    let host = WindowsHost {
        system: "windows".into(),
        path: Some(temp.path().join("empty").into_os_string()),
        program_files_x86: Some(temp.path().join("nowhere").into_os_string()),
        system_root: None,
    };
    let mut runner = FakeRunner::new(temp.path());
    let out = temp.path().join("out");
    let failure = bundle_windows(
        &options(&under_test("plain"), &out),
        &host,
        &Interrupt::default(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(failure.step, "tools");
    assert_eq!(
        failure.reason,
        "Inno Setup's compiler was not found: ISCC.exe is neither on PATH nor in %ProgramFiles(x86)%\\Inno Setup 6"
    );
    assert!(failure.is_refusal());
    assert!(runner.commands.is_empty(), "nothing ran");
    assert!(!out.exists());
}

#[cfg(not(windows))]
#[test]
fn another_system_is_refused_at_the_start() {
    let temp = tempdir();
    let mut runner = FakeRunner::new(temp.path());
    let out = temp.path().join("out");
    let failure = bundle_windows(
        &options(&under_test("plain"), &out),
        &WindowsHost::from_env(),
        &Interrupt::default(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(failure.step, "arguments");
    assert_eq!(
        failure.reason,
        format!(
            "bundle windows runs only on Windows, and this system is {}",
            std::env::consts::OS
        )
    );
    assert!(failure.is_refusal());
    assert!(runner.commands.is_empty(), "nothing ran");
    assert!(!out.exists());
}

#[test]
fn env_is_refused_with_or_without_dev() {
    let temp = tempdir();
    let out = temp.path().join("out");
    for dev in [false, true] {
        let mut runner = FakeRunner::new(temp.path());
        let options = WindowsArgs {
            dev,
            env: vec!["YARD_HOME=C:\\temp".into()],
            ..options(&under_test("other"), &out)
        };
        let failure = bundle(&options, temp.path(), &mut runner).unwrap_err();
        assert_eq!(
            (failure.step, failure.reason.as_str()),
            (
                "arguments",
                "--env: a Windows review bundle cannot carry environment values, since a shortcut and a URL command cannot set them"
            )
        );
        assert!(failure.is_refusal());
        assert!(runner.commands.is_empty());
        assert!(!out.exists());
    }
}

#[test]
fn a_dev_bundle_must_differ_from_the_production_product() {
    for (name, identifier, scheme, key) in [
        ("YARD", "test.yard.devname", "yarddevname", "name"),
        ("Yard Dev", "VN.RisoTech.Yard", "yarddevid", "identifier"),
        ("Yard Dev", "test.yard.devscheme", "yard", "url_scheme"),
    ] {
        let temp = tempdir();
        let config = config_in(temp.path(), name, "0.1.0", identifier, scheme);
        let mut runner = FakeRunner::new(temp.path());
        let options = WindowsArgs {
            dev: true,
            ..options(&config, &temp.path().join("out"))
        };
        let failure = bundle(&options, temp.path(), &mut runner).unwrap_err();
        assert_eq!(failure.step, "config", "{name}");
        assert_eq!(
            failure.reason,
            format!(
                "{}: --dev requires a `{key}` different from the production product's",
                config.display()
            )
        );
        assert!(runner.commands.is_empty(), "nothing ran");
        assert!(!temp.path().join("out").exists());
    }
}

#[test]
fn a_bad_config_is_refused_with_the_config_readers_message() {
    let configs = xtask::repository().join("crates/yard_build_config/tests/configs");
    let mut bad = 0;
    for entry in std::fs::read_dir(configs).unwrap() {
        let folder = entry.unwrap().path();
        if !folder
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("bad-")
        {
            continue;
        }
        bad += 1;
        let config = folder.join("product.toml").canonicalize().unwrap();
        let temp = tempdir();
        let mut runner = FakeRunner::new(temp.path());
        let failure = bundle(
            &options(&config, &temp.path().join("out")),
            temp.path(),
            &mut runner,
        )
        .unwrap_err();
        assert_eq!(failure.step, "config");
        assert_eq!(
            failure.reason,
            yard_build_config::read(&config).unwrap_err()
        );
        assert!(runner.commands.is_empty());
    }
    assert!(bad >= 1, "the bad configs are found");
}

#[test]
fn the_name_and_the_version_must_be_windows_file_names() {
    for (name, version, key) in [
        ("Lab: Notes", "0.1.0", "name"),
        ("Lab\\Notes", "0.1.0", "name"),
        ("Lab?", "0.1.0", "name"),
        ("Lab.", "0.1.0", "name"),
        ("Lab ", "0.1.0", "name"),
        (" Lab", "0.1.0", "name"),
        ("con", "0.1.0", "name"),
        ("COM1.notes", "0.1.0", "name"),
        ("Lab", "0.1/0", "version"),
        ("Lab", "0.1*", "version"),
    ] {
        let temp = tempdir();
        let config = config_in(temp.path(), name, version, "test.yard.lab", "yardlab");
        let mut runner = FakeRunner::new(temp.path());
        let failure = bundle(
            &options(&config, &temp.path().join("out")),
            temp.path(),
            &mut runner,
        )
        .unwrap_err();
        assert_eq!(failure.step, "config", "{name:?} {version:?}");
        assert_eq!(
            failure.reason,
            format!(
                "{}: `{key}` must be a Windows file name: none of < > : \" / \\ | ? * or a control character, no space at either end, no dot at the end, and not a device name such as CON",
                config.display()
            )
        );
        assert!(runner.commands.is_empty());
    }
}

#[test]
fn the_identifier_must_fit_app_id() {
    let temp = tempdir();
    let identifier = format!("test.{}", "a".repeat(123));
    assert_eq!(identifier.len(), 128);
    let config = config_in(temp.path(), "Lab", "0.1.0", &identifier, "yardlab");
    let mut runner = FakeRunner::new(temp.path());
    let failure = bundle(
        &options(&config, &temp.path().join("out")),
        temp.path(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(failure.step, "config");
    assert_eq!(
        failure.reason,
        format!(
            "{}: `identifier` is the installer's AppId, which Inno Setup limits to 127 characters",
            config.display()
        )
    );
    assert!(runner.commands.is_empty());
}

/// `<out>` holding a previous installer and zip of `plain`, each `old`.
fn previous_package(out: &Path) -> (PathBuf, PathBuf) {
    let arch = std::env::consts::ARCH;
    std::fs::create_dir_all(out).unwrap();
    let setup = out.join(format!("Lab-0.1.0-{arch}-setup.exe"));
    let zip = out.join(format!("Lab-0.1.0-windows-{arch}.zip"));
    std::fs::write(&setup, b"old").unwrap();
    std::fs::write(&zip, b"old").unwrap();
    (setup, zip)
}

#[test]
fn a_new_package_replaces_the_previous_one() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let (setup, zip) = previous_package(&out);
    bundle(
        &options(&under_test("plain"), &out),
        temp.path(),
        &mut FakeRunner::new(temp.path()),
    )
    .unwrap();
    assert_eq!(std::fs::read(&setup).unwrap(), b"setup");
    assert_eq!(std::fs::read(&zip).unwrap(), b"made");
    assert_eq!(entries(&out).len(), 2, "the temporary folder is gone");
}

#[test]
fn a_failed_publish_puts_the_previous_package_back() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let (setup, zip) = previous_package(&out);
    let mut runner = FakeRunner::new(temp.path());
    // The staged zip disappears once it is hashed, so publishing it fails
    // after the new installer was published.
    runner.on_command = Some(Box::new(|command| {
        if command.program == "certutil" && command.args[1].ends_with("release.zip") {
            std::fs::remove_file(&command.args[1]).unwrap();
        }
    }));
    let failure = bundle(
        &options(&under_test("plain"), &out),
        temp.path(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(failure.step, "zip");
    assert!(
        failure.reason.starts_with("could not move "),
        "{}",
        failure.reason
    );
    assert_eq!(std::fs::read(&setup).unwrap(), b"old");
    assert_eq!(std::fs::read(&zip).unwrap(), b"old");
    assert_eq!(entries(&out).len(), 2, "the temporary folder is gone");
}

#[test]
fn a_failed_installer_leaves_the_previous_package_alone() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let (setup, zip) = previous_package(&out);
    let mut runner = FakeRunner::new(temp.path());
    runner.fail = Some("ISCC");
    let failure = bundle(
        &options(&under_test("plain"), &out),
        temp.path(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(failure.step, "installer");
    assert!(!failure.is_refusal());
    assert_eq!(std::fs::read(&setup).unwrap(), b"old");
    assert_eq!(std::fs::read(&zip).unwrap(), b"old");
    assert_eq!(entries(&out).len(), 2);
}
