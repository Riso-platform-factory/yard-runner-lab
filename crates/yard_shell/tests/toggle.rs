use gpui::{AppContext, InteractiveElement, IntoElement, Styled, TestAppContext, px};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use yard_layout::{Operation, decode};
use yard_shell as shell;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-toggle-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
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

fn errors() -> Vec<String> {
    records()
        .into_iter()
        .filter(|(level, _)| *level == log::Level::Error)
        .map(|(_, message)| message)
        .collect()
}

fn warnings() -> Vec<String> {
    records()
        .into_iter()
        .filter(|(level, _)| *level == log::Level::Warn)
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
        let mut captured = self.0.lock().unwrap();
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
        // The UI kit's lines are kept at every level: the only line a test can
        // read a toggle's drawn `toggled` and `disabled` flags from is the
        // `trace` `rendering icon button` of `yard_ui::IconButton`. The shell's
        // own TRACE lines are per-frame reads and are dropped.
        if !(target.starts_with("yard_shell") || target.starts_with("yard_ui"))
            || (target.starts_with("yard_shell")
                && *event.metadata().level() == tracing::Level::TRACE)
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
}

// Every test runs under the one lock and the one subscriber, so no thread
// reaches the shell's or the UI kit's log lines without them.
fn captured(cx: &mut TestAppContext, test: impl FnOnce(&mut TestAppContext, &Trace)) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || test(cx, &trace));
    assert!(trace.0.lock().unwrap().entered.is_empty());
}

// A card fills the zone its container gives it and is found as `CARD-<node>`.
struct Card {
    node: String,
    empty: bool,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
    }
}

impl shell::Emptiable for Card {
    fn is_empty(&self, _: &gpui::App) -> bool {
        self.empty
    }
}

// The view each node got, so a card can be made empty while the test runs.
type Views = Rc<RefCell<BTreeMap<String, gpui::WeakEntity<Card>>>>;

// `host` registers `tour.card`, which can be empty, and the required
// `tour.required`.
fn extension(app: &mut TestAppContext) -> (shell::Extension, Views) {
    let views = Views::default();
    let host = app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        let created = views.clone();
        host.register(cx, "tour.card", &[], move |node, _, _, cx| {
            let card = cx.new(|_| Card {
                node: node.to_string(),
                empty: false,
            });
            created
                .borrow_mut()
                .insert(node.to_string(), card.downgrade());
            card.into()
        });
        host.register_empty::<Card>(cx, "tour.card");
        host.register(cx, "tour.required", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                empty: false,
            })
            .into()
        });
        host.require(cx, "tour.required");
        host
    });
    (host, views)
}

// Draw without forcing a refresh: operations must schedule it themselves.
fn draw(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) {
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| {
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
    window.update(app, |_, window, _| window.refresh()).unwrap();
    draw(window, app);
    window
}

// A toggle's node bounds are the bounds of its button: a click there is a
// click on the button.
fn click_toggle(visual: &mut gpui::VisualTestContext, toggle: &str) {
    let bounds = visual
        .update(|_, cx| shell::node_bounds(cx, toggle))
        .unwrap_or_else(|| panic!("toggle {toggle} is drawn"));
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
}

// Changes a card's state and notifies, without drawing.
fn set_empty(app: &mut TestAppContext, views: &Views, node: &str, empty: bool) {
    app.update(|cx| {
        views
            .borrow()
            .get(node)
            .and_then(|view| view.upgrade())
            .expect("the node has a view")
            .update(cx, |card, cx| {
                card.empty = empty;
                cx.notify();
            });
    });
}

fn icon_buttons(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.contains("rendering icon button"))
        .cloned()
        .collect()
}

// The most recent `rendering icon button` line since `before`: a toggle's
// drawn `toggled` and `disabled` flags.
fn last_button(trace: &Trace, before: usize) -> Option<String> {
    icon_buttons(&trace.since(before)).last().cloned()
}

fn saved_ops(layout_path: &std::path::Path) -> Vec<Operation> {
    decode(&fs::read_to_string(layout_path).unwrap()).unwrap()
}

fn is_hide(operation: &Operation, node: &str) -> bool {
    matches!(operation, Operation::SetHidden { node: id, hidden: true } if id.as_str() == node)
}

fn is_show(operation: &Operation, node: &str) -> bool {
    matches!(operation, Operation::SetHidden { node: id, hidden: false } if id.as_str() == node)
}

// A toggle shows its node when hidden and hides it when shown, with one
// `show` or `hide` operation a click, and is drawn toggled while its node is
// shown and not empty.
#[gpui::test]
fn a_toggle_shows_and_hides_its_node_with_one_operation_and_is_toggled_while_shown(
    cx: &mut TestAppContext,
) {
    captured(cx, |cx, trace| {
        let mut app = cx.new_app();
        let (host, views) = extension(&mut app);
        app.update(|cx| {
            host.place_root(cx, "root", "yard.row");
            host.place(cx, "left", "tour.card", "root", "children");
            host.place(cx, "main", "tour.card", "root", "children");
            host.place_with_props(
                cx,
                "toggle_left",
                "yard.toggle",
                "status_bar",
                "left",
                json!({"node": "left", "icon": "info", "tooltip": "Left Panel"}),
            );
        });
        let window = open_and_draw(&mut app);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);
        // `left` is shown from the start: the button is drawn toggled.
        let before = trace.mark();
        draw(window, &mut app);
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=true"), "{button}");
        assert!(button.contains("disabled=false"), "{button}");

        // A click hides it, with one `hide` operation, and the button follows.
        click_toggle(&mut visual, "toggle_left");
        assert!(app.update(|cx| shell::is_hidden(cx, "left")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 1);
        let before = trace.mark();
        draw(window, &mut app);
        assert!(visual.debug_bounds("CARD-left").is_none());
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=false"), "{button}");

        // A click shows it again, with one `show` operation.
        click_toggle(&mut visual, "toggle_left");
        assert!(!app.update(|cx| shell::is_hidden(cx, "left")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
        let before = trace.mark();
        draw(window, &mut app);
        assert!(visual.debug_bounds("CARD-left").is_some());
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=true"), "{button}");

        // Toggled reads the tree: an empty node is shown, not hidden, and yet
        // its button is not toggled.
        set_empty(&mut app, &views, "left", true);
        let before = trace.mark();
        draw(window, &mut app);
        assert!(app.update(|cx| shell::is_empty(cx, "left")));
        assert!(!app.update(|cx| shell::is_hidden(cx, "left")));
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=false"), "{button}");

        // And a click on the shown-but-empty node still hides it.
        click_toggle(&mut visual, "toggle_left");
        assert!(app.update(|cx| shell::is_hidden(cx, "left")));
        set_empty(&mut app, &views, "left", false);
        click_toggle(&mut visual, "toggle_left");
        assert!(!app.update(|cx| shell::is_hidden(cx, "left")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 4);
        let before = trace.mark();
        draw(window, &mut app);
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=true"), "{button}");
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// With `alone`, showing a node hides the other nodes shown in the same slot
// of the same parent, each with a `hide` operation, before showing it.
#[gpui::test]
fn with_alone_showing_a_node_hides_the_other_shown_nodes_of_its_slot_first(
    cx: &mut TestAppContext,
) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let layout_path = temp.path().join("data").join("layout.json");
        let mut app = cx.new_app();
        app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
        let (host, _views) = extension(&mut app);
        app.update(|cx| {
            host.place_root(cx, "root", "yard.row");
            host.place(cx, "side", "yard.column", "root", "children");
            host.place(cx, "left", "tour.card", "side", "children");
            host.place(cx, "extra", "tour.card", "side", "children");
            host.place(cx, "main", "tour.card", "root", "children");
            host.place_with_props(
                cx,
                "toggle_extra",
                "yard.toggle",
                "status_bar",
                "left",
                json!({"node": "extra", "icon": "plus", "alone": true}),
            );
            host.set_hidden(cx, "extra", true);
        });
        let window = open_and_draw(&mut app);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);
        assert!(app.update(|cx| shell::is_hidden(cx, "extra")));
        assert!(!app.update(|cx| shell::is_hidden(cx, "left")));
        // `left` and `extra` share the slot of `side`: showing `extra` hides
        // `left` first, then shows `extra` — a `hide` and a `show`, in that
        // order.
        click_toggle(&mut visual, "toggle_extra");
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
        // The `hide` of `left` is recorded before the `show` of `extra`.
        let ops = saved_ops(&layout_path);
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(is_hide(&ops[0], "left"), "{ops:?}");
        assert!(is_show(&ops[1], "extra"), "{ops:?}");
        assert!(app.update(|cx| shell::is_hidden(cx, "left")));
        assert!(!app.update(|cx| shell::is_hidden(cx, "extra")));
        let before = trace.mark();
        draw(window, &mut app);
        assert!(visual.debug_bounds("CARD-left").is_none());
        assert!(visual.debug_bounds("CARD-extra").is_some());
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=true"), "{button}");

        // A click on the now-shown node hides it again, one `hide` operation.
        click_toggle(&mut visual, "toggle_extra");
        assert!(app.update(|cx| shell::is_hidden(cx, "extra")));
        assert!(app.update(|cx| shell::is_hidden(cx, "left")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 3);
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A toggle of a required node cannot hide it: the `INFO`, and the toggle
// stays toggled.
#[gpui::test]
fn a_toggle_of_a_required_node_cannot_hide_it(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let mut app = cx.new_app();
        let (host, _views) = extension(&mut app);
        app.update(|cx| {
            host.place_root(cx, "root", "yard.row");
            host.place(cx, "pinned", "tour.required", "root", "children");
            host.place(cx, "main", "tour.card", "root", "children");
            host.place_with_props(
                cx,
                "toggle_pinned",
                "yard.toggle",
                "status_bar",
                "left",
                json!({"node": "pinned", "icon": "info"}),
            );
        });
        let window = open_and_draw(&mut app);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);
        let before = trace.mark();
        click_toggle(&mut visual, "toggle_pinned");
        let lines = trace.since(before);
        assert!(
            lines.iter().any(|line| line
                .starts_with(r#"layout-op op="hide" node="pinned" extension="yard": INFO"#)
                && line.contains("cannot be hidden: it is required")),
            "missing the refused hide of pinned in {lines:#?}"
        );
        assert!(!app.update(|cx| shell::is_hidden(cx, "pinned")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
        // The toggle follows the real tree: `pinned` is still shown, so the
        // button is still toggled.
        let before = trace.mark();
        draw(window, &mut app);
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=true"), "{button}");
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A toggle that names a node not in the tree, itself, or a node above it is
// drawn disabled and warns once, and a click on it runs no operation.
#[gpui::test]
fn a_toggle_naming_a_missing_node_itself_or_an_ancestor_is_disabled(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let mut app = cx.new_app();
        let (host, _views) = extension(&mut app);
        app.update(|cx| {
            host.place_root(cx, "root", "yard.row");
            host.place(cx, "left", "tour.card", "root", "children");
            host.place(cx, "main", "tour.card", "root", "children");
            // `toggle_up` is placed under `root`, so `root` is above it.
            host.place_with_props(
                cx,
                "toggle_up",
                "yard.toggle",
                "root",
                "children",
                json!({"node": "root", "icon": "info"}),
            );
            host.place_with_props(
                cx,
                "toggle_ghost",
                "yard.toggle",
                "status_bar",
                "left",
                json!({"node": "ghost", "icon": "info"}),
            );
            host.place_with_props(
                cx,
                "toggle_self",
                "yard.toggle",
                "status_bar",
                "left",
                json!({"node": "toggle_self", "icon": "info"}),
            );
        });
        let window = open_and_draw(&mut app);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);
        // Every one is drawn and disabled, and its WARN is logged once — a
        // second frame warns nothing more.
        for toggle in ["toggle_ghost", "toggle_self", "toggle_up"] {
            assert!(
                visual
                    .update(|_, cx| shell::node_bounds(cx, toggle))
                    .is_some(),
                "{toggle} is drawn"
            );
        }
        let before = trace.mark();
        draw(window, &mut app);
        draw(window, &mut app);
        let buttons = icon_buttons(&trace.since(before));
        assert!(!buttons.is_empty(), "{buttons:?}");
        assert!(
            buttons.iter().all(|line| line.contains("disabled=true")),
            "every bad toggle is drawn disabled: {buttons:?}"
        );
        let warns = warnings();
        assert_eq!(warns.len(), 3, "{warns:?}");
        // The WARN names the bad toggle or its target; either way it names one.
        for (toggle, target) in [
            ("toggle_ghost", "ghost"),
            ("toggle_self", "toggle_self"),
            ("toggle_up", "root"),
        ] {
            assert!(
                warns
                    .iter()
                    .any(|warn| warn.contains(toggle) || warn.contains(target)),
                "missing a WARN for {toggle} in {warns:?}"
            );
        }
        // A click on a disabled toggle runs no operation.
        for toggle in ["toggle_ghost", "toggle_self", "toggle_up"] {
            click_toggle(&mut visual, toggle);
        }
        assert!(!app.update(|cx| shell::is_hidden(cx, "left")));
        assert!(!app.update(|cx| shell::is_hidden(cx, "root")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// The toggle's state follows a hide or a show made by another path: an
// extension's call, a snap, and a reset.
#[gpui::test]
fn a_toggles_state_follows_hides_and_shows_made_by_other_paths(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let mut app = cx.new_app();
        let (host, _views) = extension(&mut app);
        app.update(|cx| {
            host.place_root_with_props(cx, "root", "yard.row", json!({"sizes": {"left": 240}}));
            host.place(cx, "left", "tour.card", "root", "children");
            host.place(cx, "main", "tour.card", "root", "children");
            host.place_with_props(
                cx,
                "toggle_left",
                "yard.toggle",
                "status_bar",
                "left",
                json!({"node": "left", "icon": "info"}),
            );
        });
        let window = open_and_draw(&mut app);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);

        // An extension's call hides and shows `left`; the button follows.
        app.update(|cx| host.set_hidden(cx, "left", true));
        let before = trace.mark();
        draw(window, &mut app);
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=false"), "{button}");
        app.update(|cx| host.set_hidden(cx, "left", false));
        let before = trace.mark();
        draw(window, &mut app);
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=true"), "{button}");

        // A snap: `left` is dragged shut past half its minimum, and the toggle
        // follows. Dragging its collapsed line back out shows it again.
        let at = {
            let bounds = visual.debug_bounds("CARD-left").expect("left draws");
            gpui::point(bounds.right(), bounds.center().y)
        };
        let target = gpui::point(px(30.), at.y);
        visual.simulate_mouse_move(at, None, Default::default());
        visual.simulate_mouse_down(at, gpui::MouseButton::Left, Default::default());
        visual.simulate_mouse_move(target, gpui::MouseButton::Left, Default::default());
        visual.simulate_mouse_up(target, gpui::MouseButton::Left, Default::default());
        assert!(app.update(|cx| shell::is_hidden(cx, "left")));
        let before = trace.mark();
        draw(window, &mut app);
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=false"), "{button}");
        let at = {
            let bounds = visual.debug_bounds("CARD-main").expect("main draws");
            gpui::point(bounds.left(), bounds.center().y)
        };
        let target = gpui::point(px(240.), at.y);
        visual.simulate_mouse_move(at, None, Default::default());
        visual.simulate_mouse_down(at, gpui::MouseButton::Left, Default::default());
        visual.simulate_mouse_move(target, gpui::MouseButton::Left, Default::default());
        visual.simulate_mouse_up(target, gpui::MouseButton::Left, Default::default());
        assert!(!app.update(|cx| shell::is_hidden(cx, "left")));
        let before = trace.mark();
        draw(window, &mut app);
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=true"), "{button}");

        // A reset drops the toggle's own hide with the user layer, and the
        // button follows the default back.
        click_toggle(&mut visual, "toggle_left");
        assert!(app.update(|cx| shell::is_hidden(cx, "left")));
        gpui::AnyWindowHandle::from(window)
            .update(&mut app, |_, window, cx| host.reset_layout(window, cx))
            .unwrap();
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
        assert!(!app.update(|cx| shell::is_hidden(cx, "left")));
        let before = trace.mark();
        draw(window, &mut app);
        let button = last_button(trace, before).expect("the toggle's button is drawn");
        assert!(button.contains("toggled=true"), "{button}");
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// The operations a toggle records are saved and restored at the next start,
// and the toggles' state follows the restored tree.
#[gpui::test]
fn the_toggles_operations_are_saved_and_restored(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let layout_path = temp.path().join("data").join("layout.json");
        // `left` and `extra` share the slot of `side`, `extra` hidden at
        // start; a toggle for each, both `alone`.
        let place = |app: &mut TestAppContext, host: &shell::Extension| {
            app.update(|cx| {
                host.place_root(cx, "root", "yard.row");
                host.place(cx, "side", "yard.column", "root", "children");
                host.place(cx, "left", "tour.card", "side", "children");
                host.place(cx, "extra", "tour.card", "side", "children");
                host.place(cx, "main", "tour.card", "root", "children");
                host.place_with_props(
                    cx,
                    "toggle_left",
                    "yard.toggle",
                    "status_bar",
                    "left",
                    json!({"node": "left", "icon": "info", "alone": true}),
                );
                host.place_with_props(
                    cx,
                    "toggle_extra",
                    "yard.toggle",
                    "status_bar",
                    "left",
                    json!({"node": "extra", "icon": "plus", "alone": true}),
                );
                host.set_hidden(cx, "extra", true);
            });
        };
        {
            let mut app = cx.new_app();
            app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
            let (host, _views) = extension(&mut app);
            place(&mut app, &host);
            let window = open_and_draw(&mut app);
            let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);
            click_toggle(&mut visual, "toggle_extra");
            assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
            // The `hide` of `left` is saved before the `show` of `extra`.
            let ops = saved_ops(&layout_path);
            assert_eq!(ops.len(), 2, "{ops:?}");
            assert!(is_hide(&ops[0], "left"), "{ops:?}");
            assert!(is_show(&ops[1], "extra"), "{ops:?}");
        }
        {
            let mut app = cx.new_app();
            app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
            let (host, _views) = extension(&mut app);
            place(&mut app, &host);
            let window = open_and_draw(&mut app);
            // The layer is restored: `extra` shown and `left` hidden, and the
            // toggles follow — exactly one of them is toggled.
            assert!(app.update(|cx| shell::is_hidden(cx, "left")));
            assert!(!app.update(|cx| shell::is_hidden(cx, "extra")));
            assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
            let before = trace.mark();
            draw(window, &mut app);
            let buttons = icon_buttons(&trace.since(before));
            assert_eq!(
                buttons
                    .iter()
                    .filter(|line| line.contains("toggled=true"))
                    .count(),
                1,
                "{buttons:?}"
            );
            // A reset drops the layer: `left` shown and `extra` hidden again.
            gpui::AnyWindowHandle::from(window)
                .update(&mut app, |_, window, cx| host.reset_layout(window, cx))
                .unwrap();
            assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
            assert!(!app.update(|cx| shell::is_hidden(cx, "left")));
            assert!(app.update(|cx| shell::is_hidden(cx, "extra")));
            let before = trace.mark();
            draw(window, &mut app);
            let buttons = icon_buttons(&trace.since(before));
            assert_eq!(
                buttons
                    .iter()
                    .filter(|line| line.contains("toggled=true"))
                    .count(),
                1,
                "{buttons:?}"
            );
        }
        assert!(errors().is_empty(), "{:?}", errors());
    });
}
