use gpui::{AppContext, IntoElement, ParentElement, Styled, TestAppContext, px};
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
use yard_layout::NodeId;
use yard_shell as shell;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-contexts-{}-{}-{nanos:x}",
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

fn errors() -> Vec<String> {
    RECORDS
        .lock()
        .unwrap()
        .iter()
        .filter(|(level, _)| *level == log::Level::Error)
        .map(|(_, message)| message.clone())
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
        if !(target.starts_with("yard_shell") || target.starts_with("yard_layout"))
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

// The model a `tour.counter` node gives to the nodes under it: a number.
struct Count(u32);

fn counter_value(node: &NodeId) -> u32 {
    match node.as_str() {
        "counter" => 10,
        "inner" => 11,
        "counter2" => 20,
        _ => 0,
    }
}

type Shown = Rc<RefCell<Vec<(String, Option<u32>)>>>;

// A view that asks for a Count on every draw and records what it receives.
struct Asking {
    node: String,
    shown: Shown,
}

impl gpui::Render for Asking {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let value = shell::context::<Count>(cx, &self.node).map(|model| model.read(cx).0);
        let node = self.node.clone();
        let shown = self.shown.clone();
        gpui::div().size_full().child(
            gpui::canvas(
                move |_, _, _| shown.borrow_mut().push((node, value)),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
    }
}

// A view that draws its `body` slot.
struct Container {
    node: String,
    slots: shell::Slots,
    shown: Shown,
}

impl gpui::Render for Container {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let node = self.node.clone();
        let shown = self.shown.clone();
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                gpui::canvas(
                    move |_, _, _| shown.borrow_mut().push((node, None)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(
                self.slots
                    .views("body")
                    .into_iter()
                    .map(|view| gpui::div().flex_1().min_h_0().child(view)),
            )
    }
}

fn draw(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext, shown: &Shown) {
    shown.borrow_mut().clear();
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .unwrap();
}

fn open_and_draw(app: &mut TestAppContext, shown: &Shown) -> gpui::WindowHandle<shell::Shell> {
    let window = app.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), shell::new_shell)
            .unwrap()
    });
    app.simulate_window_resize(window.into(), gpui::size(px(900.), px(600.)));
    window.update(app, |_, window, _| window.refresh()).unwrap();
    draw(window, app, shown);
    window
}

fn names(shown: &Shown) -> Vec<String> {
    shown
        .borrow()
        .iter()
        .map(|(node, _)| node.clone())
        .collect()
}

// The model value a node's view last recorded, or none if it recorded none.
fn seen(shown: &Shown, node: &str) -> Option<u32> {
    shown
        .borrow()
        .iter()
        .rev()
        .find(|(name, _)| name == node)
        .and_then(|(_, value)| *value)
}

// Every node of the default tree, in draw order.
fn expected_tree() -> Vec<String> {
    [
        "counter", "left", "right", "inner", "deep", "twin", "pair", "needy", "carrier",
        "counter2", "deep2", "other", "outside",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

// Asserts an INFO event holding `message` inside the group `group`.
fn assert_info_in(lines: &[String], group: &str, message: &str) {
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(&format!("{group}: INFO")) && line.contains(message)),
        "missing INFO {message:?} in {group} in {lines:#?}"
    );
}

// The default tree: `counter` gives a Count(10) to `left`, `right`, `inner`,
// and `needy` under `pair`; `inner` gives a Count(11) to `deep` and `twin`;
// `counter2` gives a Count(20) to `deep2` inside `carrier`; `other` is a plain
// container and `outside` asks for a Count that nothing gives.
fn setup(app: &mut TestAppContext) -> (shell::Extension, shell::Extension, Shown) {
    let shown = Shown::default();
    let (host, guest) = app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        let guest = shell::Extension::new(cx, "guest").unwrap();
        let counter_shown = shown.clone();
        host.register(cx, "tour.counter", &["body"], move |node, slots, _, cx| {
            cx.new(|_| Container {
                node: node.to_string(),
                slots,
                shown: counter_shown.clone(),
            })
            .into()
        });
        host.provide::<Count>(cx, "tour.counter", |node, cx| {
            cx.new(|_| Count(counter_value(node)))
        });
        let card_shown = shown.clone();
        host.register(cx, "tour.card", &["body"], move |node, slots, _, cx| {
            cx.new(|_| Container {
                node: node.to_string(),
                slots,
                shown: card_shown.clone(),
            })
            .into()
        });
        let count_shown = shown.clone();
        host.register(cx, "tour.count_card", &[], move |node, _, _, cx| {
            cx.new(|_| Asking {
                node: node.to_string(),
                shown: count_shown.clone(),
            })
            .into()
        });
        host.require_context::<Count>(cx, "tour.count_card");
        let leaf_shown = shown.clone();
        host.register(cx, "tour.leaf", &[], move |node, _, _, cx| {
            cx.new(|_| Asking {
                node: node.to_string(),
                shown: leaf_shown.clone(),
            })
            .into()
        });
        guest.register(cx, "guest.card", &[], |_, _, _, cx| {
            cx.new(|_| gpui::Empty).into()
        });
        host.place_root(cx, "root", "yard.row");
        host.place(cx, "counter", "tour.counter", "root", "children");
        host.place(cx, "left", "tour.count_card", "counter", "body");
        host.place(cx, "right", "tour.count_card", "counter", "body");
        host.place(cx, "inner", "tour.counter", "counter", "body");
        host.place(cx, "deep", "tour.count_card", "inner", "body");
        host.place(cx, "twin", "tour.count_card", "inner", "body");
        host.place(cx, "pair", "tour.card", "counter", "body");
        host.place(cx, "needy", "tour.count_card", "pair", "body");
        host.place(cx, "carrier", "tour.card", "root", "children");
        host.place(cx, "counter2", "tour.counter", "carrier", "body");
        host.place(cx, "deep2", "tour.count_card", "counter2", "body");
        host.place(cx, "other", "tour.card", "root", "children");
        host.place(cx, "outside", "tour.leaf", "root", "children");
        (host, guest)
    });
    (host, guest, shown)
}

#[gpui::test]
fn two_nodes_under_one_giving_node_share_its_model_and_a_change_shows_in_both(
    cx: &mut TestAppContext,
) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (_host, _guest, shown) = setup(&mut app);
    let window = open_and_draw(&mut app, &shown);
    let (left, right) = app.update(|cx| {
        (
            shell::context::<Count>(cx, "left").unwrap(),
            shell::context::<Count>(cx, "right").unwrap(),
        )
    });
    // One model for both: counter's, the giving node above them.
    assert_eq!(left.entity_id(), right.entity_id());
    assert_eq!(app.update(|cx| left.read(cx).0), 10);
    assert_eq!(seen(&shown, "left"), Some(10));
    assert_eq!(seen(&shown, "right"), Some(10));
    // A change to it shows in both.
    app.update(|cx| left.update(cx, |count, _| count.0 = 2));
    draw(window, &mut app, &shown);
    assert_eq!(seen(&shown, "left"), Some(2));
    assert_eq!(seen(&shown, "right"), Some(2));
    // `needy` under `pair` receives the same model.
    let needy = app.update(|cx| shell::context::<Count>(cx, "needy").unwrap());
    assert_eq!(needy.entity_id(), left.entity_id());
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_node_under_two_giving_nodes_receives_the_nearest_ones_model(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (_host, _guest, shown) = setup(&mut app);
    let _window = open_and_draw(&mut app, &shown);
    let (deep, twin, left, deep2) = app.update(|cx| {
        (
            shell::context::<Count>(cx, "deep").unwrap(),
            shell::context::<Count>(cx, "twin").unwrap(),
            shell::context::<Count>(cx, "left").unwrap(),
            shell::context::<Count>(cx, "deep2").unwrap(),
        )
    });
    // deep and twin share inner's model, not counter's above them all and not
    // counter2's in another subtree.
    assert_eq!(deep.entity_id(), twin.entity_id());
    assert_ne!(deep.entity_id(), left.entity_id());
    assert_ne!(deep.entity_id(), deep2.entity_id());
    assert_eq!(app.update(|cx| deep.read(cx).0), 11);
    assert_eq!(app.update(|cx| left.read(cx).0), 10);
    assert_eq!(app.update(|cx| deep2.read(cx).0), 20);
    assert_eq!(seen(&shown, "deep"), Some(11));
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_node_with_no_giving_node_above_receives_the_windows_model_or_none(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    let window = open_and_draw(&mut app, &shown);
    // Nothing gives a Count above outside, and the window gives none.
    assert!(
        app.update(|cx| shell::context::<Count>(cx, "outside"))
            .is_none()
    );
    // A type nothing gives is none anywhere.
    assert!(
        app.update(|cx| shell::context::<String>(cx, "left"))
            .is_none()
    );
    // The window is given a model.
    let window_model = app.update(|cx| {
        let model = cx.new(|_| Count(77));
        host.provide_window::<Count>(cx, model.clone());
        model
    });
    assert_eq!(
        app.update(|cx| shell::context::<Count>(cx, "outside").unwrap().entity_id()),
        window_model.entity_id()
    );
    // The nearest giving node still wins over the window's model.
    let left = app.update(|cx| shell::context::<Count>(cx, "left").unwrap());
    assert_ne!(left.entity_id(), window_model.entity_id());
    assert_eq!(app.update(|cx| left.read(cx).0), 10);
    // The asking view draws the window's model too.
    draw(window, &mut app, &shown);
    assert_eq!(seen(&shown, "outside"), Some(77));
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn after_a_move_under_another_giving_node_context_returns_that_nodes_model(
    cx: &mut TestAppContext,
) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    let window = open_and_draw(&mut app, &shown);
    let counter_model = app.update(|cx| shell::context::<Count>(cx, "left").unwrap());
    assert_eq!(app.update(|cx| counter_model.read(cx).0), 10);
    app.update(|cx| host.move_node(cx, "left", "counter2", "body", 0));
    draw(window, &mut app, &shown);
    // `context` returns counter2's model now, the one deep2 receives.
    let (left, deep2) = app.update(|cx| {
        (
            shell::context::<Count>(cx, "left").unwrap(),
            shell::context::<Count>(cx, "deep2").unwrap(),
        )
    });
    assert_eq!(left.entity_id(), deep2.entity_id());
    assert_ne!(left.entity_id(), counter_model.entity_id());
    assert_eq!(app.update(|cx| left.read(cx).0), 20);
    // And the view drew the model of its new place.
    assert_eq!(seen(&shown, "left"), Some(20));
    assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 1);
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn placing_adding_and_moving_a_requiring_node_without_a_model_above_are_refused(
    cx: &mut TestAppContext,
) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, _guest, shown) = setup(&mut app);
        // A place at startup is refused: stray is never in the tree.
        app.update(|cx| host.place(cx, "stray", "tour.count_card", "root", "children"));
        let window = open_and_draw(&mut app, &shown);
        assert_eq!(names(&shown), expected_tree());
        let any_window = gpui::AnyWindowHandle::from(window);
        any_window
            .update(&mut app, |_, window, cx| {
                host.add(window, cx, "added", "tour.count_card", "root", "children");
            })
            .unwrap();
        app.update(|cx| {
            host.move_node(cx, "left", "root", "children", 0);
            // `pair` carries `needy`: moving it out from under the model is
            // refused too.
            host.move_node(cx, "pair", "root", "children", 0);
        });
        draw(window, &mut app, &shown);
        // The tree is unchanged and nothing was recorded in the user layer.
        assert_eq!(names(&shown), expected_tree());
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
        // left and needy still receive counter's model.
        for node in ["left", "needy"] {
            assert_eq!(
                app.update(|cx| shell::context::<Count>(cx, node).unwrap().read(cx).0),
                10
            );
        }
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    for (group, message) in [
        (
            r#"node-place node="stray" component="tour.count_card" parent="root" slot="children""#,
            "stray requires a Count model, and none is above root",
        ),
        (
            r#"layout-op op="add" node="added" extension="tour""#,
            "added requires a Count model, and none is above root",
        ),
        (
            r#"layout-op op="move" node="left" extension="tour""#,
            "left requires a Count model, and none is above root",
        ),
        (
            r#"layout-op op="move" node="pair" extension="tour""#,
            "needy requires a Count model, and none is above root",
        ),
    ] {
        assert_info_in(&captured.lines, group, message);
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn moving_a_giving_nodes_parent_carrying_its_requiring_children_is_allowed(
    cx: &mut TestAppContext,
) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    let window = open_and_draw(&mut app, &shown);
    let before = app.update(|cx| shell::context::<Count>(cx, "deep2").unwrap());
    // `carrier` moves under `other`, a place with no model, and carries
    // counter2 and its requiring deep2 along: allowed.
    app.update(|cx| host.move_node(cx, "carrier", "other", "body", 0));
    draw(window, &mut app, &shown);
    let after = app.update(|cx| shell::context::<Count>(cx, "deep2").unwrap());
    assert_eq!(before.entity_id(), after.entity_id());
    assert_eq!(
        names(&shown),
        [
            "counter", "left", "right", "inner", "deep", "twin", "pair", "needy", "other",
            "carrier", "counter2", "deep2", "outside",
        ]
    );
    assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 1);
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_requiring_node_where_the_window_gives_the_model_is_allowed(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    let window_model = app.update(|cx| {
        let model = cx.new(|_| Count(77));
        host.provide_window::<Count>(cx, model.clone());
        // No node above `root` gives a Count, but the window does.
        host.place(cx, "windowed", "tour.count_card", "root", "children");
        model
    });
    let _window = open_and_draw(&mut app, &shown);
    assert!(names(&shown).contains(&"windowed".to_string()));
    assert_eq!(
        app.update(|cx| shell::context::<Count>(cx, "windowed").unwrap().entity_id()),
        window_model.entity_id()
    );
    assert_eq!(seen(&shown, "windowed"), Some(77));
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_saved_move_refused_by_the_rule_is_skipped_at_restore(cx: &mut TestAppContext) {
    let _guard = lock();
    let temp = TempDir::new();
    let layout_path = temp.path().join("layout.json");
    fs::write(
        &layout_path,
        r#"{"version": 1, "operations": [
            {"op": "move", "node": "left", "parent": "root", "slot": "children", "index": 0},
            {"op": "hide", "node": "right"}
        ]}"#,
    )
    .unwrap();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
        let (_host, _guest, shown) = setup(&mut app);
        let _window = open_and_draw(&mut app, &shown);
        // The refused move is skipped: left stays under counter. The hide of
        // right still applies.
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 1);
        assert!(app.update(|cx| shell::is_hidden(cx, "right")));
        assert_eq!(
            app.update(|cx| shell::context::<Count>(cx, "left").unwrap().read(cx).0),
            10
        );
        assert_eq!(
            names(&shown),
            [
                "counter", "left", "inner", "deep", "twin", "pair", "needy", "carrier", "counter2",
                "deep2", "other", "outside",
            ]
        );
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    assert!(
        captured.lines.iter().any(|line| {
            line.starts_with("layout-restore file=layout.json: INFO")
                && line.contains("skipped")
                && line.contains(r#"node="left""#)
                && line.contains("requires a Count model, and none is above root")
        }),
        "missing the skipped move in {:#?}",
        captured.lines
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_nodes_model_is_dropped_when_its_node_is_removed(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    let window = open_and_draw(&mut app, &shown);
    let weak = app.update(|cx| shell::context::<Count>(cx, "left").unwrap().downgrade());
    assert!(weak.upgrade().is_some());
    app.update(|cx| host.remove(cx, "counter"));
    draw(window, &mut app, &shown);
    assert!(
        weak.upgrade().is_none(),
        "counter's model outlived its node"
    );
    // A surviving node's model lives on.
    let deep2 = app.update(|cx| shell::context::<Count>(cx, "deep2").unwrap());
    assert_eq!(app.update(|cx| deep2.read(cx).0), 20);
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn provide_and_require_context_before_and_after_register_both_work(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    app.update(|cx| {
        // Declared before the component is registered.
        host.provide::<Count>(cx, "tour.early_giver", |_, cx| cx.new(|_| Count(3)));
        host.require_context::<Count>(cx, "tour.early_card");
        let early_shown = shown.clone();
        host.register(
            cx,
            "tour.early_giver",
            &["body"],
            move |node, slots, _, cx| {
                cx.new(|_| Container {
                    node: node.to_string(),
                    slots,
                    shown: early_shown.clone(),
                })
                .into()
            },
        );
        let early_card_shown = shown.clone();
        host.register(cx, "tour.early_card", &[], move |node, _, _, cx| {
            cx.new(|_| Asking {
                node: node.to_string(),
                shown: early_card_shown.clone(),
            })
            .into()
        });
        // Declared after the component is registered.
        let late_shown = shown.clone();
        host.register(
            cx,
            "tour.late_giver",
            &["body"],
            move |node, slots, _, cx| {
                cx.new(|_| Container {
                    node: node.to_string(),
                    slots,
                    shown: late_shown.clone(),
                })
                .into()
            },
        );
        host.provide::<Count>(cx, "tour.late_giver", |_, cx| cx.new(|_| Count(4)));
        let late_card_shown = shown.clone();
        host.register(cx, "tour.late_card", &[], move |node, _, _, cx| {
            cx.new(|_| Asking {
                node: node.to_string(),
                shown: late_card_shown.clone(),
            })
            .into()
        });
        host.require_context::<Count>(cx, "tour.late_card");
        host.place(cx, "early", "tour.early_giver", "root", "children");
        host.place(cx, "early_card", "tour.early_card", "early", "body");
        host.place(cx, "late", "tour.late_giver", "root", "children");
        host.place(cx, "late_card", "tour.late_card", "late", "body");
    });
    let _window = open_and_draw(&mut app, &shown);
    assert_eq!(
        app.update(|cx| shell::context::<Count>(cx, "early_card")
            .unwrap()
            .read(cx)
            .0),
        3
    );
    assert_eq!(
        app.update(|cx| shell::context::<Count>(cx, "late_card").unwrap().read(cx).0),
        4
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn the_declaration_refusals_each_log_an_error(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    let _window = open_and_draw(&mut app, &shown);
    // A component may both give and require the same type: tour.counter now
    // requires a Count too, and tour.count_card gives one. Neither is a
    // refusal.
    app.update(|cx| {
        host.require_context::<Count>(cx, "tour.counter");
        host.provide::<Count>(cx, "tour.count_card", |_, cx| cx.new(|_| Count(0)));
    });
    assert!(errors().is_empty(), "{:?}", errors());
    app.update(|cx| {
        host.provide::<Count>(cx, "bad component", |_, cx| cx.new(|_| Count(0)));
        host.provide::<Count>(cx, "guest.card", |_, cx| cx.new(|_| Count(0)));
        // tour.counter already gives a Count, and tour.count_card already
        // requires one: the same declaration again is refused.
        host.provide::<Count>(cx, "tour.counter", |_, cx| cx.new(|_| Count(0)));
        host.require_context::<Count>(cx, "bad component");
        host.require_context::<Count>(cx, "guest.card");
        host.require_context::<Count>(cx, "tour.count_card");
        // A window model given twice for one type is refused; the first stays.
        let first = cx.new(|_| Count(1));
        host.provide_window::<Count>(cx, first);
        let second = cx.new(|_| Count(2));
        host.provide_window::<Count>(cx, second);
    });
    let errors = errors();
    assert_eq!(errors.len(), 7, "{errors:?}");
    for (component, expected) in [
        ("bad component", 2),
        ("guest.card", 2),
        ("tour.counter", 1),
        ("tour.count_card", 1),
    ] {
        assert_eq!(
            errors
                .iter()
                .filter(|message| message.contains(component))
                .count(),
            expected,
            "expected {expected} ERRORs for {component} in {errors:?}"
        );
    }
    // The first window model stays.
    assert_eq!(
        app.update(|cx| shell::context::<Count>(cx, "outside").map(|m| m.read(cx).0)),
        Some(1)
    );
    // The declarations that stand still work.
    assert_eq!(
        app.update(|cx| shell::context::<Count>(cx, "left").map(|m| m.read(cx).0)),
        Some(10)
    );
}
