use gpui::{AppContext, IntoElement, ParentElement, SharedString, Styled, TestAppContext, px};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use yard_shell as shell;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs.
const PROBE: &str = "item_slots_probe";

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
        if attributes.metadata().target().starts_with("yard_shell") {
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
        if (!target.starts_with("yard_shell") && target != PROBE)
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
}

// The one process-wide diagnostic logger this file installs once; alone here,
// it cannot collide with another test's logger.
struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell")
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        match record.level() {
            log::Level::Error => tracing::error!(target: PROBE, "{}", record.args()),
            log::Level::Warn => tracing::warn!(target: PROBE, "{}", record.args()),
            log::Level::Info => tracing::info!(target: PROBE, "{}", record.args()),
            log::Level::Debug => tracing::debug!(target: PROBE, "{}", record.args()),
            log::Level::Trace => tracing::trace!(target: PROBE, "{}", record.args()),
        }
    }
    fn flush(&self) {}
}

// The model the item slot is over: a list of names.
struct Names(Vec<SharedString>);

impl shell::Items for Names {
    fn items(&self) -> Vec<SharedString> {
        self.0.clone()
    }
}

struct Card;

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div().size_full()
    }
}

struct Container {
    slots: shell::Slots,
}

impl gpui::Render for Container {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div().size_full().flex().flex_col().children(
            self.slots
                .views("children")
                .into_iter()
                .map(|view| gpui::div().flex_1().min_h_0().child(view)),
        )
    }
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
    window.update(app, |_, window, _| window.refresh()).unwrap();
    draw(window, app);
    window
}

// True when `stack` holds each group of `chain` inside the one before it.
fn inside(stack: &[String], chain: &[&str]) -> bool {
    let mut from = 0;
    for group in chain {
        match stack[from..]
            .iter()
            .position(|label| label.starts_with(*group))
        {
            Some(at) => from += at + 1,
            None => return false,
        }
    }
    true
}

// Alone in this binary: every call reaching these log lines shares this one
// subscriber and logger.
#[gpui::test]
fn item_slots_log_their_groups_and_each_refusal(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    let (mark_change, mark_refusal, mark_bare) =
        tracing::subscriber::with_default(trace.clone(), || {
            let mut app = cx.new_app();
            let host = app.update(|cx| {
                yard_ui::init(cx);
                let host = shell::Extension::new(cx, "tour").unwrap();
                let guest = shell::Extension::new(cx, "guest").unwrap();
                host.register(cx, "tour.names", &["children"], |_, slots, _, cx| {
                    cx.new(|_| Container { slots }).into()
                });
                host.provide::<Names>(cx, "tour.names", |_, cx| {
                    cx.new(|_| Names(vec!["ada".into(), "bob".into(), "cy".into()]))
                });
                host.register(cx, "tour.card", &[], |_, _, _, cx| cx.new(|_| Card).into());
                guest.register(cx, "guest.card", &["body"], |_, _, _, cx| {
                    cx.new(|_| Card).into()
                });
                host.place_root(cx, "root", "tour.names");
                host.place(cx, "row", "yard.row", "root", "children");
                host.item_slot::<Names>(cx, "row", "children");
                host.place(cx, "name", "tour.card", "row", "children");
                // Declaring an item slot on a node another extension placed
                // is a bug: an ERROR inside the declaration's own group.
                guest.place(cx, "theirs", "guest.card", "root", "children");
                host.item_slot::<Names>(cx, "theirs", "body");
                host
            });
            let window = open_and_draw(&mut app);
            let names = app.update(|cx| shell::context::<Names>(cx, "name").unwrap());
            // `cy` stays, `dee` and `eve` arrive, `ada` and `bob` leave.
            let mark_change = trace.mark();
            app.update(|cx| {
                names.update(cx, |model, cx| {
                    model.0 = vec!["cy".into(), "dee".into(), "eve".into()];
                    cx.notify();
                });
            });
            draw(window, &mut app);
            assert_eq!(
                app.update(|cx| shell::item(cx, "name@dee")),
                Some(SharedString::from("dee"))
            );
            assert!(app.update(|cx| shell::item(cx, "name@ada")).is_none());
            // An operation that names a copy is refused with an INFO.
            let mark_refusal = trace.mark();
            app.update(|cx| host.set_hidden(cx, "name@dee", true));
            // A slot whose model is found nowhere warns once, however many
            // frames it sits through.
            let mut bare = cx.new_app();
            bare.update(|cx| {
                yard_ui::init(cx);
                let host = shell::Extension::new(cx, "tour").unwrap();
                host.register(cx, "tour.card", &[], |_, _, _, cx| cx.new(|_| Card).into());
                host.place_root(cx, "root", "yard.row");
                host.item_slot::<Names>(cx, "root", "children");
                host.place(cx, "name", "tour.card", "root", "children");
            });
            let mark_bare = trace.mark();
            let bare_window = open_and_draw(&mut bare);
            draw(bare_window, &mut bare);
            (mark_change, mark_refusal, mark_bare)
        });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    let lines: Vec<String> = captured
        .lines
        .iter()
        .map(|line| line.replace('"', ""))
        .collect();
    let stacks: Vec<Vec<String>> = captured
        .span_stacks
        .iter()
        .map(|stack| stack.iter().map(|label| label.replace('"', "")).collect())
        .collect();
    // Each declaration opens `item-slot-declare{node, slot, model}` and logs
    // `declared`; a refused one logs its ERROR inside its own group.
    assert!(
        lines.iter().any(|line| line
            .starts_with("item-slot-declare node=row slot=children model=")
            && line.contains("Names")
            && line.contains("DEBUG")
            && line.contains("declared")),
        "missing `declared` inside row's item-slot-declare: {lines:#?}"
    );
    assert!(
        lines.iter().any(
            |line| line.starts_with("item-slot-declare node=theirs slot=body model=")
                && line.contains("ERROR")
        ),
        "missing the refused declaration's ERROR inside its group: {lines:#?}"
    );
    // `items-changed{slot, added, removed}` names the slot and the keys that
    // came and went.
    assert!(
        stacks.iter().any(|stack| stack.last().is_some_and(|label| {
            label.starts_with("items-changed")
                && label.contains("slot")
                && label.contains("added")
                && label.contains("removed")
                && label.contains("dee")
                && label.contains("eve")
                && label.contains("ada")
                && label.contains("bob")
        })),
        "missing items-changed{{slot, added, removed}}: {stacks:#?}"
    );
    // It holds an `item-copy{slot, key}` for each new key.
    for key in ["dee", "eve"] {
        assert!(
            stacks.iter().any(|stack| {
                stack
                    .last()
                    .is_some_and(|label| label.starts_with("item-copy") && label.contains(key))
                    && inside(&stack[..stack.len() - 1], &["items-changed"])
            }),
            "missing item-copy {{{key}}} inside items-changed: {stacks:#?}"
        );
    }
    // The new copy's view is created inside its `item-copy`.
    assert!(
        stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("component-create") && label.contains("name@dee")
            }) && inside(&stack[..stack.len() - 1], &["items-changed", "item-copy"])
                && stack
                    .iter()
                    .any(|label| label.starts_with("item-copy") && label.contains("dee"))
        }),
        "missing name@dee's component-create inside item-copy: {stacks:#?}"
    );
    // And a `view dropped` for each key that left.
    for copy in ["name@ada", "name@bob"] {
        assert!(
            lines.iter().skip(mark_change).any(|line| {
                line.starts_with("items-changed")
                    && line.contains("DEBUG")
                    && line.contains("view dropped")
                    && line.contains(copy)
            }),
            "missing `view dropped` for {copy} inside items-changed: {lines:#?}"
        );
    }
    // A copy is not a node of the tree: naming it is refused with an INFO
    // inside layout-op.
    assert!(
        lines.iter().skip(mark_refusal).any(|line| {
            line.starts_with("layout-op op=hide node=name@dee extension=tour: INFO")
                && line.contains("name@dee is a copy of an item slot")
        }),
        "missing the refused op's INFO: {lines:#?}"
    );
    // The item slot that finds no model warns once.
    let warns: Vec<&String> = lines
        .iter()
        .skip(mark_bare)
        .filter(|line| line.contains(": WARN"))
        .collect();
    assert_eq!(warns.len(), 1, "{lines:#?}");
    assert!(
        warns[0].contains("model"),
        "the one WARN is the missing-model one: {warns:?}"
    );
}
