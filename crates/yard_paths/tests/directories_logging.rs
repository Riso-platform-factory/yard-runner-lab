//! The log of working out the application's directories: the work runs inside its
//! group, which names where the directories come from and each of them.
//!
//! This file sets `TESTAPP_HOME` for the whole process, so it holds one test.

use std::sync::{Arc, Mutex};

/// The fields of a span or an event, with paths shortened to their last
/// part, which does not differ from run to run.
#[derive(Default)]
struct Fields(String);
impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let value = format!("{value:?}");
        let value = match field.name() {
            "path" => value.rsplit(['/', '\\']).next().unwrap().to_owned(),
            _ => value,
        };
        write!(self.0, " {}={value}", field.name()).unwrap();
    }
}

/// The groups in the order they open, and under each the lines logged in it.
#[derive(Clone, Default)]
struct Trace(
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Option<tracing::span::Id>>>,
);
impl tracing::Subscriber for Trace {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let mut lines = self.0.lock().unwrap();
        lines.push(format!(
            "{} {}{}",
            attributes.metadata().level(),
            attributes.metadata().name(),
            fields.0
        ));
        tracing::span::Id::from_u64(lines.len() as u64)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        assert!(
            self.1.lock().unwrap().is_some(),
            "event is inside paths-resolve"
        );
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.0
            .lock()
            .unwrap()
            .push(format!("  {}{}", event.metadata().level(), fields.0));
    }
    fn enter(&self, id: &tracing::span::Id) {
        assert!(self.1.lock().unwrap().replace(id.clone()).is_none());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(self.1.lock().unwrap().take().as_ref(), Some(id));
    }
}

#[test]
fn working_out_the_directories_runs_inside_its_group() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: this file holds one test, and it has started no thread yet.
    unsafe { std::env::set_var("TESTAPP_HOME", home.path().join("home")) };
    let trace = Trace::default();
    let paths = tracing::subscriber::with_default(trace.clone(), || {
        yard_paths::Paths::from_env(&yard_product::Product {
            name: "testapp",
            version: "0",
            identifier: "test.app",
        })
    });
    assert_eq!(paths.origin(), yard_paths::Origin::Home);
    assert!(
        trace.1.lock().unwrap().is_none(),
        "paths-resolve has exited"
    );
    let lines = trace.0.lock().unwrap();
    assert_eq!(
        *lines,
        [
            "INFO paths-resolve",
            "  DEBUG message=resolved origin=Home variable=TESTAPP_HOME",
            "  DEBUG message=config path=config",
            "  DEBUG message=data path=data",
            "  DEBUG message=logs path=logs",
        ],
        "{lines:#?}"
    );
}
