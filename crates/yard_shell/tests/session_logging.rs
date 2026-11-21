use gpui::{AppContext, InteractiveElement, IntoElement, Styled, TestAppContext};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use yard_shell as shell;
use yard_shell::{SessionWindow, new_session_shell, saved_session, track_session};

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs, with the records' groups.
const PROBE: &str = "session_logging_probe";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-session-logging-{}-{}-{nanos:x}",
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

struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell") || metadata.target().starts_with("yard_paths")
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
            && !target.starts_with("yard_paths")
            && target != PROBE)
            || *event.metadata().level() == tracing::Level::TRACE
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
            .to_vec()
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
            .to_vec()
    }
}

// A card fills the zone its container gives it and is found as `CARD-<node>`.
struct Card {
    node: String,
}

impl gpui::Render for Card {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div()
            .debug_selector(|| format!("CARD-{}", self.node))
            .size_full()
    }
}

// `host` registers `tour.card` and places the default `yard.row` of three
// cards.
fn extension(app: &mut TestAppContext) -> shell::Extension {
    app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        host.register(cx, "tour.card", &[], |node, _, _, cx| {
            cx.new(|_| Card {
                node: node.to_string(),
            })
            .into()
        });
        host.place_root(cx, "root", "yard.row");
        for node in ["a", "b", "c"] {
            host.place(cx, node, "tour.card", "root", "children");
        }
        host
    })
}

fn open(app: &mut TestAppContext) -> gpui::WindowHandle<shell::Shell> {
    app.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), shell::new_shell)
            .unwrap()
    })
}

// The window the startup opens for `saved`: a root built by
// `new_session_shell` on a window with the saved bounds and display.
fn reopen(app: &mut TestAppContext, saved: &SessionWindow) -> gpui::WindowHandle<shell::Shell> {
    app.update(|cx| {
        cx.open_window(
            gpui::WindowOptions {
                window_bounds: saved.bounds,
                display_id: saved.display,
                ..Default::default()
            },
            |window, cx| new_session_shell(window, cx, saved),
        )
        .unwrap()
    })
}

// The window with the keyboard focus is the one calls act on.
fn activate(window: gpui::WindowHandle<shell::Shell>, app: &mut TestAppContext) {
    window
        .update(app, |_, window, _| window.activate_window())
        .unwrap();
    app.run_until_parked();
}

// The changes made so far reach the file: deferred layout work runs, the
// 100 ms wait of a save passes, and the write itself finishes.
fn settle(app: &mut TestAppContext) {
    app.run_until_parked();
    app.executor().advance_clock(Duration::from_millis(200));
    app.run_until_parked();
}

// The contract's logging, alone in this binary: `session-restore{file}`
// inside `app-startup`, ending `no saved session`, the `WARN`, or
// `restored {windows}`; a save is `session-save{windows}` holding
// `file-write{path}`; each window opened from the session gets its
// `window-open` with `window{id}` and `layout-restore`.
#[gpui::test]
fn session_logging_covers_the_contract_rows(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    let temp = TempDir::new();
    let file = temp.path().join("session.json");
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();

        // No file: `session-restore` inside `app-startup` ends
        // `no saved session`.
        let mark = trace.mark();
        let stacks_mark = trace.stack_mark();
        {
            let _startup = tracing::info_span!("app-startup").entered();
            assert!(app.update(|cx| saved_session(cx, &file)).is_none());
        }
        let lines = trace.since(mark);
        assert!(
            lines.contains(
                &"session-restore file=session.json: DEBUG message=no saved session".to_string()
            ),
            "missing `no saved session` in {lines:#?}"
        );
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.contains(&vec![
                "app-startup".to_string(),
                "session-restore file=session.json".to_string(),
            ]),
            "session-restore was not inside app-startup: {stacks:#?}"
        );

        // A file that cannot be read ends the same group with the `WARN`.
        fs::write(&file, "this is not json").unwrap();
        let mark = trace.mark();
        let before = warnings().len();
        {
            let _startup = tracing::info_span!("app-startup").entered();
            assert!(app.update(|cx| saved_session(cx, &file)).is_none());
        }
        assert_eq!(warnings().len(), before + 1, "{:?}", warnings());
        let lines = trace.since(mark);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("session-restore file=session.json") && line.contains(": WARN")
            }),
            "the broken session's WARN was not inside session-restore: {lines:#?}"
        );

        // A run of two windows: a save is `session-save{windows}` holding
        // `file-write{path}`.
        fs::remove_file(&file).unwrap();
        let host = extension(&mut app);
        app.update(|cx| track_session(cx, file.clone()));
        let first = open(&mut app);
        activate(first, &mut app);
        app.update(|cx| host.set_hidden(cx, "a", true));
        let second = open(&mut app);
        activate(second, &mut app);
        app.update(|cx| host.set_hidden(cx, "b", true));

        let stacks_mark = trace.stack_mark();
        settle(&mut app);
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.iter().any(|stack| {
                stack.ends_with(&[
                    "session-save windows=2".to_string(),
                    "file-write path=session.json".to_string(),
                ])
            }),
            "missing `session-save windows=2` holding `file-write`: {stacks:#?}"
        );

        // The next start: `session-restore` inside `app-startup` ends
        // `restored {windows}`.
        let mut restart = cx.new_app();
        extension(&mut restart);
        let mark = trace.mark();
        let stacks_mark = trace.stack_mark();
        let saved = {
            let _startup = tracing::info_span!("app-startup").entered();
            restart
                .update(|cx| saved_session(cx, &file))
                .expect("a saved session")
        };
        let lines = trace.since(mark);
        assert!(
            lines.contains(
                &"session-restore file=session.json: DEBUG message=restored windows=2".to_string()
            ),
            "missing `restored windows=2` in {lines:#?}"
        );
        let stacks = trace.stacks_since(stacks_mark);
        assert!(
            stacks.contains(&vec![
                "app-startup".to_string(),
                "session-restore file=session.json".to_string(),
            ]),
            "session-restore was not inside app-startup: {stacks:#?}"
        );

        // Each window opened from the session: its `window-open`, with
        // `window{id}` and `layout-restore` inside it, as for the main
        // window.
        let stacks_mark = trace.stack_mark();
        {
            let _open = tracing::info_span!("window-open").entered();
            for window in &saved {
                reopen(&mut restart, window);
            }
        }
        let stacks = trace.stacks_since(stacks_mark);
        let reopened = stacks
            .iter()
            .filter(|stack| {
                let open = stack
                    .iter()
                    .position(|label| label.starts_with("window-open"));
                let id = stack
                    .iter()
                    .position(|label| label.starts_with("window id="));
                let restore = stack
                    .iter()
                    .position(|label| label.starts_with("layout-restore"));
                matches!(
                    (open, id, restore),
                    (Some(open), Some(id), Some(restore)) if open < id && id < restore
                )
            })
            .count();
        assert_eq!(
            reopened,
            saved.len(),
            "a session window's layout-restore was not inside window{{id}} in window-open: {stacks:#?}"
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
