use gpui::{AnyWindowHandle, TestAppContext};
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
        captured
            .spans
            .insert(id, format!("{}{}", attributes.metadata().name(), fields.0));
        tracing::span::Id::from_u64(id)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if !event.metadata().target().starts_with("ui_gallery") {
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

static DIAGNOSTICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("ui_gallery")
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

fn run_child(case: Option<&str>) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "gallery_builds_its_pages_and_logs_per_case",
            "--nocapture",
        ])
        .env("UI_GALLERY_TEST_CHILD", case.unwrap_or("unset"));
    match case {
        Some(value) => {
            command.env("UI_GALLERY", value);
        }
        None => {
            command.env_remove("UI_GALLERY");
        }
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "child {case:?} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[gpui::test]
fn gallery_builds_its_pages_and_logs_per_case(cx: &mut TestAppContext) {
    let Ok(mode) = std::env::var("UI_GALLERY_TEST_CHILD") else {
        run_child(None);
        run_child(Some(""));
        run_child(Some("all"));
        run_child(Some("window-controls"));
        run_child(Some("unknown"));
        return;
    };

    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    cx.update(yard_ui::init);

    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        cx.update(|cx| {
            let ext = yard_shell::Extension::new(cx, "ui_gallery").unwrap();
            ui_gallery::init(cx, &ext);
        });
        let window = cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
                .unwrap()
        });
        let handle = AnyWindowHandle::from(window);
        handle
            .update(cx, |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            })
            .unwrap();
        let mut visual = gpui::VisualTestContext::from_window(handle, cx);
        let selectors: &[&str] = &[
            "TAB-icon",
            "TAB-button",
            "TAB-divider",
            "TAB-modal",
            "TAB-list",
            "TAB-text-input",
            "TAB-scroll",
            "TAB-key-hint",
            "TAB-tooltip",
            "TAB-window-controls",
        ];
        for &selector in selectors {
            let bounds = match visual.debug_bounds(selector) {
                Some(bounds) => bounds,
                None if mode != "all" => continue,
                None => panic!("expected gallery tab {selector} to be placed"),
            };
            visual.simulate_click(bounds.center(), Default::default());
            handle
                .update(&mut visual, |_, window, cx| {
                    window.refresh();
                    let _ = window.draw(cx);
                })
                .unwrap();
        }
    });

    let captured = trace.0.lock().unwrap();
    let diagnostics = DIAGNOSTICS.lock().unwrap();
    match mode.as_str() {
        "unset" | "" => {
            assert!(
                !captured
                    .spans
                    .values()
                    .any(|span| span.starts_with("gallery-build")),
                "unexpected gallery-build span: {:?}",
                captured.spans
            );
            assert!(
                captured.lines.is_empty(),
                "unexpected lines: {:?}",
                captured.lines
            );
            assert!(
                diagnostics.is_empty(),
                "unexpected diagnostics: {diagnostics:?}"
            );
        }
        "all" => {
            let expected_pages: &[&str] = &[
                "text",
                "icon",
                "button",
                "divider",
                "modal",
                "list",
                "text-input",
                "scroll",
                "key-hint",
                "tooltip",
                "window-controls",
            ];
            let expected_span = format!("gallery-build pages={}", expected_pages.len());
            assert!(
                captured.spans.values().any(|span| span == &expected_span),
                "expected {expected_span}, got {:?}",
                captured.spans
            );
            let build_lines: Vec<_> = captured
                .lines
                .iter()
                .filter(|line| !line.contains(": TRACE "))
                .cloned()
                .collect();
            let expected_build_lines: Vec<_> = expected_pages
                .iter()
                .map(|page| format!("{expected_span}: DEBUG message=page placed page={page}"))
                .collect();
            assert_eq!(
                build_lines, expected_build_lines,
                "unexpected non-trace lines: {:?}",
                captured.lines
            );
            for name in expected_pages {
                let page = format!("page={name}");
                assert!(
                    captured
                        .lines
                        .iter()
                        .any(|line| line.contains(": TRACE") && line.contains(&page)),
                    "expected {page} render trace in {:?}",
                    captured.lines
                );
            }
            assert!(
                diagnostics.is_empty(),
                "unexpected diagnostics: {diagnostics:?}"
            );
        }
        "window-controls" => {
            let expected_span = "gallery-build pages=1";
            let build_lines: Vec<_> = captured
                .lines
                .iter()
                .filter(|line| !line.contains(": TRACE "))
                .cloned()
                .collect();
            assert_eq!(
                build_lines,
                vec![format!(
                    "{expected_span}: DEBUG message=page placed page=window-controls"
                )],
                "unexpected non-trace lines: {:?}",
                captured.lines
            );
            assert!(
                captured
                    .lines
                    .iter()
                    .any(|line| line.contains(": TRACE") && line.contains("page=window-controls")),
                "expected the page's render trace in {:?}",
                captured.lines
            );
            assert!(
                diagnostics.is_empty(),
                "unexpected diagnostics: {diagnostics:?}"
            );
        }
        "unknown" => {
            assert!(
                !captured
                    .spans
                    .values()
                    .any(|span| span.starts_with("gallery-build")),
                "unexpected gallery-build span: {:?}",
                captured.spans
            );
            assert!(
                captured.lines.is_empty(),
                "unexpected lines: {:?}",
                captured.lines
            );
            assert_eq!(
                diagnostics.len(),
                1,
                "expected one warning: {diagnostics:?}"
            );
            assert!(
                diagnostics[0].starts_with("WARN ") && diagnostics[0].contains("unknown"),
                "unexpected diagnostic: {}",
                diagnostics[0]
            );
        }
        other => panic!("unexpected mode {other}"),
    }
}
