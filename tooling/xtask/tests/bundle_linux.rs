#![cfg(feature = "dev")]
//! `bundle linux` lays out `<name>.app` and its archive from each config
//! under test, on any system, with the commands it would run recorded
//! instead of run: the layout, its modes, the `.desktop` file, the icons, and
//! `install.sh` follow the config; a review bundle runs `dev`; and what
//! breaks a rule is refused before anything runs.

#[allow(dead_code)]
mod support;

use std::path::{Path, PathBuf};
use support::{DIGEST, FakeRunner, config_in, tempdir, under_test};
use xtask::{Failure, Interrupt, LinuxArgs, LinuxBundled, bundle_linux};

fn options(config: &Path, out: &Path) -> LinuxArgs {
    LinuxArgs {
        config: Some(config.to_owned()),
        out: Some(out.to_owned()),
        ..LinuxArgs::default()
    }
}

/// Bundles as on Linux, whatever this system is.
fn bundle(options: &LinuxArgs, runner: &mut FakeRunner) -> Result<LinuxBundled, Failure> {
    bundle_linux(options, "linux", &Interrupt::default(), runner)
}

fn entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Every file below `folder`, relative to it, with its mode.
fn files(folder: &Path) -> Vec<(String, u32)> {
    fn walk(root: &Path, folder: &Path, files: &mut Vec<(String, u32)>) {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                // With `/` between the parts on every system, as in the archive.
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .display()
                    .to_string()
                    .replace('\\', "/");
                files.push((relative, mode(&path)));
            }
        }
    }
    let mut files = Vec::new();
    walk(folder, folder, &mut files);
    files.sort();
    files
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode(_: &Path) -> u32 {
    0
}

fn icon_size(path: &Path) -> (u32, u32) {
    image::image_dimensions(path).unwrap()
}

/// A config in `folder` as `config_in` writes it, with a square icon of
/// `size` pixels.
fn config_with_icon(folder: &Path, size: u32) -> PathBuf {
    let config = config_in(folder, "Lab", "0.1.0", "test.yard.lab", "yardlab");
    image::RgbImage::from_fn(size, size, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    })
    .save(folder.join("icon.png"))
    .unwrap();
    config
}

#[test]
fn each_config_lays_out_its_package() {
    for (folder, product, name, version, identifier, scheme) in [
        ("plain", "Lab", "lab", "0.1.0", "test.yard.lab", "yardlab"),
        (
            "spaced",
            "Lab Notes Pro",
            "lab-notes-pro",
            "0.2.0",
            "test.yard.labnotes",
            "yardlabnotes",
        ),
        (
            "other",
            "Other Lab",
            "other-lab",
            "1.0.0",
            "test.yard.other",
            "yardother",
        ),
    ] {
        let temp = tempdir();
        let out = temp.path().join("out");
        let config = under_test(folder);
        let mut runner = FakeRunner::new(temp.path());
        let bundled = bundle(&options(&config, &out), &mut runner).unwrap();

        let app = out.join(format!("{name}.app"));
        let archive_name = format!("{name}-{version}-linux-{}.tar.gz", std::env::consts::ARCH);
        assert_eq!(
            bundled,
            LinuxBundled {
                app: app.clone(),
                archive: (out.join(&archive_name), DIGEST.to_owned()),
            }
        );
        assert_eq!(
            entries(&out),
            [archive_name, format!("{name}.app")],
            "{folder}: only the package stays"
        );
        assert_eq!(
            files(&app),
            [
                (format!("bin/{name}"), 0o755),
                ("install.sh".to_owned(), 0o755),
                (format!("share/applications/{identifier}.desktop"), 0o644),
                (
                    format!("share/icons/hicolor/512x512/apps/{identifier}.png"),
                    0o644
                ),
            ]
            .map(|(path, mode)| (path, if cfg!(unix) { mode } else { 0 })),
            "{folder}: the layout; a 512 icon makes no 1024 one"
        );
        assert_eq!(
            std::fs::read(app.join("bin").join(name)).unwrap(),
            b"the yard binary",
            "{folder}: the app is the reported binary"
        );
        let icon = app.join(format!("share/icons/hicolor/512x512/apps/{identifier}.png"));
        assert_eq!(icon_size(&icon), (512, 512));

        let desktop =
            std::fs::read_to_string(app.join(format!("share/applications/{identifier}.desktop")))
                .unwrap();
        assert_eq!(
            desktop,
            format!(
                "[Desktop Entry]\nVersion=1.0\nType=Application\nName={product}\n\
                 Exec={name} %U\nTryExec={name}\nIcon={identifier}\nStartupNotify=true\n\
                 MimeType=x-scheme-handler/{scheme};\n"
            ),
            "{folder}: the .desktop file"
        );

        let install = std::fs::read_to_string(app.join("install.sh")).unwrap();
        for line in [
            "#!/bin/sh".to_owned(),
            "set -eu".to_owned(),
            format!("product='{product}'"),
            format!("name='{name}'"),
            format!("identifier='{identifier}'"),
            format!("binary='bin/{name}'"),
            format!("    printf '%s\\n' 'Name={product}'"),
            format!("    printf '%s\\n' 'MimeType=x-scheme-handler/{scheme};'"),
        ] {
            assert!(
                install.lines().any(|l| l == line),
                "{folder}: install.sh has {line:?}:\n{install}"
            );
        }

        assert_eq!(
            runner.programs(),
            ["cargo", "cargo", "tar", "sha256sum"],
            "{folder}: the commands in order"
        );
        let stages = &runner.commands[0];
        assert_eq!(stages.args, ["test", "-p", "yard", "--test", "stages"]);
        assert_eq!(stages.dir.as_deref(), Some(xtask::repository().as_path()));
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
        assert_eq!(
            set,
            [
                ("CARGO_PROFILE_RELEASE_STRIP", "debuginfo"),
                ("PRODUCT_CONFIG", config.to_str().unwrap()),
            ],
            "{folder}: built from the config, without debug data"
        );
        assert!(
            build
                .envs
                .iter()
                .any(|(key, value)| key == "MACOS_CERTIFICATE" && value.is_none()),
            "{folder}: no secret reaches cargo"
        );
        let tar = &runner.commands[2];
        let staged = Path::new(&tar.args[1]);
        assert_eq!(tar.args[0], "-czf");
        assert_eq!(tar.args[2], "-C");
        assert_eq!(tar.args[3], staged.parent().unwrap().display().to_string());
        assert_eq!(tar.args[4], format!("{name}.app"));
        assert_eq!(
            staged.parent().unwrap().parent(),
            Some(out.as_path()),
            "{folder}: archived in the staging folder, inside out"
        );
        assert_eq!(runner.commands[3].args, [tar.args[1].as_str()]);
    }
}

#[test]
fn icons_are_made_down_to_each_size_and_never_up() {
    for (source, made) in [
        (512, vec![512]),
        (1024, vec![512, 1024]),
        (1100, vec![512, 1024]),
    ] {
        let temp = tempdir();
        let config = config_with_icon(temp.path(), source);
        let out = temp.path().join("out");
        let bundled = bundle(&options(&config, &out), &mut FakeRunner::new(temp.path())).unwrap();
        let hicolor = bundled.app.join("share/icons/hicolor");
        let mut sizes: Vec<String> = made.iter().map(|size| format!("{size}x{size}")).collect();
        sizes.sort();
        assert_eq!(entries(&hicolor), sizes, "from {source}");
        for size in made {
            let icon = hicolor.join(format!("{size}x{size}/apps/test.yard.lab.png"));
            assert_eq!(icon_size(&icon), (size, size), "from {source}");
            if size == source {
                assert_eq!(
                    std::fs::read(&icon).unwrap(),
                    std::fs::read(temp.path().join("icon.png")).unwrap(),
                    "an icon of the source's size is the source"
                );
            }
        }
    }
}

#[test]
fn the_desktop_name_is_escaped() {
    let temp = tempdir();
    let config = config_in(
        temp.path(),
        " Lab\\Notes\tPro ",
        "0.1.0",
        "test.yard.escaped",
        "yardescaped",
    );
    let bundled = bundle(
        &options(&config, &temp.path().join("out")),
        &mut FakeRunner::new(temp.path()),
    )
    .unwrap();
    assert!(bundled.app.ends_with("lab-notes-pro.app"));
    let desktop = std::fs::read_to_string(
        bundled
            .app
            .join("share/applications/test.yard.escaped.desktop"),
    )
    .unwrap();
    assert!(
        desktop
            .lines()
            .any(|line| line == "Name=\\sLab\\\\Notes\\tPro\\s"),
        "{desktop}"
    );
    let install = std::fs::read_to_string(bundled.app.join("install.sh")).unwrap();
    assert!(
        install
            .lines()
            .any(|line| line == "    printf '%s\\n' 'Name=\\sLab\\\\Notes\\tPro\\s'"),
        "{install}"
    );
    assert!(
        install
            .lines()
            .any(|line| line == "product=' Lab\\Notes\tPro '"),
        "{install}"
    );
}

#[test]
fn a_dev_bundle_runs_libexec_with_dev_and_its_environment() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let mut runner = FakeRunner::new(temp.path());
    let options = LinuxArgs {
        dev: true,
        env: vec!["YARD_HOME=/tmp/a b".into(), "_X1=it's $1".into()],
        ..options(&under_test("plain"), &out)
    };
    let bundled = bundle(&options, &mut runner).unwrap();

    let app = &bundled.app;
    let unix = |mode| if cfg!(unix) { mode } else { 0 };
    let layout: Vec<(String, u32)> = files(app)
        .into_iter()
        .filter(|(path, _)| !path.starts_with("share/"))
        .collect();
    assert_eq!(
        layout,
        [
            ("bin/lab".to_owned(), unix(0o755)),
            ("install.sh".to_owned(), unix(0o755)),
            ("libexec/lab".to_owned(), unix(0o755)),
        ]
    );
    assert_eq!(
        std::fs::read(app.join("libexec/lab")).unwrap(),
        b"the yard binary"
    );
    assert_eq!(
        std::fs::read_to_string(app.join("bin/lab")).unwrap(),
        "#!/bin/sh\n\
         self=$0\n\
         while [ -L \"$self\" ]; do\n\
         \x20   link=$(readlink \"$self\")\n\
         \x20   case $link in\n\
         \x20   /*) self=$link ;;\n\
         \x20   *) self=$(dirname \"$self\")/$link ;;\n\
         \x20   esac\n\
         done\n\
         real=\"$(dirname \"$self\")/../libexec/lab\"\n\
         export YARD_HOME='/tmp/a b'\n\
         export _X1='it'\\''s $1'\n\
         exec \"$real\" dev \"$@\"\n"
    );
    let install = std::fs::read_to_string(app.join("install.sh")).unwrap();
    assert!(
        install.lines().any(|line| line == "binary='libexec/lab'"),
        "ldd checks the real binary: {install}"
    );
    let build = &runner.commands[1];
    assert!(
        !build.args.iter().any(|arg| arg == "--no-default-features"),
        "a review bundle builds with dev: {:?}",
        build.args
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn another_system_is_refused_at_the_start() {
    let temp = tempdir();
    let mut runner = FakeRunner::new(temp.path());
    let out = temp.path().join("out");
    let failure = bundle_linux(
        &options(&under_test("plain"), &out),
        std::env::consts::OS,
        &Interrupt::default(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(failure.step, "arguments");
    assert_eq!(
        failure.reason,
        format!(
            "bundle linux runs only on Linux, and this system is {}",
            std::env::consts::OS
        )
    );
    assert!(failure.is_refusal());
    assert!(runner.commands.is_empty(), "nothing ran");
    assert!(!out.exists());
}

#[test]
fn env_needs_dev_and_a_shell_style_key() {
    let temp = tempdir();
    let out = temp.path().join("out");
    for (dev, env, reason) in [
        (false, "YARD_HOME=/tmp", "--env requires --dev".to_owned()),
        (
            true,
            "1KEY=value",
            "--env \"1KEY=value\" requires KEY=VALUE with a shell-style KEY".to_owned(),
        ),
        (
            true,
            "NOVALUE",
            "--env \"NOVALUE\" requires KEY=VALUE with a shell-style KEY".to_owned(),
        ),
    ] {
        let mut runner = FakeRunner::new(temp.path());
        let options = LinuxArgs {
            dev,
            env: vec![env.into()],
            ..options(&under_test("plain"), &out)
        };
        let failure = bundle(&options, &mut runner).unwrap_err();
        assert_eq!((failure.step, failure.reason), ("arguments", reason));
        assert!(runner.commands.is_empty());
        assert!(!out.exists());
    }
}

#[test]
fn a_dev_bundle_must_differ_from_the_production_product() {
    for (name, identifier, scheme, key) in [
        ("YARD", "test.yard.devname", "yarddevname", "name"),
        // The same Linux `<name>`, `yard`, as the production product.
        ("Yard!", "test.yard.devfile", "yarddevfile", "name"),
        ("Yard Dev", "VN.RisoTech.Yard", "yarddevid", "identifier"),
        ("Yard Dev", "test.yard.devscheme", "yard", "url_scheme"),
    ] {
        let temp = tempdir();
        let config = config_in(temp.path(), name, "0.1.0", identifier, scheme);
        let mut runner = FakeRunner::new(temp.path());
        let options = LinuxArgs {
            dev: true,
            ..options(&config, &temp.path().join("out"))
        };
        let failure = bundle(&options, &mut runner).unwrap_err();
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
        let failure = bundle(&options(&config, &temp.path().join("out")), &mut runner).unwrap_err();
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
fn the_version_is_a_single_path_component() {
    let temp = tempdir();
    let config = config_in(temp.path(), "Lab", "0.1/0", "test.yard.lab", "yardlab");
    let mut runner = FakeRunner::new(temp.path());
    let failure = bundle(&options(&config, &temp.path().join("out")), &mut runner).unwrap_err();
    assert_eq!(failure.step, "config");
    assert_eq!(
        failure.reason,
        format!(
            "{}: version must be a single path component",
            config.display()
        )
    );
    assert!(runner.commands.is_empty());
}

/// `<out>` holding a previous `lab.app` and archive, each with the text
/// `old`.
fn previous_package(out: &Path) -> (PathBuf, PathBuf) {
    let app = out.join("lab.app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("old"), b"old").unwrap();
    let archive = out.join(format!("lab-0.1.0-linux-{}.tar.gz", std::env::consts::ARCH));
    std::fs::write(&archive, b"old").unwrap();
    (app, archive)
}

#[test]
fn a_new_package_replaces_the_previous_one() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let (app, archive) = previous_package(&out);
    bundle(
        &options(&under_test("plain"), &out),
        &mut FakeRunner::new(temp.path()),
    )
    .unwrap();
    assert!(!app.join("old").exists());
    assert_eq!(std::fs::read(&archive).unwrap(), b"made");
    assert_eq!(entries(&out).len(), 2, "the temporary folder is gone");
}

#[test]
fn a_failed_publish_puts_the_previous_package_back() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let (app, archive) = previous_package(&out);
    let mut runner = FakeRunner::new(temp.path());
    // The staged archive disappears once it is hashed, so publishing it
    // fails after the new app was published.
    runner.on_command = Some(Box::new(|command| {
        if command.program == "sha256sum" {
            std::fs::remove_file(&command.args[0]).unwrap();
        }
    }));
    let failure = bundle(&options(&under_test("plain"), &out), &mut runner).unwrap_err();
    assert_eq!(failure.step, "archive");
    assert!(
        failure.reason.starts_with("could not move "),
        "{}",
        failure.reason
    );
    assert_eq!(std::fs::read(app.join("old")).unwrap(), b"old");
    assert_eq!(entries(&app), ["old"], "the new app was taken back");
    assert_eq!(std::fs::read(&archive).unwrap(), b"old");
    assert_eq!(entries(&out).len(), 2, "the temporary folder is gone");
}

#[test]
fn a_failed_step_leaves_the_previous_package_alone() {
    let temp = tempdir();
    let out = temp.path().join("out");
    let (app, archive) = previous_package(&out);
    let mut runner = FakeRunner::new(temp.path());
    runner.fail = Some("tar");
    let failure = bundle(&options(&under_test("plain"), &out), &mut runner).unwrap_err();
    assert_eq!(failure.step, "archive");
    assert_eq!(entries(&app), ["old"]);
    assert_eq!(std::fs::read(&archive).unwrap(), b"old");
    assert_eq!(entries(&out).len(), 2);
}
