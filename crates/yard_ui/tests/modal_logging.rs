// Alone in this binary: captures tracing output and installs the process-wide
// diagnostic logger, either of which would race with another test reaching the
// same log lines.
use gpui::{ParentElement, Styled, TestAppContext};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use yard_ui::{
    Button, ButtonStyle, ElevationIndex, IconButton, IconName, Label, Modal, ModalFooter,
    ModalHeader, Section, SectionHeader, h_flex, v_flex,
};

struct Page;

impl gpui::Render for Page {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        v_flex().size_full().child(
            Modal::new("preferences")
                .header(
                    ModalHeader::new()
                        .headline("Quarterly report")
                        .description("Export the report")
                        .on_dismiss(|_, _| {
                            tracing::debug!("owner dismissed");
                        }),
                )
                .section(
                    Section::new()
                        .header(SectionHeader::new("Rows"))
                        .child(Label::new("first row")),
                )
                .section(
                    Section::new()
                        .header(SectionHeader::new("Columns"))
                        .child(Label::new("first column")),
                )
                .footer(
                    ModalFooter::new()
                        .start_slot(
                            h_flex()
                                .gap_2()
                                .child(Button::new("cancel", "Cancel"))
                                .child(
                                    IconButton::new("help", IconName::Info)
                                        .layer(ElevationIndex::ModalSurface),
                                )
                                .child(IconButton::new("add", IconName::Plus)),
                        )
                        .end_slot(
                            Button::new("confirm", "Confirm")
                                .style(ButtonStyle::Filled)
                                .layer(ElevationIndex::ModalSurface),
                        ),
                ),
        )
    }
}

struct Bare;

impl gpui::Render for Bare {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        Modal::new("bare")
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

// Every `TRACE` event from the modal part: one for the modal's own line, and
// none from its sections, section headers, or footer.
fn modal_draw_lines(captured: &Captured) -> Vec<&str> {
    captured
        .lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.contains(": TRACE yard_ui::modal"))
        .collect()
}

fn button_draw_line<'a>(captured: &'a Captured, id: &str) -> &'a str {
    captured
        .lines
        .iter()
        .map(String::as_str)
        .find(|line| line.contains(": TRACE yard_ui") && line.contains(&format!("id={id}")))
        .unwrap_or_else(|| panic!("a trace line names {id}: {:?}", captured.lines))
}

fn nothing_at_warn_or_error(captured: &Captured) -> bool {
    !captured
        .lines
        .iter()
        .any(|line| line.contains(": WARN ") || line.contains(": ERROR "))
        && !captured
            .spans
            .values()
            .any(|span| span.starts_with("WARN") || span.starts_with("ERROR"))
}

#[gpui::test]
fn drawing_a_modal_logs_one_trace_line_and_a_close_click_groups_the_owners_line(
    cx: &mut TestAppContext,
) {
    cx.update(yard_ui::init);
    // Until a `tracing` dispatcher has been set in the process, `tracing`'s
    // `log` feature (which GPUI's Linux platform turns on, through calloop)
    // sends every trace line to the `log` crate too; one set here keeps the
    // trace out of the diagnostic log on every system, as in the app.
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {});
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let (_page, cx) = cx.add_window_view(|_, _| Page);
    let trace = Trace::default();

    tracing::subscriber::with_default(trace.clone(), || {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    });

    {
        let mut captured = trace.0.lock().unwrap();
        let draws = modal_draw_lines(&captured);
        assert_eq!(
            draws.len(),
            1,
            "the modal and its parts log exactly one trace line per draw: {:?}",
            captured.lines
        );
        let modal = draws[0];
        assert!(
            modal.contains("id=") && modal.contains("preferences"),
            "the line names the modal's id: {modal}"
        );
        assert!(
            modal.contains("headline=") && modal.contains("Quarterly report"),
            "the line names the headline: {modal}"
        );
        for field in [
            "description=true",
            "close_button=true",
            "sections=2",
            "footer=true",
        ] {
            assert!(modal.contains(field), "the line has {field}: {modal}");
        }
        let confirm = button_draw_line(&captured, "confirm");
        assert!(
            confirm.contains("layer=") && confirm.contains("ModalSurface"),
            "a layered button's draw line names its layer: {confirm}"
        );
        let help = button_draw_line(&captured, "help");
        assert!(
            help.contains("layer=") && help.contains("ModalSurface"),
            "a layered icon button's draw line names its layer: {help}"
        );
        let cancel = button_draw_line(&captured, "cancel");
        assert!(
            !cancel.contains(" layer="),
            "a button without a layer logs no layer field: {cancel}"
        );
        let add = button_draw_line(&captured, "add");
        assert!(
            !add.contains(" layer="),
            "an icon button without a layer logs no layer field: {add}"
        );
        assert!(
            nothing_at_warn_or_error(&captured),
            "nothing at warn or error while drawing: {:?} {:?}",
            captured.spans,
            captured.lines
        );
        captured.lines.clear();
        captured.entered.clear();
    }

    let close = cx
        .debug_bounds("ICON-Close")
        .expect("the close button's bounds")
        .center();
    tracing::subscriber::with_default(trace.clone(), || {
        cx.simulate_click(close, gpui::Modifiers::default());
    });

    {
        let mut captured = trace.0.lock().unwrap();
        let clicks: Vec<_> = captured
            .spans
            .values()
            .filter(|span| span.starts_with("DEBUG button-click"))
            .collect();
        assert_eq!(
            clicks.len(),
            1,
            "the click opens one click group: {:?}",
            captured.spans
        );
        assert!(
            clicks[0].contains("id=") && clicks[0].contains("preferences"),
            "the close button's id comes from the modal's: {clicks:?}"
        );
        assert!(
            captured
                .lines
                .iter()
                .any(|line| line.starts_with("DEBUG button-click")
                    && line.contains("owner dismissed")),
            "the owner's line is inside the click group: {:?}",
            captured.lines
        );
        assert!(
            nothing_at_warn_or_error(&captured),
            "nothing at warn or error while clicking: {:?} {:?}",
            captured.spans,
            captured.lines
        );
        captured.lines.clear();
        captured.entered.clear();
    }

    let (_bare, cx) = cx.add_window_view(|_, _| Bare);
    tracing::subscriber::with_default(trace.clone(), || {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    });

    {
        let captured = trace.0.lock().unwrap();
        let draws = modal_draw_lines(&captured);
        assert_eq!(
            draws.len(),
            1,
            "a bare modal logs exactly one trace line per draw: {:?}",
            captured.lines
        );
        let modal = draws[0];
        assert!(
            modal.contains("id=bare "),
            "the line names the modal's id: {modal}"
        );
        assert!(
            modal.contains("headline= "),
            "the line's headline field is empty: {modal}"
        );
        for field in [
            "description=false",
            "close_button=false",
            "sections=0",
            "footer=false",
        ] {
            assert!(modal.contains(field), "the line has {field}: {modal}");
        }
        assert!(
            nothing_at_warn_or_error(&captured),
            "nothing at warn or error while drawing the bare modal: {:?} {:?}",
            captured.spans,
            captured.lines
        );
    }

    assert!(
        DIAGNOSTICS.lock().unwrap().is_empty(),
        "two draws and a click log nothing to the diagnostic log"
    );
}
