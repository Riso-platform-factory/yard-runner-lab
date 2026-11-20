// The check and the install run only on macOS until the release-updates slice.
#![cfg(target_os = "macos")]

use gpui::TestAppContext;
use http_client::{AsyncBody, FakeHttpClient, Response};
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, MutexGuard, Once},
    time::Duration,
};
use yard_product::Product;
use yard_shell::Extension;

const NAME: &str = "yard-check";
const IDENTIFIER: &str = "vn.risotech.yard";
const URL: &str = "http://127.0.0.1:8765/latest.json";
const ZIP_URL: &str = "https://releases.example.com/download/yard-check-0.2.0.zip?channel=alpha";
const FIRST_CHECK: Duration = Duration::from_secs(5);
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(10 * 60);
// Tells `inside_a_bundle_a_stalled_download_is_given_up_after_ten_minutes`
// that it runs inside the bundle its parent test built.
const INSIDE_BUNDLE: &str = "YARD_UPDATE_INSTALL_RULES_INSIDE_BUNDLE";

// Every test in this file shares one process-wide diagnostic logger and runs
// under one lock, so a test never sees another test's records.
static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());
static LOCK: Mutex<()> = Mutex::new(());
static INIT: Once = Once::new();

struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_update")
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            RECORDS
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((record.level(), record.args().to_string()));
        }
    }
    fn flush(&self) {}
}

fn lock() -> MutexGuard<'static, ()> {
    INIT.call_once(|| {
        log::set_logger(&Diagnostics).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
    });
    let guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    RECORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
    guard
}

fn records_at(level: log::Level) -> Vec<String> {
    RECORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter(|(at, _)| *at == level)
        .map(|(_, message)| message.clone())
        .collect()
}

fn info_plist(identifier: &str, version: &str, executable: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>CFBundleIdentifier</key>\n\
         \t<string>{identifier}</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\
         \t<string>{version}</string>\n\
         \t<key>CFBundleExecutable</key>\n\
         \t<string>{executable}</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

fn run(command: &mut Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?} failed: {status}");
}

fn sign(app: &Path) {
    run(Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(app));
}

// `<dir>/<NAME>.app` with an `Info.plist` and a script holding `marker`,
// signed ad hoc.
fn make_bundle(dir: &Path, version: &str, marker: &str) -> PathBuf {
    let app = dir.join(format!("{NAME}.app"));
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        info_plist(IDENTIFIER, version, NAME),
    )
    .unwrap();
    let executable = macos.join(NAME);
    std::fs::write(&executable, format!("#!/bin/sh\n# {marker}\nexit 0\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    sign(&app);
    app
}

fn sha256(file: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(file)
        .output()
        .unwrap();
    assert!(output.status.success(), "shasum failed: {output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

// `source` zipped as `ditto -c -k --keepParent` makes a release.
fn zip_app(source: &Path, zip: &Path) -> (Vec<u8>, String) {
    run(Command::new("ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(source)
        .arg(zip));
    (std::fs::read(zip).unwrap(), sha256(zip))
}

// A good release 0.2.0 built in `dir`: the zip's bytes and digest.
fn good_release(dir: &Path) -> (Vec<u8>, String) {
    let app = make_bundle(dir, "0.2.0", "version 0.2.0");
    zip_app(&app, &dir.join("release.zip"))
}

// Adds an entry named `name` to `zip` with Python's `zipfile`, which keeps
// names that `ditto` never writes.
fn add_entry(zip: &Path, name: &str) -> (Vec<u8>, String) {
    run(Command::new("/usr/bin/python3")
        .args([
            "-I",
            "-c",
            "import sys, zipfile\n\
             with zipfile.ZipFile(sys.argv[1], 'a') as z: z.writestr(sys.argv[2], 'x')",
        ])
        .arg(zip)
        .arg(name));
    (std::fs::read(zip).unwrap(), sha256(zip))
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    let mut entries = BTreeMap::new();
    collect(dir, dir, &mut entries);
    entries
}

fn collect(root: &Path, dir: &Path, entries: &mut BTreeMap<PathBuf, (Vec<u8>, u32)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap().to_owned();
        let file_type = entry.file_type().unwrap();
        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).unwrap();
            entries.insert(relative, (target.into_os_string().into_encoded_bytes(), 0));
        } else if file_type.is_dir() {
            entries.insert(relative.clone(), (Vec::new(), mode));
            collect(root, &path, entries);
        } else {
            entries.insert(relative, (std::fs::read(&path).unwrap(), mode));
        }
    }
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn version_of(app: &Path) -> String {
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
    let after = plist
        .split("<key>CFBundleShortVersionString</key>")
        .nth(1)
        .unwrap();
    let start = after.find("<string>").unwrap() + "<string>".len();
    let end = start + after[start..].find("</string>").unwrap();
    after[start..end].to_owned()
}

// The addresses a fake client was asked for.
#[derive(Clone, Default)]
struct Requests(Arc<Mutex<Vec<String>>>);

impl Requests {
    fn client(&self, body: Vec<u8>) -> Arc<dyn http_client::HttpClient> {
        let requests = self.clone();
        FakeHttpClient::create(move |request| {
            requests
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(request.uri().to_string());
            let body = body.clone();
            async move {
                Ok(Response::builder()
                    .status(200)
                    .body(AsyncBody::from(body))
                    .unwrap())
            }
        })
    }

    fn all(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

fn install(
    client: Arc<dyn http_client::HttpClient>,
    bundle: PathBuf,
    url: &str,
    sha256: &str,
) -> Result<(), String> {
    futures::executor::block_on(yard_update::install(
        client, bundle, NAME, IDENTIFIER, "0.2.0", url, sha256,
    ))
}

// Runs an install of `zip` over a running bundle 0.1.0 that must refuse it:
// `Err`, one WARN, no ERROR, the bundle byte for byte as it was, and nothing
// left beside it; the WARN says `reason`.
fn refused(zip: Vec<u8>, sha256: &str, reason: &str) {
    let temp = tempfile::tempdir().unwrap();
    let bundle = make_bundle(temp.path(), "0.1.0", "the running version");
    let before = snapshot(&bundle);
    let requests = Requests::default();
    let result = install(requests.client(zip), bundle.clone(), ZIP_URL, sha256);
    assert!(result.is_err(), "the release is refused");
    assert_eq!(
        records_at(log::Level::Warn).len(),
        1,
        "{:?}",
        records_at(log::Level::Warn)
    );
    assert!(
        records_at(log::Level::Warn)[0].contains(reason),
        "the WARN says {reason:?}: {:?}",
        records_at(log::Level::Warn)
    );
    assert!(records_at(log::Level::Error).is_empty());
    assert_eq!(snapshot(&bundle), before, "the running bundle is untouched");
    assert_eq!(entries(temp.path()), vec![format!("{NAME}.app")]);
}

#[test]
fn a_good_release_takes_the_path_and_nothing_is_left_beside_it() {
    let _lock = lock();
    let temp = tempfile::tempdir().unwrap();
    let bundle = make_bundle(temp.path(), "0.1.0", "the running version");
    let release = tempfile::tempdir().unwrap();
    let (zip, sha256) = good_release(release.path());
    let requests = Requests::default();
    install(requests.client(zip), bundle.clone(), ZIP_URL, &sha256).unwrap();
    assert_eq!(version_of(&bundle), "0.2.0");
    assert!(
        std::fs::read_to_string(bundle.join("Contents/MacOS").join(NAME))
            .unwrap()
            .contains("version 0.2.0")
    );
    assert_eq!(
        entries(temp.path()),
        vec![format!("{NAME}.app")],
        "no `.old`, no temporary folder"
    );
    assert!(records_at(log::Level::Warn).is_empty());
}

#[test]
fn a_failing_exchange_leaves_the_running_bundle_untouched() {
    let _lock = lock();
    let temp = tempfile::tempdir().unwrap();
    let bundle = make_bundle(temp.path(), "0.1.0", "the running version");
    let release = tempfile::tempdir().unwrap();
    let (zip, sha256) = good_release(release.path());
    let before = snapshot(&bundle);
    // An immutable running bundle cannot be moved: every check passes and
    // the exchange itself fails.
    run(Command::new("chflags").arg("uchg").arg(&bundle));
    let requests = Requests::default();
    let result = install(requests.client(zip), bundle.clone(), ZIP_URL, &sha256);
    run(Command::new("chflags").arg("nouchg").arg(&bundle));
    let error = result.expect_err("the exchange fails");
    assert!(error.contains("exchanging"), "{error}");
    assert_eq!(records_at(log::Level::Warn).len(), 1);
    assert!(records_at(log::Level::Error).is_empty());
    assert_eq!(snapshot(&bundle), before, "the running bundle is untouched");
    assert_eq!(entries(temp.path()), vec![format!("{NAME}.app")]);
}

#[test]
fn a_zip_with_a_parent_component_is_refused() {
    let _lock = lock();
    let release = tempfile::tempdir().unwrap();
    good_release(release.path());
    let (zip, sha256) = add_entry(&release.path().join("release.zip"), "../x");
    refused(zip, &sha256, "`..`");
}

#[test]
fn a_zip_with_an_absolute_path_is_refused() {
    let _lock = lock();
    let release = tempfile::tempdir().unwrap();
    good_release(release.path());
    let (zip, sha256) = add_entry(&release.path().join("release.zip"), "/tmp/x");
    refused(zip, &sha256, "absolute path");
}

#[test]
fn a_zip_with_an_extra_file_beside_the_app_is_refused() {
    let _lock = lock();
    let release = tempfile::tempdir().unwrap();
    let folder = release.path().join("folder");
    std::fs::create_dir_all(&folder).unwrap();
    make_bundle(&folder, "0.2.0", "version 0.2.0");
    std::fs::write(folder.join("extra.txt"), "beside the app\n").unwrap();
    // Without `--keepParent` the entries are `yard-check.app/…` and
    // `extra.txt`.
    let zip = release.path().join("release.zip");
    run(Command::new("ditto")
        .args(["-c", "-k"])
        .arg(&folder)
        .arg(&zip));
    refused(
        std::fs::read(&zip).unwrap(),
        &sha256(&zip),
        "\"extra.txt\", outside",
    );
}

#[test]
fn a_symbolic_link_outside_the_bundle_is_refused() {
    let _lock = lock();
    let release = tempfile::tempdir().unwrap();
    let app = make_bundle(release.path(), "0.2.0", "version 0.2.0");
    std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    std::os::unix::fs::symlink("/tmp", app.join("Contents/Resources/outside")).unwrap();
    let (zip, sha256) = zip_app(&app, &release.path().join("release.zip"));
    refused(zip, &sha256, "points outside the bundle");
}

#[test]
fn an_http_address_to_another_host_is_refused_without_a_request() {
    let _lock = lock();
    let temp = tempfile::tempdir().unwrap();
    let bundle = make_bundle(temp.path(), "0.1.0", "the running version");
    let before = snapshot(&bundle);
    let requests = Requests::default();
    let result = install(
        requests.client(Vec::new()),
        bundle.clone(),
        "http://example.com/x.zip",
        "00",
    );
    assert!(result.is_err());
    assert!(
        requests.all().is_empty(),
        "no request: {:?}",
        requests.all()
    );
    assert_eq!(records_at(log::Level::Warn).len(), 1);
    assert!(records_at(log::Level::Error).is_empty());
    assert_eq!(snapshot(&bundle), before);
    assert_eq!(entries(temp.path()), vec![format!("{NAME}.app")]);
}

#[test]
fn a_bundle_on_disk_as_new_as_the_release_is_not_replaced() {
    let _lock = lock();
    for on_disk in ["0.2.0", "0.3.0"] {
        RECORDS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        let temp = tempfile::tempdir().unwrap();
        let bundle = make_bundle(temp.path(), on_disk, "already replaced");
        let before = snapshot(&bundle);
        let release = tempfile::tempdir().unwrap();
        let (zip, sha256) = good_release(release.path());
        let requests = Requests::default();
        let result = install(requests.client(zip), bundle.clone(), ZIP_URL, &sha256);
        assert!(result.is_err(), "{on_disk}: not replaced");
        assert!(requests.all().is_empty(), "{on_disk}: nothing downloaded");
        assert!(
            records_at(log::Level::Warn).is_empty(),
            "{on_disk}: an expected refusal, not a WARN"
        );
        assert_eq!(snapshot(&bundle), before);
        assert_eq!(entries(temp.path()), vec![format!("{NAME}.app")]);
    }
}

// `start` reaches the install only from inside a bundle: this test's own
// binary, copied into a fake `yard-check.app`, runs the inner test there.
#[test]
fn through_start_a_stalled_download_is_given_up_after_ten_minutes() {
    let _lock = lock();
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join(format!("{NAME}.app"));
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        info_plist(IDENTIFIER, "0.1.0", NAME),
    )
    .unwrap();
    let executable = macos.join(NAME);
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    sign(&app);
    let status = Command::new(&executable)
        .args([
            "--exact",
            "inside_a_bundle_a_stalled_download_is_given_up_after_ten_minutes",
            "--nocapture",
        ])
        .env(INSIDE_BUNDLE, "1")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "the run inside the bundle failed: {status}"
    );
}

#[gpui::test]
fn inside_a_bundle_a_stalled_download_is_given_up_after_ten_minutes(cx: &mut TestAppContext) {
    if std::env::var_os(INSIDE_BUNDLE).is_none() {
        return;
    }
    let _lock = lock();
    cx.update(yard_ui::init);
    let extension = cx.update(|cx| {
        yard_shell::bind_default_keys(cx);
        Extension::new(cx, "update").unwrap()
    });
    let window = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
            .unwrap()
    });
    cx.simulate_window_resize(window.into(), gpui::size(gpui::px(800.), gpui::px(600.)));
    let bundle = yard_update::running_bundle().unwrap();
    let before = snapshot(&bundle);
    let release = format!(
        "{{\"version\":\"0.2.0\",\"url\":\"{ZIP_URL}\",\"sha256\":\"{}\"}}",
        "00".repeat(32)
    );
    // The release file answers; the zip's answer never comes.
    let client = FakeHttpClient::create(move |request| {
        let uri = request.uri().to_string();
        let release = release.clone();
        async move {
            if uri != URL {
                futures::future::pending::<()>().await;
            }
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(release))
                .unwrap())
        }
    });
    let product = Product::new(NAME, "0.1.0", IDENTIFIER);
    cx.update(move |cx| yard_update::start(cx, &extension, product, URL, client));
    cx.run_until_parked();
    cx.executor().advance_clock(FIRST_CHECK);
    cx.run_until_parked();
    cx.executor()
        .advance_clock(DOWNLOAD_LIMIT - Duration::from_secs(60));
    cx.run_until_parked();
    assert!(
        records_at(log::Level::Warn).is_empty(),
        "still downloading at nine minutes"
    );
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    let warnings = records_at(log::Level::Warn);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("10 minutes"), "{warnings:?}");
    assert!(records_at(log::Level::Error).is_empty());
    assert_eq!(snapshot(&bundle), before, "the running bundle is untouched");
    assert_eq!(
        entries(bundle.parent().unwrap()),
        vec![format!("{NAME}.app")],
        "the temporary folder is removed"
    );
}
