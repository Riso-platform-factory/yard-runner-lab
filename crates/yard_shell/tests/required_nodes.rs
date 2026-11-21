use gpui::{
    AppContext, InteractiveElement, IntoElement, ParentElement, Styled, TestAppContext, px,
};
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
use yard_shell::{self as shell, Hide};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-required-{}-{}-{nanos:x}",
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct CardProps {
    tint: String,
}

type Shown = Rc<RefCell<Vec<String>>>;
type Told = Rc<RefCell<Vec<(String, CardProps)>>>;

struct Card {
    node: String,
    shown: Shown,
    told: Told,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let node = self.node.clone();
        let shown = self.shown.clone();
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
            .child(
                gpui::canvas(
                    move |_, _, _| shown.borrow_mut().push(node),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }
}

impl shell::PropsView<CardProps> for Card {
    fn set_props(&mut self, props: CardProps, _: &mut gpui::Context<Self>) {
        self.told.borrow_mut().push((self.node.clone(), props));
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

/// The node the dev shell says has the focus, if any.
fn focused(visual: &mut gpui::VisualTestContext) -> Option<String> {
    visual.update(|window, cx| shell::focused_node(window, cx).map(|node| node.to_string()))
}

// The default layout: a required `editor` inside `main` inside `wrap`, a `side`
// card beside them, and `notes` as the required node's sibling.
fn setup(app: &mut TestAppContext) -> (shell::Extension, shell::Extension, Told, Shown) {
    let told = Told::default();
    let shown = Shown::default();
    let (host, guest) = app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        let guest = shell::Extension::new(cx, "guest").unwrap();
        let shown = shown.clone();
        let told = told.clone();
        let card_shown = shown.clone();
        let card_told = told.clone();
        host.register_with_props::<CardProps, Card>(
            cx,
            "tour.editor",
            &[],
            move |_, node, _, _, _| Card {
                node: node.to_string(),
                shown: shown.clone(),
                told: told.clone(),
            },
        );
        host.require(cx, "tour.editor");
        host.register(cx, "tour.card", &[], move |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                shown: card_shown.clone(),
                told: card_told.clone(),
            })
            .into()
        });
        guest.register(cx, "guest.card", &[], |_, _, _, cx| {
            cx.new(|_| gpui::Empty).into()
        });
        host.place_root(cx, "root", "yard.row");
        host.place(cx, "side", "tour.card", "root", "children");
        host.place(cx, "wrap", "yard.column", "root", "children");
        host.place(cx, "main", "yard.row", "wrap", "children");
        host.place(cx, "editor", "tour.editor", "main", "children");
        host.place(cx, "notes", "tour.card", "main", "children");
        (host, guest)
    });
    (host, guest, told, shown)
}

#[gpui::test]
fn hiding_and_removing_a_required_node_are_refused_and_change_nothing(cx: &mut TestAppContext) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, _guest, _told, shown) = setup(&mut app);
        let window = open_and_draw(&mut app, &shown);
        assert_eq!(*shown.borrow(), ["side", "editor", "notes"]);
        app.update(|cx| {
            host.set_hidden(cx, "editor", true);
            host.remove(cx, "editor");
        });
        draw(window, &mut app, &shown);
        // Both are refused with an INFO: the tree is unchanged and nothing was
        // recorded in the user layer.
        assert_eq!(*shown.borrow(), ["side", "editor", "notes"]);
        assert!(!app.update(|cx| shell::is_hidden(cx, "editor")));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    for (op, words) in [
        ("hide", "editor cannot be hidden: it is required"),
        ("remove", "editor cannot be removed: it is required"),
    ] {
        let group = format!(r#"layout-op op="{op}" node="editor" extension="tour": INFO"#);
        assert!(
            captured
                .lines
                .iter()
                .any(|line| line.starts_with(&group) && line.contains(words)),
            "missing the refused {op} in {:#?}",
            captured.lines
        );
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn hiding_and_removing_ancestors_of_a_required_node_are_refused_naming_it(cx: &mut TestAppContext) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (host, _guest, _told, shown) = setup(&mut app);
        let window = open_and_draw(&mut app, &shown);
        app.update(|cx| {
            host.set_hidden(cx, "main", true);
            host.remove(cx, "main");
            host.set_hidden(cx, "wrap", true);
            host.remove(cx, "wrap");
        });
        draw(window, &mut app, &shown);
        assert_eq!(*shown.borrow(), ["side", "editor", "notes"]);
        for node in ["main", "wrap"] {
            assert!(!app.update(|cx| shell::is_hidden(cx, node)), "{node}");
        }
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    for (node, op, words) in [
        (
            "main",
            "hide",
            "main cannot be hidden: it holds the required node editor",
        ),
        (
            "main",
            "remove",
            "main cannot be removed: it holds the required node editor",
        ),
        (
            "wrap",
            "hide",
            "wrap cannot be hidden: it holds the required node editor",
        ),
        (
            "wrap",
            "remove",
            "wrap cannot be removed: it holds the required node editor",
        ),
    ] {
        let group = format!(r#"layout-op op="{op}" node="{node}" extension="tour": INFO"#);
        assert!(
            captured
                .lines
                .iter()
                .any(|line| line.starts_with(&group) && line.contains(words)),
            "missing the refused {op} of {node} in {:#?}",
            captured.lines
        );
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_sibling_of_the_required_node_can_be_hidden_and_removed(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, _told, shown) = setup(&mut app);
    let window = open_and_draw(&mut app, &shown);
    app.update(|cx| host.set_hidden(cx, "notes", true));
    draw(window, &mut app, &shown);
    assert!(app.update(|cx| shell::is_hidden(cx, "notes")));
    assert_eq!(*shown.borrow(), ["side", "editor"]);
    app.update(|cx| host.remove(cx, "notes"));
    draw(window, &mut app, &shown);
    assert_eq!(*shown.borrow(), ["side", "editor"]);
    // The sibling is removed, not just hidden: a show is refused and nothing
    // comes back.
    app.update(|cx| host.set_hidden(cx, "notes", false));
    draw(window, &mut app, &shown);
    assert_eq!(*shown.borrow(), ["side", "editor"]);
    assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
    assert!(!app.update(|cx| shell::is_hidden(cx, "editor")));
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn the_required_node_can_be_moved_zoomed_and_have_its_props_set(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, told, shown) = setup(&mut app);
    let window = open_and_draw(&mut app, &shown);
    app.update(|cx| {
        host.move_node(cx, "editor", "root", "children", 0);
        host.set_props(cx, "editor", json!({"tint": "warning"}));
    });
    draw(window, &mut app, &shown);
    assert_eq!(*shown.borrow(), ["editor", "side", "notes"]);
    assert_eq!(
        *told.borrow(),
        [(
            "editor".to_string(),
            CardProps {
                tint: "warning".into()
            }
        )]
    );
    // The required node can also be zoomed: it fills the body alone.
    app.update(|cx| host.set_zoomed(cx, "editor"));
    draw(window, &mut app, &shown);
    assert_eq!(
        app.update(|cx| shell::zoomed(cx)).as_deref(),
        Some("editor")
    );
    assert_eq!(*shown.borrow(), ["editor"]);
    assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn the_hide_action_on_the_focused_required_node_is_refused(cx: &mut TestAppContext) {
    let _guard = lock();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let (_host, _guest, _told, shown) = setup(&mut app);
        let window = open_and_draw(&mut app, &shown);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);
        let card = visual
            .debug_bounds("CARD-editor")
            .expect("editor's card draws");
        visual.simulate_click(card.center(), gpui::Modifiers::default());
        draw(window, &mut app, &shown);
        assert_eq!(focused(&mut visual).as_deref(), Some("editor"));
        // `yard::Hide` has no default key; it is dispatched on the focus.
        visual.dispatch_action(Hide);
        draw(window, &mut app, &shown);
        assert_eq!(*shown.borrow(), ["side", "editor", "notes"]);
        assert!(!app.update(|cx| shell::is_hidden(cx, "editor")));
        assert_eq!(focused(&mut visual).as_deref(), Some("editor"));
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    assert!(
        captured.lines.iter().any(|line| line
            .starts_with(r#"layout-op op="hide" node="editor" extension="yard": INFO"#)
            && line.contains("editor cannot be hidden: it is required")),
        "missing the refused Hide action in {:#?}",
        captured.lines
    );
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn a_saved_hide_of_a_required_node_is_skipped_at_restore(cx: &mut TestAppContext) {
    let _guard = lock();
    let temp = TempDir::new();
    let layout_path = temp.path().join("layout.json");
    fs::write(
        &layout_path,
        r#"{"version": 1, "operations": [
            {"op": "hide", "node": "editor"},
            {"op": "remove", "node": "main"},
            {"op": "hide", "node": "side"}
        ]}"#,
    )
    .unwrap();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
        let (_host, _guest, _told, shown) = setup(&mut app);
        let _window = open_and_draw(&mut app, &shown);
        // The required node's hide and its ancestor's remove are skipped; the
        // next operation is still applied.
        assert!(!app.update(|cx| shell::is_hidden(cx, "editor")));
        assert!(app.update(|cx| shell::is_hidden(cx, "side")));
        assert_eq!(*shown.borrow(), ["editor", "notes"]);
        assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 1);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    for skipped in [
        r#"skipped op="hide" node="editor" reason="editor cannot be hidden: it is required""#,
        r#"skipped op="remove" node="main" reason="main cannot be removed: it holds the required node editor""#,
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
fn require_before_and_after_register_both_work(cx: &mut TestAppContext) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, told, shown) = setup(&mut app);
    app.update(|cx| {
        let shown = shown.clone();
        let told = told.clone();
        let late_shown = shown.clone();
        let late_told = told.clone();
        // tour.early is declared required before it is registered.
        host.require(cx, "tour.early");
        host.register(cx, "tour.early", &[], move |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                shown: shown.clone(),
                told: told.clone(),
            })
            .into()
        });
        // tour.late is declared required after it is registered.
        host.register(cx, "tour.late", &[], move |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                shown: late_shown.clone(),
                told: late_told.clone(),
            })
            .into()
        });
        host.require(cx, "tour.late");
        host.place(cx, "early", "tour.early", "root", "children");
        host.place(cx, "late", "tour.late", "root", "children");
    });
    let window = open_and_draw(&mut app, &shown);
    app.update(|cx| {
        host.set_hidden(cx, "early", true);
        host.set_hidden(cx, "late", true);
    });
    draw(window, &mut app, &shown);
    assert_eq!(
        *shown.borrow(),
        ["side", "editor", "notes", "early", "late"]
    );
    for node in ["early", "late"] {
        assert!(!app.update(|cx| shell::is_hidden(cx, node)), "{node}");
    }
    assert!(errors().is_empty(), "{:?}", errors());
}

#[gpui::test]
fn require_is_refused_for_another_extensions_component_an_invalid_id_and_a_second_time(
    cx: &mut TestAppContext,
) {
    let _guard = lock();
    let mut app = cx.new_app();
    let (host, _guest, _told, shown) = setup(&mut app);
    app.update(|cx| {
        host.require(cx, "bad component");
        host.require(cx, "guest.card");
        // tour.editor was declared required in the setup; a second time refuses.
        host.require(cx, "tour.editor");
    });
    let errors = errors();
    assert_eq!(errors.len(), 3, "{errors:?}");
    for component in ["bad component", "guest.card", "tour.editor"] {
        assert!(
            errors.iter().any(|message| message.contains(component)),
            "missing an ERROR for {component} in {errors:?}"
        );
    }
    // The declaration that stands still protects its nodes.
    let window = open_and_draw(&mut app, &shown);
    app.update(|cx| host.set_hidden(cx, "editor", true));
    draw(window, &mut app, &shown);
    assert!(!app.update(|cx| shell::is_hidden(cx, "editor")));
    assert_eq!(*shown.borrow(), ["side", "editor", "notes"]);
}
