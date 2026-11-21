use gpui::{AppContext, IntoElement, ParentElement, Styled, TestAppContext, px};
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
use yard_layout::{NodeId, Operation, Position, SlotName, decode};
use yard_shell as shell;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-placement-order-{}-{}-{nanos:x}",
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
        // The shell opens the group; the model in yard_layout logs the
        // position's resolution and the refusal inside it.
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

fn id(value: &str) -> NodeId {
    NodeId::parse(value).unwrap()
}

fn slot(value: &str) -> SlotName {
    SlotName::parse(value).unwrap()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct CardProps {
    title: String,
    tint: String,
    count: u32,
}

impl CardProps {
    fn new(title: &str, tint: &str, count: u32) -> Self {
        Self {
            title: title.into(),
            tint: tint.into(),
            count,
        }
    }
}

type Shown = Rc<RefCell<Vec<String>>>;
type Created = Rc<RefCell<Vec<(String, CardProps)>>>;

// A card records its node each time it is drawn.
struct Card {
    node: String,
    shown: Shown,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let node = self.node.clone();
        let shown = self.shown.clone();
        gpui::div().size_full().child(
            gpui::canvas(
                move |_, _, _| shown.borrow_mut().push(node),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
    }
}

// A card made with typed props, recording them at creation.
struct PropsCard {
    node: String,
    shown: Shown,
}

impl gpui::Render for PropsCard {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let node = self.node.clone();
        let shown = self.shown.clone();
        gpui::div().size_full().child(
            gpui::canvas(
                move |_, _, _| shown.borrow_mut().push(node),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
    }
}

impl shell::PropsView<CardProps> for PropsCard {
    fn set_props(&mut self, _: CardProps, _: &mut gpui::Context<Self>) {}
}

// Draw without forcing a refresh: operations must schedule it themselves.
fn draw(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext, shown: &Shown) {
    shown.borrow_mut().clear();
    gpui::AnyWindowHandle::from(window)
        .update(app, |_, window, cx| {
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

fn created_props(created: &Created, node: &str) -> Option<CardProps> {
    created
        .borrow()
        .iter()
        .rev()
        .find(|(name, _)| name == node)
        .map(|(_, props)| props.clone())
}

// `root`, a yard.row holding the tour cards `a`, `b`, and `c`. `tour` offers
// a plain card and a card with props, `guest` offers its own card.
fn setup(app: &mut TestAppContext) -> (shell::Extension, shell::Extension, Created, Shown) {
    let created = Created::default();
    let shown = Shown::default();
    let (host, guest) = app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        let guest = shell::Extension::new(cx, "guest").unwrap();
        let created = created.clone();
        let shown = shown.clone();
        let card_shown = shown.clone();
        host.register(cx, "tour.card", &[], move |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                shown: card_shown.clone(),
            })
            .into()
        });
        let props_shown = shown.clone();
        host.register_with_props::<CardProps, PropsCard>(
            cx,
            "tour.tinted",
            &[],
            move |props, node, _, _, _| {
                created.borrow_mut().push((node.to_string(), props));
                PropsCard {
                    node: node.to_string(),
                    shown: props_shown.clone(),
                }
            },
        );
        let guest_shown = shown.clone();
        guest.register(cx, "guest.card", &[], move |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                shown: guest_shown.clone(),
            })
            .into()
        });
        host.place_root(cx, "root", "yard.row");
        for name in ["a", "b", "c"] {
            host.place(cx, name, "tour.card", "root", "children");
        }
        (host, guest)
    });
    (host, guest, created, shown)
}

#[gpui::test]
fn a_second_extension_places_its_node_before_the_firsts_at_startup(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (_host, guest, _created, shown) = setup(&mut app);
    app.update(|cx| {
        guest.place_at(
            cx,
            "guest",
            "guest.card",
            "root",
            "children",
            Position::Before(id("b")),
        );
    });
    let _window = open_and_draw(&mut app, &shown);
    assert_eq!(*shown.borrow(), ["a", "guest", "b", "c"]);
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_placement_naming_a_node_not_yet_placed_is_refused(cx: &mut TestAppContext) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, guest, _created, shown) = setup(&mut app);
        app.update(|cx| {
            // `late` is not placed yet: the position is refused, `guest` is
            // never in the tree, and the host's `late` still places.
            guest.place_at(
                cx,
                "guest",
                "guest.card",
                "root",
                "children",
                Position::Before(id("late")),
            );
            host.place(cx, "late", "tour.card", "root", "children");
        });
        let _window = open_and_draw(&mut app, &shown);
        assert_eq!(*shown.borrow(), ["a", "b", "c", "late"]);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    assert!(
        captured.lines.iter().any(|line| line.starts_with(
            r#"node-place node="guest" component="guest.card" parent="root" slot="children": INFO"#
        ) && line.contains("late is not in root.children")),
        "missing the refused place in {:#?}",
        captured.lines
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn place_with_and_add_with_place_a_node_with_props_at_a_position(cx: &mut TestAppContext) {
    let _guard = lock();
    let temp = TempDir::new();
    let layout_path = temp.path().join("layout.json");
    let mut app = cx.new_app();
    app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
    let (host, _guest, created, shown) = setup(&mut app);
    app.update(|cx| {
        // A `Placement` carries props and a position; `Null` props give none.
        host.place_with(
            cx,
            "alpha",
            "tour.tinted",
            "root",
            "children",
            shell::Placement {
                props: json!({"title": "Alpha", "tint": "accent"}),
                at: Position::Before(id("a")),
            },
        );
        host.place_with(
            cx,
            "plain",
            "tour.card",
            "root",
            "children",
            shell::Placement {
                props: serde_json::Value::Null,
                at: Position::First,
            },
        );
    });
    let window = open_and_draw(&mut app, &shown);
    assert_eq!(*shown.borrow(), ["plain", "alpha", "a", "b", "c"]);
    assert_eq!(
        created_props(&created, "alpha"),
        Some(CardProps::new("Alpha", "accent", 0))
    );
    gpui::AnyWindowHandle::from(window)
        .update(&mut app, |_, window, cx| {
            host.add_with(
                window,
                cx,
                "added",
                "tour.tinted",
                "root",
                "children",
                shell::Placement {
                    props: json!({"title": "Added", "tint": "success"}),
                    at: Position::After(id("b")),
                },
            );
            host.add_at(
                window,
                cx,
                "front",
                "tour.card",
                "root",
                "children",
                Position::Index(0),
            );
            host.add(window, cx, "tail", "tour.card", "root", "children");
        })
        .unwrap();
    draw(window, &mut app, &shown);
    assert_eq!(
        *shown.borrow(),
        ["front", "plain", "alpha", "a", "b", "added", "c", "tail"]
    );
    assert_eq!(
        created_props(&created, "added"),
        Some(CardProps::new("Added", "success", 0))
    );
    // Each position is recorded as it was asked; a plain `add` records last.
    let ops = decode(&fs::read_to_string(&layout_path).unwrap()).unwrap();
    assert_eq!(ops.len(), 3);
    match &ops[0] {
        Operation::Add { node, at, .. } => {
            assert_eq!(node.id, id("added"));
            assert_eq!(*at, Position::After(id("b")));
        }
        other => panic!("expected an add, got {other:?}"),
    }
    match &ops[1] {
        Operation::Add { node, at, .. } => {
            assert_eq!(node.id, id("front"));
            assert_eq!(*at, Position::Index(0));
        }
        other => panic!("expected an add, got {other:?}"),
    }
    match &ops[2] {
        Operation::Add { node, at, .. } => {
            assert_eq!(node.id, id("tail"));
            assert_eq!(*at, Position::Last);
        }
        other => panic!("expected an add, got {other:?}"),
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn move_to_with_first_moves_a_tab_to_the_front_of_its_bar(cx: &mut TestAppContext) {
    let _guard = lock();
    let temp = TempDir::new();
    let layout_path = temp.path().join("layout.json");
    let mut app = cx.new_app();
    app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
    let shown = Shown::default();
    let host = app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        let shown = shown.clone();
        host.register(cx, "tour.card", &[], move |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                shown: shown.clone(),
            })
            .into()
        });
        host.place_root(cx, "root", "yard.row");
        host.place(cx, "tabs", "yard.tabs", "root", "children");
        for name in ["a", "b", "c"] {
            host.place(cx, name, "tour.card", "tabs", "children");
        }
        host
    });
    let window = open_and_draw(&mut app, &shown);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);
    assert!(
        visual.debug_bounds("TAB-a").unwrap().origin.x
            < visual.debug_bounds("TAB-c").unwrap().origin.x
    );
    // `c` moves to the front of its bar.
    app.update(|cx| host.move_to(cx, "c", "tabs", "children", Position::First));
    draw(window, &mut app, &shown);
    assert!(
        visual.debug_bounds("TAB-c").unwrap().origin.x
            < visual.debug_bounds("TAB-a").unwrap().origin.x
    );
    assert!(
        visual.debug_bounds("TAB-a").unwrap().origin.x
            < visual.debug_bounds("TAB-b").unwrap().origin.x
    );
    // A named position works too: `b` right after `c`.
    app.update(|cx| {
        host.move_to(cx, "b", "tabs", "children", Position::After(id("c")));
    });
    draw(window, &mut app, &shown);
    assert!(
        visual.debug_bounds("TAB-c").unwrap().origin.x
            < visual.debug_bounds("TAB-b").unwrap().origin.x
    );
    assert!(
        visual.debug_bounds("TAB-b").unwrap().origin.x
            < visual.debug_bounds("TAB-a").unwrap().origin.x
    );
    // And move_node keeps its meaning: an index.
    app.update(|cx| host.move_node(cx, "a", "tabs", "children", 0));
    draw(window, &mut app, &shown);
    assert!(
        visual.debug_bounds("TAB-a").unwrap().origin.x
            < visual.debug_bounds("TAB-c").unwrap().origin.x
    );
    let ops = decode(&fs::read_to_string(&layout_path).unwrap()).unwrap();
    assert_eq!(ops.len(), 3);
    for (op, node, at) in [
        (&ops[0], "c", Position::First),
        (&ops[1], "b", Position::After(id("c"))),
        (&ops[2], "a", Position::Index(0)),
    ] {
        match op {
            Operation::Move {
                node: moved,
                parent,
                slot: name,
                at: asked,
            } => {
                assert_eq!(*moved, id(node));
                assert_eq!(*parent, id("tabs"));
                assert_eq!(*name, slot("children"));
                assert_eq!(*asked, at);
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_saved_move_with_after_is_restored_and_a_gone_anchor_is_skipped(cx: &mut TestAppContext) {
    let _guard = lock();
    let temp = TempDir::new();
    let layout_path = temp.path().join("layout.json");
    fs::write(
        &layout_path,
        r#"{"version": 1, "operations": [
            {"op": "move", "node": "c", "parent": "root", "slot": "children", "after": "a"},
            {"op": "move", "node": "b", "parent": "root", "slot": "children", "after": "ghost"},
            {"op": "add", "node": "extra", "component": "tour.card", "extension": "tour", "parent": "root", "slot": "children", "before": "b"}
        ]}"#,
    )
    .unwrap();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
        let (_host, _guest, _created, shown) = setup(&mut app);
        let _window = open_and_draw(&mut app, &shown);
        // `c` is restored right after `a`; `b`'s move is skipped, its anchor
        // gone; the add before `b` still applies.
        assert_eq!(*shown.borrow(), ["a", "c", "extra", "b"]);
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    // The skipped operation logs its INFO inside layout-restore, and the
    // model's refusal names the anchor that is not in the slot.
    assert!(
        captured.lines.iter().any(
            |line| line.starts_with("layout-restore file=layout.json: INFO")
                && line.contains("skipped")
                && line.contains(r#"node="b""#)
        ),
        "missing the skipped move in {:#?}",
        captured.lines
    );
    assert!(
        captured.lines.iter().any(
            |line| line.starts_with("layout-restore file=layout.json: INFO")
                && line.contains("ghost is not in root.children")
        ),
        "missing the refusal in {:#?}",
        captured.lines
    );
    assert!(errors().is_empty(), "{:?}", errors());
}
