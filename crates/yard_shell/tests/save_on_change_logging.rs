use gpui::{AppContext, InteractiveElement, IntoElement, Styled, TestAppContext, px};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use yard_shell as shell;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs.
const PROBE: &str = "save_on_change_probe";

static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

// The one process-wide diagnostic logger this file installs once; alone in
// its binary, it cannot collide with another test's logger.
struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell")
            || metadata.target().starts_with("yard_state")
            || metadata.target().starts_with("yard_paths")
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        RECORDS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((record.level(), record.args().to_string()));
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

fn errors() -> Vec<String> {
    RECORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter(|(level, _)| *level == log::Level::Error)
        .map(|(_, message)| message.clone())
        .collect()
}

// The groups the code opens and the lines it logs: a span stack records the
// groups a yard_* group was opened inside, so `file-write` is checked inside
// `state-save`, not merely beside it.
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
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let id = captured.spans.len() as u64 + 1;
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let label = format!("{}{}", attributes.metadata().name(), fields.0);
        let target = attributes.metadata().target();
        if target.starts_with("yard_shell")
            || target.starts_with("yard_state")
            || target.starts_with("yard_paths")
        {
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
        if !(target.starts_with("yard_shell")
            || target.starts_with("yard_state")
            || target.starts_with("yard_paths")
            || target == PROBE)
        {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let group = captured
            .entered
            .last()
            .map(|id| captured.spans[id].as_str())
            .unwrap_or("none");
        let line = format!("{group}: {}{}", event.metadata().level(), fields.0);
        captured.lines.push(line);
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entered
            .push(id.into_u64());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(
            self.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entered
                .pop(),
            Some(id.into_u64())
        );
    }
}

impl Trace {
    fn mark(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .lines
            .len()
    }
    fn since(&self, mark: usize) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .lines[mark..]
            .iter()
            .map(|line| line.replace('"', ""))
            .collect()
    }
    fn stack_mark(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .span_stacks
            .len()
    }
    fn stacks_since(&self, mark: usize) -> Vec<Vec<String>> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .span_stacks[mark..]
            .iter()
            .map(|stack| stack.iter().map(|label| label.replace('"', "")).collect())
            .collect()
    }
}

// A card fills the zone its container gives it and is found as `CARD-<node>`;
// its `count` is the state it gives the shell as JSON and takes back.
struct Card {
    node: String,
    count: u64,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
    }
}

impl shell::NodeState for Card {
    fn save_state(&self, _: &gpui::App) -> serde_json::Value {
        json!({"count": self.count})
    }
    fn restore_state(
        &mut self,
        state: serde_json::Value,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) {
        if let Some(count) = state.get("count").and_then(serde_json::Value::as_u64) {
            self.count = count;
        }
    }
}

// `host` registers `tour.card` with its state, and places the default
// `yard.row` of three cards.
fn extension(app: &mut TestAppContext) -> shell::Extension {
    app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        host.register(cx, "tour.card", &[], move |node, _, _, cx| {
            let card = cx.new(|_| Card {
                node: node.to_string(),
                count: 0,
            });
            card.into()
        });
        host.register_state::<Card>(cx, "tour.card");
        host.place_root(cx, "root", "yard.row");
        for node in ["a", "b", "c"] {
            host.place(cx, node, "tour.card", "root", "children");
        }
        host
    })
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

// The contract's logging: `state changed` at `trace`, and each save the
// `state-save` group names — `reason=change` after the wait, `reason=quit` at
// quit — holding `file-write`.
#[gpui::test]
fn save_on_change_logging(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("state.json");
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let _host = extension(&mut app);
        app.update(|cx| shell::set_state_file(cx, file.clone()));
        let _window = open_and_draw(&mut app);

        // `state_changed` can run on every keystroke: it is a `trace`.
        let mark = trace.mark();
        app.update(shell::state_changed);
        let lines = trace.since(mark);
        assert!(
            lines
                .iter()
                .any(|line| line.contains(": TRACE") && line.contains("state changed")),
            "missing `state changed` at trace: {lines:#?}"
        );

        // When no change has come for 300 ms, the save is the
        // `state-save{windows, nodes}` group with `reason=change`, holding
        // `file-write`.
        let stacks_mark = trace.stack_mark();
        app.executor().advance_clock(Duration::from_millis(300));
        app.run_until_parked();
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.iter().any(|stack| {
                let save = stack.iter().position(|label| {
                    label.starts_with("state-save")
                        && label.contains("windows=1")
                        && label.contains("nodes=4")
                        && label.contains("reason=change")
                });
                let write = stack.iter().position(|label| {
                    label.starts_with("file-write") && label.contains("state.json")
                });
                matches!((save, write), (Some(save), Some(write)) if save < write)
            }),
            "no `state-save{{windows=1, nodes=4, reason=change}}` holding `file-write`: {stacks:#?}"
        );

        // Quitting writes once more: `state-save{reason=quit}` holding
        // `file-write`.
        let stacks_mark = trace.stack_mark();
        app.quit();
        app.run_until_parked();
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.iter().any(|stack| {
                let save = stack.iter().position(|label| {
                    label.starts_with("state-save")
                        && label.contains("windows=1")
                        && label.contains("nodes=4")
                        && label.contains("reason=quit")
                });
                let write = stack.iter().position(|label| {
                    label.starts_with("file-write") && label.contains("state.json")
                });
                matches!((save, write), (Some(save), Some(write)) if save < write)
            }),
            "no `state-save{{windows=1, nodes=4, reason=quit}}` holding `file-write`: {stacks:#?}"
        );
        assert!(errors().is_empty(), "{:?}", errors());
    });
    assert!(
        trace
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entered
            .is_empty()
    );
}
