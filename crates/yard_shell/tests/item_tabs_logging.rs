use gpui::{AppContext, IntoElement, ParentElement, SharedString, Styled, TestAppContext, px};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use yard_shell as shell;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs.
const PROBE: &str = "item_tabs_probe";

#[derive(Default)]
struct Captured {
    spans: BTreeMap<u64, String>,
    entered: Vec<u64>,
    lines: Vec<String>,
    // The groups each line was logged inside, parallel to `lines`: a line
    // the contract places inside a group may sit under another group in it.
    line_stacks: Vec<Vec<String>>,
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
        let stack: Vec<String> = captured
            .entered
            .iter()
            .map(|entered| captured.spans[entered].clone())
            .collect();
        let group = stack.last().map(|label| label.as_str()).unwrap_or("none");
        let line = format!("{group}: {}{}", event.metadata().level(), fields.0);
        captured.lines.push(line);
        captured.line_stacks.push(stack);
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.0.lock().unwrap().entered.push(id.into_u64());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(self.0.lock().unwrap().entered.pop(), Some(id.into_u64()));
    }
}

impl Trace {
    // `(lines, span_stacks)` lengths: where a slice of what follows starts.
    fn marks(&self) -> (usize, usize) {
        let captured = self.0.lock().unwrap();
        (captured.lines.len(), captured.span_stacks.len())
    }
}

// The one process-wide diagnostic logger this file installs once; alone
// here, it cannot collide with another test's logger.
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

// The model the item tabs are over: a list of names. `close_item` removes
// the key, except `ada`, which it keeps; `new_item` adds `new-1`, except
// while `still`, when it adds nothing — each a change the contract logs for.
struct Names {
    names: Vec<SharedString>,
    still: bool,
}

impl shell::Items for Names {
    fn items(&self) -> Vec<SharedString> {
        self.names.clone()
    }
}

impl shell::ItemTabs for Names {
    fn close_item(
        &mut self,
        key: &SharedString,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if key.as_str() != "ada" {
            self.names.retain(|name| name != key);
        }
        cx.notify();
    }

    fn new_item(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if !self.still {
            self.names.push("new-1".into());
        }
        cx.notify();
    }
}

struct Card;

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div().size_full()
    }
}

struct Frame(shell::Slots);

impl gpui::Render for Frame {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div().size_full().flex().flex_col().children(
            self.0
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

// True when a line holding every part of `parts` was logged while a group
// named `group` holding `group_part` was open.
fn held_by(
    lines: &[String],
    stacks: &[Vec<String>],
    group: &str,
    group_part: &str,
    parts: &[&str],
) -> bool {
    lines.iter().zip(stacks.iter()).any(|(line, stack)| {
        parts.iter().all(|part| line.contains(part))
            && stack
                .iter()
                .any(|label| label.starts_with(group) && label.contains(group_part))
    })
}

// Alone in this binary: every call reaching these log lines shares this one
// subscriber and logger. Each mark is `(lines, span_stacks)` lengths taken
// just before the click or change whose logs it means to read.
#[gpui::test]
fn item_close_and_item_new_hold_what_the_models_notify_causes(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    let (close, kept, added, still) = tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let model = app.update(|cx| {
            yard_ui::init(cx);
            let host = shell::Extension::new(cx, "tour").unwrap();
            let model = cx.new(|_| Names {
                names: vec!["ada".into(), "bob".into(), "cy".into()],
                still: false,
            });
            host.provide_window(cx, model.clone());
            host.register(cx, "tour.frame", &["children"], |_, slots, _, cx| {
                cx.new(|_| Frame(slots)).into()
            });
            host.register(cx, "tour.card", &[], |_, _, _, cx| cx.new(|_| Card).into());
            host.place_root(cx, "root", "tour.frame");
            host.place(cx, "tabs", "yard.tabs", "root", "children");
            host.item_tabs::<Names>(cx, "tabs");
            host.place(cx, "name", "tour.card", "tabs", "children");
            // A node that is not a `yard.tabs`: an ERROR inside the
            // declaration's own group.
            host.place(cx, "row", "yard.row", "root", "children");
            host.item_tabs::<Names>(cx, "row");
            model
        });
        let window = open_and_draw(&mut app);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), &app);

        // `bob`'s close: `item-close{tabs, key}` holds the `items-changed`
        // the model's notify causes and the dropped copy's `view dropped`.
        let close = trace.marks();
        let bob = visual
            .debug_bounds("TAB-name@bob")
            .expect("bob's tab draws")
            .center();
        visual.simulate_mouse_move(bob, None, gpui::Modifiers::default());
        draw(window, &mut app);
        let button = visual
            .debug_bounds("CLOSE-name@bob")
            .expect("bob's close button draws on hover")
            .center();
        visual.simulate_click(button, gpui::Modifiers::default());
        draw(window, &mut app);

        // `ada`, which the model keeps: `item-close` holds `items
        // unchanged`.
        let kept = trace.marks();
        let ada = visual
            .debug_bounds("TAB-name@ada")
            .expect("ada's tab draws")
            .center();
        visual.simulate_mouse_move(ada, None, gpui::Modifiers::default());
        draw(window, &mut app);
        let button = visual
            .debug_bounds("CLOSE-name@ada")
            .expect("ada's close button draws on hover")
            .center();
        visual.simulate_click(button, gpui::Modifiers::default());
        draw(window, &mut app);

        // The new button, found as `NEW-<tabs>`: `item-new{tabs}` holds
        // the `items-changed` with the new key's `item-copy`, and `tab
        // selected` for the new item.
        let added = trace.marks();
        let button = visual
            .debug_bounds("NEW-tabs")
            .expect("the new button draws")
            .center();
        visual.simulate_click(button, gpui::Modifiers::default());
        draw(window, &mut app);

        // A model that adds nothing: `item-new` holds `items unchanged`.
        app.update(|cx| model.update(cx, |names, _| names.still = true));
        let still = trace.marks();
        let button = visual
            .debug_bounds("NEW-tabs")
            .expect("the new button draws")
            .center();
        visual.simulate_click(button, gpui::Modifiers::default());
        draw(window, &mut app);
        (close, kept, added, still)
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    let lines: Vec<String> = captured
        .lines
        .iter()
        .map(|line| line.replace('"', ""))
        .collect();
    let line_stacks: Vec<Vec<String>> = captured
        .line_stacks
        .iter()
        .map(|stack| stack.iter().map(|label| label.replace('"', "")).collect())
        .collect();
    let span_stacks: Vec<Vec<String>> = captured
        .span_stacks
        .iter()
        .map(|stack| stack.iter().map(|label| label.replace('"', "")).collect())
        .collect();
    drop(captured);

    // Declaring item tabs opens `item-slot-declare{node, slot, model}` and
    // logs `item tabs`; a refused one logs its ERROR inside its own group.
    assert!(
        lines.iter().any(|line| line
            .starts_with("item-slot-declare node=tabs slot=children model=")
            && line.contains("Names")
            && line.contains("DEBUG")
            && line.contains("item tabs")),
        "missing `item tabs` inside tabs's item-slot-declare: {lines:#?}"
    );
    assert!(
        lines.iter().any(
            |line| line.starts_with("item-slot-declare node=row slot=children")
                && line.contains("ERROR")
        ),
        "missing the refused declaration's ERROR inside its group: {lines:#?}"
    );

    // bob's `item-close{tabs, key}` holds `items-changed` with bob removed,
    // and `view dropped` for its copy.
    assert!(
        span_stacks[close.1..].iter().any(|stack| {
            inside(stack, &["item-close", "items-changed"])
                && stack.iter().any(|label| {
                    label.starts_with("item-close")
                        && label.contains("tabs=tabs")
                        && label.contains("key=bob")
                })
                && stack.last().is_some_and(|label| {
                    label.starts_with("items-changed")
                        && label.contains("removed")
                        && label.contains("bob")
                })
        }),
        "missing items-changed{{removed=bob}} inside item-close{{tabs, key=bob}}: {span_stacks:#?}"
    );
    assert!(
        held_by(
            &lines[close.0..],
            &line_stacks[close.0..],
            "item-close",
            "key=bob",
            &["DEBUG", "view dropped", "name@bob"]
        ),
        "missing `view dropped` for name@bob inside item-close: {lines:#?}"
    );

    // The kept item's `item-close` holds `items unchanged`, and no
    // `items-changed`.
    assert!(
        held_by(
            &lines[kept.0..],
            &line_stacks[kept.0..],
            "item-close",
            "key=ada",
            &["DEBUG", "items unchanged"]
        ),
        "missing `items unchanged` inside item-close{{key=ada}}: {lines:#?}"
    );
    assert!(
        !span_stacks[kept.1..].iter().any(|stack| {
            inside(stack, &["item-close", "items-changed"])
                && stack
                    .iter()
                    .any(|label| label.starts_with("item-close") && label.contains("key=ada"))
        }),
        "a kept item has no items-changed inside item-close: {span_stacks:#?}"
    );

    // `item-new{tabs}` holds `items-changed` with the new key's `item-copy`,
    // and `tab selected` for the new item.
    assert!(
        span_stacks[added.1..].iter().any(|stack| {
            inside(stack, &["item-new", "items-changed", "item-copy"])
                && stack
                    .iter()
                    .any(|label| label.starts_with("item-new") && label.contains("tabs=tabs"))
                && stack.iter().any(|label| {
                    label.starts_with("items-changed")
                        && label.contains("added")
                        && label.contains("new-1")
                })
                && stack.last().is_some_and(|label| {
                    label.starts_with("item-copy") && label.contains("key=new-1")
                })
        }),
        "missing item-copy{{key=new-1}} inside items-changed inside item-new: {span_stacks:#?}"
    );
    assert!(
        held_by(
            &lines[added.0..],
            &line_stacks[added.0..],
            "item-new",
            "tabs=tabs",
            &["DEBUG", "tab selected", "name@new-1"]
        ),
        "missing `tab selected` for the new item inside item-new: {lines:#?}"
    );

    // The still model's `item-new` holds `items unchanged`, and no
    // `items-changed`.
    assert!(
        held_by(
            &lines[still.0..],
            &line_stacks[still.0..],
            "item-new",
            "tabs=tabs",
            &["DEBUG", "items unchanged"]
        ),
        "missing `items unchanged` inside item-new: {lines:#?}"
    );
    assert!(
        !span_stacks[still.1..]
            .iter()
            .any(|stack| inside(stack, &["item-new", "items-changed"])),
        "an unchanged model has no items-changed inside item-new: {span_stacks:#?}"
    );
}
