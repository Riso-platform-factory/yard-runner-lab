use gpui::{AppContext, AssetSource};
use std::sync::{Arc, Mutex};
use yard_ui::{Assets, Icon, IconName, IconSize, TextColor};

// One page drawing one icon, which must log one trace line per draw.
struct Page;

impl gpui::Render for Page {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        Icon::new(IconName::Close)
            .size(IconSize::Small)
            .color(TextColor::Warning)
    }
}

#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Vec<String>>>);

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write;
        write!(&mut self.0, " {}={value}", field.name()).unwrap();
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        write!(&mut self.0, " {}={value:?}", field.name()).unwrap();
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

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if !event.metadata().target().starts_with("yard_ui") {
            return;
        }
        let mut fields = Fields(event.metadata().level().to_string());
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }

    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

static DIAGNOSTICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_ui")
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            DIAGNOSTICS
                .lock()
                .unwrap()
                .push(format!("{} {}", record.level(), record.args()));
        }
    }

    fn flush(&self) {}
}

// Alone in this binary: installs the process-wide diagnostic logger.
#[test]
fn an_icon_draw_logs_one_trace_line_and_a_missing_asset_warns() {
    let mut cx = gpui::HeadlessAppContext::with_asset_source(
        Arc::new(gpui::NoopTextSystem),
        Arc::new(Assets),
    );
    cx.update(yard_ui::init);
    // Until a `tracing` dispatcher has been set in the process, `tracing`'s
    // `log` feature (which GPUI's Linux platform turns on, through calloop)
    // sends every trace line to the `log` crate too; one set here keeps the
    // trace out of the diagnostic log on every system, as in the app.
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {});
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    let window = cx
        .open_window(gpui::size(gpui::px(400.), gpui::px(300.)), |_, cx| {
            cx.new(|_| Page)
        })
        .unwrap();
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
            let _ = window.draw(cx);
        })
        .unwrap();
    });

    let events = trace.0.lock().unwrap();
    assert_eq!(
        events.len(),
        2,
        "two draws of one icon must log one line each: {events:?}"
    );
    for event in events.iter() {
        assert!(event.starts_with("TRACE"), "a line above trace: {event}");
        assert!(
            event.contains("Close"),
            "the icon's name must be on its line: {event}"
        );
        assert!(
            event.contains("Small"),
            "the icon's size must be on its line: {event}"
        );
        assert!(
            event.contains("Warning"),
            "the icon's color must be on its line: {event}"
        );
    }
    drop(events);
    assert!(
        DIAGNOSTICS.lock().unwrap().is_empty(),
        "a draw must not log to the diagnostic log"
    );

    assert!(Assets.load("icons/nope.svg").unwrap().is_none());
    let diagnostics = DIAGNOSTICS.lock().unwrap();
    assert_eq!(
        diagnostics.len(),
        1,
        "a missing asset must log one warning: {diagnostics:?}"
    );
    assert!(
        diagnostics[0].starts_with("WARN"),
        "the line must be a WARN: {}",
        diagnostics[0]
    );
    assert!(
        diagnostics[0].contains("icons/nope.svg"),
        "the line must name the path: {}",
        diagnostics[0]
    );
}
