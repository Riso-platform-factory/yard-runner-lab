// Alone in this binary: captures tracing output, which would race with any
// other test reaching the same log lines.
use gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement, Pixels, PlatformInput,
    Point, ResizeEdge, Styled, TestAppContext, point, px,
};
use std::{
    collections::BTreeMap,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use yard_ui::{
    WindowButtons, WindowControlAction, WindowsWindowButtons, client_decorations,
    default_button_layout, h_flex, v_flex,
};

struct Page;

impl gpui::Render for Page {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        client_decorations(
            v_flex()
                .size_full()
                .child(WindowButtons::new("buttons", default_button_layout().right))
                .child(
                    h_flex()
                        .w(px(200.))
                        .justify_end()
                        .child(WindowsWindowButtons::new("windows-buttons")),
                )
                .child(
                    h_flex()
                        .w(px(200.))
                        .justify_end()
                        .child(WindowsWindowButtons::new("preview-buttons").preview()),
                ),
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
        lines.iter().any(|line| line
            == "none: TRACE yard_ui::window_controls message=rendering windows caption buttons maximized=false preview=false"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line
            == "none: TRACE yard_ui::window_controls message=rendering windows caption buttons maximized=false preview=true"),
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

    // An action that Windows does itself logs only, in the same group.
    tracing::subscriber::with_default(trace.clone(), || {
        WindowControlAction::Maximize.leave_to_system();
    });
    let (spans, lines) = take_lines(&trace);
    assert_eq!(
        spans,
        vec!["INFO window-control action=maximize".to_string()]
    );
    assert_eq!(
        lines,
        vec![
            "INFO window-control action=maximize: DEBUG yard_ui::window_controls message=left to the system"
                .to_string()
        ]
    );

    // The Windows buttons end the 200 px row below the Linux buttons: 36 px
    // each, 32 px high, from y = 20. A click logs its action, and no element
    // stops the press or the release: GPUI on Windows hands the system the
    // caption button's action only then.
    for (x, action) in [(110., "minimize"), (146., "maximize"), (182., "close")] {
        let position = point(px(x), px(36.));
        let propagated = tracing::subscriber::with_default(trace.clone(), || {
            cx.update(|window, cx| {
                let down = window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
                let up = window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    }),
                    cx,
                );
                (down.propagate, up.propagate)
            })
        });
        assert_eq!(propagated, (true, true), "{action}");
        let (spans, lines) = take_lines(&trace);
        let group = format!("INFO window-control action={action}");
        assert!(spans.contains(&group), "{action}: {spans:?}");
        assert!(
            lines.iter().any(|line| line
                == &format!("{group}: DEBUG yard_ui::window_controls message=left to the system")),
            "{action}: {lines:?}"
        );
    }

    // Windows acts only on a left release over a caption button, so a
    // right click logs nothing, even one that ends a left press on close;
    // and a preview, 32 px lower, logs nothing at all.
    let close = point(px(182.), px(36.));
    let preview_close = point(px(182.), px(68.));
    let sequences: [&[(MouseButton, bool, Point<Pixels>)]; 3] = [
        &[
            (MouseButton::Right, true, close),
            (MouseButton::Right, false, close),
        ],
        &[
            (MouseButton::Left, true, close),
            (MouseButton::Right, true, close),
            (MouseButton::Right, false, close),
            (MouseButton::Left, false, close),
        ],
        &[
            (MouseButton::Left, true, preview_close),
            (MouseButton::Left, false, preview_close),
        ],
    ];
    for sequence in sequences {
        tracing::subscriber::with_default(trace.clone(), || {
            cx.update(|window, cx| {
                for &(button, down, position) in sequence {
                    let input = if down {
                        PlatformInput::MouseDown(MouseDownEvent {
                            button,
                            position,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                            first_mouse: false,
                        })
                    } else {
                        PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                        })
                    };
                    window.dispatch_event(input, cx);
                }
            })
        });
        let (spans, lines) = take_lines(&trace);
        assert!(
            !spans.iter().any(|span| span.contains("window-control")),
            "{sequence:?}: {spans:?} {lines:?}"
        );
    }

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
