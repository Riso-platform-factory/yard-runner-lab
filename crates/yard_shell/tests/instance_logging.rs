//! The log of one instance: `instance-check` names the socket and says what
//! the start found, and `instance-request` counts the URLs of a request from
//! a later start, with refused requests as warnings.
//!
//! This file installs the process-wide diagnostic logger, and its one test
//! is alone in the binary, so no other test reaches the same lines.

use gpui::{App, TestAppContext};
use std::sync::{Arc, Mutex};
use yard_shell as shell;

static RECORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell")
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            // The diagnostic log's records show in the trace, as yard_log
            // shows them, so their group can be checked.
            tracing::event!(target: "diagnostic", tracing::Level::WARN, "{}", record.args());
            RECORDS
                .lock()
                .unwrap()
                .push(format!("{} {}", record.level(), record.args()));
        }
    }
    fn flush(&self) {}
}

#[derive(Default)]
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let value = format!("{value:?}").replace('"', "");
        let value = match field.name() {
            // The socket's folder differs from run to run.
            "socket" => value.rsplit('/').next().unwrap().to_owned(),
            // Only the start of a refusal's text, which names the error.
            "message" => value.split(" (").next().unwrap().to_owned(),
            _ => value,
        };
        write!(self.0, " {}={value}", field.name()).unwrap();
    }
}

#[derive(Default)]
struct Captured {
    spans: Vec<String>,
    entered: Vec<u64>,
    lines: Vec<String>,
}

#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Captured>>);

impl tracing::Subscriber for Trace {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell") || metadata.target() == "diagnostic"
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let mut captured = self.0.lock().unwrap();
        captured.spans.push(format!(
            "{} {}{}",
            attributes.metadata().level(),
            attributes.metadata().name(),
            fields.0
        ));
        tracing::span::Id::from_u64(captured.spans.len() as u64)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap();
        let group = captured
            .entered
            .last()
            .map(|id| captured.spans[*id as usize - 1].clone())
            .unwrap_or_else(|| "none".into());
        captured
            .lines
            .push(format!("{group}: {}{}", event.metadata().level(), fields.0));
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.0.lock().unwrap().entered.push(id.into_u64());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(self.0.lock().unwrap().entered.pop(), Some(id.into_u64()));
    }
}

impl Trace {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.0.lock().unwrap().lines)
    }
}

fn strings(arguments: &[&str]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string())
        .collect()
}

#[gpui::test]
fn one_instance_logs_what_each_start_and_request_does(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("instance.sock");
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        // A socket left by an instance that ended without removing it.
        drop(std::os::unix::net::UnixDatagram::bind(&path).unwrap());
        let first = shell::check_instance(&path, &[]);
        assert!(matches!(first, shell::Instance::First(_)));
        assert_eq!(
            trace.take(),
            [
                "INFO instance-check socket=instance.sock: DEBUG message=stale socket removed",
                "INFO instance-check socket=instance.sock: DEBUG message=listening",
            ]
        );

        let sent = shell::check_instance(&path, &strings(&["yard://tour/a"]));
        assert!(matches!(sent, shell::Instance::Sent));
        assert_eq!(
            trace.take(),
            [
                "INFO instance-check socket=instance.sock: DEBUG message=sent to the running instance bytes=15"
            ]
        );

        let refused = shell::check_instance(&path, &["a".repeat(shell::MAX_REQUEST_LEN)]);
        assert!(matches!(refused, shell::Instance::Refused(_)));
        assert_eq!(
            trace.take(),
            [
                "INFO instance-check socket=instance.sock: WARN message=the running instance was not reached: the request is 65538 bytes, more than the 65536 the running instance reads"
            ]
        );

        let long = home.path().join("a".repeat(200)).join("instance.sock");
        assert!(matches!(
            shell::check_instance(&long, &[]),
            shell::Instance::Alone
        ));
        let lines = trace.take();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(
            lines[0].starts_with(
                "INFO instance-check socket=instance.sock: WARN message=the instance socket /"
            ) && lines[0].ends_with("/instance.sock cannot be used"),
            "{lines:#?}"
        );

        cx.add_empty_window();
        let open = |cx: &mut App, request: &[u8]| {
            shell::handle_instance_request(cx, request, &|_, _| {});
        };
        let with_urls =
            shell::encode_request(&strings(&["dev", "yard://tour/a", "yard://guest/b"])).unwrap();
        cx.update(|cx| open(cx, &with_urls));
        assert_eq!(
            trace.take(),
            [
                "INFO instance-request urls=2: DEBUG message=not a URL, ignored argument=dev",
                "INFO instance-request urls=2: DEBUG message=opening the URLs",
            ]
        );

        cx.update(|cx| open(cx, &shell::encode_request(&[]).unwrap()));
        assert_eq!(
            trace.take(),
            ["INFO instance-request urls=0: DEBUG message=activated"]
        );

        cx.update(|cx| open(cx, &[shell::REQUEST_VERSION + 1]));
        assert_eq!(
            trace.take(),
            [
                "INFO instance-request: WARN message=a request from another start was refused: the request is version 2, and this build reads version 1"
            ]
        );
    });
    assert!(trace.0.lock().unwrap().entered.is_empty());
    let records = RECORDS.lock().unwrap();
    assert!(
        records.iter().all(|record| record.starts_with("WARN ")),
        "refusals are warnings, never errors: {records:#?}"
    );
    assert_eq!(records.len(), 3, "{records:#?}");
}
