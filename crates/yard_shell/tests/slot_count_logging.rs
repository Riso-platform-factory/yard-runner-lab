use gpui::{AppContext, IntoElement, ParentElement, Styled, TestAppContext, px};
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

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-shell-slot-count-log-{}-{}-{nanos:x}",
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

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs.
const PROBE: &str = "slot_count_probe";

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

struct Frame {
    slots: shell::Slots,
}

impl gpui::Render for Frame {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::div().size_full().flex().flex_col().children(
            self.slots
                .views("content")
                .into_iter()
                .map(|view| gpui::div().flex_1().min_h_0().child(view)),
        )
    }
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

// `frame`, a tour.frame whose `content` slot is declared for one and holds
// `first`, beside `loose` in a row.
fn setup(app: &mut TestAppContext) -> shell::Extension {
    app.update(|cx| {
        yard_ui::init(cx);
        let host = shell::Extension::new(cx, "tour").unwrap();
        host.register(cx, "tour.card", &[], |_, _, _, cx| cx.new(|_| Card).into());
        host.register(cx, "tour.frame", &["content"], move |_, slots, _, cx| {
            cx.new(|_| Frame { slots }).into()
        });
        host.slot_for_one(cx, "tour.frame", "content");
        host.place_root(cx, "root", "yard.row");
        host.place(cx, "frame", "tour.frame", "root", "children");
        host.place(cx, "first", "tour.card", "frame", "content");
        host.place(cx, "loose", "tour.card", "root", "children");
        host
    })
}

// Alone in this binary: every call reaching these log lines shares this one
// subscriber and logger.
#[gpui::test]
fn slot_count_logs_the_declare_group_and_each_refusal(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let temp = TempDir::new();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let mut app = cx.new_app();
        let host = setup(&mut app);
        app.update(|cx| {
            // A `slot_for_one` that is refused is a bug of the extension: an
            // ERROR inside the declaration's own group.
            host.slot_for_one(cx, "guest.card", "content");
            host.slot_for_one(cx, "tour.frame", "content");
            host.register(cx, "tour.panel", &["content"], |_, slots, _, cx| {
                cx.new(|_| Frame { slots }).into()
            });
            host.slot_for_one(cx, "tour.panel", "content");
            host.slot_for_one(cx, "tour.panel", "content");
            // A second node placed at startup is refused inside node-place.
            host.place(cx, "extra", "tour.card", "frame", "content");
        });
        let window = open_and_draw(&mut app);
        // An `add` into the full slot is refused inside layout-op{op=add}.
        gpui::AnyWindowHandle::from(window)
            .update(&mut app, |_, window, cx| {
                host.add(window, cx, "second", "tour.card", "frame", "content");
            })
            .unwrap();

        // A saved `move` the rule refuses is skipped at the next start.
        let layout_path = temp.path().join("layout.json");
        fs::write(
            &layout_path,
            r#"{"version": 1, "operations": [
                {"op": "move", "node": "loose", "parent": "frame", "slot": "content", "index": 0},
                {"op": "hide", "node": "loose"}
            ]}"#,
        )
        .unwrap();
        let mut restored = cx.new_app();
        restored.update(|cx| shell::set_layout_file(cx, layout_path.clone()));
        setup(&mut restored);
        let _window = open_and_draw(&mut restored);
    });
    let captured = trace.0.lock().unwrap();
    assert!(captured.entered.is_empty());
    let lines: Vec<String> = captured
        .lines
        .iter()
        .map(|line| line.replace('"', ""))
        .collect();
    // A slot declared for one says so inside its own group, whether the
    // declaration comes before or after the component is registered.
    for component in ["tour.frame", "tour.panel"] {
        let group = format!("slot-declare component={component} slot=content:");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&group)
                    && line.contains("DEBUG message=slot for one")),
            "{component}'s group logs `slot for one`: {lines:#?}"
        );
    }
    // Each refused declaration is an ERROR inside the slot's own group.
    for component in ["guest.card", "tour.frame", "tour.panel"] {
        assert!(
            lines.iter().any(|line| line
                .starts_with(&format!("slot-declare component={component} slot=content:"))
                && line.contains("ERROR")),
            "{component}'s group logs the refusal: {lines:#?}"
        );
    }
    // The second `slot_for_one` of tour.panel's content gives no second
    // `slot for one`. tour.frame's declaration is not counted: the second
    // application's setup declares it again.
    assert_eq!(
        lines
            .iter()
            .filter(
                |line| line.starts_with("slot-declare component=tour.panel slot=content:")
                    && line.contains("DEBUG message=slot for one")
            )
            .count(),
        1,
        "{lines:#?}"
    );
    // The refused place logs its INFO inside node-place{node, …}.
    assert!(
        lines.iter().any(|line| line.starts_with(
            "node-place node=extra component=tour.card parent=frame slot=content: INFO"
        ) && line
            .contains("frame.content holds one node, and already holds first")),
        "the refused place: {lines:#?}"
    );
    // The refused add logs its INFO inside layout-op{op=add}.
    assert!(
        lines.iter().any(|line| line
            .starts_with("layout-op op=add node=second extension=tour: INFO")
            && line.contains("frame.content holds one node, and already holds first")),
        "the refused add: {lines:#?}"
    );
    // The saved move into the full slot is skipped, with the same reason.
    assert!(
        lines.iter().any(|line| line
            .starts_with("layout-restore file=layout.json: INFO")
            && line.contains(
                "skipped op=move node=loose reason=frame.content holds one node, and already holds first"
            )),
        "the skipped restore: {lines:#?}"
    );
}
