use gpui::{AppContext, TestAppContext, px};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use yard_shell::{saved_window_bounds, track_window_bounds};

const PROBE: &str = "window_bounds_probe";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-window-bounds-logging-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// A probe of the diagnostic log: every record the parts reach through `log`
// is re-made through `tracing`, so the trace below holds both logs.
struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_shell") || metadata.target().starts_with("yard_paths")
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
            && !target.starts_with("yard_paths")
            && target != PROBE)
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

fn write_saved(file: &Path, display: &str, state: &str, bounds: gpui::Bounds<gpui::Pixels>) {
    fs::write(
        file,
        json!({
            "version": 1,
            "display": display,
            "bounds": {
                "state": state,
                "x": f64::from(bounds.origin.x),
                "y": f64::from(bounds.origin.y),
                "width": f64::from(bounds.size.width),
                "height": f64::from(bounds.size.height),
            },
        })
        .to_string(),
    )
    .unwrap();
}

// A restore is `window-bounds-restore{file}` ending in `debug` `restored
// {state, display}`; a save is `window-bounds-save{state}` holding foundation
// 6's `file-write{path}`.
#[gpui::test]
fn window_bounds_log_their_groups(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let temp = TempDir::new();
        let file = temp.path().join("window.json");
        let mut app = cx.new_app();
        let display_uuid = app.update(|cx| {
            cx.displays()
                .into_iter()
                .next()
                .unwrap()
                .uuid()
                .unwrap()
                .to_string()
        });
        write_saved(
            &file,
            &display_uuid,
            "windowed",
            gpui::Bounds::new(
                gpui::point(px(120.), px(80.)),
                gpui::size(px(1024.), px(768.)),
            ),
        );
        let restored = app.update(|cx| saved_window_bounds(cx, &file));
        assert!(restored.is_some(), "a valid saved file was not restored");

        let window = app.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                        gpui::point(px(100.), px(60.)),
                        gpui::size(px(800.), px(600.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| gpui::Empty),
            )
            .unwrap()
        });
        let any_window = gpui::AnyWindowHandle::from(window);
        let track_file = file.clone();
        any_window
            .update(&mut app, move |_, window, cx| {
                track_window_bounds(window, cx, track_file)
            })
            .unwrap();
        any_window
            .update(&mut app, |_, window, _| window.activate_window())
            .unwrap();
        app.run_until_parked();
        app.simulate_window_resize(any_window, gpui::size(px(900.), px(650.)));
        app.executor().advance_clock(Duration::from_millis(150));

        let captured = trace.0.lock().unwrap();
        assert!(captured.entered.is_empty());
    });
    let captured = trace.0.lock().unwrap();
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

    assert!(
        stacks.iter().any(|stack| stack.last().is_some_and(|label| {
            label.starts_with("window-bounds-restore") && label.contains("file=window.json")
        })),
        "missing window-bounds-restore{{file=window.json}}: {stacks:#?}"
    );
    assert!(
        lines.iter().any(|line| {
            line.starts_with("window-bounds-restore")
                && line.contains("file=window.json")
                && line.contains(": DEBUG")
                && line.contains("restored")
                && line.contains("state=windowed")
                && line.contains("display=")
        }),
        "missing `restored {{state, display}}` inside window-bounds-restore: {lines:#?}"
    );
    assert!(
        stacks.iter().any(|stack| {
            stack.last().is_some_and(|label| {
                label.starts_with("file-write") && label.contains("path=window.json")
            }) && stack[..stack.len() - 1].iter().any(|label| {
                label.starts_with("window-bounds-save") && label.contains("state=windowed")
            })
        }),
        "missing file-write{{path=window.json}} inside window-bounds-save{{state=windowed}}: {stacks:#?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains(": WARN") || line.contains(": ERROR")),
        "a clean restore and save logged a problem: {lines:#?}"
    );
}
