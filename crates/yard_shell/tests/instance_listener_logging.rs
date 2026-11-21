//! The log of the Windows listener's thread: a message that the running
//! instance refuses there is a warning in the group `instance-request`.
//!
//! The work runs on the listener's thread, so this file installs the
//! process-wide trace subscriber and diagnostic logger, and its one test is
//! alone in the binary.

#![cfg(windows)]

use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::{
    Win32::{
        Foundation::CloseHandle,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING,
            WriteFile,
        },
        System::{RemoteDesktop::ProcessIdToSessionId, Threading::GetCurrentProcessId},
    },
    core::HSTRING,
};
use yard_shell as shell;

/// The product's identifier in this test.
const ID: &str = "test.yard.instance-listener-logging";

static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell")
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            // As yard_log shows them, in the trace, so their group shows.
            if record.level() == log::Level::Error {
                tracing::event!(target: "diagnostic", tracing::Level::ERROR, "{}", record.args());
            } else {
                tracing::event!(target: "diagnostic", tracing::Level::WARN, "{}", record.args());
            }
        }
    }
    fn flush(&self) {}
}

#[derive(Default)]
struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

thread_local! {
    static ENTERED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct Trace {
    spans: Mutex<Vec<&'static str>>,
}

impl tracing::Subscriber for Trace {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell") || metadata.target() == "diagnostic"
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut spans = self.spans.lock().unwrap();
        spans.push(attributes.metadata().name());
        tracing::span::Id::from_u64(spans.len() as u64)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = Message::default();
        event.record(&mut message);
        let group = ENTERED
            .with(|entered| entered.borrow().last().copied())
            .map(|id| self.spans.lock().unwrap()[id as usize - 1])
            .unwrap_or("none");
        LINES.lock().unwrap().push(format!(
            "{group}: {} {}",
            event.metadata().level(),
            message.0
        ));
    }
    fn enter(&self, id: &tracing::span::Id) {
        ENTERED.with(|entered| entered.borrow_mut().push(id.into_u64()));
    }
    fn exit(&self, _: &tracing::span::Id) {
        ENTERED.with(|entered| entered.borrow_mut().pop());
    }
}

fn pipe_name(paths: &yard_paths::Paths) -> String {
    let mut session = 0;
    // SAFETY: `session` is a valid place for the id.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }.unwrap();
    format!(r"\\.\pipe\{}-{session}", shell::instance_name(paths, ID))
}

#[test]
fn a_message_refused_by_the_listener_is_a_warning_in_its_group() {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    tracing::subscriber::set_global_default(Trace {
        spans: Mutex::new(Vec::new()),
    })
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let paths = yard_paths::Paths::under(home.path());
    let shell::Instance::Listening(_listener) = shell::check_instance(&paths, ID, &[]) else {
        panic!("the first start listens");
    };

    // SAFETY: the name is a valid string, and the handle is closed below.
    let client = unsafe {
        CreateFileW(
            &HSTRING::from(pipe_name(&paths)),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .unwrap();
    let message = vec![1; shell::MAX_REQUEST_LEN + 1];
    // The listener may drop the pipe before the whole message is written.
    // SAFETY: `client` is an open pipe.
    let _ = unsafe { WriteFile(client, Some(&message), None, None) };
    // SAFETY: opened above, closed once.
    unsafe { CloseHandle(client) }.unwrap();

    let expected = "instance-request: WARN a request from another start was refused: the request is more than the 65536 bytes the running instance reads";
    let started = Instant::now();
    loop {
        let lines = LINES.lock().unwrap().clone();
        if lines.iter().any(|line| line == expected) {
            assert!(
                lines.iter().all(|line| !line.contains("ERROR")),
                "{lines:#?}"
            );
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no refusal in its group: {lines:#?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
