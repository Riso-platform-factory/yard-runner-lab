use gpui::{AppContext, InteractiveElement, IntoElement, Styled, TestAppContext, px};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use yard_shell as shell;
use yard_shell::{SessionWindow, new_session_shell, saved_session};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-node-state-{}-{}-{nanos:x}",
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
    fn since(&self, mark: usize) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .lines[mark..]
            .to_vec()
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

// A card fills the zone its container gives it and is found as `CARD-<node>`;
// its `count` is the state it gives the shell as JSON and takes back.
struct Card {
    node: String,
    count: u64,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
    }
}

impl shell::NodeState for Card {
    fn save_state(&self, _: &gpui::App) -> serde_json::Value {
        json!({"count": self.count})
    }
    fn restore_state(
        &mut self,
        state: serde_json::Value,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) {
        if let Some(count) = state.get("count").and_then(serde_json::Value::as_u64) {
            self.count = count;
        }
    }
}

// The view each node's card got in each window: window ID, then node ID.
type Views = Rc<RefCell<BTreeMap<u64, BTreeMap<String, gpui::WeakEntity<Card>>>>>;

// `host` registers `tour.card` with its state, and places the default
// `yard.row` of three cards.
fn extension(app: &mut TestAppContext) -> (shell::Extension, Views) {
    let views = Views::default();
    let host = app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        let created = views.clone();
        host.register(cx, "tour.card", &[], move |node, _, window, cx| {
            let window_id = window.window_handle().window_id().as_u64();
            let card = cx.new(|_| Card {
                node: node.to_string(),
                count: 0,
            });
            created
                .borrow_mut()
                .entry(window_id)
                .or_default()
                .insert(node.to_string(), card.downgrade());
            card.into()
        });
        host.register_state::<Card>(cx, "tour.card");
        host.place_root(cx, "root", "yard.row");
        for node in ["a", "b", "c"] {
            host.place(cx, node, "tour.card", "root", "children");
        }
        host
    });
    (host, views)
}

fn draw(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) {
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .unwrap();
}

fn open_and_draw(app: &mut TestAppContext) -> gpui::WindowHandle<shell::Shell> {
    let window = app.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), shell::new_shell)
            .unwrap()
    });
    app.simulate_window_resize(window.into(), gpui::size(px(900.), px(600.)));
    draw(window, app);
    window
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

fn window_id(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) -> u64 {
    window
        .update(app, |_, window, _| {
            window.window_handle().window_id().as_u64()
        })
        .unwrap()
}

fn card(views: &Views, window: u64, node: &str) -> gpui::Entity<Card> {
    views.borrow()[&window][node]
        .upgrade()
        .unwrap_or_else(|| panic!("{node} has a view in window {window}"))
}

fn count(views: &Views, app: &mut TestAppContext, window: u64, node: &str) -> u64 {
    let card = card(views, window, node);
    app.update(|cx| card.read(cx).count)
}

fn set_count(views: &Views, app: &mut TestAppContext, window: u64, node: &str, count: u64) {
    let card = card(views, window, node);
    app.update(|cx| card.update(cx, |card, _| card.count = count));
}

// The changes made so far reach the session file: deferred work runs, the
// 100 ms wait of a save passes, and the write itself finishes.
fn settle(app: &mut TestAppContext) {
    app.run_until_parked();
    app.executor().advance_clock(Duration::from_millis(200));
    app.run_until_parked();
}

fn state_json(file: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
}

// The windows of a saved session file, as feature 10's file holds them.
fn write_session_file(file: &Path, windows: usize) {
    let window = json!({
        "display": null,
        "bounds": {"state": "windowed", "x": 0., "y": 0., "width": 800., "height": 600.},
        "layout": "yard.default",
        "layers": {"yard.default": []},
    });
    fs::write(
        file,
        json!({"version": 1, "windows": vec![window; windows]}).to_string(),
    )
    .unwrap();
}

// The windows the opener opened, oldest first.
type Opened = Rc<RefCell<Vec<gpui::WindowHandle<shell::Shell>>>>;

// The opener the composition root gives the shell opens a window as the
// startup opens the main one, and activates it, as the OS does for a window
// it puts on screen.
fn opener(app: &mut TestAppContext) -> Opened {
    let opened = Opened::default();
    let recorded = opened.clone();
    app.update(move |cx| {
        shell::set_window_opener(cx, move |cx| {
            let window = cx
                .open_window(gpui::WindowOptions::default(), shell::new_shell)
                .unwrap();
            window
                .update(cx, |_, window, _| window.activate_window())
                .unwrap();
            recorded.borrow_mut().push(window);
        });
    });
    opened
}

// `yard::NewWindow` dispatched on `window`.
fn dispatch_new_window(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) {
    app.dispatch_action(window.into(), shell::NewWindow);
}

fn user_layer_len(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) -> usize {
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| {
            shell::window_user_layer_len(window, cx)
        })
        .unwrap()
}

// A stateful card counted to 3, the application quits, and a new run with
// the file shows the card at 3. Every node's own state is saved under it,
// so each card gets its own count back.
#[gpui::test]
fn a_card_counted_to_three_quits_and_the_next_run_shows_three(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("state.json");
        let mut app = cx.new_app();
        let (_host, views) = extension(&mut app);
        app.update(|cx| shell::set_state_file(cx, file.clone()));
        let window = open_and_draw(&mut app);
        let id = window_id(window, &mut app);
        set_count(&views, &mut app, id, "a", 3);
        set_count(&views, &mut app, id, "b", 1);
        set_count(&views, &mut app, id, "c", 2);
        app.quit();
        app.run_until_parked();

        // The file: `version` 1 and the one window's nodes, each with its
        // `component` and `state`.
        let json = state_json(&file);
        assert_eq!(json["version"], 1, "{json}");
        let windows = json["windows"]
            .as_array()
            .unwrap_or_else(|| panic!("no `windows` in {json}"));
        assert_eq!(windows.len(), 1, "{json}");
        for (node, count) in [("a", 3), ("b", 1), ("c", 2)] {
            assert_eq!(windows[0][node]["component"], "tour.card", "{windows:?}");
            assert_eq!(windows[0][node]["state"]["count"], count, "{windows:?}");
        }

        let mut restart = cx.new_app();
        let (_host, views) = extension(&mut restart);
        restart.update(|cx| shell::set_state_file(cx, file.clone()));
        let window = open_and_draw(&mut restart);
        let id = window_id(window, &mut restart);
        assert_eq!(count(&views, &mut restart, id, "a"), 3);
        assert_eq!(count(&views, &mut restart, id, "b"), 1);
        assert_eq!(count(&views, &mut restart, id, "c"), 2);
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A node whose saved state was written for another component gets nothing —
// with the `debug` `state skipped {node, saved, component}` — and keeps its
// default. A node whose component matches is restored.
#[gpui::test]
fn a_node_whose_component_changed_gets_no_state(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let file = temp.path().join("state.json");
        fs::write(
            &file,
            json!({
                "version": 1,
                "windows": [{
                    "a": {"component": "tour.other", "state": {"count": 9}},
                    "b": {"component": "tour.card", "state": {"count": 5}},
                }],
            })
            .to_string(),
        )
        .unwrap();
        let mut app = cx.new_app();
        let (_host, views) = extension(&mut app);
        app.update(|cx| shell::set_state_file(cx, file.clone()));
        let mark = trace.mark();
        let window = open_and_draw(&mut app);
        let id = window_id(window, &mut app);
        assert_eq!(
            count(&views, &mut app, id, "a"),
            0,
            "a's saved component is not tour.card"
        );
        assert_eq!(count(&views, &mut app, id, "b"), 5);
        assert_eq!(count(&views, &mut app, id, "c"), 0);
        let lines: Vec<String> = trace
            .since(mark)
            .iter()
            .map(|line| line.replace('"', ""))
            .collect();
        assert!(
            lines.iter().any(|line| {
                line.contains("DEBUG")
                    && line.contains("state skipped")
                    && line.contains("node=a")
                    && line.contains("tour.other")
                    && line.contains("tour.card")
            }),
            "missing `state skipped {{node, saved, component}}` for a: {lines:#?}"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// Each window keeps its own nodes' states, saved in the session's order —
// the main window first — and the window opened i-th from the session gets
// the i-th window's states back.
#[gpui::test]
fn two_windows_keep_their_own_states_in_order(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("state.json");
        let session_file = temp.path().join("session.json");
        let mut app = cx.new_app();
        let (_host, views) = extension(&mut app);
        app.update(|cx| shell::set_state_file(cx, file.clone()));
        let first = open_and_draw(&mut app);
        let first_id = window_id(first, &mut app);
        set_count(&views, &mut app, first_id, "a", 3);
        let second = open_and_draw(&mut app);
        let second_id = window_id(second, &mut app);
        set_count(&views, &mut app, second_id, "a", 7);
        app.quit();
        app.run_until_parked();

        // The file holds both windows in the session's order: the first
        // window, the main one, first.
        let json = state_json(&file);
        let windows = json["windows"]
            .as_array()
            .unwrap_or_else(|| panic!("no `windows` in {json}"));
        assert_eq!(windows.len(), 2, "{json}");
        assert_eq!(windows[0]["a"]["state"]["count"], 3, "{windows:?}");
        assert_eq!(windows[1]["a"]["state"]["count"], 7, "{windows:?}");

        // A new run from saved files: two windows open from the session, in
        // order, and each gets its own window's states.
        write_session_file(&session_file, 2);
        let mut restart = cx.new_app();
        let (_host, views) = extension(&mut restart);
        restart.update(|cx| shell::set_state_file(cx, file.clone()));
        let saved = restart
            .update(|cx| saved_session(cx, &session_file))
            .expect("a saved session");
        assert_eq!(saved.len(), 2, "{saved:?}");
        let first = reopen(&mut restart, &saved[0]);
        let second = reopen(&mut restart, &saved[1]);
        let first_id = window_id(first, &mut restart);
        let second_id = window_id(second, &mut restart);
        assert_eq!(
            count(&views, &mut restart, first_id, "a"),
            3,
            "the first session window did not get the first window's states"
        );
        assert_eq!(
            count(&views, &mut restart, second_id, "a"),
            7,
            "the second session window did not get the second window's states"
        );
        assert_eq!(count(&views, &mut restart, first_id, "b"), 0);
        assert_eq!(count(&views, &mut restart, second_id, "b"), 0);
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A window opened with `yard::NewWindow` gets no state, though the file
// still holds another window's; it has its own user layer.
#[gpui::test]
fn a_window_opened_with_new_window_gets_no_state(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("state.json");
        fs::write(
            &file,
            json!({
                "version": 1,
                "windows": [
                    {"a": {"component": "tour.card", "state": {"count": 3}}},
                    {"a": {"component": "tour.card", "state": {"count": 7}}},
                ],
            })
            .to_string(),
        )
        .unwrap();
        let mut app = cx.new_app();
        let (host, views) = extension(&mut app);
        app.update(|cx| shell::set_state_file(cx, file.clone()));
        let opened = opener(&mut app);
        let main = open_and_draw(&mut app);
        let main_id = window_id(main, &mut app);
        assert_eq!(
            count(&views, &mut app, main_id, "a"),
            3,
            "the one window did not get the first window's states"
        );

        activate(main, &mut app);
        app.update(|cx| host.set_hidden(cx, "b", true));
        dispatch_new_window(main, &mut app);
        let second = opened.borrow().first().copied().unwrap();
        let second_id = window_id(second, &mut app);
        assert_eq!(
            count(&views, &mut app, second_id, "a"),
            0,
            "a window opened with NewWindow got the second window's states"
        );
        // `window_user_layer_len` gives each window's own user layer.
        assert_eq!(user_layer_len(main, &mut app), 1);
        assert_eq!(user_layer_len(second, &mut app), 0);
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A state is given once: a node removed and added again in the same run
// gets a view that starts from its default, not from the saved state.
#[gpui::test]
fn a_view_created_again_in_the_run_starts_from_its_default(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("state.json");
        fs::write(
            &file,
            json!({
                "version": 1,
                "windows": [{"a": {"component": "tour.card", "state": {"count": 5}}}],
            })
            .to_string(),
        )
        .unwrap();
        let mut app = cx.new_app();
        let (host, views) = extension(&mut app);
        app.update(|cx| shell::set_state_file(cx, file.clone()));
        let window = open_and_draw(&mut app);
        let id = window_id(window, &mut app);
        assert_eq!(count(&views, &mut app, id, "a"), 5);
        let previous = views.borrow()[&id]["a"].clone();

        app.update(|cx| host.remove(cx, "a"));
        draw(window, &mut app);
        assert!(previous.upgrade().is_none(), "a's view outlived its node");
        gpui::AnyWindowHandle::from(window)
            .update(&mut app, |_, window, cx| {
                host.add(window, cx, "a", "tour.card", "root", "children")
            })
            .unwrap();
        draw(window, &mut app);
        assert_eq!(
            count(&views, &mut app, id, "a"),
            0,
            "the view created again did not start from its default"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// Closing the last window quits the application, so that window's states
// are taken when it closes, before its views are dropped, and reach the
// file.
#[gpui::test]
fn closing_the_last_window_keeps_its_states_in_the_file(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("state.json");
        let mut app = cx.new_app();
        let (_host, views) = extension(&mut app);
        app.update(|cx| shell::set_state_file(cx, file.clone()));
        let window = open_and_draw(&mut app);
        let id = window_id(window, &mut app);
        set_count(&views, &mut app, id, "a", 3);
        window
            .update(&mut app, |_, window, _| window.remove_window())
            .unwrap();
        settle(&mut app);
        app.quit();
        app.run_until_parked();

        let json = state_json(&file);
        let windows = json["windows"]
            .as_array()
            .unwrap_or_else(|| panic!("no `windows` in {json}"));
        assert_eq!(windows.len(), 1, "{json}");
        assert_eq!(windows[0]["a"]["state"]["count"], 3, "{windows:?}");
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// `register_state` is refused with an `ERROR` for an invalid ID, a
// component outside the extension's namespace, and a second registration;
// a registration that was not refused still gives back its state.
#[gpui::test]
fn register_state_refusals_log_errors_and_valid_ones_still_work(cx: &mut TestAppContext) {
    captured(cx, |cx, _| {
        let temp = TempDir::new();
        let file = temp.path().join("state.json");
        fs::write(
            &file,
            json!({
                "version": 1,
                "windows": [{"a": {"component": "tour.card", "state": {"count": 4}}}],
            })
            .to_string(),
        )
        .unwrap();
        let mut app = cx.new_app();
        let (host, views) = extension(&mut app);
        app.update(|cx| {
            shell::set_state_file(cx, file.clone());
            host.register_state::<Card>(cx, "bad component");
            host.register_state::<Card>(cx, "guest.card");
            // tour.card's state is already registered in `extension`.
            host.register_state::<Card>(cx, "tour.card");
        });
        let errors = errors();
        assert_eq!(errors.len(), 3, "{errors:?}");
        for component in ["bad component", "guest.card", "tour.card"] {
            assert!(
                errors.iter().any(|message| message.contains(component)),
                "no ERROR names {component}: {errors:?}"
            );
        }

        let window = open_and_draw(&mut app);
        let id = window_id(window, &mut app);
        assert_eq!(
            count(&views, &mut app, id, "a"),
            4,
            "the accepted registration did not give back its state"
        );
    });
}
