use gpui::{AppContext, TestAppContext, px};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use yard_shell::{saved_window_bounds, track_window_bounds};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-window-bounds-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
        metadata.target().starts_with("yard_shell") || metadata.target().starts_with("yard_paths")
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            RECORDS
                .lock()
                .unwrap()
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
    let guard = LOCK.lock().unwrap();
    RECORDS.lock().unwrap().clear();
    guard
}

fn records() -> Vec<(log::Level, String)> {
    RECORDS.lock().unwrap().clone()
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
        let mut captured = self.0.lock().unwrap();
        let id = captured.spans.len() as u64 + 1;
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let label = format!("{}{}", attributes.metadata().name(), fields.0);
        let target = attributes.metadata().target();
        if target.starts_with("yard_shell") || target.starts_with("yard_paths") {
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
        if (!target.starts_with("yard_shell") && !target.starts_with("yard_paths"))
            || *event.metadata().level() == tracing::Level::TRACE
        {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap();
        let group = captured
            .entered
            .last()
            .map(|id| captured.spans[id].as_str())
            .unwrap_or("none");
        let line = format!("{group}: {}{}", event.metadata().level(), fields.0);
        captured.lines.push(line);
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.0.lock().unwrap().entered.push(id.into_u64());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(self.0.lock().unwrap().entered.pop(), Some(id.into_u64()));
    }
}

impl Trace {
    fn mark(&self) -> usize {
        self.0.lock().unwrap().lines.len()
    }
    fn since(&self, mark: usize) -> Vec<String> {
        self.0.lock().unwrap().lines[mark..].to_vec()
    }
    fn stack_mark(&self) -> usize {
        self.0.lock().unwrap().span_stacks.len()
    }
    // Span groups opened since `mark` whose own label starts with `prefix`.
    fn groups(&self, mark: usize, prefix: &str) -> usize {
        self.0.lock().unwrap().span_stacks[mark..]
            .iter()
            .filter(|stack| stack.last().is_some_and(|label| label.starts_with(prefix)))
            .count()
    }
}

// Every test runs under the one lock and the one subscriber, so no thread
// reaches the shell's log lines without them.
fn captured(cx: &mut TestAppContext, test: impl FnOnce(&mut TestAppContext, &Trace)) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || test(cx, &trace));
    assert!(trace.0.lock().unwrap().entered.is_empty());
}

// Writes `file` in the contract's format for `state` on the screen whose UUID
// is `display`, with `bounds` as the window's windowed bounds in points.
fn write_saved(file: &Path, display: &str, state: &str, bounds: gpui::Bounds<gpui::Pixels>) {
    fs::write(
        file,
        json!({
            "version": 1,
            "display": display,
            "bounds": {
                "state": state,
                "x": f64::from(bounds.origin.x),
                "y": f64::from(bounds.origin.y),
                "width": f64::from(bounds.size.width),
                "height": f64::from(bounds.size.height),
            },
        })
        .to_string(),
    )
    .unwrap();
}

fn display_id_and_uuid(app: &TestAppContext) -> (gpui::DisplayId, String) {
    app.update(|cx| {
        let display = cx.displays().into_iter().next().unwrap();
        (display.id(), display.uuid().unwrap().to_string())
    })
}

// Windowed, maximized, and full-screen bounds with a display UUID are read
// back the same: the same `WindowBounds` on the screen the UUID names.
#[gpui::test]
fn saved_bounds_for_every_state_read_back_the_same(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("window.json");
        let app = cx.new_app();
        let (display_id, display_uuid) = display_id_and_uuid(&app);
        let bounds = gpui::Bounds::new(
            gpui::point(px(120.), px(80.)),
            gpui::size(px(1024.), px(768.)),
        );
        for (state, expected) in [
            ("windowed", gpui::WindowBounds::Windowed(bounds)),
            ("maximized", gpui::WindowBounds::Maximized(bounds)),
            ("fullscreen", gpui::WindowBounds::Fullscreen(bounds)),
        ] {
            write_saved(&file, &display_uuid, state, bounds);
            let saved = app.update(|cx| saved_window_bounds(cx, &file));
            assert_eq!(saved, Some((expected, display_id)), "{state}");
        }
        assert!(warnings().is_empty(), "{:?}", warnings());
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A missing file gives `None` with `no saved window bounds`; a file that
// cannot be read, one that is not valid JSON, and one with another `version`
// each give `None` with a WARN and leave the file alone.
#[gpui::test]
fn missing_unreadable_broken_and_other_version_files_give_none(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let file = temp.path().join("window.json");
        let app = cx.new_app();
        let (_, display_uuid) = display_id_and_uuid(&app);
        let bounds = gpui::Bounds::new(
            gpui::point(px(120.), px(80.)),
            gpui::size(px(1024.), px(768.)),
        );

        // No file at all.
        let mark = trace.mark();
        let stacks = trace.stack_mark();
        assert!(app.update(|cx| saved_window_bounds(cx, &file)).is_none());
        let lines = trace.since(mark);
        assert!(
            lines.contains(
                &"window-bounds-restore file=window.json: DEBUG message=no saved window bounds"
                    .to_string()
            ),
            "missing `no saved window bounds` in {lines:#?}"
        );
        assert_eq!(trace.groups(stacks, "window-bounds-restore"), 1);

        // A file that cannot be read: a directory is not valid JSON bytes.
        fs::create_dir(&file).unwrap();
        let before = warnings().len();
        let stacks = trace.stack_mark();
        assert!(app.update(|cx| saved_window_bounds(cx, &file)).is_none());
        assert_eq!(warnings().len(), before + 1, "{:?}", warnings());
        assert_eq!(trace.groups(stacks, "window-bounds-restore"), 1);
        fs::remove_dir(&file).unwrap();

        // Not valid JSON.
        fs::write(&file, "this is not json").unwrap();
        let before = warnings().len();
        assert!(app.update(|cx| saved_window_bounds(cx, &file)).is_none());
        assert_eq!(warnings().len(), before + 1, "{:?}", warnings());
        assert_eq!(fs::read_to_string(&file).unwrap(), "this is not json");

        // Another version.
        fs::write(
            &file,
            json!({
                "version": 2,
                "display": display_uuid,
                "bounds": {
                    "state": "windowed",
                    "x": f64::from(bounds.origin.x),
                    "y": f64::from(bounds.origin.y),
                    "width": f64::from(bounds.size.width),
                    "height": f64::from(bounds.size.height),
                },
            })
            .to_string(),
        )
        .unwrap();
        let before = warnings().len();
        assert!(app.update(|cx| saved_window_bounds(cx, &file)).is_none());
        assert_eq!(warnings().len(), before + 1, "{:?}", warnings());
        let left: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            left["version"], 2,
            "a wrong-version file was not left alone"
        );

        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A saved UUID that no screen has gives `None` with the INFO: the window
// opens at the default place, as Zed's `build_window_options` finds no
// display.
#[gpui::test]
fn a_saved_uuid_no_screen_has_gives_none(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let file = temp.path().join("window.json");
        let app = cx.new_app();
        write_saved(
            &file,
            "00000000-0000-0000-0000-000000000000",
            "windowed",
            gpui::Bounds::new(
                gpui::point(px(120.), px(80.)),
                gpui::size(px(1024.), px(768.)),
            ),
        );

        let mark = trace.mark();
        let stacks = trace.stack_mark();
        assert!(app.update(|cx| saved_window_bounds(cx, &file)).is_none());
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("window-bounds-restore") && line.contains(": INFO")
            }),
            "missing the INFO inside window-bounds-restore in {lines:#?}"
        );
        assert_eq!(trace.groups(stacks, "window-bounds-restore"), 1);
        assert!(warnings().is_empty(), "{:?}", warnings());
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// An inactive window's bounds changes are not saved.
#[gpui::test]
fn an_inactive_windows_bounds_changes_are_not_saved(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let file = temp.path().join("window.json");
        let mut app = cx.new_app();
        let window = app.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                        gpui::point(px(100.), px(60.)),
                        gpui::size(px(800.), px(600.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| gpui::Empty),
            )
            .unwrap()
        });
        let any_window = gpui::AnyWindowHandle::from(window);
        let track_file = file.clone();
        any_window
            .update(&mut app, move |_, window, cx| {
                track_window_bounds(window, cx, track_file)
            })
            .unwrap();
        assert!(
            !any_window
                .update(&mut app, |_, window, _| window.is_window_active())
                .unwrap(),
            "the test window opened active"
        );

        let stacks = trace.stack_mark();
        app.simulate_window_resize(any_window, gpui::size(px(860.), px(660.)));
        app.simulate_window_resize(any_window, gpui::size(px(880.), px(680.)));
        app.executor().advance_clock(Duration::from_millis(300));
        assert!(!file.exists(), "an inactive window's change was saved");
        assert_eq!(
            trace.groups(stacks, "window-bounds-save"),
            0,
            "a save ran for an inactive window"
        );
        assert!(warnings().is_empty(), "{:?}", warnings());
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// Bounds changes while the window is active wait 100 ms and write once:
// several resizes inside the wait join the one queued save. The file holds
// the contract's fields and reads back the same.
#[gpui::test]
fn resizes_inside_the_100_ms_wait_write_the_file_once(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let file = temp.path().join("window.json");
        let mut app = cx.new_app();
        let (display_id, display_uuid) = display_id_and_uuid(&app);
        let window = app.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                        gpui::point(px(100.), px(60.)),
                        gpui::size(px(800.), px(600.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| gpui::Empty),
            )
            .unwrap()
        });
        let any_window = gpui::AnyWindowHandle::from(window);
        let track_file = file.clone();
        any_window
            .update(&mut app, move |_, window, cx| {
                track_window_bounds(window, cx, track_file)
            })
            .unwrap();
        any_window
            .update(&mut app, |_, window, _| window.activate_window())
            .unwrap();
        app.run_until_parked();
        assert!(
            any_window
                .update(&mut app, |_, window, _| window.is_window_active())
                .unwrap(),
            "the test window stayed inactive"
        );
        // Activation itself reports a bounds change; flush whatever it queued.
        app.executor().advance_clock(Duration::from_millis(150));
        if file.exists() {
            fs::remove_file(&file).unwrap();
        }

        // Three resizes inside the wait join one save; nothing is written
        // before the 100 ms are up.
        let stacks = trace.stack_mark();
        for (width, height) in [(900., 700.), (1000., 800.), (1100., 900.)] {
            app.simulate_window_resize(any_window, gpui::size(px(width), px(height)));
        }
        app.executor().advance_clock(Duration::from_millis(99));
        assert!(!file.exists(), "the save wrote before its 100 ms wait");
        app.executor().advance_clock(Duration::from_millis(1));
        assert!(file.exists(), "the queued save did not write");
        assert_eq!(
            trace.groups(stacks, "file-write"),
            1,
            "several resizes inside the wait wrote more than once"
        );
        assert_eq!(trace.groups(stacks, "window-bounds-save"), 1);

        // The file holds the contract's format and the last bounds.
        let text = fs::read_to_string(&file).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["display"], display_uuid);
        assert_eq!(json["bounds"]["state"], "windowed");
        assert_eq!(json["bounds"]["x"], 100.);
        assert_eq!(json["bounds"]["y"], 60.);
        assert_eq!(json["bounds"]["width"], 1100.);
        assert_eq!(json["bounds"]["height"], 900.);
        let saved = app.update(|cx| saved_window_bounds(cx, &file));
        assert_eq!(
            saved,
            Some((
                gpui::WindowBounds::Windowed(gpui::Bounds::new(
                    gpui::point(px(100.), px(60.)),
                    gpui::size(px(1100.), px(900.)),
                )),
                display_id
            ))
        );
        assert!(warnings().is_empty(), "{:?}", warnings());
        assert!(errors().is_empty(), "{:?}", errors());
    });
}
