// The check and the install run only on macOS until the release-updates slice.
#![cfg(target_os = "macos")]

use gpui::TestAppContext;
use http_client::{AsyncBody, FakeHttpClient, HttpClient, Response};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use yard_product::Product;
use yard_shell::Extension;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs.
const PROBE: &str = "update_check_probe";

const URL: &str = "http://127.0.0.1:8765/latest.json";
const FIRST_CHECK: Duration = Duration::from_secs(5);
const NEXT_CHECK: Duration = Duration::from_secs(24 * 60 * 60);
const SHA256: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

// The one process-wide diagnostic logger this file installs once; alone in
// its binary, it cannot collide with another test's logger.
struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_update")
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        RECORDS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((record.level(), record.args().to_string()));
        match record.level() {
            log::Level::Error => tracing::error!(target: PROBE, "{}", record.args()),
            log::Level::Warn => tracing::warn!(target: PROBE, "{}", record.args()),
            log::Level::Info => tracing::info!(target: PROBE, "{}", record.args()),
            log::Level::Debug => tracing::debug!(target: PROBE, "{}", record.args()),
            log::Level::Trace => tracing::trace!(target: PROBE, "{}", record.args()),
        }
    }
    fn flush(&self) {}
}

fn errors() -> Vec<String> {
    RECORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter(|(level, _)| *level == log::Level::Error)
        .map(|(_, message)| message.clone())
        .collect()
}

// The groups the code opens and the lines it logs inside them.
#[derive(Default)]
struct Captured {
    spans: BTreeMap<u64, String>,
    entered: Vec<u64>,
    lines: Vec<String>,
}

#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Captured>>);

#[derive(Default)]
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        write!(self.0, " {}={value:?}", field.name()).unwrap();
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
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let id = captured.spans.len() as u64 + 1;
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        captured
            .spans
            .insert(id, format!("{}{}", attributes.metadata().name(), fields.0));
        tracing::span::Id::from_u64(id)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let target = event.metadata().target();
        if !(target.starts_with("yard_update") || target == PROBE) {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let stack = if captured.entered.is_empty() {
            "none".to_string()
        } else {
            captured
                .entered
                .iter()
                .map(|id| captured.spans[id].as_str())
                .collect::<Vec<_>>()
                .join(" > ")
        };
        captured
            .lines
            .push(format!("{stack}: {}{}", event.metadata().level(), fields.0));
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entered
            .push(id.into_u64());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(
            self.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entered
                .pop(),
            Some(id.into_u64())
        );
    }
}

// The spans ever created whose name starts with `prefix`, quotes stripped for
// field-value checks.
fn spans_named(trace: &Trace, prefix: &str) -> Vec<String> {
    trace
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .spans
        .values()
        .map(|span| span.replace('"', ""))
        .filter(|span| span.starts_with(prefix))
        .collect()
}

// The logged lines so far, quotes stripped.
fn lines(trace: &Trace) -> Vec<String> {
    trace
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .lines
        .iter()
        .map(|line| line.replace('"', ""))
        .collect()
}

// The groups enclosing `line`, innermost last, or "none".
fn stack_of(line: &str) -> &str {
    line.split_once(": ").map(|(stack, _)| stack).unwrap_or("")
}

// The line after its enclosing groups.
fn body_of(line: &str) -> &str {
    line.split_once(": ").map(|(_, body)| body).unwrap_or(line)
}

// Whether one of the groups enclosing `line` is `update-check` for the
// product's current version.
fn in_check_group(line: &str) -> bool {
    stack_of(line)
        .split(" > ")
        .any(|group| group.starts_with("update-check") && group.contains("current=0.1.0"))
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

// A fake client that answers every request with `status` and `body`, kept in
// its concrete type so the test can swap the answer with `replace_handler`.
fn client(status: u16, body: impl Into<String>) -> Arc<http_client::HttpClientWithUrl> {
    let body = body.into();
    FakeHttpClient::create(move |_request| {
        let body = body.clone();
        async move {
            Ok(Response::builder()
                .status(status)
                .body(AsyncBody::from(body))
                .unwrap())
        }
    })
}

// Advances the test clock by `duration` and runs everything it unblocks. The
// first park lets a just-spawned timer register its deadline before the clock
// moves.
fn advance(app: &mut TestAppContext, duration: Duration) {
    app.run_until_parked();
    app.executor().advance_clock(duration);
    app.run_until_parked();
}

// Alone in this binary: every captured callsite runs under one scoped
// subscriber, and no other test reaches the check's lines beside it.
#[gpui::test]
fn update_check_logging(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        app.update(yard_ui::init);
        let extension = app.update(|cx| {
            // The startup binds the default keys in `app-init`, before extensions run.
            yard_shell::bind_default_keys(cx);
            Extension::new(cx, "update").unwrap()
        });
        let window = app.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
                .unwrap()
        });
        app.simulate_window_resize(window.into(), gpui::size(gpui::px(800.), gpui::px(600.)));
        let client = client(200, release("0.2.0"));
        app.update(|cx| yard_update::start(cx, &extension, product(), URL, client.clone()));

        // A newer version: `update-check{current}` holding
        // `newer {version}`, and the toast's `toast shown {version}`.
        advance(&mut app, FIRST_CHECK);
        assert!(
            spans_named(&trace, "update-check")
                .iter()
                .any(|span| span.contains("current=0.1.0")),
            "the check opened `update-check{{current=0.1.0}}`: {:?}",
            spans_named(&trace, "update-check")
        );
        assert!(
            lines(&trace).iter().any(|line| {
                in_check_group(line)
                    && line.contains(": DEBUG")
                    && body_of(line).contains("newer")
                    && body_of(line).contains("version=0.2.0")
            }),
            "`newer {{version=0.2.0}}` inside `update-check{{current=0.1.0}}`: {:#?}",
            lines(&trace)
        );
        assert!(
            lines(&trace).iter().any(|line| {
                line.contains(": DEBUG")
                    && body_of(line).contains("toast shown")
                    && body_of(line).contains("version=0.2.0")
            }),
            "`toast shown {{version=0.2.0}}`: {:#?}",
            lines(&trace)
        );

        // The same version at the next check is not shown again:
        // `already shown {version}`.
        advance(&mut app, NEXT_CHECK);
        assert!(
            lines(&trace).iter().any(|line| {
                line.contains(": DEBUG")
                    && body_of(line).contains("already shown")
                    && body_of(line).contains("version=0.2.0")
            }),
            "`already shown {{version=0.2.0}}`: {:#?}",
            lines(&trace)
        );

        // An equal version: `up to date {version}` inside `update-check`.
        client.as_fake().replace_handler(|_, _| async move {
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(release("0.1.0")))
                .unwrap())
        });
        advance(&mut app, NEXT_CHECK);
        assert!(
            lines(&trace).iter().any(|line| {
                in_check_group(line)
                    && line.contains(": DEBUG")
                    && body_of(line).contains("up to date")
                    && body_of(line).contains("version=0.1.0")
            }),
            "`up to date {{version=0.1.0}}` inside `update-check{{current=0.1.0}}`: {:#?}",
            lines(&trace)
        );

        // A failure: the WARN with its reason inside `update-check`, and
        // still never an ERROR.
        client.as_fake().replace_handler(|_, _| async move {
            Ok(Response::builder()
                .status(500)
                .body(AsyncBody::from(release("0.2.0")))
                .unwrap())
        });
        advance(&mut app, NEXT_CHECK);
        assert!(
            lines(&trace)
                .iter()
                .any(|line| in_check_group(line) && line.contains(": WARN")),
            "the WARN inside `update-check{{current=0.1.0}}`: {:#?}",
            lines(&trace)
        );
        assert!(errors().is_empty(), "{:?}", errors());
        assert!(
            lines(&trace).iter().all(|line| !line.contains(": ERROR")),
            "never an ERROR: {:#?}",
            lines(&trace)
        );
    });
    assert!(
        trace
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entered
            .is_empty(),
        "every group closed"
    );
}
