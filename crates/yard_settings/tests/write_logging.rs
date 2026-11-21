use gpui::TestAppContext;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use yard_settings::{Settings, get, init, load, register, write};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "yard-settings-write-logging-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct Group {
    id: u64,
    seq: u64,
    parent: Option<u64>,
    level: tracing::Level,
    name: &'static str,
    fields: BTreeMap<&'static str, String>,
}

#[derive(Debug)]
struct Event {
    seq: u64,
    group: Option<u64>,
    level: tracing::Level,
    message: Option<String>,
    fields: BTreeMap<&'static str, String>,
}

#[derive(Debug)]
struct Diagnostic {
    level: log::Level,
    group: Option<u64>,
    message: String,
}

struct Captured {
    next_id: u64,
    next_seq: u64,
    spans: BTreeMap<u64, &'static str>,
    entered: Vec<u64>,
    entered_groups: BTreeSet<u64>,
    groups: Vec<Group>,
    events: Vec<Event>,
    diagnostics: Vec<Diagnostic>,
}

impl Captured {
    const fn new() -> Self {
        Self {
            next_id: 1,
            next_seq: 1,
            spans: BTreeMap::new(),
            entered: Vec::new(),
            entered_groups: BTreeSet::new(),
            groups: Vec::new(),
            events: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn next_sequence(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    fn current_settings_group(&self) -> Option<u64> {
        self.entered
            .iter()
            .rev()
            .copied()
            .find(|id| self.spans[id].starts_with("yard_settings"))
    }
}

static CAPTURED: Mutex<Captured> = Mutex::new(Captured::new());

struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_settings")
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        let mut captured = CAPTURED.lock().unwrap();
        let group = captured.current_settings_group();
        captured.diagnostics.push(Diagnostic {
            level: record.level(),
            group,
            message,
        });
    }

    fn flush(&self) {}
}

#[derive(Default)]
struct Fields {
    message: Option<String>,
    values: BTreeMap<&'static str, String>,
}

impl Fields {
    fn insert(&mut self, field: &tracing::field::Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.values.insert(field.name(), value);
        }
    }
}

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.insert(field, value.to_owned());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.insert(field, value.to_string());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.insert(field, value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.insert(field, value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.insert(field, format!("{value:?}"));
    }
}

struct Trace;

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
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let metadata = attributes.metadata();
        let target = metadata.target();
        let mut captured = CAPTURED.lock().unwrap();
        let id = captured.next_id;
        captured.next_id += 1;
        if target.starts_with("yard_settings") {
            let seq = captured.next_sequence();
            let parent = captured.current_settings_group();
            captured.groups.push(Group {
                id,
                seq,
                parent,
                level: *metadata.level(),
                name: metadata.name(),
                fields: fields.values,
            });
        }
        captured.spans.insert(id, target);
        tracing::span::Id::from_u64(id)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let metadata = event.metadata();
        if !metadata.target().starts_with("yard_settings") {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = CAPTURED.lock().unwrap();
        let seq = captured.next_sequence();
        let group = captured.current_settings_group();
        captured.events.push(Event {
            seq,
            group,
            level: *metadata.level(),
            message: fields.message,
            fields: fields.values,
        });
    }

    fn enter(&self, id: &tracing::span::Id) {
        let id = id.into_u64();
        let mut captured = CAPTURED.lock().unwrap();
        if captured.spans[&id].starts_with("yard_settings") {
            captured.entered_groups.insert(id);
        }
        captured.entered.push(id);
    }

    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(CAPTURED.lock().unwrap().entered.pop(), Some(id.into_u64()));
    }
}

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
#[serde(default)]
struct First {
    greeting: String,
    size: u32,
    enabled: bool,
}

impl Default for First {
    fn default() -> Self {
        Self {
            greeting: "hello".into(),
            size: 14,
            enabled: true,
        }
    }
}

impl Settings for First {
    const KEY: &'static str = "first";
}

#[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
#[serde(default)]
struct Second {
    label: String,
    limit: usize,
}

impl Default for Second {
    fn default() -> Self {
        Self {
            label: "panel".into(),
            limit: 3,
        }
    }
}

impl Settings for Second {
    const KEY: &'static str = "second";
}

fn assert_write_group(
    group: &Group,
    entered: &BTreeSet<u64>,
    expected_section: &str,
    expected_field: &str,
) {
    assert!(
        entered.contains(&group.id),
        "group {} was opened but never entered",
        group.id
    );
    assert_eq!(group.level, tracing::Level::INFO);
    assert_eq!(group.name, "settings-write");
    assert_eq!(
        group.fields,
        BTreeMap::from([
            ("section", expected_section.to_owned()),
            ("field", expected_field.to_owned()),
        ]),
        "unexpected settings-write fields: {:#?}",
        group.fields
    );
}

fn assert_path_fields(fields: &BTreeMap<&'static str, String>, expected_path: &Path) {
    assert_eq!(
        fields.keys().copied().collect::<Vec<_>>(),
        ["path"],
        "unexpected fields: {fields:#?}"
    );
    let actual = &fields["path"];
    let display = expected_path.display().to_string();
    let debug = format!("{expected_path:?}");
    assert!(
        actual == &display || actual == &debug,
        "expected path {display} (or {debug}), got {actual}"
    );
}

fn assert_reload_group(
    group: &Group,
    entered: &BTreeSet<u64>,
    expected_path: &Path,
    write_group: u64,
) {
    assert!(
        entered.contains(&group.id),
        "group {} was opened but never entered",
        group.id
    );
    assert_eq!(group.level, tracing::Level::INFO);
    assert_eq!(group.name, "settings-reload");
    assert_eq!(
        group.parent,
        Some(write_group),
        "settings-reload of a write must belong to settings-write"
    );
    assert_path_fields(&group.fields, expected_path);
}

fn assert_debug_event(
    event: &Event,
    expected_group: u64,
    expected_message: &str,
    expected_fields: &[(&'static str, &str)],
) {
    assert_eq!(event.group, Some(expected_group));
    assert_eq!(event.level, tracing::Level::DEBUG);
    assert_eq!(event.message.as_deref(), Some(expected_message));
    let expected: BTreeMap<&'static str, String> = expected_fields
        .iter()
        .map(|(k, v)| (*k, (*v).to_owned()))
        .collect();
    assert_eq!(event.fields, expected);
}

fn assert_written_event(event: &Event, expected_group: u64, expected_path: &Path) {
    assert_eq!(event.group, Some(expected_group));
    assert_eq!(event.level, tracing::Level::DEBUG);
    assert_eq!(event.message.as_deref(), Some("written"));
    assert_path_fields(&event.fields, expected_path);
}

fn groups_since(captured: &Captured, cursor: usize, expected: usize) -> &[Group] {
    let groups = &captured.groups[cursor..];
    assert_eq!(
        groups.len(),
        expected,
        "expected {expected} groups, got {groups:#?}"
    );
    groups
}

fn events_since(captured: &Captured, cursor: usize, expected: usize) -> Vec<&Event> {
    // The contract's `trace` lines are not asserted.
    let events: Vec<&Event> = captured.events[cursor..]
        .iter()
        .filter(|event| event.level != tracing::Level::TRACE)
        .collect();
    assert_eq!(
        events.len(),
        expected,
        "expected {expected} events, got {events:#?}"
    );
    events
}

fn diagnostics_since(captured: &Captured, cursor: usize, expected: usize) -> &[Diagnostic] {
    let diagnostics = &captured.diagnostics[cursor..];
    assert_eq!(
        diagnostics.len(),
        expected,
        "expected {expected} diagnostics, got {diagnostics:#?}"
    );
    diagnostics
}

fn assert_diagnostic(
    diagnostic: &Diagnostic,
    expected_group: u64,
    expected_level: log::Level,
    expected_message: &str,
) {
    assert_eq!(diagnostic.level, expected_level);
    assert_eq!(diagnostic.group, Some(expected_group));
    assert_eq!(diagnostic.message, expected_message);
}

fn assert_diagnostic_reason(
    diagnostic: &Diagnostic,
    expected_group: u64,
    expected_level: log::Level,
    expected_prefix: &str,
) {
    assert_eq!(diagnostic.level, expected_level);
    assert_eq!(diagnostic.group, Some(expected_group));
    let Some(reason) = diagnostic.message.strip_prefix(expected_prefix) else {
        panic!("unexpected diagnostic message: {}", diagnostic.message);
    };
    assert!(!reason.trim().is_empty());
}

// Alone in this binary: installs the process-wide diagnostic logger and captures every callsite.
#[gpui::test]
fn write_operations_open_groups_and_log_every_contract_line(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let temp = TempDir::new();

    tracing::subscriber::with_default(Trace, || {
        let path = temp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"b": 1, "first": {"size": 3, "greeting": "hi"}, "a": 2}"#,
        )
        .unwrap();
        cx.update(|cx| {
            init(cx);
            load(cx, path.clone());
            register::<First>(cx);
        });
        cx.read(|cx| assert_eq!(get::<First>(cx).size, 3));

        let (mut groups_cursor, mut events_cursor, mut diagnostics_cursor) = {
            let captured = CAPTURED.lock().unwrap();
            (
                captured.groups.len(),
                captured.events.len(),
                captured.diagnostics.len(),
            )
        };

        // A write opens `settings-write{section, field}` around `written{path}` and
        // the `settings-reload` of the new content, with its `changed` lines.
        cx.update(|cx| write::<First>(cx, "size", json!(4)));
        cx.read(|cx| assert_eq!(get::<First>(cx).size, 4));
        {
            let captured = CAPTURED.lock().unwrap();
            let groups = groups_since(&captured, groups_cursor, 2);
            assert_write_group(&groups[0], &captured.entered_groups, "first", "size");
            assert_reload_group(&groups[1], &captured.entered_groups, &path, groups[0].id);
            let events = events_since(&captured, events_cursor, 3);
            assert_written_event(events[0], groups[0].id, &path);
            assert_debug_event(events[1], groups[1].id, "changed", &[("section", "first")]);
            assert_debug_event(events[2], groups[1].id, "reloaded", &[("changed", "1")]);
            assert!(
                events[0].seq < groups[1].seq,
                "`written` must precede the settings-reload group"
            );
            diagnostics_since(&captured, diagnostics_cursor, 0);
            groups_cursor = captured.groups.len();
            events_cursor = captured.events.len();
            diagnostics_cursor = captured.diagnostics.len();
        }

        // The same value logs `unchanged`: no `written`, no reload, no diagnostic.
        cx.update(|cx| write::<First>(cx, "size", json!(4)));
        {
            let captured = CAPTURED.lock().unwrap();
            let groups = groups_since(&captured, groups_cursor, 1);
            assert_write_group(&groups[0], &captured.entered_groups, "first", "size");
            let events = events_since(&captured, events_cursor, 1);
            assert_debug_event(events[0], groups[0].id, "unchanged", &[]);
            diagnostics_since(&captured, diagnostics_cursor, 0);
            groups_cursor = captured.groups.len();
            events_cursor = captured.events.len();
            diagnostics_cursor = captured.diagnostics.len();
        }

        // A file that cannot be read warns inside the write's group.
        fs::write(&path, r#"{"first": "#).unwrap();
        cx.update(|cx| write::<First>(cx, "size", json!(9)));
        cx.read(|cx| assert_eq!(get::<First>(cx).size, 4));
        {
            let captured = CAPTURED.lock().unwrap();
            let groups = groups_since(&captured, groups_cursor, 1);
            assert_write_group(&groups[0], &captured.entered_groups, "first", "size");
            events_since(&captured, events_cursor, 0);
            let diagnostics = diagnostics_since(&captured, diagnostics_cursor, 1);
            assert_diagnostic_reason(
                &diagnostics[0],
                groups[0].id,
                log::Level::Warn,
                "the settings file is not written: ",
            );
            groups_cursor = captured.groups.len();
            events_cursor = captured.events.len();
            diagnostics_cursor = captured.diagnostics.len();
        }

        // A section that is not a JSON object warns the same way.
        fs::write(&path, r#"{"first": 42, "b": 1}"#).unwrap();
        cx.update(|cx| write::<First>(cx, "size", json!(9)));
        cx.read(|cx| assert_eq!(get::<First>(cx).size, 4));
        {
            let captured = CAPTURED.lock().unwrap();
            let groups = groups_since(&captured, groups_cursor, 1);
            assert_write_group(&groups[0], &captured.entered_groups, "first", "size");
            events_since(&captured, events_cursor, 0);
            let diagnostics = diagnostics_since(&captured, diagnostics_cursor, 1);
            assert_diagnostic_reason(
                &diagnostics[0],
                groups[0].id,
                log::Level::Warn,
                "the settings file is not written: ",
            );
            groups_cursor = captured.groups.len();
            events_cursor = captured.events.len();
            diagnostics_cursor = captured.diagnostics.len();
        }

        // A value of the wrong type is the code's mistake: an ERROR.
        fs::write(&path, r#"{"first": {"size": 4, "greeting": "hi"}, "b": 1}"#).unwrap();
        cx.update(|cx| write::<First>(cx, "size", json!("big")));
        cx.read(|cx| assert_eq!(get::<First>(cx).size, 4));
        {
            let captured = CAPTURED.lock().unwrap();
            let groups = groups_since(&captured, groups_cursor, 1);
            assert_write_group(&groups[0], &captured.entered_groups, "first", "size");
            events_since(&captured, events_cursor, 0);
            let diagnostics = diagnostics_since(&captured, diagnostics_cursor, 1);
            assert_diagnostic_reason(
                &diagnostics[0],
                groups[0].id,
                log::Level::Error,
                "section first cannot take size: ",
            );
            groups_cursor = captured.groups.len();
            events_cursor = captured.events.len();
            diagnostics_cursor = captured.diagnostics.len();
        }

        // A field that is not one of the section's.
        cx.update(|cx| write::<First>(cx, "nope", json!(1)));
        {
            let captured = CAPTURED.lock().unwrap();
            let groups = groups_since(&captured, groups_cursor, 1);
            assert_write_group(&groups[0], &captured.entered_groups, "first", "nope");
            events_since(&captured, events_cursor, 0);
            let diagnostics = diagnostics_since(&captured, diagnostics_cursor, 1);
            assert_diagnostic(
                &diagnostics[0],
                groups[0].id,
                log::Level::Error,
                "section first has no field nope",
            );
            groups_cursor = captured.groups.len();
            events_cursor = captured.events.len();
            diagnostics_cursor = captured.diagnostics.len();
        }

        // A section that is not registered.
        cx.update(|cx| write::<Second>(cx, "label", json!("x")));
        {
            let captured = CAPTURED.lock().unwrap();
            let groups = groups_since(&captured, groups_cursor, 1);
            assert_write_group(&groups[0], &captured.entered_groups, "second", "label");
            events_since(&captured, events_cursor, 0);
            let diagnostics = diagnostics_since(&captured, diagnostics_cursor, 1);
            assert_diagnostic(
                &diagnostics[0],
                groups[0].id,
                log::Level::Error,
                "section second is not registered",
            );
        }

        // A file that cannot be written. On Unix the directory refuses a new
        // file; on Windows, where a read-only folder still takes new files,
        // another handle holds the file open, sharing it for reading only, so
        // the file can be read but not replaced.
        let sealed_dir = temp.path().join("sealed");
        fs::create_dir_all(&sealed_dir).unwrap();
        let sealed_path = sealed_dir.join("settings.json");
        fs::write(&sealed_path, r#"{"first": {"size": 7}}"#).unwrap();
        #[cfg(unix)]
        let original = fs::metadata(&sealed_dir).unwrap().permissions();
        #[cfg(unix)]
        {
            let mut read_only = original.clone();
            read_only.set_readonly(true);
            fs::set_permissions(&sealed_dir, read_only).unwrap();
        }

        let app = cx.new_app();
        app.update(|cx| {
            init(cx);
            load(cx, sealed_path.clone());
            register::<First>(cx);
        });
        #[cfg(windows)]
        let held = {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                // FILE_SHARE_READ
                .share_mode(0x1)
                .open(&sealed_path)
                .unwrap()
        };
        {
            let captured = CAPTURED.lock().unwrap();
            groups_cursor = captured.groups.len();
            events_cursor = captured.events.len();
            diagnostics_cursor = captured.diagnostics.len();
        }
        app.update(|cx| write::<First>(cx, "size", json!(8)));
        app.read(|cx| assert_eq!(get::<First>(cx).size, 7));
        {
            let captured = CAPTURED.lock().unwrap();
            let groups = groups_since(&captured, groups_cursor, 1);
            assert_write_group(&groups[0], &captured.entered_groups, "first", "size");
            events_since(&captured, events_cursor, 0);
            let diagnostics = diagnostics_since(&captured, diagnostics_cursor, 1);
            assert_diagnostic_reason(
                &diagnostics[0],
                groups[0].id,
                log::Level::Error,
                "the settings file cannot be written: ",
            );
        }

        // The directory is made writable again, or the file is let go, so the
        // temporary tree cleans up, and the file kept its content.
        #[cfg(unix)]
        fs::set_permissions(&sealed_dir, original).unwrap();
        #[cfg(windows)]
        drop(held);
        assert_eq!(
            fs::read_to_string(&sealed_path).unwrap(),
            r#"{"first": {"size": 7}}"#
        );
    });

    let captured = CAPTURED.lock().unwrap();
    assert!(
        captured.entered.is_empty(),
        "spans still entered: {:?}",
        captured.entered
    );
}
