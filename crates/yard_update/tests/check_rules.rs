//! The rules that keep the check safe: a bounded answer, no redirects, the
//! host only in the logs, versions compared by precedence, and the timeout.

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
const TIMEOUT: Duration = Duration::from_secs(30);
const SHA256: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

// Every test in this file shares one process-wide diagnostic logger and runs
// under one lock, so a test never sees another test's records, and no test
// reaches the check's log lines while another one captures them.
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

/// Every group and line of the lifecycle trace, as text.
#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Vec<String>>>);

#[derive(Default)]
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        write!(self.0, " {}={value:?}", field.name()).unwrap();
    }
}

impl Trace {
    fn push(&self, line: String) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(line);
    }
    fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl tracing::Subscriber for Trace {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        self.push(format!("{}{}", attributes.metadata().name(), fields.0));
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, record: &tracing::span::Record<'_>) {
        let mut fields = Fields::default();
        record.record(&mut fields);
        self.push(fields.0);
    }
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.push(format!(
            "{} {}{}",
            event.metadata().level(),
            event.metadata().target(),
            fields.0
        ));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

// The product the check compares against: version 0.1.0.
fn product() -> Product {
    Product::new("yard-check", "0.1.0", "vn.risotech.yard")
}

fn release(version: &str) -> String {
    format!(
        "{{\"version\":\"{version}\",\"url\":\"https://releases.example.com/yard-check.tar.gz\",\"sha256\":\"{SHA256}\"}}"
    )
}

#[derive(Clone, Default)]
struct Requests(Arc<Mutex<Vec<String>>>);

impl Requests {
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

    // A client that answers each request with the next of `answers`, the
    // last one repeated.
    fn client(&self, answers: Vec<(u16, String)>) -> Arc<dyn http_client::HttpClient> {
        let requests = self.clone();
        FakeHttpClient::create(move |request| {
            let index = requests.count().min(answers.len() - 1);
            requests.push(request.uri().to_string());
            let (status, body) = answers[index].clone();
            async move {
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body))
                    .unwrap())
            }
        })
    }

    // A client whose answer never comes.
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

    // A client whose request fails with an error that quotes the address,
    // as an HTTP library's error does.
    fn failing(&self) -> Arc<dyn http_client::HttpClient> {
        let requests = self.clone();
        FakeHttpClient::create(move |request| {
            let uri = request.uri().to_string();
            requests.push(uri.clone());
            async move {
                Err(std::io::Error::other(format!(
                    "error sending request for url ({uri}): connection refused"
                ))
                .into())
            }
        })
    }
}

fn open_app(cx: &mut TestAppContext) -> (Extension, gpui::WindowHandle<yard_shell::Shell>) {
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
    (extension, window)
}

fn has_toast(window: gpui::WindowHandle<yard_shell::Shell>, cx: &mut TestAppContext) -> bool {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, cx| yard_shell::has_toast(window, cx))
        .unwrap()
}

fn advance(cx: &mut TestAppContext, duration: Duration) {
    cx.run_until_parked();
    cx.executor().advance_clock(duration);
    cx.run_until_parked();
}

/// Starts one check with `client` and runs it.
fn check_once(
    cx: &mut TestAppContext,
    client: Arc<dyn http_client::HttpClient>,
) -> gpui::WindowHandle<yard_shell::Shell> {
    let (extension, window) = open_app(cx);
    cx.update(move |cx| yard_update::start(cx, &extension, product(), URL, client));
    advance(cx, FIRST_CHECK);
    window
}

#[gpui::test]
fn an_answer_longer_than_64_kib_warns_and_shows_no_toast(cx: &mut TestAppContext) {
    let _lock = lock();
    let requests = Requests::default();
    // A valid release file padded past 64 KiB with spaces.
    let body = format!("{}{}", release("0.2.0"), " ".repeat(100 * 1024));
    let window = check_once(cx, requests.client(vec![(200, body)]));
    assert_eq!(requests.count(), 1, "the check ran");
    assert!(!has_toast(window, cx), "a too long answer shows no toast");
    assert_eq!(warnings().len(), 1, "{:?}", warnings());
    assert!(warnings()[0].contains("64 KiB"), "{:?}", warnings());
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn an_answer_of_64_kib_is_read(cx: &mut TestAppContext) {
    let _lock = lock();
    let requests = Requests::default();
    let file = release("0.2.0");
    let body = format!("{file}{}", " ".repeat(64 * 1024 - file.len()));
    let window = check_once(cx, requests.client(vec![(200, body)]));
    assert!(has_toast(window, cx), "an answer of exactly 64 KiB is read");
    assert!(warnings().is_empty(), "{:?}", warnings());
}

#[gpui::test]
fn a_redirect_is_not_followed_and_warns(cx: &mut TestAppContext) {
    let _lock = lock();
    let requests = Requests::default();
    let window = check_once(cx, requests.client(vec![(302, release("0.2.0"))]));
    assert_eq!(requests.count(), 1, "the redirect was not followed");
    assert!(!has_toast(window, cx), "a redirect shows no toast");
    assert_eq!(warnings().len(), 1, "{:?}", warnings());
    assert!(warnings()[0].contains("302"), "{:?}", warnings());
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn the_logs_name_the_host_and_never_the_path_or_query(cx: &mut TestAppContext) {
    let _lock = lock();
    let url = "http://127.0.0.1:8765/private/latest.json?token=secret";
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let (extension, _window) = open_app(cx);
        let requests = Requests::default();
        let failing = requests.failing();
        let status = requests.client(vec![(500, release("0.2.0"))]);
        let newer = requests.client(vec![(200, release("0.2.0"))]);
        cx.update(|cx| {
            for client in [failing, status, newer] {
                yard_update::start(cx, &extension, product(), url, client);
            }
        });
        advance(cx, FIRST_CHECK);
        assert_eq!(requests.count(), 3, "every check ran");
    });
    let warnings = warnings();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    for warning in &warnings {
        assert!(
            warning.contains("127.0.0.1"),
            "the host is named: {warning}"
        );
    }
    let lines = trace.lines();
    assert!(!lines.is_empty(), "the trace was captured");
    for line in records()
        .into_iter()
        .map(|(_, message)| message)
        .chain(lines)
    {
        assert!(
            !line.contains("secret") && !line.contains("private"),
            "a line names the path or query: {line}"
        );
    }
}

#[gpui::test]
fn build_metadata_does_not_make_a_version_newer(cx: &mut TestAppContext) {
    let _lock = lock();
    let requests = Requests::default();
    let window = check_once(cx, requests.client(vec![(200, release("0.1.0+build.1"))]));
    assert_eq!(requests.count(), 1, "the check ran");
    assert!(
        !has_toast(window, cx),
        "0.1.0+build.1 is not newer than 0.1.0"
    );
    assert!(warnings().is_empty(), "{:?}", warnings());
}

#[gpui::test]
fn a_pre_release_of_a_later_version_is_newer(cx: &mut TestAppContext) {
    let _lock = lock();
    let requests = Requests::default();
    let window = check_once(cx, requests.client(vec![(200, release("0.2.0-beta"))]));
    assert!(has_toast(window, cx), "0.2.0-beta is newer than 0.1.0");
    assert!(warnings().is_empty(), "{:?}", warnings());
}

#[gpui::test]
fn a_toast_is_not_shown_again_for_the_same_precedence(cx: &mut TestAppContext) {
    let _lock = lock();
    let requests = Requests::default();
    let window = check_once(
        cx,
        requests.client(vec![
            (200, release("0.2.0+build.1")),
            (200, release("0.2.0+build.2")),
        ]),
    );
    assert!(has_toast(window, cx), "the first check shows the toast");
    advance(cx, NEXT_CHECK);
    assert_eq!(requests.count(), 2, "the next check ran");
    assert!(
        !has_toast(window, cx),
        "0.2.0+build.2 is the same version as 0.2.0+build.1"
    );
}

#[gpui::test]
fn a_check_without_an_answer_times_out_after_30_seconds(cx: &mut TestAppContext) {
    let _lock = lock();
    let requests = Requests::default();
    let window = check_once(cx, requests.held());
    assert_eq!(requests.count(), 1, "the check is running");
    advance(cx, TIMEOUT - Duration::from_secs(1));
    assert!(
        warnings().is_empty(),
        "no timeout before 30 s: {:?}",
        warnings()
    );
    advance(cx, Duration::from_secs(1));
    assert_eq!(warnings().len(), 1, "{:?}", warnings());
    assert!(warnings()[0].contains("30 s"), "{:?}", warnings());
    assert!(!has_toast(window, cx), "a timeout shows no toast");
    // The next check waits 24 h from the end of the timed-out one.
    advance(cx, NEXT_CHECK - Duration::from_secs(1));
    assert_eq!(requests.count(), 1, "no next check before its time");
    advance(cx, Duration::from_secs(1));
    assert_eq!(requests.count(), 2, "the next check ran at its time");
    assert!(errors().is_empty(), "{:?}", errors());
}
