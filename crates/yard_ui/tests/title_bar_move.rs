// Alone in this binary: captures tracing output, and replaces the panic hook
// while GPUI's test window panics on moving. The title bar moves the window
// only on Linux.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]

use gpui::{MouseButton, ParentElement, Styled, TestAppContext, div, point, px};
use std::{
    collections::BTreeMap,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use yard_ui::{TitleBar, v_flex};

struct Page;

impl gpui::Render for Page {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        v_flex()
            .size_full()
            .child(TitleBar::new("title-bar").left(div().child("title")))
            .child(div().flex_1())
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

#[gpui::test]
fn a_move_after_the_button_is_released_does_not_move_the_window(cx: &mut TestAppContext) {
    cx.update(yard_ui::init);
    let (_page, cx) = cx.add_window_view(|_, _| Page);
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    let trace = Trace::default();
    let start = point(px(300.), px(17.));
    tracing::subscriber::with_default(trace.clone(), || {
        cx.simulate_mouse_down(start, MouseButton::Left, Default::default());
        cx.simulate_mouse_up(start, MouseButton::Left, Default::default());
        // GPUI's test window would panic here if the move started.
        cx.simulate_mouse_move(point(px(320.), px(30.)), None, Default::default());
    });
    let captured = trace.0.lock().unwrap();
    assert!(
        !captured
            .lines
            .iter()
            .any(|line| line.contains("action=move")),
        "{:?}",
        captured.lines
    );
}

#[gpui::test]
fn dragging_the_title_bar_moves_the_window(cx: &mut TestAppContext) {
    cx.update(yard_ui::init);
    let (_page, cx) = cx.add_window_view(|_, _| Page);
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    let trace = Trace::default();
    // A point of the bar away from its items.
    let start = point(px(300.), px(17.));
    std::panic::set_hook(Box::new(|_| {}));
    let moved = tracing::subscriber::with_default(trace.clone(), || {
        cx.simulate_mouse_down(start, MouseButton::Left, Default::default());
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            cx.simulate_mouse_move(
                point(px(320.), px(30.)),
                MouseButton::Left,
                Default::default(),
            );
        }))
        .is_err()
    });
    let _ = std::panic::take_hook();
    let captured = trace.0.lock().unwrap();
    // GPUI's test window panics in `start_window_move`, after the action has
    // logged.
    assert!(moved, "the move reached the window: {:?}", captured.lines);
    assert!(
        captured
            .lines
            .iter()
            .any(|line| line == "INFO window-control action=move: DEBUG yard_ui::window_controls message=move started"),
        "{:?}",
        captured.lines
    );
}
