// Alone in this binary: captures tracing output, which would race with any
// other test reaching the same log lines.
use gpui::{ParentElement, ResizeEdge, Styled, TestAppContext, div, point, px};
use std::{
    collections::BTreeMap,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use yard_ui::{WindowButtons, WindowControlAction, client_decorations, default_button_layout};

struct Page;

impl gpui::Render for Page {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        client_decorations(
            div()
                .size_full()
                .child(WindowButtons::new("buttons", default_button_layout().right)),
            window,
            cx,
        )
    }
}

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
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write;
        write!(self.0, " {}={value}", field.name()).unwrap();
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        write!(self.0, " {}={value:?}", field.name()).unwrap();
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
        captured.spans.insert(
            id,
            format!(
                "{} {}{}",
                attributes.metadata().level(),
                attributes.metadata().name(),
                fields.0
            ),
        );
        tracing::span::Id::from_u64(id)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap();
        let group = captured
            .entered
            .last()
            .and_then(|id| captured.spans.get(id))
            .map_or_else(|| "none".to_string(), String::clone);
        captured.lines.push(format!(
            "{group}: {} {}{}",
            event.metadata().level(),
            event.metadata().target(),
            fields.0
        ));
    }

    fn enter(&self, id: &tracing::span::Id) {
        self.0.lock().unwrap().entered.push(id.into_u64());
    }

    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(self.0.lock().unwrap().entered.pop(), Some(id.into_u64()));
    }
}

fn take_lines(trace: &Trace) -> (Vec<String>, Vec<String>) {
    let mut captured = trace.0.lock().unwrap();
    let spans = captured.spans.values().cloned().collect();
    let lines = std::mem::take(&mut captured.lines);
    captured.spans.clear();
    captured.entered.clear();
    (spans, lines)
}

#[gpui::test]
fn a_draw_traces_and_each_action_logs_in_its_group(cx: &mut TestAppContext) {
    cx.update(yard_ui::init);
    let (_page, cx) = cx.add_window_view(|_, _| Page);
    let trace = Trace::default();

    tracing::subscriber::with_default(trace.clone(), || {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    });
    let (_, lines) = take_lines(&trace);
    assert!(
        lines
            .iter()
            .any(|line| line == "none: TRACE yard_ui::window_controls message=rendering window buttons buttons=3 maximized=false"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(
            |line| line.starts_with("none: TRACE yard_ui::client_decorations")
                && line.contains("rendering client decorations")
        ),
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|line| line.contains(": TRACE ")),
        "a draw logs only at trace: {lines:?}"
    );

    // GPUI's test window does not implement minimizing, zooming, moving, or
    // the window menu: it panics when the action reaches it, after the action
    // has logged.
    let cases = [
        (WindowControlAction::Minimize, "minimizing", true),
        (WindowControlAction::Maximize, "maximizing", true),
        (WindowControlAction::Restore, "restoring", true),
        (WindowControlAction::Move, "move started", true),
        (
            WindowControlAction::Menu(point(px(40.), px(10.))),
            "window menu opened",
            true,
        ),
        (
            WindowControlAction::Resize(ResizeEdge::BottomRight),
            "resize started",
            false,
        ),
    ];
    std::panic::set_hook(Box::new(|_| {}));
    for (action, message, reaches_unimplemented) in cases {
        let reached = tracing::subscriber::with_default(trace.clone(), || {
            cx.update(|window, _| {
                std::panic::catch_unwind(AssertUnwindSafe(|| action.perform(window))).is_err()
            })
        });
        assert_eq!(reached, reaches_unimplemented, "{action:?}");
        let (spans, lines) = take_lines(&trace);
        let group = format!("INFO window-control action={}", action.name());
        assert_eq!(spans, vec![group.clone()], "{action:?}");
        assert_eq!(lines.len(), 1, "{action:?}: {lines:?}");
        assert!(
            lines[0].starts_with(&format!("{group}: DEBUG yard_ui::window_controls"))
                && lines[0].contains(message),
            "{action:?}: {lines:?}"
        );
        if let WindowControlAction::Resize(_) = action {
            assert!(lines[0].contains("edge=BottomRight"), "{lines:?}");
        }
        if let WindowControlAction::Menu(_) = action {
            assert!(lines[0].contains("x=40.0 y=10.0"), "{lines:?}");
        }
    }
    let _ = std::panic::take_hook();

    // The buttons sit at the top left: 12 px of padding, then 20 px buttons
    // 12 px apart, so close is centred at (86, 10).
    tracing::subscriber::with_default(trace.clone(), || {
        cx.simulate_click(point(px(86.), px(10.)), gpui::Modifiers::default());
    });
    let (spans, lines) = take_lines(&trace);
    assert!(
        spans.contains(&"INFO window-control action=close".to_string()),
        "clicking close opens its group: {spans:?}"
    );
    assert!(
        lines.iter().any(|line| line
            == "INFO window-control action=close: DEBUG yard_ui::window_controls message=closing"),
        "{lines:?}"
    );
}
