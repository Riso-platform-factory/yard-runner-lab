use gpui::{AppContext, InteractiveElement, IntoElement, Styled, TestAppContext, px};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use yard_shell as shell;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs, with the records' groups.
const PROBE: &str = "node_state_probe";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-node-state-logging-{}-{}-{nanos:x}",
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

static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

// The one process-wide diagnostic logger this file installs once; alone
// here, it cannot collide with another test's logger.
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

fn records() -> Vec<(log::Level, String)> {
    RECORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn warnings() -> Vec<String> {
    records()
        .into_iter()
        .filter(|(level, _)| *level == log::Level::Warn)
        .map(|(_, message)| message)
        .collect()
}

fn errors() -> Vec<String> {
    records()
        .into_iter()
        .filter(|(level, _)| *level == log::Level::Error)
        .map(|(_, message)| message)
        .collect()
}

#[derive(Default)]
struct Captured {
    spans: BTreeMap<u64, String>,
    entered: Vec<u64>,
    lines: Vec<String>,
    span_stacks: Vec<Vec<String>>,
    // The stack of entered groups for each line pushed into `lines`.
    event_stacks: Vec<Vec<String>>,
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
            || *event.metadata().level() == tracing::Level::TRACE
        {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let stack: Vec<String> = captured
            .entered
            .iter()
            .map(|id| captured.spans[id].clone())
            .collect();
        let group = stack.last().cloned().unwrap_or_else(|| "none".to_string());
        let line = format!("{group}: {}{}", event.metadata().level(), fields.0);
        captured.lines.push(line.clone());
        captured.event_stacks.push(stack);
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
    // `(entered groups, line)` for each event since `mark`.
    fn events_since(&self, mark: usize) -> Vec<(Vec<String>, String)> {
        let captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        captured.event_stacks[mark..]
            .iter()
            .zip(captured.lines[mark..].iter())
            .map(|(stack, line)| {
                (
                    stack
                        .iter()
                        .map(|label| label.replace('"', ""))
                        .collect::<Vec<_>>(),
                    line.replace('"', ""),
                )
            })
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
        host.register(cx, "tour.card", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
                count: 0,
            })
            .into()
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

// The contract's logging, alone in this binary: `state-register{component}`
// ending `registered`, or an `ERROR` for a refusal; `state-restore{file}`
// ending `read {windows, nodes}`, `no saved state`, or the `WARN`; a
// `debug` `state given {node}` inside the view's `component-create`; and
// `state-save{windows, nodes}` holding `file-write{path}`.
#[gpui::test]
fn node_state_logging_covers_the_contract_rows(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    let temp = TempDir::new();
    let file = temp.path().join("state.json");
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();

        // A component registering state gets its own group ending
        // `registered`; each refusal is an `ERROR` in its group.
        let mark = trace.mark();
        let host = extension(&mut app);
        app.update(|cx| {
            host.register_state::<Card>(cx, "guest.card");
            host.register_state::<Card>(cx, "tour.card");
        });
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("state-register component=tour.card:")
                    && line.contains("DEBUG")
                    && line.contains("registered")
            }),
            "missing `registered` in state-register: {lines:#?}"
        );
        for component in ["guest.card", "tour.card"] {
            assert!(
                lines.iter().any(|line| {
                    line.starts_with(&format!("state-register component={component}:"))
                        && line.contains("ERROR")
                }),
                "missing the refusal ERROR for {component}: {lines:#?}"
            );
        }
        assert_eq!(errors().len(), 2, "{:?}", errors());

        // Reading at the start: a missing file ends the
        // `state-restore{file}` group with `no saved state`.
        let mark = trace.mark();
        let stacks_mark = trace.stack_mark();
        app.update(|cx| shell::set_state_file(cx, file.clone()));
        let _window = open_and_draw(&mut app);
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("state-restore file=state.json") && line.contains("no saved state")
            }),
            "missing `no saved state` in state-restore: {lines:#?}"
        );
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.iter().any(|stack| {
                stack
                    .last()
                    .is_some_and(|label| label.starts_with("state-restore file=state.json"))
            }),
            "no `state-restore file=state.json` group: {stacks:#?}"
        );

        // Saving at quit is `state-save{windows, nodes}` holding
        // `file-write{path}`.
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
                });
                let write = stack.iter().position(|label| {
                    label.starts_with("file-write") && label.contains("state.json")
                });
                matches!((save, write), (Some(save), Some(write)) if save < write)
            }),
            "no `state-save windows=1 nodes=4` holding `file-write`: {stacks:#?}"
        );

        // A file that cannot be read ends `state-restore` with the `WARN`,
        // never an `ERROR`.
        fs::write(&file, "this is not json").unwrap();
        let mut broken = cx.new_app();
        let _host = extension(&mut broken);
        let mark = trace.mark();
        let warnings_before = warnings().len();
        broken.update(|cx| shell::set_state_file(cx, file.clone()));
        let _window = open_and_draw(&mut broken);
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("state-restore file=state.json") && line.contains(": WARN")
            }),
            "the unreadable state file did not warn inside state-restore: {lines:#?}"
        );
        assert_eq!(warnings().len(), warnings_before + 1, "{:?}", warnings());
        assert_eq!(
            errors().len(),
            2,
            "an unreadable state file is not an ERROR: {:?}",
            errors()
        );

        // A good file: `read {windows, nodes}` ends the group, and every
        // state given is a `debug` inside the view's `component-create`.
        fs::write(
            &file,
            json!({
                "version": 1,
                "windows": [{
                    "a": {"component": "tour.card", "state": {"count": 3}},
                    "b": {"component": "tour.card", "state": {"count": 5}},
                    "c": {"component": "tour.card", "state": {"count": 7}},
                }],
            })
            .to_string(),
        )
        .unwrap();
        let mut restart = cx.new_app();
        let _host = extension(&mut restart);
        let mark = trace.mark();
        restart.update(|cx| shell::set_state_file(cx, file.clone()));
        let _window = open_and_draw(&mut restart);
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("state-restore file=state.json")
                    && line.contains("DEBUG")
                    && line.contains("read")
                    && line.contains("windows=1")
                    && line.contains("nodes=3")
            }),
            "missing `read {{windows, nodes}}` in state-restore: {lines:#?}"
        );
        let events = trace.events_since(mark);
        for node in ["a", "b", "c"] {
            assert!(
                events.iter().any(|(stack, line)| {
                    line.contains("DEBUG")
                        && line.contains("state given")
                        && line.contains(&format!("node={node}"))
                        && stack.iter().any(|label| {
                            label.starts_with("component-create")
                                && label.contains(&format!("node={node}"))
                        })
                }),
                "no `state given` for {node} inside its component-create: {events:#?}"
            );
        }
        assert_eq!(errors().len(), 2, "{:?}", errors());
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
