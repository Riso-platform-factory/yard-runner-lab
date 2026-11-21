use gpui::TestAppContext;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use yard_shell as shell;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs, with the records' groups.
const PROBE: &str = "extension_store_probe";

static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

// The one process-wide diagnostic logger this file installs once; alone
// here, it cannot collide with another test's logger.
struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell")
            || metadata.target().starts_with("yard_state")
            || metadata.target().starts_with("yard_paths")
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

// The groups the code opens and the lines it logs: a span stack records the
// groups a yard_* group was opened inside, so `file-write` is checked
// inside `store-save`, not merely beside it.
#[derive(Default)]
struct Captured {
    spans: BTreeMap<u64, String>,
    entered: Vec<u64>,
    lines: Vec<String>,
    span_stacks: Vec<Vec<String>>,
}

#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Captured>>);

#[derive(Default)]
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let rendered = format!("{value:?}");
        let rendered = match field.name() {
            "path" | "file" => rendered.rsplit(['/', '\\']).next().unwrap().to_owned(),
            _ => rendered,
        };
        write!(self.0, " {}={rendered}", field.name()).unwrap();
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
        let target = attributes.metadata().target();
        if target.starts_with("yard_shell")
            || target.starts_with("yard_state")
            || target.starts_with("yard_paths")
        {
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
        if !(target.starts_with("yard_shell")
            || target.starts_with("yard_state")
            || target.starts_with("yard_paths")
            || target == PROBE)
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
    // The lines since `mark`, without the quoting Debug fields add.
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
}

// `host` is the `tour` extension's handle on a shell whose store folder is
// `dir`: an extension keeps its named values in `<dir>/tour.json`.
fn extension(app: &TestAppContext, dir: &Path) -> shell::Extension {
    app.update(|cx| {
        yard_ui::init(cx);
        shell::set_store_dir(cx, dir.to_path_buf());
        shell::Extension::new(cx, "tour").unwrap()
    })
}

// The extension's own file in the store folder.
fn store_file(dir: &Path, extension: &str) -> PathBuf {
    dir.join(format!("{extension}.json"))
}

// The writes made so far reach the store file: deferred work runs, the
// 300 ms wait of a save passes, and the write itself finishes.
fn settle(app: &TestAppContext) {
    app.run_until_parked();
    app.executor().advance_clock(Duration::from_millis(400));
    app.run_until_parked();
}

// The contract's logging, alone in this binary: the first read or write of
// an extension opens `store-read{extension, file}`, ending with `debug`
// `read {keys}`, `no saved values`, or the `WARN`; `stored`, `store`, and
// `unstore` log `trace` `stored {key}`, `store {key}`, `unstore {key}`; and
// a save opens `store-save{extension, keys, reason}` holding
// `file-write{path}`.
#[gpui::test]
fn extension_store_logging(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    let dir = tempfile::tempdir().unwrap();
    let file = store_file(dir.path(), "tour");
    tracing::subscriber::with_default(trace.clone(), || {
        let app = cx.new_app();
        let host = extension(&app, dir.path());

        // The first write of an extension opens `store-read{extension,
        // file}`; a missing file ends it with `no saved values`.
        let mark = trace.mark();
        let stacks_mark = trace.stack_mark();
        app.update(|cx| host.store(cx, "count", json!(3)));
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.iter().any(|stack| {
                stack.last().is_some_and(|label| {
                    label.starts_with("store-read")
                        && label.contains("extension=tour")
                        && label.contains("file=tour.json")
                })
            }),
            "no `store-read extension=tour file=tour.json` group: {stacks:#?}"
        );
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("store-read")
                    && line.contains("DEBUG")
                    && line.contains("no saved values")
            }),
            "missing `no saved values` inside store-read: {lines:#?}"
        );
        // `store {key}` runs on every keystroke: it is a `trace`.
        assert!(
            lines.iter().any(|line| {
                line.contains(": TRACE")
                    && line.contains("key=count")
                    && line.contains("message=store")
            }),
            "missing `store {{key}}` at trace: {lines:#?}"
        );

        // `stored {key}` is a `trace` too, and gives the stored value back.
        let mark = trace.mark();
        assert_eq!(app.update(|cx| host.stored(cx, "count")), Some(json!(3)));
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.contains(": TRACE")
                    && line.contains("key=count")
                    && line.contains("message=stored")
            }),
            "missing `stored {{key}}` at trace: {lines:#?}"
        );

        // And `unstore {key}` is a `trace`.
        let mark = trace.mark();
        app.update(|cx| host.unstore(cx, "absent"));
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.contains(": TRACE")
                    && line.contains("key=absent")
                    && line.contains("message=unstore")
            }),
            "missing `unstore {{key}}` at trace: {lines:#?}"
        );

        // When no write has come for 300 ms, the save is the
        // `store-save{extension, keys}` group with `reason=change`, holding
        // `file-write`.
        let stacks_mark = trace.stack_mark();
        settle(&app);
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.iter().any(|stack| {
                let save = stack.iter().position(|label| {
                    label.starts_with("store-save")
                        && label.contains("extension=tour")
                        && label.contains("keys=1")
                        && label.contains("reason=change")
                });
                let write = stack.iter().position(|label| {
                    label.starts_with("file-write") && label.contains("tour.json")
                });
                matches!((save, write), (Some(save), Some(write)) if save < write)
            }),
            "no `store-save{{extension=tour, keys=1, reason=change}}` holding `file-write`: {stacks:#?}"
        );

        // Quitting writes once more: `store-save{reason=quit}` holding
        // `file-write`.
        let stacks_mark = trace.stack_mark();
        app.update(|cx| host.store(cx, "count", json!(4)));
        app.quit();
        app.run_until_parked();
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.iter().any(|stack| {
                let save = stack.iter().position(|label| {
                    label.starts_with("store-save")
                        && label.contains("extension=tour")
                        && label.contains("keys=1")
                        && label.contains("reason=quit")
                });
                let write = stack.iter().position(|label| {
                    label.starts_with("file-write") && label.contains("tour.json")
                });
                matches!((save, write), (Some(save), Some(write)) if save < write)
            }),
            "no `store-save{{extension=tour, keys=1, reason=quit}}` holding `file-write`: {stacks:#?}"
        );

        // A new run with the folder reads the file once, at the first read
        // or write, and the group ends with `debug` `read {keys}`.
        let restart = cx.new_app();
        let host = extension(&restart, dir.path());
        let mark = trace.mark();
        assert_eq!(
            restart.update(|cx| host.stored(cx, "count")),
            Some(json!(4))
        );
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("store-read")
                    && line.contains("extension=tour")
                    && line.contains("file=tour.json")
                    && line.contains("DEBUG")
                    && line.contains("read")
                    && line.contains("keys=1")
            }),
            "missing `read {{keys}}` inside store-read: {lines:#?}"
        );

        // A file that cannot be read ends `store-read` with the `WARN`,
        // never an `ERROR`.
        fs::write(&file, "this is not json").unwrap();
        let broken = cx.new_app();
        let host = extension(&broken, dir.path());
        let mark = trace.mark();
        let warnings_before = warnings().len();
        assert!(broken.update(|cx| host.stored(cx, "count")).is_none());
        let lines = trace.since(mark);
        assert!(
            lines
                .iter()
                .any(|line| { line.starts_with("store-read") && line.contains(": WARN") }),
            "the unreadable store file did not warn inside store-read: {lines:#?}"
        );
        assert_eq!(
            warnings().len(),
            warnings_before + 1,
            "an unreadable store file warned more than once: {:?}",
            warnings()
        );
        assert!(
            errors().is_empty(),
            "an unreadable store file is not an ERROR: {:?}",
            errors()
        );
    });
    assert!(
        trace
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entered
            .is_empty()
    );
}
