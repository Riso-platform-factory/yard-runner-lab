use gpui::{AppContext, InteractiveElement, IntoElement, Styled, TestAppContext, px};
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
use yard_layout::{Operation, decode};
use yard_shell as shell;
use yard_shell::{SessionWindow, new_session_shell, saved_session, track_session};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-session-{}-{}-{nanos:x}",
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

// A card fills the zone its container gives it and is found as `CARD-<node>`.
struct Card {
    node: String,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
    }
}

// `host` registers `tour.card`, places the default `yard.row` of three
// cards, and names `tour.swap`, which shows `c` first.
fn extension(app: &mut TestAppContext) -> shell::Extension {
    app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        host.register(cx, "tour.card", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
            })
            .into()
        });
        host.place_root(cx, "root", "yard.row");
        for node in ["a", "b", "c"] {
            host.place(cx, node, "tour.card", "root", "children");
        }
        host.named_layout(cx, "tour.swap", |layout| {
            layout.move_node("c", "root", "children", 0);
        });
        host
    })
}

// The files the main window keeps for itself, under `data`: `layout.json`
// and the named layouts' files under `layouts`. Returns `layout.json`'s
// path.
fn layout_files(app: &mut TestAppContext, data: &Path) -> PathBuf {
    let layout = data.join("layout.json");
    let layouts = data.join("layouts");
    let path = layout.clone();
    app.update(|cx| {
        shell::set_layout_file(cx, layout);
        shell::set_named_layout_files(cx, move |name| layouts.join(format!("{name}.json")));
    });
    path
}

// A window opened as the startup opens one: `new_shell` on the given
// windowed bounds.
fn open_at(
    app: &mut TestAppContext,
    bounds: gpui::Bounds<gpui::Pixels>,
) -> gpui::WindowHandle<shell::Shell> {
    app.update(|cx| {
        cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            shell::new_shell,
        )
        .unwrap()
    })
}

fn open(app: &mut TestAppContext) -> gpui::WindowHandle<shell::Shell> {
    app.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), shell::new_shell)
            .unwrap()
    })
}

// The window the startup opens for `saved`: a root built by
// `new_session_shell` on a window with the saved bounds and display.
fn reopen(app: &mut TestAppContext, saved: &SessionWindow) -> gpui::WindowHandle<shell::Shell> {
    app.update(|cx| {
        cx.open_window(
            gpui::WindowOptions {
                window_bounds: saved.bounds,
                display_id: saved.display,
                ..Default::default()
            },
            |window, cx| new_session_shell(window, cx, saved),
        )
        .unwrap()
    })
}

// The window with the keyboard focus is the one calls act on.
fn activate(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) {
    window
        .update(app, |_, window, _| window.activate_window())
        .unwrap();
    app.run_until_parked();
}

fn show_layout(
    window: gpui::WindowHandle<shell::Shell>,
    app: &mut TestAppContext,
    host: &shell::Extension,
    name: &str,
) {
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| host.show_layout(window, cx, name))
        .unwrap();
}

fn hidden(app: &mut TestAppContext, node: &str) -> bool {
    app.update(|cx| shell::is_hidden(cx, node))
}

fn shown(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) -> String {
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| {
            shell::shown_layout(cx, window).to_string()
        })
        .unwrap()
}

fn inner_bounds(
    window: gpui::WindowHandle<shell::Shell>,
    app: &mut TestAppContext,
) -> gpui::WindowBounds {
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, _| window.inner_window_bounds())
        .unwrap()
}

fn layer_len(app: &mut TestAppContext) -> usize {
    app.update(|cx| shell::user_layer_len(cx))
}

fn saved_ops(layout_path: &Path) -> Vec<Operation> {
    decode(&fs::read_to_string(layout_path).unwrap()).unwrap()
}

fn is_hide(operation: &Operation, node: &str) -> bool {
    matches!(operation, Operation::SetHidden { node: id, hidden: true } if id.as_str() == node)
}

fn display_id_and_uuid(app: &TestAppContext) -> (gpui::DisplayId, String) {
    app.update(|cx| {
        let display = cx.displays().into_iter().next().unwrap();
        (display.id(), display.uuid().unwrap().to_string())
    })
}

// The `windows` array of the saved session file.
fn session_windows(file: &Path) -> Vec<serde_json::Value> {
    let json: serde_json::Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    json["windows"]
        .as_array()
        .unwrap_or_else(|| panic!("no `windows` in {json}"))
        .clone()
}

// The changes made so far reach the file: deferred layout work runs, the
// 100 ms wait of a save passes, and the write itself finishes.
fn settle(app: &mut TestAppContext) {
    app.run_until_parked();
    app.executor().advance_clock(Duration::from_millis(200));
    app.run_until_parked();
}

// Renames the node of every operation on `node` to `gone`, wherever the
// session keeps the saved operations.
fn refuse_op(value: &mut serde_json::Value, node: &str) {
    match value {
        serde_json::Value::Object(map) => {
            if map.contains_key("op")
                && map.get("node").and_then(serde_json::Value::as_str) == Some(node)
            {
                map.insert("node".into(), "gone".into());
            }
            for (_, value) in map.iter_mut() {
                refuse_op(value, node);
            }
        }
        serde_json::Value::Array(items) => {
            for value in items {
                refuse_op(value, node);
            }
        }
        _ => {}
    }
}

// Two windows rearranged differently — different bounds, different layers,
// and different named layouts — are written to the session and read back
// the same: the file's `windows`, in opening order, each hold their
// `bounds` and `display` as `window.json` holds them, the named layout the
// window showed, and its user layer for each named layout it has shown. At
// the next start each window opens again in order, the first as the main
// window; the session, not `layout.json`, gives the restored layers.
#[gpui::test]
fn windows_with_bounds_display_layouts_and_layers_are_written_and_read_back_the_same(
    cx: &mut TestAppContext,
) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("session.json");
        let mut app = cx.new_app();
        layout_files(&mut app, temp.path());
        let host = extension(&mut app);
        app.update(|cx| track_session(cx, file.clone()));
        let (display_id, display_uuid) = display_id_and_uuid(&app);

        // The first window hides `a` on `yard.default`; the second hides `c`
        // there, switches to `tour.swap`, and hides `b`: the second window
        // has shown two named layouts, each with its own layer.
        let first_bounds = gpui::Bounds::new(
            gpui::point(px(120.), px(80.)),
            gpui::size(px(900.), px(600.)),
        );
        let second_bounds = gpui::Bounds::new(
            gpui::point(px(200.), px(140.)),
            gpui::size(px(700.), px(500.)),
        );
        // `simulate_window_resize` keeps the window's place: the bounds the
        // second window is saved with.
        let second_resized = gpui::size(px(640.), px(460.));
        let second_saved = gpui::Bounds::new(second_bounds.origin, second_resized);
        let first = open_at(&mut app, first_bounds);
        activate(first, &mut app);
        app.update(|cx| host.set_hidden(cx, "a", true));
        let second = open_at(&mut app, second_bounds);
        activate(second, &mut app);
        app.update(|cx| host.set_hidden(cx, "c", true));
        show_layout(second, &mut app, &host, "tour.swap");
        app.update(|cx| host.set_hidden(cx, "b", true));
        // A resize keeps the window's place and changes its size; the
        // bounds change saves the session like any change does.
        app.simulate_window_resize(gpui::AnyWindowHandle::from(second), second_resized);
        settle(&mut app);

        // The file: `version` 1 and `windows` in opening order, each with
        // `bounds` and `display` as `window.json` holds them.
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(json["version"], 1);
        let windows = json["windows"]
            .as_array()
            .unwrap_or_else(|| panic!("no `windows` in {json}"));
        assert_eq!(windows.len(), 2, "{json}");
        for (window, bounds) in windows.iter().zip([first_bounds, second_saved]) {
            assert_eq!(window["display"], display_uuid, "{window}");
            assert_eq!(window["bounds"]["state"], "windowed", "{window}");
            assert_eq!(
                window["bounds"]["x"],
                f64::from(bounds.origin.x),
                "{window}"
            );
            assert_eq!(
                window["bounds"]["y"],
                f64::from(bounds.origin.y),
                "{window}"
            );
            assert_eq!(
                window["bounds"]["width"],
                f64::from(bounds.size.width),
                "{window}"
            );
            assert_eq!(
                window["bounds"]["height"],
                f64::from(bounds.size.height),
                "{window}"
            );
        }
        // Each entry keeps the named layouts the window has shown: the
        // first stayed on `yard.default`; the second shows `tour.swap` and
        // had shown `yard.default` before it.
        assert!(
            windows[0].to_string().contains("yard.default"),
            "{windows:?}"
        );
        let second_window = windows[1].to_string();
        assert!(second_window.contains("tour.swap"), "{second_window}");
        assert!(second_window.contains("yard.default"), "{second_window}");

        // Read back: two `SessionWindow`s in opening order, each with the
        // bounds it opened with and the screen the saved UUID names.
        let saved = app
            .update(|cx| saved_session(cx, &file))
            .expect("a saved session");
        assert_eq!(saved.len(), 2);
        assert_eq!(
            saved[0].bounds,
            Some(gpui::WindowBounds::Windowed(first_bounds))
        );
        assert_eq!(saved[0].display, Some(display_id));
        assert_eq!(
            saved[1].bounds,
            Some(gpui::WindowBounds::Windowed(second_saved))
        );
        assert_eq!(saved[1].display, Some(display_id));

        // On screens that do not have the saved screen's UUID, feature 7's
        // rule gives no placement: every window still reads back, to open
        // at the default place.
        let mut restart = cx.new_app();
        let saved = restart
            .update(|cx| saved_session(cx, &file))
            .expect("a saved session");
        assert_eq!(saved.len(), 2);
        for window in &saved {
            assert_eq!(window.bounds, None);
            assert_eq!(window.display, None);
        }

        // On a run whose screen has the saved UUID — here, with the file's
        // UUID replaced by this run's — every window reads back with its
        // placement and opens again, the first as the main window.
        let next = temp.path().join("next");
        fs::create_dir_all(&next).unwrap();
        let next_file = next.join("session.json");
        let layout = layout_files(&mut restart, &next);
        let host = extension(&mut restart);
        let (restart_display, restart_uuid) = display_id_and_uuid(&restart);
        fs::write(
            &next_file,
            fs::read_to_string(&file)
                .unwrap()
                .replace(&display_uuid, &restart_uuid),
        )
        .unwrap();
        // `layout.json` holds state the session wins over.
        fs::write(
            &layout,
            r#"{"version":1,"operations":[{"op":"hide","node":"b"}]}"#,
        )
        .unwrap();
        let saved = restart
            .update(|cx| saved_session(cx, &next_file))
            .expect("a saved session");
        assert_eq!(saved.len(), 2);
        for (window, bounds) in saved.iter().zip([first_bounds, second_saved]) {
            assert_eq!(window.bounds, Some(gpui::WindowBounds::Windowed(bounds)));
            assert_eq!(window.display, Some(restart_display));
        }
        let first = reopen(&mut restart, &saved[0]);
        let second = reopen(&mut restart, &saved[1]);

        // Each recreated window shows the named layout it showed, with its
        // own layers — `layout.json` did not apply.
        assert_eq!(shown(first, &mut restart), "yard.default");
        assert_eq!(shown(second, &mut restart), "tour.swap");
        assert_eq!(
            inner_bounds(first, &mut restart),
            gpui::WindowBounds::Windowed(first_bounds)
        );
        assert_eq!(
            inner_bounds(second, &mut restart),
            gpui::WindowBounds::Windowed(second_saved)
        );
        activate(first, &mut restart);
        assert!(hidden(&mut restart, "a"));
        assert!(
            !hidden(&mut restart, "b"),
            "layout.json's hide reached the session's window"
        );
        assert_eq!(layer_len(&mut restart), 1);
        activate(second, &mut restart);
        assert!(hidden(&mut restart, "b"));
        assert!(!hidden(&mut restart, "a"));
        assert_eq!(layer_len(&mut restart), 1);
        // The second window's `yard.default` layer was saved with it too.
        show_layout(second, &mut restart, &host, "yard.default");
        assert!(hidden(&mut restart, "c"));
        assert!(!hidden(&mut restart, "b"));
        assert_eq!(layer_len(&mut restart), 1);

        // The first recreated window is the main window: its operations
        // reach `layout.json`; the second window's stay in the session.
        activate(first, &mut restart);
        restart.update(|cx| host.set_hidden(cx, "b", true));
        let ops = saved_ops(&layout);
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(is_hide(&ops[0], "a"), "{ops:?}");
        assert!(is_hide(&ops[1], "b"), "{ops:?}");
        activate(second, &mut restart);
        restart.update(|cx| host.set_hidden(cx, "a", true));
        assert!(hidden(&mut restart, "a"));
        let ops = saved_ops(&layout);
        assert_eq!(
            ops.len(),
            2,
            "a second window's operation was saved: {ops:?}"
        );
        assert!(warnings().is_empty(), "{:?}", warnings());
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A missing file, a file that cannot be read, one that is not valid JSON,
// and one with another `version` each give `None` with the log the contract
// names: `no saved session` for the missing file, the `WARN` for a file
// that cannot be read or does not decode, and `no saved session` for a
// file holding no window. The files are left alone.
#[gpui::test]
fn a_missing_broken_other_version_or_empty_file_gives_none_with_its_log(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let file = temp.path().join("session.json");
        let app = cx.new_app();

        // No file at all.
        let mark = trace.mark();
        let stacks = trace.stack_mark();
        assert!(app.update(|cx| saved_session(cx, &file)).is_none());
        let lines = trace.since(mark);
        assert!(
            lines.contains(
                &"session-restore file=session.json: DEBUG message=no saved session".to_string()
            ),
            "missing `no saved session` in {lines:#?}"
        );
        assert_eq!(trace.groups(stacks, "session-restore"), 1);

        // A file that cannot be read: a directory is not valid JSON bytes.
        fs::create_dir(&file).unwrap();
        let before = warnings().len();
        let stacks = trace.stack_mark();
        assert!(app.update(|cx| saved_session(cx, &file)).is_none());
        assert_eq!(warnings().len(), before + 1, "{:?}", warnings());
        assert_eq!(trace.groups(stacks, "session-restore"), 1);
        fs::remove_dir(&file).unwrap();

        // Not valid JSON.
        fs::write(&file, "this is not json").unwrap();
        let before = warnings().len();
        assert!(app.update(|cx| saved_session(cx, &file)).is_none());
        assert_eq!(warnings().len(), before + 1, "{:?}", warnings());
        assert_eq!(fs::read_to_string(&file).unwrap(), "this is not json");

        // Another version.
        fs::write(
            &file,
            json!({
                "version": 2,
                "windows": [{
                    "display": "00000000-0000-0000-0000-000000000000",
                    "bounds": {
                        "state": "windowed",
                        "x": 0.,
                        "y": 0.,
                        "width": 1.,
                        "height": 1.,
                    },
                    "layout": "yard.default",
                    "layers": {"yard.default": []},
                }],
            })
            .to_string(),
        )
        .unwrap();
        let before = warnings().len();
        assert!(app.update(|cx| saved_session(cx, &file)).is_none());
        assert_eq!(warnings().len(), before + 1, "{:?}", warnings());
        let left: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            left["version"], 2,
            "a wrong-version file was not left alone"
        );

        // A valid file holding no window: `no saved session`, no `WARN`.
        fs::write(&file, json!({"version": 1, "windows": []}).to_string()).unwrap();
        let mark = trace.mark();
        let stacks = trace.stack_mark();
        let before = warnings().len();
        assert!(app.update(|cx| saved_session(cx, &file)).is_none());
        assert_eq!(warnings().len(), before, "{:?}", warnings());
        let lines = trace.since(mark);
        assert!(
            lines.contains(
                &"session-restore file=session.json: DEBUG message=no saved session".to_string()
            ),
            "missing `no saved session` for no windows in {lines:#?}"
        );
        assert_eq!(trace.groups(stacks, "session-restore"), 1);

        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A saved operation the window's layout refuses — here one naming a node
// the tree does not have — is skipped with feature 12's `INFO`; the rest
// of the window's layers still apply.
#[gpui::test]
fn a_refused_operation_in_a_saved_layer_is_skipped_with_the_info(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let file = temp.path().join("session.json");
        let mut app = cx.new_app();
        layout_files(&mut app, temp.path());
        let host = extension(&mut app);
        app.update(|cx| track_session(cx, file.clone()));

        let first = open(&mut app);
        activate(first, &mut app);
        app.update(|cx| host.set_hidden(cx, "a", true));
        let second = open(&mut app);
        activate(second, &mut app);
        show_layout(second, &mut app, &host, "tour.swap");
        app.update(|cx| host.set_hidden(cx, "b", true));
        settle(&mut app);

        // The second window's one `tour.swap` operation now names a node
        // that is not in the tree.
        let mut json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        refuse_op(&mut json, "b");
        fs::write(&file, json.to_string()).unwrap();

        let mark = trace.mark();
        let saved = app
            .update(|cx| saved_session(cx, &file))
            .expect("a saved session");
        let first = reopen(&mut app, &saved[0]);
        let second = reopen(&mut app, &saved[1]);

        // The refused operation is skipped with the `INFO`; the second
        // window still shows `tour.swap`, without it and without it in its
        // layer.
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("layout-restore")
                    && line.contains(": INFO")
                    && line.contains("skipped")
                    && line.contains("\"gone\"")
            }),
            "a refused session operation did not log the INFO: {lines:#?}"
        );
        activate(second, &mut app);
        assert_eq!(shown(second, &mut app), "tour.swap");
        assert!(!hidden(&mut app, "b"), "the refused hide applied");
        assert_eq!(layer_len(&mut app), 0);
        activate(first, &mut app);
        assert!(hidden(&mut app, "a"));
        assert_eq!(layer_len(&mut app), 1);
        assert!(warnings().is_empty(), "{:?}", warnings());
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A window closed during the run leaves the session: the next save holds
// only the windows still open.
#[gpui::test]
fn a_window_closed_during_the_run_is_not_in_the_next_save(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("session.json");
        let mut app = cx.new_app();
        let host = extension(&mut app);
        app.update(|cx| track_session(cx, file.clone()));

        let first = open(&mut app);
        activate(first, &mut app);
        app.update(|cx| host.set_hidden(cx, "a", true));
        let second = open(&mut app);
        activate(second, &mut app);
        app.update(|cx| host.set_hidden(cx, "b", true));

        second
            .update(&mut app, |_, window, _| window.remove_window())
            .unwrap();
        settle(&mut app);

        assert_eq!(
            session_windows(&file).len(),
            1,
            "a closed window stayed in the session"
        );
        let saved = app
            .update(|cx| saved_session(cx, &file))
            .expect("a saved session");
        assert_eq!(saved.len(), 1);
        let recreated = reopen(&mut app, &saved[0]);
        activate(recreated, &mut app);
        assert!(hidden(&mut app, "a"));
        assert!(!hidden(&mut app, "b"), "the closed window's layer survived");
        assert!(warnings().is_empty(), "{:?}", warnings());
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// Quitting saves every window open at quit, in one save.
#[gpui::test]
fn quitting_saves_every_open_window_once(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let file = temp.path().join("session.json");
        let mut app = cx.new_app();
        let host = extension(&mut app);
        app.update(|cx| track_session(cx, file.clone()));

        let first = open(&mut app);
        activate(first, &mut app);
        app.update(|cx| host.set_hidden(cx, "a", true));
        let second = open(&mut app);
        activate(second, &mut app);
        app.update(|cx| host.set_hidden(cx, "b", true));
        settle(&mut app);
        if file.exists() {
            fs::remove_file(&file).unwrap();
        }

        let stacks = trace.stack_mark();
        app.quit();

        assert_eq!(
            session_windows(&file).len(),
            2,
            "the quit did not save every open window"
        );
        assert_eq!(
            trace.groups(stacks, "session-save"),
            1,
            "quitting did not save once"
        );
        assert!(warnings().is_empty(), "{:?}", warnings());
        assert!(errors().is_empty(), "{:?}", errors());
    });
}
