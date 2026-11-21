use gpui::TestAppContext;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Once},
    time::Duration,
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
            || metadata.target().starts_with("yard_state")
            || metadata.target().starts_with("yard_paths")
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

// The groups the code opens and the events it writes, so a test can count
// the `store-save` groups — one is one write of the file — and read the
// `debug` and `INFO` lines a run logs.
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
        captured
            .spans
            .insert(id, format!("{}{}", attributes.metadata().name(), fields.0));
        tracing::span::Id::from_u64(id)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let target = event.metadata().target();
        if !(target.starts_with("yard_shell")
            || target.starts_with("yard_state")
            || target.starts_with("yard_paths"))
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
    // The `store-save` groups opened so far: one group is one write of the
    // extension's file.
    fn store_saves(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .spans
            .values()
            .filter(|label| label.starts_with("store-save"))
            .count()
    }
    // The `file-write` groups opened so far: a run with no store folder
    // opens none.
    fn file_writes(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .spans
            .values()
            .filter(|label| label.starts_with("file-write"))
            .count()
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

fn store_json(file: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
}

// The writes made so far reach the store file: deferred work runs, the
// 300 ms wait of a save passes, and the write itself finishes.
fn settle(app: &TestAppContext) {
    app.run_until_parked();
    app.executor().advance_clock(Duration::from_millis(400));
    app.run_until_parked();
}

// A value stored is saved at quit, and a new run with the same folder reads
// it back through `stored`.
#[gpui::test]
fn a_stored_value_is_saved_at_quit_and_read_back_in_the_next_run(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        let app = cx.new_app();
        let host = extension(&app, dir.path());
        app.update(|cx| host.store(cx, "count", json!(3)));
        // A write changes the value in memory at once, so the next read
        // gives it back.
        assert_eq!(app.update(|cx| host.stored(cx, "count")), Some(json!(3)));
        app.quit();
        app.run_until_parked();

        // The file: a `version` (1) and the `values`, a map from key to
        // value.
        let json = store_json(&file);
        assert_eq!(json["version"], json!(1), "{json}");
        assert_eq!(json["values"]["count"], json!(3), "{json}");

        let restart = cx.new_app();
        let host = extension(&restart, dir.path());
        assert_eq!(
            restart.update(|cx| host.stored(cx, "count")),
            Some(json!(3)),
            "a new run with the folder did not give the stored value back"
        );
        assert!(
            restart.update(|cx| host.stored(cx, "missing")).is_none(),
            "a key that was never stored read back a value"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// Two extensions can use the same key: each reads its own value, and each
// file holds only its own extension's values.
#[gpui::test]
fn two_extensions_store_the_same_key_in_their_own_files(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let dir = tempfile::tempdir().unwrap();
        let app = cx.new_app();
        let (tour, guest) = app.update(|cx| {
            yard_ui::init(cx);
            shell::set_store_dir(cx, dir.path().to_path_buf());
            let tour = shell::Extension::new(cx, "tour").unwrap();
            let guest = shell::Extension::new(cx, "guest").unwrap();
            let _idle = shell::Extension::new(cx, "idle").unwrap();
            (tour, guest)
        });
        app.update(|cx| {
            tour.store(cx, "key", json!(1));
            tour.store(cx, "tours", json!(1));
            guest.store(cx, "key", json!(2));
            guest.store(cx, "guests", json!(2));
        });
        // The same key reads back each extension's own value: there is no
        // call that takes another extension's name.
        assert_eq!(app.update(|cx| tour.stored(cx, "key")), Some(json!(1)));
        assert_eq!(app.update(|cx| guest.stored(cx, "key")), Some(json!(2)));
        app.quit();
        app.run_until_parked();

        let tour_json = store_json(&store_file(dir.path(), "tour"));
        assert_eq!(tour_json["version"], json!(1), "{tour_json}");
        assert_eq!(
            tour_json["values"],
            json!({"key": 1, "tours": 1}),
            "{tour_json}"
        );
        let guest_json = store_json(&store_file(dir.path(), "guest"));
        assert_eq!(guest_json["version"], json!(1), "{guest_json}");
        assert_eq!(
            guest_json["values"],
            json!({"key": 2, "guests": 2}),
            "{guest_json}"
        );
        // Only the extensions whose values changed are written.
        assert!(
            !store_file(dir.path(), "idle").exists(),
            "an extension that never wrote got a file"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// Ten writes 10 ms apart are one burst: the file is written once, when no
// write has come for 300 ms, and holds the last value.
#[gpui::test]
fn ten_writes_ten_ms_apart_save_the_file_once(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        let app = cx.new_app();
        let host = extension(&app, dir.path());

        for count in 1..=10u64 {
            app.update(|cx| host.store(cx, "count", json!(count)));
            if count < 10 {
                app.executor().advance_clock(Duration::from_millis(10));
            }
        }
        assert!(
            !file.exists(),
            "a burst of ten writes saved before its last write"
        );

        app.executor().advance_clock(Duration::from_millis(299));
        app.run_until_parked();
        assert!(
            !file.exists(),
            "the save did not wait until no write had come for 300 ms"
        );

        app.executor().advance_clock(Duration::from_millis(10));
        app.run_until_parked();
        assert_eq!(store_json(&file)["values"]["count"], json!(10));
        assert_eq!(
            trace.store_saves(),
            1,
            "a burst of ten writes saved the file more than once"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// Quitting with a save waiting writes the file once, holding the value as
// it was at the quit.
#[gpui::test]
fn quitting_with_a_save_waiting_writes_the_file_once(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        let app = cx.new_app();
        let host = extension(&app, dir.path());
        app.update(|cx| host.store(cx, "count", json!(5)));

        app.quit();
        app.run_until_parked();

        assert_eq!(store_json(&file)["values"]["count"], json!(5));
        assert_eq!(
            trace.store_saves(),
            1,
            "quitting with a save waiting wrote the file more than once"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// The save after the wait writes without a quit, so a process that ends
// after it keeps the stored value for the next run.
#[gpui::test]
fn the_wait_writes_the_file_before_the_process_ends(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        let app = cx.new_app();
        let host = extension(&app, dir.path());
        app.update(|cx| host.store(cx, "count", json!(7)));
        settle(&app);
        assert_eq!(
            store_json(&file)["values"]["count"],
            json!(7),
            "the save after the wait did not write the value"
        );

        // The process ends without quitting: a new run with the folder
        // still reads the value back.
        let restart = cx.new_app();
        let host = extension(&restart, dir.path());
        assert_eq!(
            restart.update(|cx| host.stored(cx, "count")),
            Some(json!(7))
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A missing file means no values, with the `debug` `no saved values`
// ending `store-read`, and the next save writes a good file.
#[gpui::test]
fn a_missing_file_means_no_values_until_the_next_save(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        let app = cx.new_app();
        let host = extension(&app, dir.path());
        let mark = trace.mark();
        assert!(app.update(|cx| host.stored(cx, "count")).is_none());
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("store-read")
                    && line.contains("extension=tour")
                    && line.contains("file=tour.json")
                    && line.contains("DEBUG")
                    && line.contains("no saved values")
            }),
            "missing `no saved values` inside store-read: {lines:#?}"
        );

        app.update(|cx| host.store(cx, "count", json!(5)));
        settle(&app);
        let json = store_json(&file);
        assert_eq!(json["version"], json!(1), "{json}");
        assert_eq!(json["values"]["count"], json!(5), "{json}");
        assert!(
            warnings().is_empty(),
            "a missing store file warned: {:?}",
            warnings()
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A file that cannot be read means no values, with one `WARN` and never an
// `ERROR`, and the next save writes a good file over it.
#[gpui::test]
fn a_broken_file_warns_and_the_next_save_writes_a_good_file(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        fs::write(&file, "this is not json").unwrap();
        let app = cx.new_app();
        let host = extension(&app, dir.path());
        let warnings_before = warnings().len();
        assert!(app.update(|cx| host.stored(cx, "count")).is_none());
        assert_eq!(
            warnings().len(),
            warnings_before + 1,
            "a store file that cannot be read did not warn exactly once: {:?}",
            warnings()
        );

        app.update(|cx| host.store(cx, "count", json!(5)));
        settle(&app);
        let json = store_json(&file);
        assert_eq!(json["version"], json!(1), "{json}");
        assert_eq!(
            json["values"],
            json!({"count": 5}),
            "the next save did not write a good file over the broken one: {json}"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A file of another `version` means no values either, with one `WARN`, and
// the next save writes the current version over it.
#[gpui::test]
fn a_file_of_another_version_warns_and_the_next_save_writes_a_good_file(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        fs::write(
            &file,
            json!({"version": 2, "values": {"count": 9}}).to_string(),
        )
        .unwrap();
        let app = cx.new_app();
        let host = extension(&app, dir.path());
        let warnings_before = warnings().len();
        assert!(app.update(|cx| host.stored(cx, "count")).is_none());
        assert_eq!(
            warnings().len(),
            warnings_before + 1,
            "a store file of another version did not warn exactly once: {:?}",
            warnings()
        );

        app.update(|cx| host.store(cx, "count", json!(5)));
        settle(&app);
        let json = store_json(&file);
        assert_eq!(json["version"], json!(1), "{json}");
        assert_eq!(
            json["values"],
            json!({"count": 5}),
            "the other version's values were not dropped: {json}"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// With no store folder the values still work for the run, the `debug`
// `no store folder` is logged once, and nothing is written.
#[gpui::test]
fn without_a_folder_values_work_for_the_run_and_nothing_is_written(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let mark = trace.mark();
        let app = cx.new_app();
        let host = app.update(|cx| {
            yard_ui::init(cx);
            shell::Extension::new(cx, "tour").unwrap()
        });
        app.update(|cx| host.store(cx, "count", json!(4)));
        assert_eq!(app.update(|cx| host.stored(cx, "count")), Some(json!(4)));
        app.update(|cx| host.unstore(cx, "count"));
        assert!(app.update(|cx| host.stored(cx, "count")).is_none());
        app.update(|cx| host.store(cx, "count", json!(7)));
        assert_eq!(app.update(|cx| host.stored(cx, "count")), Some(json!(7)));

        settle(&app);
        app.quit();
        app.run_until_parked();

        let lines = trace.since(mark);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("DEBUG") && line.contains("no store folder"))
                .count(),
            1,
            "`no store folder` is not logged exactly once: {lines:#?}"
        );
        assert_eq!(
            trace.file_writes(),
            0,
            "a run with no store folder wrote a file"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// An empty key is refused with an `INFO` `refused {reason}`: `stored`
// gives nothing back, and `store` and `unstore` change nothing.
#[gpui::test]
fn an_empty_key_is_refused(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        let app = cx.new_app();
        let host = extension(&app, dir.path());

        let mark = trace.mark();
        assert!(app.update(|cx| host.stored(cx, "")).is_none());
        assert!(
            trace
                .since(mark)
                .iter()
                .any(|line| line.contains(": INFO") && line.contains("refused")),
            "an empty key to `stored` was not refused"
        );

        let mark = trace.mark();
        app.update(|cx| host.store(cx, "", json!(1)));
        assert!(
            trace
                .since(mark)
                .iter()
                .any(|line| line.contains(": INFO") && line.contains("refused")),
            "an empty key to `store` was not refused"
        );
        settle(&app);
        assert!(
            !file.exists() && trace.store_saves() == 0,
            "a refused write still changed the store"
        );

        let mark = trace.mark();
        app.update(|cx| host.unstore(cx, ""));
        assert!(
            trace
                .since(mark)
                .iter()
                .any(|line| line.contains(": INFO") && line.contains("refused")),
            "an empty key to `unstore` was not refused"
        );
        settle(&app);
        assert!(!file.exists(), "a refused remove still wrote the store");

        // A good key still works after the refusals.
        app.update(|cx| host.store(cx, "count", json!(1)));
        assert_eq!(app.update(|cx| host.stored(cx, "count")), Some(json!(1)));
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// Removing a key that is not there does nothing; removing a key saves the
// file without it, and removing the last key writes a file with no values.
#[gpui::test]
fn removing_a_key_and_the_last_key(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let dir = tempfile::tempdir().unwrap();
        let file = store_file(dir.path(), "tour");
        let app = cx.new_app();
        let host = extension(&app, dir.path());
        app.update(|cx| {
            host.store(cx, "count", json!(1));
            host.store(cx, "tries", json!(2));
        });
        settle(&app);
        assert_eq!(store_json(&file)["values"], json!({"count": 1, "tries": 2}));

        // Removing a key that is not there does nothing: no save, and the
        // file is as it was.
        let saves = trace.store_saves();
        app.update(|cx| host.unstore(cx, "absent"));
        settle(&app);
        assert_eq!(
            trace.store_saves(),
            saves,
            "removing a key that is not there saved anyway"
        );
        assert_eq!(store_json(&file)["values"], json!({"count": 1, "tries": 2}));

        // Removing `count` saves a file that holds only `tries`.
        app.update(|cx| host.unstore(cx, "count"));
        assert!(app.update(|cx| host.stored(cx, "count")).is_none());
        settle(&app);
        assert_eq!(store_json(&file)["values"], json!({"tries": 2}));

        // Removing the last key writes a file with no values.
        app.update(|cx| host.unstore(cx, "tries"));
        settle(&app);
        let json = store_json(&file);
        assert_eq!(json["version"], json!(1), "{json}");
        assert!(
            json["values"].as_object().is_some_and(|v| v.is_empty()),
            "removing the last key did not write a file with no values: {json}"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}
