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
// The path and query make it obvious when a log leaks them: only the host may
// appear in the logs.
const ZIP_URL: &str = "https://releases.example.com/download/yard-check-0.2.0.zip?channel=alpha";
const WRONG_SHA256: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const FIRST_CHECK: Duration = Duration::from_secs(5);
const NEXT_CHECK: Duration = Duration::from_secs(24 * 60 * 60);
// Tells `inside_a_bundle_start_installs_the_release_and_downloads_it_once`
// that it runs inside the bundle its parent test built.
const INSIDE_BUNDLE: &str = "YARD_UPDATE_INSTALL_INSIDE_BUNDLE";

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
    // A panicked test poisons the shared state; recover so the next test
    // still reports its own verdict.
    let guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    RECORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
    guard
}

fn records() -> Vec<(log::Level, String)> {
    RECORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn warnings() -> Vec<String> {
    records()
        .into_iter()
        .filter(|(level, _)| *level == log::Level::Warn)
        .map(|(_, message)| message)
        .collect()
}

fn errors() -> Vec<String> {
    records()
        .into_iter()
        .filter(|(level, _)| *level == log::Level::Error)
        .map(|(_, message)| message)
        .collect()
}

// The product the check and the install run against: `yard-check` 0.1.0.
fn product() -> Product {
    Product::new(NAME, "0.1.0", IDENTIFIER)
}

// The release file `start` reads: `url` and `sha256` name the zip to download.
fn release(version: &str, sha256: &str) -> String {
    format!("{{\"version\":\"{version}\",\"url\":\"{ZIP_URL}\",\"sha256\":\"{sha256}\"}}")
}

// A minimal `Info.plist` holding the keys an install checks.
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

// `key`'s `<string>` value in `<app>/Contents/Info.plist`.
fn plist_value(app: &Path, key: &str) -> Option<String> {
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).ok()?;
    let key = format!("<key>{key}</key>");
    let after = plist.split(key.as_str()).nth(1)?;
    let start = after.find("<string>")? + "<string>".len();
    let end = start + after[start..].find("</string>")?;
    Some(after[start..end].to_owned())
}

// `command` run to success.
fn run(command: &mut Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?} failed: {status}");
}

// Builds `<dir>/<name>.app`: the `Info.plist`, an executable script in
// `Contents/MacOS/` holding `marker` so an installed bundle can be told
// apart from the one it replaced, signed ad hoc as `cargo xtask bundle macos`.
fn make_bundle(dir: &Path, name: &str, identifier: &str, version: &str, marker: &str) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        info_plist(identifier, version, name),
    )
    .unwrap();
    let executable = macos.join(name);
    std::fs::write(&executable, format!("#!/bin/sh\n# {marker}\nexit 0\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    run(Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(&app));
    app
}

// `ditto -c -k --keepParent` of `source` into `zip`.
fn ditto_zip(source: &Path, zip: &Path) {
    run(Command::new("ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(source)
        .arg(zip));
}

// `shasum -a 256` of `file`, the digest only.
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

// A release for `version`: `<name>.app` built in `dir`, zipped as
// `ditto -c -k --keepParent` makes it; the zip's bytes and its digest.
fn release_zip(dir: &Path, identifier: &str, version: &str) -> (Vec<u8>, String) {
    let app = make_bundle(
        dir,
        NAME,
        identifier,
        version,
        &format!("version {version}"),
    );
    let zip = dir.join(format!("{NAME}-{version}.zip"));
    ditto_zip(&app, &zip);
    (std::fs::read(&zip).unwrap(), sha256(&zip))
}

// Everything under `dir`: relative path to contents and mode, so "the
// running bundle untouched" can be checked byte for byte.
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
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).unwrap();
            entries.insert(relative, (target.into_os_string().into_encoded_bytes(), 0));
        } else if file_type.is_dir() {
            entries.insert(relative.clone(), (Vec::new(), mode(&path)));
            collect(root, &path, entries);
        } else {
            entries.insert(relative, (std::fs::read(&path).unwrap(), mode(&path)));
        }
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// The names directly inside `dir`, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

// `yard_update::install` for `bundle`, with the product's name and
// identifier and the release's `version` and `sha256`, from `ZIP_URL`.
fn install(
    client: Arc<dyn http_client::HttpClient>,
    bundle: PathBuf,
    version: &str,
    sha256: &str,
) -> Result<(), String> {
    futures::executor::block_on(yard_update::install(
        client, bundle, NAME, IDENTIFIER, version, ZIP_URL, sha256,
    ))
}

// A release `install` must refuse: `Err`, one `WARN`, never an `ERROR`, and
// the bundle and its folder byte for byte as they were.
fn refused(client: Arc<dyn http_client::HttpClient>, sha256: &str) {
    let temp = tempfile::tempdir().unwrap();
    let bundle = make_bundle(
        temp.path(),
        NAME,
        IDENTIFIER,
        "0.1.0",
        "the running version",
    );
    let before = snapshot(&bundle);
    let result = install(client, bundle.clone(), "0.2.0", sha256);
    assert!(result.is_err(), "the release is refused");
    assert_eq!(
        warnings().len(),
        1,
        "a failure warns once: {:?}",
        warnings()
    );
    assert!(errors().is_empty(), "never an ERROR: {:?}", errors());
    assert!(
        records()
            .iter()
            .all(|(_, message)| !message.contains("channel") && !message.contains("download/")),
        "the release URL's path and query are never exposed: {:?}",
        records()
    );
    assert_eq!(snapshot(&bundle), before, "the bundle is untouched");
    assert_eq!(
        entries(temp.path()),
        vec![format!("{NAME}.app")],
        "the temporary folder is removed"
    );
}

// The addresses the fake clients have been asked for, in order.
#[derive(Clone, Default)]
struct Requests(Arc<Mutex<Vec<String>>>);

impl Requests {
    // A client that answers every request with `body`.
    fn client(&self, body: Vec<u8>) -> Arc<dyn http_client::HttpClient> {
        let requests = self.clone();
        FakeHttpClient::create(move |request| {
            requests.push(request.uri().to_string());
            let body = body.clone();
            async move {
                Ok(Response::builder()
                    .status(200)
                    .body(AsyncBody::from(body))
                    .unwrap())
            }
        })
    }

    // A client that answers the release file at `URL` with `json` and every
    // other request with `zip`.
    fn client_with_release(&self, json: String, zip: Vec<u8>) -> Arc<dyn http_client::HttpClient> {
        let requests = self.clone();
        FakeHttpClient::create(move |request| {
            let uri = request.uri().to_string();
            requests.push(uri.clone());
            let json = json.clone();
            let zip = zip.clone();
            async move {
                let body = if uri == URL {
                    AsyncBody::from(json)
                } else {
                    AsyncBody::from(zip)
                };
                Ok(Response::builder().status(200).body(body).unwrap())
            }
        })
    }

    fn push(&self, uri: String) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(uri);
    }

    fn count(&self, uri: &str) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|asked| asked.as_str() == uri)
            .count()
    }
}

// Opens an 800×600 dev-shell window, and the `update` extension handle the
// startup gives the check.
fn open_app(cx: &mut TestAppContext) -> (Extension, gpui::WindowHandle<yard_shell::Shell>) {
    cx.update(yard_ui::init);
    let extension = cx.update(|cx| {
        // The startup binds the default keys in `app-init`, before extensions run.
        yard_shell::bind_default_keys(cx);
        Extension::new(cx, "update").unwrap()
    });
    let window = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
            .unwrap()
    });
    cx.simulate_window_resize(window.into(), gpui::size(gpui::px(800.), gpui::px(600.)));
    (extension, window)
}

fn draw(window: gpui::WindowHandle<yard_shell::Shell>, cx: &mut TestAppContext) {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .unwrap();
}

fn has_toast(window: gpui::WindowHandle<yard_shell::Shell>, cx: &mut TestAppContext) -> bool {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| yard_shell::has_toast(window, cx))
        .unwrap()
}

// Advances the test clock by `duration` and runs everything it unblocks. The
// first park lets a just-spawned timer register its deadline before the clock
// moves.
fn advance(cx: &mut TestAppContext, duration: Duration) {
    cx.run_until_parked();
    cx.executor().advance_clock(duration);
    cx.run_until_parked();
}

// Runs the foreground until `done` or about ten seconds pass, so that the
// real work a download and an install do on other threads can finish.
fn wait_until(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) -> bool {
    for _ in 0..200 {
        cx.run_until_parked();
        if done(cx) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

// Lets the foreground and the work it may have spawned run a little, so that
// a download starting now would be seen.
fn settle(cx: &TestAppContext) {
    for _ in 0..50 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_good_release_is_installed_in_place_and_no_old_bundle_is_left() {
    let _lock = lock();
    let temp = tempfile::tempdir().unwrap();
    let bundle = make_bundle(
        temp.path(),
        NAME,
        IDENTIFIER,
        "0.1.0",
        "the running version",
    );
    let release_dir = tempfile::tempdir().unwrap();
    let (zip, sha256) = release_zip(release_dir.path(), IDENTIFIER, "0.2.0");
    let requests = Requests::default();
    let installed = install(requests.client(zip), bundle.clone(), "0.2.0", &sha256);
    assert!(installed.is_ok(), "the release installs: {installed:?}");
    assert_eq!(
        entries(temp.path()),
        vec![format!("{NAME}.app")],
        "the new bundle is in place and nothing else is left"
    );
    assert_eq!(
        plist_value(&bundle, "CFBundleShortVersionString").as_deref(),
        Some("0.2.0"),
        "the installed bundle is the release's"
    );
    assert!(
        std::fs::read_to_string(bundle.join("Contents/MacOS").join(NAME))
            .unwrap()
            .contains("version 0.2.0"),
        "the installed executable is the release's"
    );
    assert_eq!(
        requests.count(ZIP_URL),
        1,
        "the release was downloaded once"
    );
    assert!(warnings().is_empty(), "{:?}", warnings());
    assert!(errors().is_empty(), "{:?}", errors());
    assert!(
        records()
            .iter()
            .all(|(_, message)| !message.contains("channel") && !message.contains("download/")),
        "the release URL's path and query are never exposed: {:?}",
        records()
    );
}

#[test]
fn a_wrong_digest_leaves_the_running_bundle_untouched() {
    let _lock = lock();
    let release_dir = tempfile::tempdir().unwrap();
    let (zip, _) = release_zip(release_dir.path(), IDENTIFIER, "0.2.0");
    let requests = Requests::default();
    refused(requests.client(zip), WRONG_SHA256);
}

#[test]
fn a_zip_with_two_apps_leaves_the_running_bundle_untouched() {
    let _lock = lock();
    let release_dir = tempfile::tempdir().unwrap();
    let apps = release_dir.path().join("apps");
    make_bundle(&apps, NAME, IDENTIFIER, "0.2.0", "one app");
    make_bundle(&apps, "other", "com.example.other", "0.2.0", "another app");
    let zip = release_dir.path().join("two-apps.zip");
    ditto_zip(&apps, &zip);
    let requests = Requests::default();
    refused(requests.client(std::fs::read(&zip).unwrap()), &sha256(&zip));
}

#[test]
fn a_zip_with_no_app_leaves_the_running_bundle_untouched() {
    let _lock = lock();
    let release_dir = tempfile::tempdir().unwrap();
    let files = release_dir.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("readme.txt"), "there is no application here\n").unwrap();
    let zip = release_dir.path().join("no-app.zip");
    ditto_zip(&files, &zip);
    let requests = Requests::default();
    refused(requests.client(std::fs::read(&zip).unwrap()), &sha256(&zip));
}

#[test]
fn a_release_with_another_identifier_leaves_the_running_bundle_untouched() {
    let _lock = lock();
    let release_dir = tempfile::tempdir().unwrap();
    let (zip, sha256) = release_zip(release_dir.path(), "com.example.other", "0.2.0");
    let requests = Requests::default();
    refused(requests.client(zip), &sha256);
}

#[test]
fn a_release_with_another_version_leaves_the_running_bundle_untouched() {
    let _lock = lock();
    let release_dir = tempfile::tempdir().unwrap();
    // The release says 0.2.0, the bundle inside the zip says 0.3.0.
    let (zip, sha256) = release_zip(release_dir.path(), IDENTIFIER, "0.3.0");
    let requests = Requests::default();
    refused(requests.client(zip), &sha256);
}

#[test]
fn an_unsigned_release_leaves_the_running_bundle_untouched() {
    let _lock = lock();
    let release_dir = tempfile::tempdir().unwrap();
    let app = make_bundle(release_dir.path(), NAME, IDENTIFIER, "0.2.0", "unsigned");
    std::fs::remove_dir_all(app.join("Contents/_CodeSignature")).unwrap();
    let zip = release_dir.path().join("unsigned.zip");
    ditto_zip(&app, &zip);
    let requests = Requests::default();
    refused(requests.client(std::fs::read(&zip).unwrap()), &sha256(&zip));
}

#[test]
fn a_bundle_in_a_read_only_folder_is_an_error() {
    let _lock = lock();
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("folder");
    std::fs::create_dir_all(&folder).unwrap();
    let bundle = make_bundle(&folder, NAME, IDENTIFIER, "0.1.0", "the running version");
    let before = snapshot(&bundle);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let release_dir = tempfile::tempdir().unwrap();
    let (zip, sha256) = release_zip(release_dir.path(), IDENTIFIER, "0.2.0");
    let requests = Requests::default();
    let result = install(requests.client(zip), bundle.clone(), "0.2.0", &sha256);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        result.is_err(),
        "a bundle in a read-only folder cannot be replaced"
    );
    assert_eq!(snapshot(&bundle), before, "the bundle is untouched");
    assert_eq!(
        entries(&folder),
        vec![format!("{NAME}.app")],
        "nothing beside the bundle is left"
    );
    let not_installable = records()
        .iter()
        .any(|(level, message)| *level == log::Level::Debug && message.contains("not installable"));
    assert!(
        !warnings().is_empty() || not_installable,
        "a failed install warns, a refused one says `not installable`: {:?}",
        records()
    );
    assert!(errors().is_empty(), "never an ERROR: {:?}", errors());
}

#[test]
fn running_bundle_outside_an_app_is_an_error() {
    let _lock = lock();
    let reason = yard_update::running_bundle()
        .expect_err("a test binary is not inside an .app")
        .to_string();
    assert!(!reason.is_empty(), "the error says why there is no bundle");
}

// `running_bundle` finds the bundle only from a running executable inside
// `.app/Contents/MacOS`: this test's own binary, copied into a fake
// `yard-check.app`, gives the inner test exactly that.
#[test]
fn through_start_a_newer_release_is_installed_and_downloaded_once() {
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
    run(Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(&app));
    let status = Command::new(&executable)
        .args([
            "--exact",
            "inside_a_bundle_start_installs_the_release_and_downloads_it_once",
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

// Runs inside the fake `yard-check.app` the parent test built around this
// test binary: `start` finds the running bundle, downloads the release once,
// installs it, and shows its toast with `Restart`.
#[gpui::test]
fn inside_a_bundle_start_installs_the_release_and_downloads_it_once(cx: &mut TestAppContext) {
    if std::env::var_os(INSIDE_BUNDLE).is_none() {
        return;
    }
    let _lock = lock();
    let (extension, window) = open_app(cx);
    let bundle = yard_update::running_bundle().unwrap();
    let release_dir = tempfile::tempdir().unwrap();
    let (zip, sha256) = release_zip(release_dir.path(), IDENTIFIER, "0.2.0");
    let requests = Requests::default();
    let client = requests.client_with_release(release("0.2.0", &sha256), zip);
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK);
    assert!(
        wait_until(cx, |cx| has_toast(window, cx)),
        "the toast `{NAME} 0.2.0 is installed` is shown"
    );
    assert_eq!(
        plist_value(&bundle, "CFBundleShortVersionString").as_deref(),
        Some("0.2.0"),
        "the release is installed in place of the running bundle"
    );
    assert_eq!(
        entries(bundle.parent().unwrap()),
        vec![format!("{NAME}.app")],
        "no `.old` and no temporary folder is left"
    );
    assert_eq!(
        requests.count(ZIP_URL),
        1,
        "the release was downloaded once"
    );
    // The toast offers `Restart`, which restarts the application.
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    draw(window, cx);
    let action = visual
        .debug_bounds("TOAST-ACTION")
        .expect("the toast offers `Restart`");
    let restarted = cx.expect_restart();
    visual.simulate_click(action.center(), gpui::Modifiers::none());
    assert!(
        matches!(
            cx.foreground_executor()
                .block_with_timeout(Duration::from_secs(10), restarted),
            Ok(Ok(_))
        ),
        "`Restart` restarts the application"
    );
    // The next check does not download the same version again.
    advance(cx, NEXT_CHECK);
    assert!(
        wait_until(cx, |_| requests.count(URL) == 2),
        "the next check ran"
    );
    settle(cx);
    assert_eq!(
        requests.count(ZIP_URL),
        1,
        "one download per version per run"
    );
    assert!(warnings().is_empty(), "{:?}", warnings());
    assert!(errors().is_empty(), "{:?}", errors());
}
