use gpui::{App, TestAppContext};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, Mutex, Once},
};
use yard_shell as shell;

// Every test in this file shares one process-wide diagnostic logger and runs
// under one lock, so a test never sees another test's records, and a test
// that captures the lifecycle trace never runs beside another that reaches
// the same lines.
static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());
static LOCK: Mutex<()> = Mutex::new(());
static INIT: Once = Once::new();

struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell")
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

fn lock() -> std::sync::MutexGuard<'static, ()> {
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

fn errors() -> Vec<String> {
    records()
        .into_iter()
        .filter(|(level, _)| *level == log::Level::Error)
        .map(|(_, message)| message)
        .collect()
}

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
        if !event.metadata().target().starts_with("yard_shell")
            || *event.metadata().level() == tracing::Level::TRACE
        {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let group = captured
            .entered
            .last()
            .map(|id| captured.spans[id].as_str())
            .unwrap_or("none");
        let line = format!("{group}: {}{}", event.metadata().level(), fields.0);
        captured.lines.push(line);
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

impl Trace {
    fn mark(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .lines
            .len()
    }
    fn since(&self, mark: usize) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .lines[mark..]
            .iter()
            .map(|line| line.replace('"', ""))
            .collect()
    }
}

// Every test runs under the one lock and the one subscriber, so no thread
// reaches the shell's log lines without them.
fn captured(cx: &mut TestAppContext, test: impl FnOnce(&mut TestAppContext, &Trace)) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || test(cx, &trace));
    assert!(
        trace
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entered
            .is_empty()
    );
}

// `(extension, url)` for every handler call, so the order the URLs reach the
// extensions in can be checked.
type Received = Rc<RefCell<Vec<(String, String)>>>;

// The shell is given the product's URL scheme before any URL can arrive.
fn scheme(app: &TestAppContext) {
    app.update(|cx| {
        yard_ui::init(cx);
        shell::set_url_scheme(cx, "test");
    });
}

// `tour` and `guest` are loaded extensions; each records the URLs its
// handler gets.
fn extensions(app: &TestAppContext, received: &Received) -> Vec<shell::Extension> {
    let received = received.clone();
    app.update(move |cx| {
        ["tour", "guest"]
            .into_iter()
            .map(|name| {
                let host = shell::Extension::new(cx, name).unwrap();
                let received = received.clone();
                host.on_open_url(cx, move |url: &str, _: &mut App| {
                    received
                        .borrow_mut()
                        .push((name.to_string(), url.to_string()));
                });
                host
            })
            .collect()
    })
}

// A loaded extension, before or without an `on_open_url` handler.
fn extension(app: &TestAppContext, name: &str) -> shell::Extension {
    app.update(|cx| shell::Extension::new(cx, name).unwrap())
}

// The URLs the platform gives the application, as the startup passes them.
fn send_urls(app: &TestAppContext, urls: &[&str]) {
    let urls: Vec<String> = urls.iter().map(|url| url.to_string()).collect();
    app.update(|cx| shell::open_urls(cx, urls));
    app.run_until_parked();
}

// The startup's signal that every extension is loaded.
fn loaded(app: &TestAppContext) {
    app.update(shell::extensions_loaded);
    app.run_until_parked();
}

// A URL goes to the extension its host names, whole — path and query
// included — and another extension's handler is not called for it.
#[gpui::test]
fn a_url_reaches_the_extension_its_host_names_whole(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let app = cx.new_app();
        let received = Received::default();
        scheme(&app);
        let _extensions = extensions(&app, &received);
        loaded(&app);

        send_urls(&app, &["test://tour/hello?x=1", "test://guest/world?y=2"]);
        assert_eq!(
            received
                .borrow()
                .iter()
                .map(|(extension, url)| (extension.as_str(), url.as_str()))
                .collect::<Vec<_>>(),
            [
                ("tour", "test://tour/hello?x=1"),
                ("guest", "test://guest/world?y=2"),
            ],
            "the URL did not reach its extension's handler whole"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A refused URL gets one `INFO` `refused {reason}` and nothing else: a URL
// of another scheme, a URL without a host, a URL whose host is not a loaded
// extension, a URL for an extension without a handler, and a URL that does
// not parse.
#[gpui::test]
fn each_refused_url_gets_one_refused_with_its_reason_and_nothing_else(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let app = cx.new_app();
        let received = Received::default();
        scheme(&app);
        let _extensions = extensions(&app, &received);
        let _quiet = extension(&app, "quiet");
        loaded(&app);

        for (url, reason) in [
            ("https://tour/hello", None),
            ("test:///hello", None),
            ("test://nobody/x", Some("no such extension")),
            ("test://quiet/x", None),
            ("not a url", None),
        ] {
            let mark = trace.mark();
            send_urls(&app, &[url]);
            let lines = trace.since(mark);
            let [line] = lines.as_slice() else {
                panic!("a refused URL gets `refused {{reason}}` and nothing else: {lines:#?}");
            };
            assert!(
                line.contains(": INFO") && line.contains("refused") && line.contains("reason="),
                "the refusal for {url} is not `INFO` `refused {{reason}}`: {line}"
            );
            if let Some(reason) = reason {
                assert!(
                    line.contains(reason),
                    "the reason for {url} is not {reason}: {line}"
                );
            }
        }
        assert!(
            received.borrow().is_empty(),
            "a refused URL reached a handler"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// `on_open_url` registers the handler once: a second registration is refused
// with an `ERROR`, and the first handler still gets the URLs.
#[gpui::test]
fn a_second_handler_registration_is_refused_and_the_first_still_runs(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let app = cx.new_app();
        scheme(&app);
        let host = extension(&app, "tour");
        let first = Rc::new(RefCell::new(Vec::<String>::new()));
        let second = Rc::new(RefCell::new(Vec::<String>::new()));
        app.update(|cx| {
            let got = first.clone();
            host.on_open_url(cx, move |url: &str, _: &mut App| {
                got.borrow_mut().push(url.to_string());
            });
            let got = second.clone();
            host.on_open_url(cx, move |url: &str, _: &mut App| {
                got.borrow_mut().push(url.to_string());
            });
        });
        assert_eq!(
            errors().len(),
            1,
            "the second registration was not refused with an ERROR: {:?}",
            errors()
        );

        loaded(&app);
        send_urls(&app, &["test://tour/hello?x=1"]);
        assert_eq!(
            first.borrow().clone(),
            ["test://tour/hello?x=1"],
            "the first handler did not get the URL"
        );
        assert!(
            second.borrow().is_empty(),
            "the refused second registration ran"
        );
        assert_eq!(errors().len(), 1, "{:?}", errors());
    });
}

// URLs given before `extensions_loaded` are kept — before the extension
// exists and after it does — and handed over in order once it is called.
#[gpui::test]
fn urls_given_before_extensions_loaded_are_kept_and_handed_over_in_order(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let app = cx.new_app();
        let received = Received::default();
        scheme(&app);
        let mark = trace.mark();
        send_urls(&app, &["test://tour/first"]);
        assert!(
            received.borrow().is_empty(),
            "a URL given before the extensions were loaded was not kept"
        );
        assert!(
            !trace
                .since(mark)
                .iter()
                .any(|line| line.contains("refused")),
            "a kept URL was refused before `extensions_loaded`"
        );

        let _extensions = extensions(&app, &received);
        send_urls(&app, &["test://guest/second", "test://tour/third"]);
        assert!(
            received.borrow().is_empty(),
            "a URL given before `extensions_loaded` was not kept"
        );

        loaded(&app);
        assert_eq!(
            received
                .borrow()
                .iter()
                .map(|(extension, url)| (extension.as_str(), url.as_str()))
                .collect::<Vec<_>>(),
            [
                ("tour", "test://tour/first"),
                ("guest", "test://guest/second"),
                ("tour", "test://tour/third"),
            ],
            "the kept URLs were not handed over in order"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}
