//! The log of writing a file in one step: a write opens a group, and a
//! write that fails is a warning in the diagnostic log.
//!
//! This file installs a process-wide `log` logger, so it holds one test.
//!
//! `write_atomic` is not in production, so this test needs the feature `dev`.
use std::sync::{Arc, Mutex};

static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());
struct Capture;
impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        if record.target().starts_with("yard_paths") {
            RECORDS
                .lock()
                .unwrap()
                .push((record.level(), record.args().to_string()));
        }
    }
    fn flush(&self) {}
}

/// The fields of a span or an event, with paths shortened to their last
/// part, which does not differ from run to run.
#[derive(Default)]
struct Fields(String);
impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let value = format!("{value:?}");
        let value = match field.name() {
            "path" => value.rsplit(['/', '\\']).next().unwrap().to_owned(),
            _ => value,
        };
        write!(self.0, " {}={value}", field.name()).unwrap();
    }
}

/// The groups in the order they open, and under each the lines logged in it.
#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Vec<String>>>);
impl tracing::Subscriber for Trace {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let mut lines = self.0.lock().unwrap();
        lines.push(format!(
            "{} {}{}",
            attributes.metadata().level(),
            attributes.metadata().name(),
            fields.0
        ));
        tracing::span::Id::from_u64(lines.len() as u64)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.0
            .lock()
            .unwrap()
            .push(format!("  {}{}", event.metadata().level(), fields.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn a_write_opens_a_group_and_a_failed_write_is_a_warning() {
    log::set_logger(&Capture).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("data/note.json");
    // A folder where the file should be: the write fails.
    let taken = home.path().join("taken");
    std::fs::create_dir_all(&taken).unwrap();

    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        yard_paths::write_atomic(&file, b"four").unwrap();
        assert!(yard_paths::write_atomic(&taken, b"x").is_err());
    });

    let lines = trace.0.lock().unwrap();
    assert_eq!(
        *lines,
        [
            "INFO file-write path=note.json",
            "  DEBUG message=written bytes=4",
            "INFO file-write path=taken",
        ],
        "{lines:#?}"
    );
    let records = RECORDS.lock().unwrap();
    assert_eq!(records.len(), 1, "{records:#?}");
    assert_eq!(records[0].0, log::Level::Warn);
    assert!(records[0].1.starts_with("could not write "), "{records:#?}");
    assert!(records[0].1.contains("taken"), "{records:#?}");
}
