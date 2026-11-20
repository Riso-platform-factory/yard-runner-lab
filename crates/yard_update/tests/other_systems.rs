//! On a system other than macOS, the check and the install do nothing, and
//! say why once, inside their groups.
//!
//! Alone in its binary: it captures the trace of the lines it reaches.

// The check and the install run only on macOS until the release-updates slice.
#![cfg(not(target_os = "macos"))]

use gpui::TestAppContext;
use http_client::{AsyncBody, FakeHttpClient, Response};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use yard_product::Product;
use yard_shell::Extension;

const WHY: &str = "updates on this system come with the release-updates slice";

/// Every group opened, in the order of their ids, the groups entered, and
/// each line as `<group>: <level> <message>`.
#[derive(Default)]
struct Captured {
    spans: Vec<String>,
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
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let mut trace = self.0.lock().unwrap();
        trace
            .spans
            .push(format!("{}{}", attributes.metadata().name(), fields.0));
        tracing::span::Id::from_u64(trace.spans.len() as u64)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if !event.metadata().target().starts_with("yard_update") {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut trace = self.0.lock().unwrap();
        let group = trace.entered.last().map_or_else(
            || String::from("none"),
            |id| trace.spans[*id as usize - 1].clone(),
        );
        trace
            .lines
            .push(format!("{group}: {}{}", event.metadata().level(), fields.0));
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.0.lock().unwrap().entered.push(id.into_u64());
    }
    fn exit(&self, _: &tracing::span::Id) {
        self.0.lock().unwrap().entered.pop();
    }
}

#[gpui::test]
fn the_check_and_the_install_do_nothing_and_say_why_once(cx: &mut TestAppContext) {
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let app = cx.new_app();
        app.update(yard_ui::init);
        let extension = app.update(|cx| Extension::new(cx, "update").unwrap());
        let requests = Arc::new(Mutex::new(0));
        let client = FakeHttpClient::create({
            let requests = requests.clone();
            move |_| {
                *requests.lock().unwrap() += 1;
                async move {
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from("{}"))
                        .unwrap())
                }
            }
        });
        let product = Product::new("yard-check", "0.1.0", "vn.risotech.yard");
        app.update(|cx| {
            yard_update::start(
                cx,
                &extension,
                product,
                "http://127.0.0.1:8765/latest.json",
                client.clone(),
            )
        });
        app.run_until_parked();
        app.executor()
            .advance_clock(Duration::from_secs(2 * 24 * 60 * 60));
        app.run_until_parked();

        let installed = futures::executor::block_on(yard_update::install(
            client,
            std::env::temp_dir(),
            "yard-check",
            "vn.risotech.yard",
            "0.2.0",
            "https://releases.example.com/yard-check-0.2.0.zip",
            "00",
        ));
        assert_eq!(installed, Err(String::from(WHY)));
        assert_eq!(yard_update::running_bundle(), Err(String::from(WHY)));
        assert_eq!(*requests.lock().unwrap(), 0, "nothing is requested");
    });
    let trace = trace.0.lock().unwrap();
    assert_eq!(
        trace.lines,
        [
            format!("update-check current=\"0.1.0\": DEBUG message={WHY}"),
            format!("update-install version=\"0.2.0\": DEBUG message={WHY}"),
        ],
        "{:#?}",
        trace.lines
    );
}
