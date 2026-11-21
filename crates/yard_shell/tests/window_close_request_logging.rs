// Alone in this binary: captures tracing output, which would race with any
// other test reaching the same log lines.
use gpui::TestAppContext;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

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
        captured
            .lines
            .push(format!("{group}: {}{}", event.metadata().level(), fields.0));
    }

    fn enter(&self, id: &tracing::span::Id) {
        self.0.lock().unwrap().entered.push(id.into_u64());
    }

    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(self.0.lock().unwrap().entered.pop(), Some(id.into_u64()));
    }
}

// The window's close button on Windows, or the system, asks GPUI whether the
// window may close. The shell answers no and removes the window itself, as
// Zed does, so that GPUI destroys the platform window only once it is gone.
#[gpui::test]
fn close_requests_remove_the_window_once_in_their_groups(cx: &mut TestAppContext) {
    cx.update(yard_ui::init);
    let window = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
            .unwrap()
    });
    let id = gpui::AnyWindowHandle::from(window).window_id().as_u64();
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let trace = Trace::default();

    // Two requests before the first is handled, as two native close
    // messages give: the first removes the window, the second finds it gone.
    let closed_by_the_platform = tracing::subscriber::with_default(trace.clone(), || {
        let first = visual.simulate_close();
        let second = visual.simulate_close();
        visual.run_until_parked();
        (first, second)
    });

    assert_eq!(
        closed_by_the_platform,
        (false, false),
        "the platform must not close the window itself"
    );
    assert!(
        cx.update(|cx| cx.windows().is_empty()),
        "the shell removed the window"
    );
    let captured = trace.0.lock().unwrap();
    let group = format!("INFO window-close-request window={id}");
    let request = captured
        .lines
        .iter()
        .position(|line| line == &format!("{group}: DEBUG message=close requested"));
    let removal = captured
        .lines
        .iter()
        .position(|line| line == &format!("{group}: DEBUG message=removing the window"));
    assert!(
        matches!((request, removal), (Some(request), Some(removal)) if request < removal),
        "{:?}",
        captured.lines
    );
    let count = |message: &str| {
        captured
            .lines
            .iter()
            .filter(|line| *line == &format!("{group}: DEBUG message={message}"))
            .count()
    };
    assert_eq!(count("close requested"), 2, "{:?}", captured.lines);
    assert_eq!(count("removing the window"), 1, "{:?}", captured.lines);
    assert_eq!(count("already closed"), 1, "{:?}", captured.lines);
    let closed = captured
        .lines
        .iter()
        .position(|line| line == &format!("{group}: DEBUG message=already closed"));
    assert!(
        matches!((removal, closed), (Some(removal), Some(closed)) if removal < closed),
        "{:?}",
        captured.lines
    );
    assert!(
        captured
            .spans
            .values()
            .any(|span| span.starts_with("INFO window-close ")),
        "the window's release runs after: {:?}",
        captured.spans
    );
    assert!(
        captured.lines.iter().all(|line| !line.contains(": ERROR")),
        "{:?}",
        captured.lines
    );
}
