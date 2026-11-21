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
use yard_shell as shell;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-slot-count-{}-{}-{nanos:x}",
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
        if !event.metadata().target().starts_with("yard_shell")
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

type Shown = Rc<RefCell<Vec<String>>>;

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

// A frame records its node and draws the nodes of its `content` and `aux`
// slots; only `content` is declared for one.
struct Frame {
    node: String,
    slots: shell::Slots,
    shown: Shown,
}

impl gpui::Render for Frame {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let node = self.node.clone();
        let shown = self.shown.clone();
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                gpui::canvas(
                    move |_, _, _| shown.borrow_mut().push(node),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(
                self.slots
                    .views("content")
                    .into_iter()
                    .chain(self.slots.views("aux"))
                    .map(|view| gpui::div().flex_1().min_h_0().child(view)),
            )
    }
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

// The default layout: `frame`, a tour.frame whose `content` slot is declared
// for one and holds `first`; `loose`, a card beside it; and `pocket`, a second
// tour.frame left empty.
fn setup(app: &mut TestAppContext) -> (shell::Extension, shell::Extension, Shown) {
    let shown = Shown::default();
    let (host, guest) = app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        let guest = shell::Extension::new(cx, "guest").unwrap();
        let shown = shown.clone();
        let frame_shown = shown.clone();
        host.register(cx, "tour.card", &[], move |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                shown: shown.clone(),
            })
            .into()
        });
        host.register(
            cx,
            "tour.frame",
            &["content", "aux"],
            move |node, slots, _, cx| {
                cx.new(|_| Frame {
                    node: node.to_string(),
                    slots,
                    shown: frame_shown.clone(),
                })
                .into()
            },
        );
        host.slot_for_one(cx, "tour.frame", "content");
        guest.register(cx, "guest.card", &[], |_, _, _, cx| {
            cx.new(|_| gpui::Empty).into()
        });
        host.place_root(cx, "root", "yard.row");
        host.place(cx, "frame", "tour.frame", "root", "children");
        host.place(cx, "first", "tour.card", "frame", "content");
        host.place(cx, "loose", "tour.card", "root", "children");
        host.place(cx, "pocket", "tour.frame", "root", "children");
        (host, guest)
    });
    (host, guest, shown)
}

#[gpui::test]
fn a_second_node_placed_in_a_full_slot_for_one_at_startup_is_refused(cx: &mut TestAppContext) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, _guest, shown) = setup(&mut app);
        app.update(|cx| host.place(cx, "extra", "tour.card", "frame", "content"));
        let _window = open_and_draw(&mut app, &shown);
        // The place is refused: `first` stays the one node of frame.content,
        // and `extra` is never in the tree.
        assert_eq!(*shown.borrow(), ["frame", "first", "loose", "pocket"]);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    assert!(
        captured.lines.iter().any(|line| line.starts_with(
            r#"node-place node="extra" component="tour.card" parent="frame" slot="content": INFO"#
        ) && line
            .contains("frame.content holds one node, and already holds first")),
        "missing the refused place in {:#?}",
        captured.lines
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn add_and_move_into_a_full_slot_for_one_are_refused_and_the_tree_is_unchanged(
    cx: &mut TestAppContext,
) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, guest, shown) = setup(&mut app);
        let window = open_and_draw(&mut app, &shown);
        gpui::AnyWindowHandle::from(window)
            .update(&mut app, |_, window, cx| {
                host.add(window, cx, "added", "tour.card", "frame", "content");
            })
            .unwrap();
        app.update(|cx| guest.move_node(cx, "loose", "frame", "content", 0));
        draw(window, &mut app, &shown);
        // Both are refused: `first` still fills frame.content alone, `added`
        // is not in the tree, and `loose` stayed at the root.
        assert_eq!(*shown.borrow(), ["frame", "first", "loose", "pocket"]);
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    for (op, node, extension) in [("add", "added", "tour"), ("move", "loose", "guest")] {
        let group = format!(r#"layout-op op="{op}" node="{node}" extension="{extension}": INFO"#);
        assert!(
            captured.lines.iter().any(|line| line.starts_with(&group)
                && line.contains("frame.content holds one node, and already holds first")),
            "missing the refused {op} in {:#?}",
            captured.lines
        );
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn an_empty_slot_for_one_takes_a_node_by_add_and_by_move(cx: &mut TestAppContext) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, guest, shown) = setup(&mut app);
        let window = open_and_draw(&mut app, &shown);
        // `pocket.content` starts empty: an `add` fills it.
        gpui::AnyWindowHandle::from(window)
            .update(&mut app, |_, window, cx| {
                host.add(window, cx, "added", "tour.card", "pocket", "content");
            })
            .unwrap();
        draw(window, &mut app, &shown);
        assert_eq!(
            *shown.borrow(),
            ["frame", "first", "loose", "pocket", "added"]
        );
        // `frame.content` empties when `first` moves out; a `move` fills it.
        app.update(|cx| {
            host.move_node(cx, "first", "root", "children", 0);
            guest.move_node(cx, "loose", "frame", "content", 0);
        });
        draw(window, &mut app, &shown);
        assert_eq!(
            *shown.borrow(),
            ["first", "frame", "loose", "pocket", "added"]
        );
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 3);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    assert!(
        !captured
            .lines
            .iter()
            .any(|line| line.contains("holds one node")),
        "no refusal is logged for an empty slot for one: {:#?}",
        captured.lines
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn moving_the_slots_own_node_to_index_0_of_the_same_slot_is_allowed(cx: &mut TestAppContext) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, _guest, shown) = setup(&mut app);
        let window = open_and_draw(&mut app, &shown);
        app.update(|cx| host.move_node(cx, "first", "frame", "content", 0));
        draw(window, &mut app, &shown);
        assert_eq!(*shown.borrow(), ["frame", "first", "loose", "pocket"]);
        // The move was applied and recorded, not refused.
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 1);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    assert!(
        !captured
            .lines
            .iter()
            .any(|line| line.contains("holds one node")),
        "moving the slot's own node logs no refusal: {:#?}",
        captured.lines
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_slot_for_one_whose_node_is_hidden_still_refuses_another(cx: &mut TestAppContext) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, guest, shown) = setup(&mut app);
        let window = open_and_draw(&mut app, &shown);
        app.update(|cx| host.set_hidden(cx, "first", true));
        gpui::AnyWindowHandle::from(window)
            .update(&mut app, |_, window, cx| {
                host.add(window, cx, "second", "tour.card", "frame", "content");
            })
            .unwrap();
        app.update(|cx| guest.move_node(cx, "loose", "frame", "content", 0));
        draw(window, &mut app, &shown);
        // The hidden `first` still holds the slot: both changes are refused.
        assert_eq!(*shown.borrow(), ["frame", "loose", "pocket"]);
        assert!(app.update(|cx| shell::is_hidden(cx, "first")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 1);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    for (op, node, extension) in [("add", "second", "tour"), ("move", "loose", "guest")] {
        let group = format!(r#"layout-op op="{op}" node="{node}" extension="{extension}": INFO"#);
        assert!(
            captured.lines.iter().any(|line| line.starts_with(&group)
                && line.contains("frame.content holds one node, and already holds first")),
            "missing the refused {op} beside a hidden node in {:#?}",
            captured.lines
        );
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_slot_not_declared_for_one_takes_many_as_before(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, guest, shown) = setup(&mut app);
    let window = open_and_draw(&mut app, &shown);
    // `frame.aux`, on the same component as the slot for one, takes two nodes.
    gpui::AnyWindowHandle::from(window)
        .update(&mut app, |_, window, cx| {
            host.add(window, cx, "aux-one", "tour.card", "frame", "aux");
            host.add(window, cx, "aux-two", "tour.card", "frame", "aux");
        })
        .unwrap();
    draw(window, &mut app, &shown);
    assert_eq!(
        *shown.borrow(),
        ["frame", "first", "aux-one", "aux-two", "loose", "pocket"]
    );
    // `root.children`, never declared at all, still takes nodes by move.
    app.update(|cx| guest.move_node(cx, "loose", "root", "children", 0));
    draw(window, &mut app, &shown);
    assert_eq!(
        *shown.borrow(),
        ["loose", "frame", "first", "aux-one", "aux-two", "pocket"]
    );
    assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 3);
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_saved_move_into_a_full_slot_for_one_is_skipped_at_restore(cx: &mut TestAppContext) {
    let _guard = lock();
    let temp = TempDir::new();
    let layout_path = temp.path().join("layout.json");
    fs::write(
        &layout_path,
        r#"{"version": 1, "operations": [
            {"op": "move", "node": "loose", "parent": "frame", "slot": "content", "index": 0},
            {"op": "add", "node": "extra", "component": "tour.card", "extension": "tour", "parent": "frame", "slot": "content"},
            {"op": "add", "node": "saved", "component": "tour.card", "extension": "tour", "parent": "pocket", "slot": "content"},
            {"op": "hide", "node": "loose"}
        ]}"#,
    )
    .unwrap();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
        let (_host, _guest, shown) = setup(&mut app);
        let _window = open_and_draw(&mut app, &shown);
        // The move and the add into the full slot are skipped; the next
        // operations still apply: `saved` fills the empty slot for one, and
        // `loose` is hidden where it stayed.
        assert_eq!(*shown.borrow(), ["frame", "first", "pocket", "saved"]);
        assert!(app.update(|cx| shell::is_hidden(cx, "loose")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    for skipped in [
        r#"skipped op="move" node="loose" reason="frame.content holds one node, and already holds first""#,
        r#"skipped op="add" node="extra" reason="frame.content holds one node, and already holds first""#,
    ] {
        assert!(
            captured.lines.iter().any(|line| line
                .starts_with("layout-restore file=layout.json: INFO")
                && line.contains(skipped)),
            "missing {skipped} in {:#?}",
            captured.lines
        );
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn slot_for_one_before_and_after_register_both_work(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    app.update(|cx| {
        // tour.early's slot is declared for one before it is registered,
        // tour.late's after; both take one node and refuse a second.
        host.slot_for_one(cx, "tour.early", "content");
        let early_shown = shown.clone();
        host.register(cx, "tour.early", &["content"], move |node, slots, _, cx| {
            cx.new(|_| Frame {
                node: node.to_string(),
                slots,
                shown: early_shown.clone(),
            })
            .into()
        });
        let late_shown = shown.clone();
        host.register(cx, "tour.late", &["content"], move |node, slots, _, cx| {
            cx.new(|_| Frame {
                node: node.to_string(),
                slots,
                shown: late_shown.clone(),
            })
            .into()
        });
        host.slot_for_one(cx, "tour.late", "content");
        host.place(cx, "early", "tour.early", "root", "children");
        host.place(cx, "e-one", "tour.card", "early", "content");
        host.place(cx, "e-two", "tour.card", "early", "content");
        host.place(cx, "late", "tour.late", "root", "children");
        host.place(cx, "l-one", "tour.card", "late", "content");
        host.place(cx, "l-two", "tour.card", "late", "content");
    });
    let _window = open_and_draw(&mut app, &shown);
    assert_eq!(
        *shown.borrow(),
        [
            "frame", "first", "loose", "pocket", "early", "e-one", "late", "l-one"
        ]
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn slot_for_one_of_a_slot_the_component_does_not_declare_is_an_error_at_registration(
    cx: &mut TestAppContext,
) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    app.update(|cx| {
        // tour.missing's `nope` is declared for one before registration, but
        // the component registers only `content`: the declaration is dropped
        // with an ERROR, and `content` takes many.
        host.slot_for_one(cx, "tour.missing", "nope");
        let missing_shown = shown.clone();
        host.register(
            cx,
            "tour.missing",
            &["content"],
            move |node, slots, _, cx| {
                cx.new(|_| Frame {
                    node: node.to_string(),
                    slots,
                    shown: missing_shown.clone(),
                })
                .into()
            },
        );
        host.place(cx, "missing", "tour.missing", "root", "children");
        host.place(cx, "m-one", "tour.card", "missing", "content");
        host.place(cx, "m-two", "tour.card", "missing", "content");
    });
    let _window = open_and_draw(&mut app, &shown);
    assert_eq!(
        *shown.borrow(),
        [
            "frame", "first", "loose", "pocket", "missing", "m-one", "m-two"
        ]
    );
    let errors = errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[gpui::test]
fn slot_for_one_is_refused_for_another_extensions_component_an_invalid_id_or_slot_name_and_a_second_time(
    cx: &mut TestAppContext,
) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, shown) = setup(&mut app);
    app.update(|cx| {
        host.slot_for_one(cx, "guest.card", "content");
        host.slot_for_one(cx, "bad component", "content");
        host.slot_for_one(cx, "tour.card", "bad slot");
        // tour.frame's content was declared for one in the setup.
        host.slot_for_one(cx, "tour.frame", "content");
    });
    let errors = errors();
    assert_eq!(errors.len(), 4, "{errors:?}");
    for words in ["guest.card", "bad component", "bad slot", "tour.frame"] {
        assert!(
            errors.iter().any(|message| message.contains(words)),
            "missing an ERROR for {words} in {errors:?}"
        );
    }
    // The declaration that stands still refuses a second node.
    let window = open_and_draw(&mut app, &shown);
    gpui::AnyWindowHandle::from(window)
        .update(&mut app, |_, window, cx| {
            host.add(window, cx, "second", "tour.card", "frame", "content");
        })
        .unwrap();
    draw(window, &mut app, &shown);
    assert_eq!(*shown.borrow(), ["frame", "first", "loose", "pocket"]);
}
