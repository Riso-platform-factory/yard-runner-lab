use gpui::{AppContext, IntoElement, Styled, TestAppContext, px};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use yard_layout::{NodeId, Position};
use yard_shell as shell;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs.
const PROBE: &str = "placement_order_probe";

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
            && target != PROBE
        {
            return;
        }
        if *event.metadata().level() == tracing::Level::TRACE {
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

struct Card;

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div().size_full()
    }
}

fn id(value: &str) -> NodeId {
    NodeId::parse(value).unwrap()
}

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

// A line in `group` that logs the resolved `index` for `verb` (`placed` or
// `moved`), whether the index is a field or inside the message.
fn resolved(lines: &[String], group: &str, verb: &str, index: usize) -> bool {
    lines.iter().any(|line| {
        line.starts_with(group)
            && line.contains(verb)
            && (line.contains(&format!("index={index}"))
                || line.contains(&format!("{verb} {index}")))
    })
}

// Alone in this binary: every call reaching these log lines shares this one
// subscriber and logger.
#[gpui::test]
fn placement_order_logs_the_resolved_index_and_each_refusal(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let host = app.update(|cx| {
            yard_ui::init(cx);
            let host = shell::Extension::new(cx, "tour").unwrap();
            host.register(cx, "tour.card", &[], |_, _, _, cx| cx.new(|_| Card).into());
            host.place_root(cx, "root", "yard.row");
            for name in ["a", "b", "c"] {
                host.place(cx, name, "tour.card", "root", "children");
            }
            // A named position at startup: `d` before `b`, resolved to index 1.
            host.place_at(
                cx,
                "d",
                "tour.card",
                "root",
                "children",
                Position::Before(id("b")),
            );
            // A placement naming a node that is not in the slot is refused
            // inside node-place.
            host.place_at(
                cx,
                "lost",
                "tour.card",
                "root",
                "children",
                Position::After(id("ghost")),
            );
            host
        });
        let _window = open_and_draw(&mut app);
        app.update(|cx| {
            // The slot is now [a, d, b, c]: `a` moves right after `b`,
            // resolved to index 2.
            host.move_to(cx, "a", "root", "children", Position::After(id("b")));
            // A move beside a node that is not in the slot, and beside
            // itself, are refused inside layout-op.
            host.move_to(cx, "d", "root", "children", Position::Before(id("ghost")));
            host.move_to(cx, "d", "root", "children", Position::After(id("d")));
        });
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    let lines: Vec<String> = captured
        .lines
        .iter()
        .map(|line| line.replace('"', ""))
        .collect();
    // The model logs the resolved index at debug inside the change's group.
    assert!(
        resolved(
            &lines,
            "node-place node=d component=tour.card parent=root slot=children: DEBUG",
            "placed",
            1,
        ),
        "the resolved index of the place: {lines:#?}"
    );
    assert!(
        resolved(
            &lines,
            "layout-op op=move node=a extension=tour: DEBUG",
            "moved",
            2,
        ),
        "the resolved index of the move: {lines:#?}"
    );
    // A position whose node is not in the slot, or a node beside itself, is
    // an INFO in the same group.
    assert!(
        lines.iter().any(|line| line.starts_with(
            "node-place node=lost component=tour.card parent=root slot=children: INFO"
        ) && line.contains("ghost is not in root.children")),
        "the refused place: {lines:#?}"
    );
    assert!(
        lines.iter().any(
            |line| line.starts_with("layout-op op=move node=d extension=tour: INFO")
                && line.contains("ghost is not in root.children")
        ),
        "the refused move: {lines:#?}"
    );
    assert!(
        lines.iter().any(
            |line| line.starts_with("layout-op op=move node=d extension=tour: INFO")
                && line.contains("d cannot be placed beside itself")
        ),
        "the move beside itself: {lines:#?}"
    );
    // Refusals are expected: no ERROR.
    assert!(
        !lines.iter().any(|line| line.contains(": ERROR")),
        "{lines:#?}"
    );
}
