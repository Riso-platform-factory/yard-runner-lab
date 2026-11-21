use gpui::{AppContext, IntoElement, ParentElement, Styled, TestAppContext, px};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
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
            "yard-shell-save-restore-{}-{}-{nanos:x}",
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

static WARNINGS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

struct Diagnostics;
impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell") || metadata.target().starts_with("yard_paths")
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            WARNINGS
                .lock()
                .unwrap()
                .push((record.level(), record.args().to_string()));
        }
    }
    fn flush(&self) {}
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
        if (!target.starts_with("yard_shell")
            && !target.starts_with("yard_layout")
            && !target.starts_with("yard_paths"))
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
type CardViews = Rc<RefCell<BTreeMap<String, gpui::WeakEntity<Card>>>>;

struct Card {
    node: String,
    slots: shell::Slots,
    shown: Shown,
}

impl gpui::Render for Card {
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
            let _ = window.draw(cx);
        })
        .unwrap();
}

fn setup_default(
    app: &mut TestAppContext,
) -> (shell::Extension, shell::Extension, Shown, CardViews) {
    let shown = Shown::default();
    let views = Rc::new(RefCell::new(BTreeMap::new()));
    let (host, guest) = app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        let guest = shell::Extension::new(cx, "guest").unwrap();
        let shown = shown.clone();
        let views = views.clone();
        host.register(cx, "tour.card", &["body"], move |node, slots, _, cx| {
            let card = cx.new(|_| Card {
                node: node.to_string(),
                slots,
                shown: shown.clone(),
            });
            views
                .borrow_mut()
                .insert(node.to_string(), card.downgrade());
            card.into()
        });
        host.register(cx, "tour.leaf", &[], |_, _, _, cx| {
            cx.new(|_| gpui::Empty).into()
        });
        host.place_root(cx, "root", "yard.row");
        host.place(cx, "a", "tour.card", "root", "children");
        host.place(cx, "inner", "tour.card", "a", "body");
        host.place(cx, "b", "tour.card", "root", "children");
        host.place(cx, "c", "tour.card", "root", "children");
        host.place(cx, "d", "tour.card", "root", "children");
        // Init-time visibility is part of the default layout, not saved before window-open.
        host.set_hidden(cx, "d", true);
        (host, guest)
    });
    (host, guest, shown, views)
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

// Alone in this binary: every captured callsite and log warning runs here.
#[gpui::test]
fn save_and_restore_cover_every_contract_row(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let temp = TempDir::new();
    let trace = Trace::default();

    tracing::subscriber::with_default(trace.clone(), || {
        // 1. Without set_layout_file, nothing is saved or restored.
        {
            let mut app = cx.new_app();
            let (host, _, shown, _) = setup_default(&mut app);
            let window = open_and_draw(&mut app, &shown);
            gpui::AnyWindowHandle::from(window)
                .update(&mut app, |_, window, cx| {
                    host.add(window, cx, "unsaved", "tour.card", "root", "children");
                    host.reset_layout(window, cx);
                })
                .unwrap();
            let captured = trace.0.lock().unwrap();
            assert!(
                !captured
                    .span_stacks
                    .iter()
                    .flatten()
                    .any(|span| span.starts_with("layout-save")
                        || span.starts_with("layout-restore"))
            );
        }

        // 2. Missing file on startup, then saving every operation kind and reset.
        let layout_path = temp.path().join("data/layout.json");
        {
            let mut app = cx.new_app();
            app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
            assert!(
                trace
                    .0
                    .lock()
                    .unwrap()
                    .lines
                    .contains(&"none: DEBUG message=layout file set path=layout.json".to_string())
            );
            let (host, guest, shown, _) = setup_default(&mut app);
            assert!(
                !layout_path.exists(),
                "init-time operations before new_shell must not write layout.json"
            );
            let before_open = trace.0.lock().unwrap().lines.len();
            let window = open_and_draw(&mut app, &shown);
            assert!(!layout_path.exists());
            assert!(trace.0.lock().unwrap().lines[before_open..].contains(
                &"layout-restore file=layout.json: DEBUG message=no saved layout".to_string()
            ));
            assert_eq!(*shown.borrow(), ["a", "inner", "b", "c"]);

            let any_window = gpui::AnyWindowHandle::from(window);
            any_window
                .update(&mut app, |_, window, cx| {
                    host.add(window, cx, "extra", "tour.card", "root", "children");
                    host.add(window, cx, "nested", "tour.card", "extra", "body");
                    guest.remove(cx, "b");
                    guest.move_node(cx, "c", "root", "children", 0);
                    guest.set_hidden(cx, "a", true);
                    guest.set_hidden(cx, "d", false);
                })
                .unwrap();
            draw(window, &mut app, &shown);
            assert_eq!(*shown.borrow(), ["c", "d", "extra", "nested"]);

            let saved_text = fs::read_to_string(&layout_path).unwrap();
            let decoded = yard_layout::decode(&saved_text).unwrap();
            assert_eq!(decoded.len(), 6);
            assert!(saved_text.contains(r#""version": 1"#));
            for op_name in ["add", "remove", "move", "hide", "show"] {
                assert!(
                    saved_text.contains(&format!(r#""op": "{op_name}""#)),
                    "missing {op_name} in {saved_text}"
                );
            }

            let captured = trace.0.lock().unwrap();
            assert!(captured.span_stacks.iter().any(|stack| {
                stack
                    == &[
                        "window id=4294967297".to_string(),
                        r#"layout-op op="show" node="d" extension="guest""#.to_string(),
                        "layout-save operations=6".to_string(),
                        "file-write path=layout.json".to_string(),
                    ]
            }));
            assert!(
                captured
                    .lines
                    .contains(&"layout-save operations=6: DEBUG message=saved".to_string())
            );
        }

        // 3. Restoring every operation kind creates views for restored nodes without saving.
        {
            let mut app = cx.new_app();
            app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
            let (host, _, shown, views) = setup_default(&mut app);
            let stacks_before = trace.0.lock().unwrap().span_stacks.len();
            let lines_before = trace.0.lock().unwrap().lines.len();
            let window = open_and_draw(&mut app, &shown);

            assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 6);
            assert_eq!(*shown.borrow(), ["c", "d", "extra", "nested"]);
            for present in ["a", "inner", "c", "d", "extra", "nested"] {
                assert!(
                    views
                        .borrow()
                        .get(present)
                        .and_then(|v| v.upgrade())
                        .is_some(),
                    "restored node {present} must have a view"
                );
            }
            assert!(
                !views.borrow().contains_key("b"),
                "removed node b must have no view"
            );

            {
                let captured = trace.0.lock().unwrap();
                assert!(
                    !captured.span_stacks[stacks_before..]
                        .iter()
                        .flatten()
                        .any(|span| span.starts_with("layout-save")),
                    "restoring must not save"
                );
                assert!(captured.lines[lines_before..].contains(
                    &"layout-restore file=layout.json: DEBUG message=restored applied=6 skipped=0"
                        .to_string()
                ));
            }

            // Resetting saves an empty user layer; a second reset drops nothing and does not save.
            let stacks_before_reset = trace.0.lock().unwrap().span_stacks.len();
            gpui::AnyWindowHandle::from(window)
                .update(&mut app, |_, window, cx| {
                    host.reset_layout(window, cx);
                })
                .unwrap();
            draw(window, &mut app, &shown);
            assert_eq!(*shown.borrow(), ["a", "inner", "b", "c"]);
            let after_reset = fs::read_to_string(&layout_path).unwrap();
            assert_eq!(yard_layout::decode(&after_reset), Ok(Vec::new()));
            assert!(
                trace.0.lock().unwrap().span_stacks[stacks_before_reset..]
                    .iter()
                    .any(|stack| {
                        stack
                            == &[
                                "window id=4294967297".to_string(),
                                r#"layout-reset extension="tour""#.to_string(),
                                "layout-save operations=0".to_string(),
                                "file-write path=layout.json".to_string(),
                            ]
                    })
            );

            let stacks_before_empty_reset = trace.0.lock().unwrap().span_stacks.len();
            gpui::AnyWindowHandle::from(window)
                .update(&mut app, |_, window, cx| {
                    host.reset_layout(window, cx);
                })
                .unwrap();
            assert!(
                !trace.0.lock().unwrap().span_stacks[stacks_before_empty_reset..]
                    .iter()
                    .flatten()
                    .any(|span| span.starts_with("layout-save")),
                "empty reset must not save"
            );
        }

        // 4. Skipping every shell and model refusal while applying the rest; next op rewrites.
        let mixed_json = r#"{
  "version": 1,
  "operations": [
    {"op": "add", "node": "bad_ns", "component": "tour.card", "extension": "guest", "parent": "root", "slot": "children"},
    {"op": "add", "node": "bad_comp", "component": "tour.missing", "extension": "tour", "parent": "root", "slot": "children"},
    {"op": "add", "node": "bad_slot", "component": "tour.card", "extension": "tour", "parent": "a", "slot": "undeclared"},
    {"op": "move", "node": "b", "parent": "a", "slot": "undeclared", "index": 0},
    {"op": "add", "node": "a", "component": "tour.card", "extension": "tour", "parent": "root", "slot": "children"},
    {"op": "add", "node": "orphan", "component": "tour.card", "extension": "tour", "parent": "gone", "slot": "children"},
    {"op": "remove", "node": "gone"},
    {"op": "remove", "node": "root"},
    {"op": "move", "node": "gone", "parent": "root", "slot": "children", "index": 0},
    {"op": "move", "node": "b", "parent": "gone", "slot": "children", "index": 0},
    {"op": "move", "node": "root", "parent": "a", "slot": "body", "index": 0},
    {"op": "move", "node": "a", "parent": "inner", "slot": "body", "index": 0},
    {"op": "hide", "node": "gone"},
    {"op": "show", "node": "root"},
    {"op": "add", "node": "kept_card", "component": "tour.card", "extension": "tour", "parent": "root", "slot": "children"},
    {"op": "show", "node": "d"}
  ]
}"#;
        fs::write(&layout_path, mixed_json).unwrap();
        {
            let mut app = cx.new_app();
            app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
            let (_, guest, shown, views) = setup_default(&mut app);
            let lines_before = trace.0.lock().unwrap().lines.len();
            let window = open_and_draw(&mut app, &shown);

            assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 2);
            assert_eq!(*shown.borrow(), ["a", "inner", "b", "c", "d", "kept_card"]);
            assert!(
                views
                    .borrow()
                    .get("kept_card")
                    .and_then(|v| v.upgrade())
                    .is_some()
            );
            assert_eq!(
                fs::read_to_string(&layout_path).unwrap(),
                mixed_json,
                "restore leaves skipped operations untouched on disk until the next save"
            );

            let restore_lines = trace.0.lock().unwrap().lines[lines_before..].to_vec();
            for (op, node, reason) in [
                (
                    "add",
                    "bad_ns",
                    "component is not in the extension's namespace",
                ),
                ("add", "bad_comp", "component is not registered"),
                ("add", "bad_slot", "parent component does not declare slot"),
                ("move", "b", "parent component does not declare slot"),
                ("add", "a", "the node ID is already in the tree"),
                ("add", "orphan", "the parent is not in the tree"),
                ("remove", "gone", "the node is not in the tree"),
                (
                    "remove",
                    "root",
                    "the root cannot be removed, moved, hidden, or shown",
                ),
                ("move", "gone", "the node is not in the tree"),
                ("move", "b", "the parent is not in the tree"),
                (
                    "move",
                    "root",
                    "the root cannot be removed, moved, hidden, or shown",
                ),
                (
                    "move",
                    "a",
                    "moving a node into itself or its subtree would make a cycle",
                ),
                ("hide", "gone", "the node is not in the tree"),
                (
                    "show",
                    "root",
                    "the root cannot be removed, moved, hidden, or shown",
                ),
            ] {
                let expected = format!(
                    "layout-restore file=layout.json: INFO message=skipped op={op:?} node={node:?} reason={reason:?}"
                );
                assert!(
                    restore_lines.contains(&expected),
                    "missing {expected} in {restore_lines:#?}"
                );
            }
            assert!(
                restore_lines.contains(
                    &"layout-restore file=layout.json: DEBUG message=restored applied=2 skipped=14"
                        .to_string()
                )
            );

            // Next recorded operation rewrites the file without any skipped operation.
            app.update(|cx| guest.set_hidden(cx, "b", true));
            draw(window, &mut app, &shown);
            let rewritten = fs::read_to_string(&layout_path).unwrap();
            let decoded = yard_layout::decode(&rewritten).unwrap();
            assert_eq!(decoded.len(), 3);
            assert!(!rewritten.contains("bad_ns"));
            assert!(!rewritten.contains("orphan"));
        }

        // 5. Invalid, wrong-version, and unreadable files warn and stay untouched.
        WARNINGS.lock().unwrap().clear();
        for bad_contents in [
            "not json",
            r#"{"version": 2, "operations": []}"#,
            r#"{"version": 1, "operations": [{"op": "unknown", "node": "a"}]}"#,
        ] {
            fs::write(&layout_path, bad_contents).unwrap();
            let warnings_before = WARNINGS.lock().unwrap().len();
            let mut app = cx.new_app();
            app.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
            let (_, _, shown, _) = setup_default(&mut app);
            let _window = open_and_draw(&mut app, &shown);
            assert_eq!(app.update(|cx| shell::user_layer_len(cx)), 0);
            assert_eq!(*shown.borrow(), ["a", "inner", "b", "c"]);
            assert_eq!(
                fs::read_to_string(&layout_path).unwrap(),
                bad_contents,
                "invalid layout file must be left untouched"
            );
            let warnings = WARNINGS.lock().unwrap();
            assert_eq!(warnings.len(), warnings_before + 1, "{warnings:#?}");
            let (level, message) = warnings.last().unwrap();
            assert_eq!(*level, log::Level::Warn);
            assert!(
                message.starts_with("the saved layout cannot be read: "),
                "{message}"
            );
        }

        // Unreadable path (a directory where layout.json is expected) warns on restore and on save.
        let dir_path = temp.path().join("as_directory");
        fs::create_dir_all(&dir_path).unwrap();
        let warnings_before = WARNINGS.lock().unwrap().len();
        {
            let mut app = cx.new_app();
            app.update(|cx| shell::set_layout_file(cx, dir_path.clone()));
            let (_, guest, shown, _) = setup_default(&mut app);
            let window = open_and_draw(&mut app, &shown);
            assert_eq!(*shown.borrow(), ["a", "inner", "b", "c"]);
            assert!(dir_path.is_dir());
            {
                let warnings = WARNINGS.lock().unwrap();
                assert_eq!(warnings.len(), warnings_before + 1);
                assert!(
                    warnings
                        .last()
                        .unwrap()
                        .1
                        .starts_with("the saved layout cannot be read: ")
                );
            }
            // Failed save warns via write_atomic, while the layout in memory still updates.
            app.update(|cx| guest.set_hidden(cx, "b", true));
            draw(window, &mut app, &shown);
            assert_eq!(*shown.borrow(), ["a", "inner", "c"]);
            assert_eq!(WARNINGS.lock().unwrap().len(), warnings_before + 2);
        }
    });

    assert!(trace.0.lock().unwrap().entered.is_empty());
}
