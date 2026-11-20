// The check and the install run only on macOS until the release-updates slice.
#![cfg(target_os = "macos")]

use gpui::TestAppContext;
use http_client::{AsyncBody, FakeHttpClient, Response};
use std::{
    sync::{Arc, Mutex, MutexGuard, Once},
    time::Duration,
};
use yard_product::Product;
use yard_shell::Extension;

const URL: &str = "http://127.0.0.1:8765/latest.json";
const FIRST_CHECK: Duration = Duration::from_secs(5);
const NEXT_CHECK: Duration = Duration::from_secs(24 * 60 * 60);
const SHA256: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

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

// The product the check compares against: version 0.1.0.
fn product() -> Product {
    Product::new("yard-check", "0.1.0", "vn.risotech.yard")
}

// A release file for `version`, with the `url` and `sha256` fields that must
// be present even though this feature reads `version` only.
fn release(version: &str) -> String {
    format!(
        "{{\"version\":\"{version}\",\"url\":\"https://releases.example.com/yard-check-{version}.tar.gz\",\"sha256\":\"{SHA256}\"}}"
    )
}

// The addresses the fake clients have been asked for, in order.
#[derive(Clone, Default)]
struct Requests(Arc<Mutex<Vec<String>>>);

impl Requests {
    // A client that answers every request with `status` and `body`.
    fn client(&self, status: u16, body: impl Into<String>) -> Arc<dyn http_client::HttpClient> {
        let requests = self.clone();
        let body = body.into();
        FakeHttpClient::create(move |request| {
            requests.push(request.uri().to_string());
            let body = body.clone();
            async move {
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body))
                    .unwrap())
            }
        })
    }

    // A client whose answer never comes: the request stays in flight.
    fn held(&self) -> Arc<dyn http_client::HttpClient> {
        let requests = self.clone();
        FakeHttpClient::create(move |request| {
            requests.push(request.uri().to_string());
            async move {
                std::future::pending::<()>().await;
                Ok(Response::builder()
                    .status(200)
                    .body(AsyncBody::from(release("0.2.0")))
                    .unwrap())
            }
        })
    }

    fn push(&self, uri: String) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(uri);
    }

    fn count(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    fn uris(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
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

#[gpui::test]
fn a_newer_version_shows_one_toast(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, window) = open_app(cx);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let requests = Requests::default();
    let client = requests.client(200, release("0.2.0"));
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK);
    draw(window, cx);
    assert!(
        has_toast(window, cx),
        "the toast `yard-check 0.2.0 is available` is shown"
    );
    assert!(visual.debug_bounds("TOAST").is_some(), "the toast is drawn");
    assert_eq!(
        requests.uris(),
        vec![URL.to_string()],
        "the check asked the update address"
    );
    assert!(warnings().is_empty(), "{:?}", warnings());
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn the_same_and_an_older_version_show_no_toast(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, window) = open_app(cx);
    let requests = Requests::default();
    let same = requests.client(200, release("0.1.0"));
    let older = requests.client(200, release("0.0.9"));
    cx.update(move |cx| {
        yard_update::start(cx, &extension, product(), URL, same);
        yard_update::start(cx, &extension, product(), URL, older);
    });
    advance(cx, FIRST_CHECK);
    assert_eq!(requests.count(), 2, "both checks ran");
    assert!(
        !has_toast(window, cx),
        "no toast for the same or an older version"
    );
    assert!(warnings().is_empty(), "{:?}", warnings());
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_second_check_of_the_same_version_does_not_show_the_toast_again(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, window) = open_app(cx);
    let requests = Requests::default();
    let client = requests.client(200, release("0.2.0"));
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK);
    assert!(has_toast(window, cx), "the first check shows the toast");
    advance(cx, NEXT_CHECK);
    assert_eq!(requests.count(), 2, "the next check ran at 24 h");
    assert!(
        !has_toast(window, cx),
        "the toast is not shown again for the same version"
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_failure_status_warns_and_shows_no_toast(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, window) = open_app(cx);
    let requests = Requests::default();
    let client = requests.client(500, release("0.2.0"));
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK);
    assert_eq!(requests.count(), 1, "the check ran");
    assert!(!has_toast(window, cx), "a failure status shows no toast");
    assert_eq!(
        warnings().len(),
        1,
        "a failure status warns once: {:?}",
        warnings()
    );
    assert!(errors().is_empty(), "never an ERROR: {:?}", errors());
    advance(cx, NEXT_CHECK);
    assert_eq!(
        requests.count(),
        2,
        "the next check ran at its time despite the failure"
    );
    assert_eq!(
        warnings().len(),
        2,
        "the next check warns the same way: {:?}",
        warnings()
    );
}

#[gpui::test]
fn a_broken_release_file_warns_and_shows_no_toast(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, window) = open_app(cx);
    let requests = Requests::default();
    let client = requests.client(200, "this is not a release file");
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK);
    assert_eq!(requests.count(), 1, "the check ran");
    assert!(!has_toast(window, cx), "a broken file shows no toast");
    assert_eq!(
        warnings().len(),
        1,
        "a broken file warns once: {:?}",
        warnings()
    );
    assert!(errors().is_empty(), "never an ERROR: {:?}", errors());
}

#[gpui::test]
fn a_release_file_missing_url_or_sha256_warns_and_shows_no_toast(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, window) = open_app(cx);
    let requests = Requests::default();
    let missing_url = requests.client(
        200,
        format!("{{\"version\":\"0.2.0\",\"sha256\":\"{SHA256}\"}}"),
    );
    let missing_sha256 = requests.client(
        200,
        "{\"version\":\"0.2.0\",\"url\":\"https://releases.example.com/yard-check-0.2.0.tar.gz\"}",
    );
    cx.update(move |cx| {
        yard_update::start(cx, &extension, product(), URL, missing_url);
        yard_update::start(cx, &extension, product(), URL, missing_sha256);
    });
    advance(cx, FIRST_CHECK);
    assert_eq!(requests.count(), 2, "both checks ran");
    assert!(
        !has_toast(window, cx),
        "a file missing `url` or `sha256` shows no toast"
    );
    assert_eq!(
        warnings().len(),
        2,
        "each missing field warns once: {:?}",
        warnings()
    );
    assert!(errors().is_empty(), "never an ERROR: {:?}", errors());
}

#[gpui::test]
fn an_unreadable_version_warns_and_shows_no_toast(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, window) = open_app(cx);
    let requests = Requests::default();
    let client = requests.client(
        200,
        format!(
            "{{\"version\":\"latest\",\"url\":\"https://releases.example.com/yard-check-latest.tar.gz\",\"sha256\":\"{SHA256}\"}}"
        ),
    );
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK);
    assert_eq!(requests.count(), 1, "the check ran");
    assert!(
        !has_toast(window, cx),
        "an unreadable version shows no toast"
    );
    assert_eq!(
        warnings().len(),
        1,
        "an unreadable version warns once: {:?}",
        warnings()
    );
    assert!(errors().is_empty(), "never an ERROR: {:?}", errors());
}

#[gpui::test]
fn the_first_check_waits_five_seconds_and_the_next_twenty_four_hours(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, _window) = open_app(cx);
    let requests = Requests::default();
    let client = requests.client(200, release("0.1.0"));
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK - Duration::from_secs(1));
    assert_eq!(requests.count(), 0, "no check before 5 s");
    advance(cx, Duration::from_secs(1));
    assert_eq!(requests.count(), 1, "the first check ran at 5 s");
    advance(cx, NEXT_CHECK - Duration::from_secs(1));
    assert_eq!(requests.count(), 1, "no next check before 24 h");
    advance(cx, Duration::from_secs(1));
    assert_eq!(requests.count(), 2, "the next check ran at 24 h");
    assert_eq!(
        requests.uris(),
        vec![URL.to_string(), URL.to_string()],
        "every check asks the update address"
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn no_check_runs_while_one_is_running(cx: &mut TestAppContext) {
    let _lock = lock();
    let (extension, _window) = open_app(cx);
    let requests = Requests::default();
    let client = requests.held();
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK);
    assert_eq!(requests.count(), 1, "the first check is running");
    // Inside the check's own 30 s failure window no other check can start.
    advance(cx, Duration::from_secs(20));
    assert_eq!(
        requests.count(),
        1,
        "no second check runs while the first is in flight"
    );
    advance(cx, Duration::from_secs(5));
    assert_eq!(
        requests.count(),
        1,
        "the running check is still the only one"
    );
    assert!(errors().is_empty(), "{:?}", errors());
}
