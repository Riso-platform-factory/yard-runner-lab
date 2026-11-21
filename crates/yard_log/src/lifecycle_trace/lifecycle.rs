use super::{
    Outcome, Record,
    known_errors::{self, KnownError},
    writer::Message,
};
use serde_json::{Map, Value};
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    time::Instant,
};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id},
};
use tracing_log::NormalizeEvent;
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

#[derive(Clone, Copy)]
pub(crate) enum Phase {
    Startup,
    Running,
    Shutdown,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Startup => "app-startup",
            Self::Running => "app-running",
            Self::Shutdown => "app-shutdown",
        }
    }
}

pub(crate) struct Lifecycle {
    start: Instant,
    next_id: AtomicU64,
    phase: Mutex<(Phase, u64)>,
    phase_severity: [AtomicU8; 3],
    /// The groups that have started and not ended.
    open: Mutex<BTreeSet<u64>>,
    finished: AtomicBool,
    /// Serializes record emission with the terminal run-end record.
    recording: Mutex<()>,
    known: &'static [KnownError],
    pub(crate) tx: mpsc::Sender<Message>,
}

impl Lifecycle {
    pub(crate) fn new(tx: mpsc::Sender<Message>, known: &'static [KnownError]) -> Self {
        Self {
            start: Instant::now(),
            next_id: AtomicU64::new(1),
            phase: Mutex::new((Phase::Startup, 0)),
            phase_severity: std::array::from_fn(|_| AtomicU8::new(0)),
            open: Mutex::new(BTreeSet::new()),
            finished: AtomicBool::new(false),
            recording: Mutex::new(()),
            tx,
            known,
        }
    }

    fn t(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
    fn send(&self, record: Record) {
        let _ = self.tx.send(Message::Record(record));
    }

    pub(crate) fn start_run(&self, name: &str, version: &str, identifier: &str) {
        let mut phase = self.phase.lock().unwrap();
        self.send(Record::RunStart {
            t: self.t(),
            name: name.into(),
            version: version.into(),
            identifier: identifier.into(),
            os: std::env::consts::OS.into(),
        });
        phase.1 = self.id();
        self.start_phase(*phase);
    }

    fn start_phase(&self, (phase, id): (Phase, u64)) {
        self.send(Record::GroupStart {
            t: self.t(),
            id,
            parent: None,
            name: phase.name().into(),
            fields: Map::new(),
            thread: thread_name(),
        });
    }

    fn end_phase(&self, (phase, id): (Phase, u64)) {
        self.send(Record::GroupEnd {
            t: self.t(),
            id,
            outcome: outcome(&self.phase_severity[phase as usize]),
        });
    }

    pub(crate) fn set_phase(&self, next: Phase) {
        let _recording = self.recording.lock().unwrap();
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        let mut phase = self.phase.lock().unwrap();
        self.end_phase(*phase);
        *phase = (next, self.id());
        self.start_phase(*phase);
    }

    /// Ends the run: groups still open end as interrupted, newest first,
    /// then the phase, then `run-end`. Only the first call does anything.
    pub(crate) fn finish(&self) {
        self.finish_with(Outcome::Interrupted);
    }

    /// Ends the run in the panic hook, before unwinding crosses platform frames.
    pub(crate) fn finish_failed(&self) {
        self.fail_phase(2);
        self.finish_with(Outcome::Failure);
    }

    fn finish_with(&self, open_outcome: Outcome) {
        let _recording = self.recording.lock().unwrap();
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let phase = self.phase.lock().unwrap();
        let open = std::mem::take(&mut *self.open.lock().unwrap());
        for id in open.into_iter().rev() {
            self.send(Record::GroupEnd {
                t: self.t(),
                id,
                outcome: open_outcome.clone(),
            });
        }
        self.end_phase(*phase);
        self.send(Record::RunEnd { t: self.t() });
    }

    /// Marks the phase that is current now as failed, or failed with a known
    /// error. A group that started in an earlier phase and logs an error now
    /// marks this one: the earlier phase has already ended.
    fn fail_phase(&self, severity: u8) {
        let phase = self.phase.lock().unwrap();
        self.phase_severity[phase.0 as usize].fetch_max(severity, Ordering::Relaxed);
    }
}

fn thread_name() -> String {
    let thread = std::thread::current();
    thread
        .name()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:?}", thread.id()))
}

fn outcome(severity: &AtomicU8) -> Outcome {
    match severity.load(Ordering::Relaxed) {
        0 => Outcome::Success,
        1 => Outcome::KnownFailure,
        _ => Outcome::Failure,
    }
}

struct GroupData {
    id: u64,
    severity: AtomicU8,
}

pub(crate) struct LifecycleLayer(pub(crate) Arc<Lifecycle>);

// TODO(lifecycle-recorded-fields): Fields set with Span::record after span creation never reach the trace because this layer has no on_record. Add on_record handling and trace records for field updates when supporting recorded fields.
impl<S> Layer<S> for LifecycleLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("new span exists");
        let parent = span
            .parent()
            .and_then(|parent| parent.extensions().get::<GroupData>().map(|data| data.id));
        let id = self.0.id();
        let mut visitor = Fields::default();
        attrs.record(&mut visitor);
        let _recording = self.0.recording.lock().unwrap();
        if self.0.finished.load(Ordering::Acquire) {
            return;
        }
        let group_start = |parent| Record::GroupStart {
            t: self.0.t(),
            id,
            parent: Some(parent),
            name: attrs.metadata().name().into(),
            fields: visitor.fields,
            thread: thread_name(),
        };
        self.0.open.lock().unwrap().insert(id);
        match parent {
            Some(parent) => self.0.send(group_start(parent)),
            None => {
                // Send while holding the lock so the phase cannot end first.
                let phase = self.0.phase.lock().unwrap();
                self.0.send(group_start(phase.1));
            }
        }
        span.extensions_mut().insert(GroupData {
            id,
            severity: AtomicU8::new(0),
        });
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let group = ctx
            .event_span(event)
            .and_then(|span| span.extensions().get::<GroupData>().map(|data| data.id));
        let mut visitor = Fields {
            event: true,
            ..Fields::default()
        };
        event.record(&mut visitor);
        let _recording = self.0.recording.lock().unwrap();
        if self.0.finished.load(Ordering::Acquire) {
            return;
        }
        let error = *metadata.level() == tracing::Level::ERROR;
        let known = if error {
            known_errors::find(
                self.0.known,
                metadata.target(),
                metadata.line(),
                &visitor.message,
            )
        } else {
            None
        };
        let severity = if known.is_some() { 1 } else { 2 };
        if error && let Some(scope) = ctx.event_scope(event) {
            for span in scope {
                if let Some(data) = span.extensions().get::<GroupData>() {
                    data.severity.fetch_max(severity, Ordering::Relaxed);
                }
            }
        }
        let line = |group| Record::Line {
            t: self.0.t(),
            group,
            level: metadata.level().to_string(),
            target: metadata.target().into(),
            message: visitor.message,
            known: known.map(|error| error.slug.into()),
            fields: visitor.fields,
        };
        if error {
            self.0.fail_phase(severity);
        }
        match group {
            Some(group) => self.0.send(line(group)),
            None => {
                // Send while holding the lock so the phase cannot end first.
                let phase = self.0.phase.lock().unwrap();
                self.0.send(line(phase.1));
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let _recording = self.0.recording.lock().unwrap();
        if self.0.finished.load(Ordering::Acquire) {
            return;
        }
        if let Some(span) = ctx.span(&id)
            && let Some(data) = span.extensions().get::<GroupData>()
            // A group that ended as interrupted when the run ended has no
            // second end.
            && self.0.open.lock().unwrap().remove(&data.id)
        {
            self.0.send(Record::GroupEnd {
                t: self.0.t(),
                id: data.id,
                outcome: outcome(&data.severity),
            });
        }
    }
}

#[derive(Default)]
struct Fields {
    event: bool,
    fields: Map<String, Value>,
    message: String,
}

impl Fields {
    fn insert(&mut self, field: &Field, value: Value) {
        if self.event && field.name() == "message" {
            self.message = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
        } else if !self.event || !field.name().starts_with("log.") {
            self.fields.insert(field.name().into(), value);
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.insert(field, format!("{value:?}").into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::{Registry, prelude::*};

    const TEST_KNOWN: &[KnownError] = &[
        KnownError {
            slug: "test-known",
            target: "known_target",
            message_contains: &["known problem"],
            lines: None,
            reason: "Test error",
            tracking: None,
        },
        KnownError {
            slug: "test-known-at-lines",
            target: "located_target",
            message_contains: &["located problem"],
            lines: Some((40, 42)),
            reason: "Test error logged from one place",
            tracking: None,
        },
    ];

    fn capture(f: impl FnOnce(&Lifecycle)) -> Vec<Record> {
        let (tx, rx) = mpsc::channel();
        let lifecycle = Arc::new(Lifecycle::new(tx, TEST_KNOWN));
        lifecycle.start_run("sample", "test", "com.example.sample");
        tracing::subscriber::with_default(
            Registry::default().with(LifecycleLayer(lifecycle.clone())),
            || f(&lifecycle),
        );
        rx.try_iter()
            .map(|message| match message {
                Message::Record(record) => record,
                Message::Flush(_) | Message::File(_) => panic!("unexpected message"),
            })
            .collect()
    }

    fn group(records: &[Record], name: &str) -> (u64, Option<u64>) {
        records
            .iter()
            .find_map(|record| match record {
                Record::GroupStart {
                    id,
                    parent,
                    name: actual,
                    ..
                } if actual == name => Some((*id, *parent)),
                _ => None,
            })
            .unwrap()
    }

    fn end(records: &[Record], id: u64) -> &Outcome {
        records
            .iter()
            .find_map(|record| match record {
                Record::GroupEnd {
                    id: actual,
                    outcome,
                    ..
                } if *actual == id => Some(outcome),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn parentless_span_belongs_to_startup() {
        let records = capture(|_| {
            let _span = tracing::info_span!("work");
        });
        assert_eq!(
            group(&records, "work").1,
            Some(group(&records, "app-startup").0)
        );
    }

    #[test]
    fn nested_parents_and_unique_ids() {
        let records = capture(|_| {
            {
                let parent = tracing::info_span!("parent");
                let _entered = parent.enter();
                let _child = tracing::info_span!("child");
            }
            for _ in 0..1000 {
                let _span = tracing::info_span!("sequential");
            }
        });
        assert_eq!(
            group(&records, "child").1,
            Some(group(&records, "parent").0)
        );
        let ids: Vec<_> = records
            .iter()
            .filter_map(|record| match record {
                Record::GroupStart { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 1003);
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            ids.len()
        );
    }

    #[test]
    fn events_attach_to_span_or_current_phase() {
        let records = capture(|_| {
            tracing::info!("outside");
            let span = tracing::info_span!("work");
            let _entered = span.enter();
            tracing::info!(
                count = 2_u64,
                ok = true,
                ratio = 1.5,
                name = "sample",
                "inside"
            );
        });
        for (message, expected) in [
            ("outside", group(&records, "app-startup").0),
            ("inside", group(&records, "work").0),
        ] {
            assert!(records.iter().any(|record| matches!(record,
                Record::Line { group, message: actual, .. } if *group == expected && actual == message)));
        }
        let fields = records
            .iter()
            .find_map(|record| match record {
                Record::Line {
                    message, fields, ..
                } if message == "inside" => Some(fields),
                _ => None,
            })
            .unwrap();
        assert_eq!(fields["count"], 2);
        assert_eq!(fields["ok"], true);
        assert_eq!(fields["ratio"], 1.5);
        assert_eq!(fields["name"], "sample");
    }

    #[test]
    fn error_marks_ancestors_and_phase_but_not_sibling() {
        let records = capture(|lifecycle| {
            {
                let parent = tracing::info_span!("parent");
                let _parent = parent.enter();
                {
                    let nested = tracing::info_span!("nested");
                    let _nested = nested.enter();
                    tracing::error!("failure");
                }
                let _sibling = tracing::info_span!("sibling");
            }
            lifecycle.finish();
        });
        for name in ["app-startup", "parent", "nested"] {
            assert_eq!(*end(&records, group(&records, name).0), Outcome::Failure);
        }
        assert!(records.iter().any(|record| matches!(record,
            Record::Line { level, known: None, .. } if level == "ERROR")));
        assert_eq!(
            *end(&records, group(&records, "sibling").0),
            Outcome::Success
        );
    }

    #[test]
    fn known_error_marks_nested_span_ancestors_and_phase() {
        let records = capture(|lifecycle| {
            {
                let parent = tracing::info_span!("parent");
                let _parent = parent.enter();
                let nested = tracing::info_span!("nested");
                let _nested = nested.enter();
                tracing::error!(target: "known_target", "known problem");
            }
            lifecycle.finish();
        });
        for name in ["app-startup", "parent", "nested"] {
            assert_eq!(
                *end(&records, group(&records, name).0),
                Outcome::KnownFailure
            );
        }
        assert!(records.iter().any(|record| matches!(record,
            Record::Line { group: id, known: Some(slug), .. }
            if *id == group(&records, "nested").0 && slug == "test-known")));
    }

    #[test]
    fn unknown_error_takes_precedence_in_either_order() {
        for known_first in [true, false] {
            let records = capture(|lifecycle| {
                {
                    let span = tracing::info_span!("work");
                    let _entered = span.enter();
                    for known in [known_first, !known_first] {
                        if known {
                            tracing::error!(target: "known_target", "known problem");
                        } else {
                            tracing::error!("unknown problem");
                        }
                    }
                }
                lifecycle.finish();
            });
            for name in ["work", "app-startup"] {
                assert_eq!(*end(&records, group(&records, name).0), Outcome::Failure);
            }
        }
    }

    #[test]
    fn matching_warning_does_not_mark_known_or_fail_group() {
        let records = capture(|lifecycle| {
            {
                let span = tracing::info_span!("work");
                let _entered = span.enter();
                tracing::warn!(target: "known_target", "known problem");
            }
            lifecycle.finish();
        });
        for name in ["work", "app-startup"] {
            assert_eq!(*end(&records, group(&records, name).0), Outcome::Success);
        }
        assert!(records.iter().any(|record| matches!(record,
            Record::Line { level, known: None, .. } if level == "WARN")));
    }

    #[test]
    fn parentless_known_error_marks_current_phase() {
        let records = capture(|lifecycle| {
            tracing::error!(target: "known_target", "known problem");
            lifecycle.finish();
        });
        assert_eq!(
            *end(&records, group(&records, "app-startup").0),
            Outcome::KnownFailure
        );
        assert!(records.iter().any(|record| matches!(record,
            Record::Line { known: Some(slug), .. } if slug == "test-known")));
    }

    #[test]
    fn a_known_error_with_lines_matches_a_log_record_only_from_those_lines() {
        let records = capture(|_| {
            for line in [41, 302] {
                tracing_log::format_trace(
                    &log::Record::builder()
                        .level(log::Level::Error)
                        .target("located_target")
                        .file(Some("crates/located/src/platform.rs"))
                        .line(Some(line))
                        .args(format_args!("located problem at {line}"))
                        .build(),
                )
                .unwrap();
            }
        });
        let known = |wanted: &str| {
            records.iter().find_map(|record| match record {
                Record::Line { message, known, .. } if message == wanted => Some(known.clone()),
                _ => None,
            })
        };
        assert_eq!(
            known("located problem at 41"),
            Some(Some("test-known-at-lines".into()))
        );
        assert_eq!(known("located problem at 302"), Some(None));
    }

    #[test]
    fn log_records_keep_target_message_and_group() {
        let records = capture(|_| {
            let span = tracing::info_span!("work");
            let _entered = span.enter();
            tracing_log::format_trace(
                &log::Record::builder()
                    .level(log::Level::Warn)
                    .target("diagnostic_target")
                    .module_path(Some("diagnostic_module"))
                    .file(Some("source.rs"))
                    .line(Some(42))
                    .args(format_args!("diagnostic message"))
                    .build(),
            )
            .unwrap();
        });
        let (id, _) = group(&records, "work");
        assert!(records.iter().any(|record| matches!(record,
            Record::Line { group, target, message, fields, level, .. }
            if *group == id && target == "diagnostic_target" && message == "diagnostic message"
                && level == "WARN" && fields.is_empty())));
    }

    #[test]
    fn groups_and_lines_without_a_parent_never_reference_an_ended_phase() {
        let (tx, rx) = mpsc::channel();
        let lifecycle = Arc::new(Lifecycle::new(tx, TEST_KNOWN));
        lifecycle.start_run("sample", "test", "com.example.sample");
        let dispatch =
            tracing::Dispatch::new(Registry::default().with(LifecycleLayer(lifecycle.clone())));
        let worker = std::thread::spawn(move || {
            tracing::dispatcher::with_default(&dispatch, || {
                for _ in 0..10_000 {
                    let _span = tracing::info_span!("work");
                    tracing::info!("line");
                }
            })
        });
        for _ in 0..1_000 {
            lifecycle.set_phase(Phase::Running);
        }
        worker.join().unwrap();

        let mut ended = std::collections::HashSet::new();
        for message in rx.try_iter() {
            let Message::Record(record) = message else {
                continue;
            };
            match record {
                Record::GroupEnd { id, .. } => {
                    ended.insert(id);
                }
                Record::GroupStart {
                    parent: Some(group),
                    ..
                }
                | Record::Line { group, .. } => {
                    assert!(
                        !ended.contains(&group),
                        "record references ended group {group}"
                    );
                }
                _ => {}
            }
        }
    }

    #[test]
    fn phase_transition_ends_startup_before_running() {
        let records = capture(|lifecycle| {
            lifecycle.set_phase(Phase::Running);
            let _span = tracing::info_span!("running-work");
        });
        let startup = group(&records, "app-startup").0;
        let running = group(&records, "app-running").0;
        assert_eq!(group(&records, "running-work").1, Some(running));
        let end = records
            .iter()
            .position(|record| matches!(record, Record::GroupEnd { id, .. } if *id == startup))
            .unwrap();
        let start = records
            .iter()
            .position(|record| matches!(record, Record::GroupStart { id, .. } if *id == running))
            .unwrap();
        assert!(end < start);
    }

    fn ends(records: &[Record]) -> Vec<(u64, Outcome)> {
        records
            .iter()
            .filter_map(|record| match record {
                Record::GroupEnd { id, outcome, .. } => Some((*id, outcome.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_error_after_the_phase_changed_marks_the_phase_current_then() {
        let records = capture(|lifecycle| {
            // Opened while starting up, still open when the app runs.
            let span = tracing::info_span!("long-work");
            lifecycle.set_phase(Phase::Running);
            span.in_scope(|| tracing::error!("broke"));
            drop(span);
            lifecycle.finish();
        });
        let startup = group(&records, "app-startup").0;
        let running = group(&records, "app-running").0;
        let long = group(&records, "long-work").0;
        let ends = ends(&records);
        assert!(ends.contains(&(startup, Outcome::Success)), "{ends:?}");
        assert!(ends.contains(&(running, Outcome::Failure)), "{ends:?}");
        assert!(ends.contains(&(long, Outcome::Failure)), "{ends:?}");
    }

    #[test]
    fn groups_open_when_the_run_ends_are_interrupted_and_the_run_ends_once() {
        let records = capture(|lifecycle| {
            let outer = tracing::info_span!("outer").entered();
            let inner = tracing::info_span!("inner").entered();
            lifecycle.finish();
            lifecycle.finish();
            // Spans that close after the run ended add no second end.
            drop(inner);
            drop(outer);
        });
        let outer = group(&records, "outer").0;
        let inner = group(&records, "inner").0;
        let startup = group(&records, "app-startup").0;
        // Newest first, then the phase, then the run.
        assert_eq!(
            ends(&records),
            [
                (inner, Outcome::Interrupted),
                (outer, Outcome::Interrupted),
                (startup, Outcome::Success)
            ]
        );
        let ended = records
            .iter()
            .filter(|record| matches!(record, Record::RunEnd { .. }))
            .count();
        assert_eq!(ended, 1);
        assert!(matches!(records.last(), Some(Record::RunEnd { .. })));
    }
}
