use gpui::{ParentElement, Styled, px};
use std::sync::{Arc, Mutex};
use yard_ui::{Divider, DividerColor, h_flex, v_flex};

// One page drawing a divider of each direction, style, and color, each of
// which must log one trace line per draw.
struct Page;

impl gpui::Render for Page {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        v_flex()
            .w(px(200.))
            .child(Divider::horizontal().color(DividerColor::Border))
            .child(
                h_flex()
                    .h(px(80.))
                    .child(Divider::vertical_dashed().color(DividerColor::BorderFaded)),
            )
            .child(Divider::horizontal_dashed().color(DividerColor::BorderVariant))
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
#[gpui::test]
fn drawing_dividers_logs_one_trace_line_per_draw(cx: &mut gpui::TestAppContext) {
    cx.update(yard_ui::init);
    // Until a `tracing` dispatcher has been set in the process, `tracing`'s
    // `log` feature (which GPUI's Linux platform turns on, through calloop)
    // sends every trace line to the `log` crate too; one set here keeps the
    // trace out of the diagnostic log on every system, as in the app.
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {});
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    let window = cx.add_window(|_, _| Page);
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        gpui::AnyWindowHandle::from(window)
            .update(cx, |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
                let _ = window.draw(cx);
            })
            .unwrap();
    });

    let events = trace.0.lock().unwrap();
    assert_eq!(
        events.len(),
        6,
        "two draws of three dividers must log one line each: {events:?}"
    );
    for event in events.iter() {
        assert!(event.starts_with("TRACE"), "a line above trace: {event}");
    }

    let solid: Vec<_> = events
        .iter()
        .filter(|event| event.contains("Horizontal") && event.contains("Solid"))
        .collect();
    assert_eq!(
        solid.len(),
        2,
        "the solid horizontal divider must log once per draw: {events:?}"
    );
    for event in solid {
        assert!(
            event.contains("Border"),
            "its color must be on its line: {event}"
        );
        assert!(
            !event.contains("Faded") && !event.contains("Variant"),
            "its color is Border, not BorderFaded or BorderVariant: {event}"
        );
    }

    let vertical: Vec<_> = events
        .iter()
        .filter(|event| event.contains("Vertical"))
        .collect();
    assert_eq!(
        vertical.len(),
        2,
        "the vertical divider must log once per draw: {events:?}"
    );
    for event in vertical {
        assert!(
            event.contains("Dashed"),
            "its style must be on its line: {event}"
        );
        assert!(
            event.contains("BorderFaded"),
            "its color must be on its line: {event}"
        );
    }

    let dashed: Vec<_> = events
        .iter()
        .filter(|event| event.contains("Horizontal") && event.contains("Dashed"))
        .collect();
    assert_eq!(
        dashed.len(),
        2,
        "the horizontal dashed divider must log once per draw: {events:?}"
    );
    for event in dashed {
        assert!(
            event.contains("BorderVariant"),
            "its color must be on its line: {event}"
        );
    }

    drop(events);
    assert!(
        DIAGNOSTICS.lock().unwrap().is_empty(),
        "a draw must not log to the diagnostic log"
    );
}
