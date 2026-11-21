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
use yard_shell as shell;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-toggle-rules-{}-{}-{nanos:x}",
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

// The `toggled` flag of one toggle's button, from the last `rendering icon
// button` line since `before` whose `id` is that toggle's.
fn toggled_of(trace: &Trace, before: usize, toggle: &str) -> Option<bool> {
    let id = format!("id=toggle:{toggle} ");
    icon_buttons(&trace.since(before))
        .into_iter()
        .rev()
        .find(|line| line.contains(&id))
        .map(|line| line.contains("toggled=true"))
}

// The titles of the tooltips shown since the trace began, from their
// `tooltip-show{title}` groups.
fn tooltips(trace: &Trace) -> Vec<String> {
    trace
        .0
        .lock()
        .unwrap()
        .spans
        .values()
        .filter_map(|label| label.strip_prefix("tooltip-show title=").map(str::to_owned))
        .collect()
}

// A card that gives its tab title.
struct TitledCard;

impl gpui::Render for TitledCard {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div().size_full()
    }
}

impl shell::Titled for TitledCard {
    fn title(&self, _: &gpui::App) -> gpui::SharedString {
        "Titled Node".into()
    }
}

fn hover(visual: &mut gpui::VisualTestContext, toggle: &str) {
    let bounds = visual
        .update(|_, cx| shell::node_bounds(cx, toggle))
        .unwrap_or_else(|| panic!("toggle {toggle} is drawn"));
    visual.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(2));
    visual.run_until_parked();
}

// A problem that does not change warns once, whatever props update comes in
// between; a problem that changes warns again.
#[gpui::test]
fn a_toggle_warns_once_per_problem_across_props_updates(cx: &mut TestAppContext) {
    captured(cx, |cx, _trace| {
        let mut app = cx.new_app();
        let (host, _views) = extension(&mut app);
        app.update(|cx| {
            host.place_root(cx, "root", "yard.row");
            host.place(cx, "main", "tour.card", "root", "children");
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
        let count = |toggle: &str| {
            warnings()
                .iter()
                .filter(|warn| warn.contains(toggle))
                .count()
        };
        assert_eq!(count("toggle_ghost"), 1, "{:?}", warnings());
        assert_eq!(count("toggle_self"), 1, "{:?}", warnings());

        // Only the tooltip changes: the missing target is the same problem.
        app.update(|cx| host.set_props(cx, "toggle_ghost", json!({"tooltip": "Ghost"})));
        app.update(|cx| host.set_props(cx, "toggle_ghost", json!({"alone": true})));
        draw(window, &mut app);
        draw(window, &mut app);
        assert_eq!(count("toggle_ghost"), 1, "{:?}", warnings());

        // Naming itself, then a missing node: two problems, two WARNs.
        app.update(|cx| host.set_props(cx, "toggle_self", json!({"node": "nowhere"})));
        draw(window, &mut app);
        draw(window, &mut app);
        assert_eq!(count("toggle_self"), 2, "{:?}", warnings());
        assert!(
            warnings().iter().any(|warn| warn.contains("nowhere")),
            "{:?}",
            warnings()
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// The tooltip is the node's title when it has one, else its ID, and the
// `tooltip` prop overrides both.
#[gpui::test]
fn a_toggles_tooltip_is_its_prop_else_the_nodes_title_else_its_id(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let mut app = cx.new_app();
        let (host, _views) = extension(&mut app);
        app.update(|cx| {
            host.register(cx, "tour.titled", &[], |_, _, _, cx| {
                cx.new(|_| TitledCard).into()
            });
            host.register_title::<TitledCard>(cx, "tour.titled");
            host.place_root(cx, "root", "yard.row");
            host.place(cx, "titled", "tour.titled", "root", "children");
            host.place(cx, "plain", "tour.card", "root", "children");
            for (toggle, props) in [
                ("toggle_titled", json!({"node": "titled", "icon": "info"})),
                ("toggle_plain", json!({"node": "plain", "icon": "plus"})),
                (
                    "toggle_custom",
                    json!({"node": "plain", "icon": "warning", "tooltip": "Custom Tip"}),
                ),
            ] {
                host.place_with_props(cx, toggle, "yard.toggle", "status_bar", "left", props);
            }
        });
        let window = open_and_draw(&mut app);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);
        for (toggle, title) in [
            ("toggle_titled", "Titled Node"),
            ("toggle_plain", "plain"),
            ("toggle_custom", "Custom Tip"),
        ] {
            visual.simulate_mouse_move(gpui::point(px(450.), px(200.)), None, Default::default());
            visual.run_until_parked();
            hover(&mut visual, toggle);
            let shown = tooltips(trace);
            assert!(
                shown.iter().any(|shown| shown == title),
                "{toggle} shows the tooltip {title}: {shown:?}"
            );
        }
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// With `alone`, a required sibling cannot be hidden: its `hide` is refused
// with the INFO of required nodes, it stays shown, and the node is shown.
#[gpui::test]
fn with_alone_a_required_sibling_stays_shown_and_the_node_is_still_shown(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let mut app = cx.new_app();
        let (host, _views) = extension(&mut app);
        app.update(|cx| {
            host.place_root(cx, "root", "yard.row");
            host.place(cx, "side", "yard.column", "root", "children");
            host.place(cx, "pinned", "tour.required", "side", "children");
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
        let before = trace.mark();
        click_toggle(&mut visual, "toggle_extra");
        let lines = trace.since(before);
        assert!(
            lines.iter().any(|line| line
                .starts_with(r#"layout-op op="hide" node="pinned" extension="yard": INFO"#)
                && line.contains("cannot be hidden: it is required")),
            "missing the refused hide of pinned in {lines:#?}"
        );
        assert!(!app.update(|cx| shell::is_hidden(cx, "pinned")));
        assert!(!app.update(|cx| shell::is_hidden(cx, "extra")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 1);
        let before = trace.mark();
        draw(window, &mut app);
        assert!(visual.debug_bounds("CARD-pinned").is_some());
        assert!(visual.debug_bounds("CARD-extra").is_some());
        assert_eq!(toggled_of(trace, before, "toggle_extra"), Some(true));
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// A node that stops being empty without any operation, only its view
// notifying, makes its toggle toggled on the next draw.
#[gpui::test]
fn a_toggle_follows_emptiness_that_changes_without_an_operation(cx: &mut TestAppContext) {
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
                json!({"node": "left", "icon": "info"}),
            );
        });
        let window = open_and_draw(&mut app);
        set_empty(&mut app, &views, "left", true);
        let before = trace.mark();
        draw(window, &mut app);
        assert_eq!(toggled_of(trace, before, "toggle_left"), Some(false));
        set_empty(&mut app, &views, "left", false);
        let before = trace.mark();
        draw(window, &mut app);
        assert_eq!(toggled_of(trace, before, "toggle_left"), Some(true));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
        assert!(errors().is_empty(), "{:?}", errors());
    });
}

// After a restore and after a reset, each toggle is toggled exactly as its
// own node is shown, read by the button's ID.
#[gpui::test]
fn each_toggle_follows_its_node_after_restore_and_reset(cx: &mut TestAppContext) {
    captured(cx, |cx, trace| {
        let temp = TempDir::new();
        let layout_path = temp.path().join("data").join("layout.json");
        let place = |app: &mut TestAppContext, host: &shell::Extension| {
            app.update(|cx| {
                host.place_root(cx, "root", "yard.row");
                host.place(cx, "side", "yard.column", "root", "children");
                host.place(cx, "left", "tour.card", "side", "children");
                host.place(cx, "extra", "tour.card", "side", "children");
                host.place(cx, "main", "tour.card", "root", "children");
                host.place(cx, "right", "tour.card", "root", "children");
                for (toggle, node, alone) in [
                    ("toggle_left", "left", true),
                    ("toggle_extra", "extra", true),
                    ("toggle_right", "right", false),
                ] {
                    host.place_with_props(
                        cx,
                        toggle,
                        "yard.toggle",
                        "status_bar",
                        "left",
                        json!({"node": node, "icon": "info", "alone": alone}),
                    );
                }
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
            click_toggle(&mut visual, "toggle_right");
            assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 3);
        }
        {
            let mut app = cx.new_app();
            app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
            let (host, _views) = extension(&mut app);
            place(&mut app, &host);
            let window = open_and_draw(&mut app);
            let before = trace.mark();
            draw(window, &mut app);
            assert_eq!(toggled_of(trace, before, "toggle_left"), Some(false));
            assert_eq!(toggled_of(trace, before, "toggle_extra"), Some(true));
            assert_eq!(toggled_of(trace, before, "toggle_right"), Some(false));
            gpui::AnyWindowHandle::from(window)
                .update(&mut app, |_, window, cx| host.reset_layout(window, cx))
                .unwrap();
            let before = trace.mark();
            draw(window, &mut app);
            assert_eq!(toggled_of(trace, before, "toggle_left"), Some(true));
            assert_eq!(toggled_of(trace, before, "toggle_extra"), Some(false));
            assert_eq!(toggled_of(trace, before, "toggle_right"), Some(true));
        }
        assert!(errors().is_empty(), "{:?}", errors());
    });
}
