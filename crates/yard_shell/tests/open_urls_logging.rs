use gpui::{App, TestAppContext};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, Mutex},
};
use yard_shell as shell;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs, with the records' groups.
const PROBE: &str = "open_urls_probe";

static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

// The one process-wide diagnostic logger this file installs once; alone
// here, it cannot collide with another test's logger.
struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell")
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
    span_stacks: Vec<Vec<String>>,
    // The stack of entered groups for each line pushed into `lines`.
    event_stacks: Vec<Vec<String>>,
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
        let label = format!("{}{}", attributes.metadata().name(), fields.0);
        if attributes.metadata().target().starts_with("yard_shell") {
            let mut stack: Vec<String> = captured
                .entered
                .iter()
                .map(|entered| captured.spans[entered].clone())
                .collect();
            stack.push(label.clone());
            captured.span_stacks.push(stack);
        }
        captured.spans.insert(id, label);
        tracing::span::Id::from_u64(id)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let target = event.metadata().target();
        if !(target.starts_with("yard_shell") || target == PROBE)
            || *event.metadata().level() == tracing::Level::TRACE
        {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let stack: Vec<String> = captured
            .entered
            .iter()
            .map(|id| captured.spans[id].clone())
            .collect();
        let group = stack.last().cloned().unwrap_or_else(|| "none".to_string());
        let line = format!("{group}: {}{}", event.metadata().level(), fields.0);
        captured.lines.push(line);
        captured.event_stacks.push(stack);
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
    fn stack_mark(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .span_stacks
            .len()
    }
    fn stacks_since(&self, mark: usize) -> Vec<Vec<String>> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .span_stacks[mark..]
            .iter()
            .map(|stack| stack.iter().map(|label| label.replace('"', "")).collect())
            .collect()
    }
    // `(entered groups, line)` for each event since `mark`.
    fn events_since(&self, mark: usize) -> Vec<(Vec<String>, String)> {
        let captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        captured.event_stacks[mark..]
            .iter()
            .zip(captured.lines[mark..].iter())
            .map(|(stack, line)| {
                (
                    stack
                        .iter()
                        .map(|label| label.replace('"', ""))
                        .collect::<Vec<_>>(),
                    line.replace('"', ""),
                )
            })
            .collect()
    }
}

// `(extension, url)` for every handler call, so the order the URLs reach the
// extensions in can be checked.
type Received = Rc<RefCell<Vec<(String, String)>>>;

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

// A `url-open{scheme, host}` group opened immediately inside `urls-open{count}`.
fn url_open_in(stack: &[String], count: usize, scheme: &str, host: &str) -> bool {
    stack.windows(2).any(|pair| {
        pair[0].starts_with("urls-open")
            && pair[0].contains(&format!("count={count}"))
            && pair[1].starts_with("url-open")
            && pair[1].contains(&format!("scheme={scheme}"))
            && host_is(&pair[1], host)
    })
}

// The group names `host`, or, when `host` is empty, no host at all.
fn host_is(label: &str, host: &str) -> bool {
    if host.is_empty() {
        !label.contains("host=")
    } else {
        label.contains(&format!("host={host}"))
    }
}

// The event was recorded while `url-open{scheme, host}` was entered.
fn in_url_open(stack: &[String], scheme: &str, host: &str) -> bool {
    stack.iter().any(|label| {
        label.starts_with("url-open")
            && label.contains(&format!("scheme={scheme}"))
            && host_is(label, host)
    })
}

// `debug` `handled` in `url-open`, after the handler's own work there.
fn handled_after_work(events: &[(Vec<String>, String)], scheme: &str, host: &str) -> bool {
    let work = events.iter().position(|(stack, line)| {
        in_url_open(stack, scheme, host) && line.contains(&format!("{host} handler ran"))
    });
    let handled = events.iter().position(|(stack, line)| {
        in_url_open(stack, scheme, host) && line.contains("DEBUG") && line.contains("handled")
    });
    matches!((work, handled), (Some(work), Some(handled)) if work < handled)
}

// `INFO` `refused {reason}` inside `url-open{scheme, host}`.
fn refused(events: &[(Vec<String>, String)], scheme: &str, host: &str, reason: &str) -> bool {
    events.iter().any(|(stack, line)| {
        in_url_open(stack, scheme, host)
            && line.contains(": INFO")
            && line.contains("refused")
            && line.contains(reason)
    })
}

// Alone in this binary: the contract's logging for a URL's trip through the
// shell. `urls-open{count}` holds a `url-open{scheme, host}` per URL; a URL
// given early is `kept until extensions load` there, and they are `handed
// over {count}` in order; `handled` is in `url-open`, around the handler's
// own work; a refusal is an `INFO` `refused {reason}`; and no line holds a
// URL's path or query.
#[gpui::test]
fn open_urls_logging_covers_the_contract_rows(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    let received = Received::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let app = cx.new_app();
        app.update(|cx| {
            yard_ui::init(cx);
            shell::set_url_scheme(cx, "test");
        });
        let got = received.clone();
        let _extensions = app.update(move |cx| {
            ["tour", "guest"]
                .into_iter()
                .map(|name| {
                    let host = shell::Extension::new(cx, name).unwrap();
                    let got = got.clone();
                    host.on_open_url(cx, move |url: &str, _: &mut App| {
                        // The handler's own work, inside `url-open`.
                        tracing::debug!(target: PROBE, "{} handler ran", name);
                        got.borrow_mut().push((name.to_string(), url.to_string()));
                    });
                    host
                })
                .collect::<Vec<shell::Extension>>()
        });

        // Two URLs arrive before `extensions_loaded`: one `urls-open{count}`
        // group holds a `url-open{scheme, host}` group for each, where the
        // URL is `kept until extensions load`.
        let mark = trace.mark();
        let stacks_mark = trace.stack_mark();
        send_urls(
            &app,
            &[
                "test://tour/hello?token=secret",
                "test://guest/world?qq=zz9",
            ],
        );
        let stacks = trace.stacks_since(stacks_mark);
        for host in ["tour", "guest"] {
            assert!(
                stacks
                    .iter()
                    .any(|stack| url_open_in(stack, 2, "test", host)),
                "no `url-open{{scheme=test host={host}}}` inside `urls-open{{count=2}}`: {stacks:#?}"
            );
        }
        let events = trace.events_since(mark);
        for host in ["tour", "guest"] {
            assert!(
                events.iter().any(|(stack, line)| {
                    line.contains("DEBUG")
                        && line.contains("kept until extensions load")
                        && in_url_open(stack, "test", host)
                }),
                "no `kept until extensions load` inside `url-open{{scheme=test host={host}}}`: {events:#?}"
            );
        }
        assert!(
            received.borrow().is_empty(),
            "a URL given before `extensions_loaded` was not kept"
        );

        // At `extensions_loaded` they are handed over in order, and each is
        // `handled` in its `url-open`, around the handler's own work.
        let mark = trace.mark();
        loaded(&app);
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.contains("DEBUG") && line.contains("handed over") && line.contains("count=2")
            }),
            "missing `handed over {{count=2}}`: {lines:#?}"
        );
        assert_eq!(
            received
                .borrow()
                .iter()
                .map(|(extension, url)| (extension.as_str(), url.as_str()))
                .collect::<Vec<_>>(),
            [
                ("tour", "test://tour/hello?token=secret"),
                ("guest", "test://guest/world?qq=zz9"),
            ],
            "the kept URLs were not handed over in order"
        );
        let events = trace.events_since(mark);
        for host in ["tour", "guest"] {
            assert!(
                handled_after_work(&events, "test", host),
                "missing `handled` around the handler's work inside `url-open{{scheme=test host={host}}}`: {events:#?}"
            );
        }

        // After they are loaded, a batch routes at once: the URL whose host
        // names a loaded extension is `handled`; another scheme and a host
        // that is not a loaded extension are `refused {reason}`, each under
        // its `url-open{scheme, host}`.
        let mark = trace.mark();
        let stacks_mark = trace.stack_mark();
        send_urls(
            &app,
            &[
                "test://tour/again?token=secret",
                "https://tour/mixed?token=secret",
                "test://secret-token/y?token=secret",
            ],
        );
        let stacks = trace.stacks_since(stacks_mark);
        // A host that names no extension is left out of the group.
        for (scheme, host) in [("test", "tour"), ("https", "tour"), ("test", "")] {
            assert!(
                stacks
                    .iter()
                    .any(|stack| url_open_in(stack, 3, scheme, host)),
                "no `url-open{{scheme={scheme} host={host}}}` inside `urls-open{{count=3}}`: {stacks:#?}"
            );
        }
        let events = trace.events_since(mark);
        assert!(
            events.iter().any(|(stack, line)| {
                line.contains("DEBUG")
                    && line.contains("handled")
                    && in_url_open(stack, "test", "tour")
            }),
            "missing `handled` inside `url-open{{scheme=test host=tour}}`: {events:#?}"
        );
        for (scheme, host, reason) in [
            ("https", "tour", "reason="),
            ("test", "", "reason=no such extension"),
        ] {
            assert!(
                refused(&events, scheme, host, reason),
                "missing `INFO` `refused {{{reason}}}` inside `url-open{{scheme={scheme} host={host}}}`: {events:#?}"
            );
        }
        assert_eq!(
            received
                .borrow()
                .iter()
                .map(|(extension, url)| (extension.as_str(), url.as_str()))
                .collect::<Vec<_>>(),
            [
                ("tour", "test://tour/hello?token=secret"),
                ("guest", "test://guest/world?qq=zz9"),
                ("tour", "test://tour/again?token=secret"),
            ],
            "a refused URL reached a handler"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });

    // Privacy: the logs name the scheme and the host only, never the path or
    // the query.
    let captured = trace.0.lock().unwrap_or_else(|error| error.into_inner());
    let texts: Vec<&str> = captured
        .lines
        .iter()
        .chain(captured.spans.values())
        .chain(captured.span_stacks.iter().flatten())
        .chain(captured.event_stacks.iter().flatten())
        .map(String::as_str)
        .collect();
    for needle in [
        "hello",
        "world",
        "again",
        "mixed",
        "token",
        "secret",
        "qq",
        "zz9",
        "secret-token",
    ] {
        assert!(
            texts.iter().all(|text| !text.contains(needle)),
            "a log line or group holds a URL's path or query: {needle}"
        );
    }
    assert!(captured.entered.is_empty());
}
