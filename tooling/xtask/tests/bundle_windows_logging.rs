#![cfg(feature = "dev")]
//! `bundle_windows` runs inside the group `bundle-windows{config, dev}`: a
//! `debug` line per step, a `warn` for the signing that comes later, a
//! refusal at `info` and never an `ERROR`, and a failed step at `error`.
//!
//! This test captures `tracing` output and starts `yard_log`, which installs
//! a process-wide `log` logger, so it is alone in its file and no other test
//! in this binary reaches the same log lines.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};
use support::{FakeRunner, tempdir, under_test};
use tracing::Level;
use xtask::{Interrupt, WindowsArgs, WindowsHost, bundle_windows};

/// A group that opened, or a line logged inside one.
#[derive(Debug)]
enum Record {
    Group {
        id: tracing::span::Id,
        name: &'static str,
        fields: Vec<(String, String)>,
    },
    Line {
        group: Option<tracing::span::Id>,
        level: tracing::Level,
        message: String,
    },
}

/// The fields of a span or an event as `name=value` pairs. A `?`-formatted
/// string's quotes are stripped so `name`, `%name`, and `?name` all read as
/// the bare value.
#[derive(Default)]
struct Fields(Vec<(String, String)>);
impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().into(), value.into()));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let debug = format!("{value:?}");
        let value = debug
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(debug.as_str());
        self.0.push((field.name().into(), value.into()));
    }
}

/// The groups that opened and the lines logged, in order; each line keeps the
/// group it was logged in, `None` when no group is open.
#[derive(Clone, Default)]
struct Trace {
    records: Arc<Mutex<Vec<Record>>>,
    open: Arc<Mutex<Vec<tracing::span::Id>>>,
}
impl tracing::Subscriber for Trace {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let mut records = self.records.lock().unwrap();
        let id = tracing::span::Id::from_u64(records.len() as u64 + 1);
        records.push(Record::Group {
            id: id.clone(),
            name: attributes.metadata().name(),
            fields: fields.0,
        });
        id
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let message = fields
            .0
            .into_iter()
            .find(|(name, _)| name.as_str() == "message")
            .map(|(_, value)| value)
            .unwrap_or_default();
        let group = self.open.lock().unwrap().last().cloned();
        self.records.lock().unwrap().push(Record::Line {
            group,
            level: *event.metadata().level(),
            message,
        });
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.open.lock().unwrap().push(id.clone());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(self.open.lock().unwrap().pop().as_ref(), Some(id));
    }
}

/// Starts `yard_log` as `xtask::main` does, with the lifecycle trace's own
/// output off, so the diagnostic lines (`log`) reach the test's subscriber as
/// they reach the lifecycle trace.
fn start_logs() {
    // SAFETY: the only test of this binary sets it before any thread reads
    // the environment.
    unsafe { std::env::set_var("XTASK_TEST_LIFECYCLE", "off") };
    yard_log::start(yard_product::Product::new(
        "xtask-test",
        "0.1.0",
        "dev.yard.xtask-test",
    ));
}

/// A Windows host whose `PATH` holds a fake `ISCC.exe`, and whose
/// `%SystemRoot%\System32` holds fake `tar.exe` and `certutil.exe`.
fn host(temp: &std::path::Path, system: &str) -> WindowsHost {
    let inno = temp.join("inno");
    std::fs::create_dir_all(&inno).unwrap();
    std::fs::write(inno.join("ISCC.exe"), b"").unwrap();
    let root = temp.join("Windows");
    std::fs::create_dir_all(root.join("System32")).unwrap();
    for tool in ["tar.exe", "certutil.exe"] {
        std::fs::write(root.join("System32").join(tool), b"").unwrap();
    }
    WindowsHost {
        system: system.into(),
        path: Some(inno.into_os_string()),
        program_files_x86: None,
        system_root: Some(root.into_os_string()),
    }
}

#[test]
fn bundling_runs_inside_the_bundle_windows_group() {
    start_logs();
    let temp = tempdir();
    let config = under_test("plain");
    let options = WindowsArgs {
        config: Some(config.clone()),
        out: Some(temp.path().join("out")),
        ..WindowsArgs::default()
    };
    let refused = WindowsArgs {
        env: vec!["YARD_HOME=C:\\temp".into()],
        ..options.clone()
    };
    let dev = WindowsArgs {
        config: Some(under_test("other")),
        dev: true,
        ..options.clone()
    };
    let signed = WindowsArgs {
        config: Some(under_test("signed")),
        ..options.clone()
    };
    let windows = host(temp.path(), "windows");
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        let interrupt = Interrupt::default();
        bundle_windows(
            &options,
            &windows,
            &interrupt,
            &mut FakeRunner::new(temp.path()),
        )
        .unwrap();
        bundle_windows(
            &refused,
            &windows,
            &interrupt,
            &mut FakeRunner::new(temp.path()),
        )
        .unwrap_err();
        let mut failing = FakeRunner::new(temp.path());
        failing.fail = Some("ISCC");
        bundle_windows(&options, &windows, &interrupt, &mut failing).unwrap_err();
        bundle_windows(
            &dev,
            &windows,
            &interrupt,
            &mut FakeRunner::new(temp.path()),
        )
        .unwrap();
        bundle_windows(
            &options,
            &host(temp.path(), "macos"),
            &interrupt,
            &mut FakeRunner::new(temp.path()),
        )
        .unwrap_err();
        bundle_windows(
            &signed,
            &windows,
            &interrupt,
            &mut FakeRunner::new(temp.path()),
        )
        .unwrap();
    });
    let records = trace.records.lock().unwrap();

    let groups: Vec<(tracing::span::Id, Vec<(String, String)>)> = records
        .iter()
        .filter_map(|record| match record {
            Record::Group { id, name, fields } if *name == "bundle-windows" => {
                Some((id.clone(), fields.clone()))
            }
            _ => None,
        })
        .collect();
    let field = |key: &str, value: &str| (key.to_owned(), value.to_owned());
    let plain = config.display().to_string();
    let other = under_test("other").display().to_string();
    let signed = under_test("signed").display().to_string();
    assert_eq!(
        groups
            .iter()
            .map(|(_, fields)| fields.clone())
            .collect::<Vec<_>>(),
        [
            vec![field("config", &plain), field("dev", "false")],
            vec![field("config", &plain), field("dev", "false")],
            vec![field("config", &plain), field("dev", "false")],
            vec![field("config", &other), field("dev", "true")],
            vec![field("config", &plain), field("dev", "false")],
            vec![field("config", &signed), field("dev", "false")],
        ],
        "one group per run, with its fields"
    );
    let lines_in = |id: &tracing::span::Id| -> Vec<(Level, String)> {
        records
            .iter()
            .filter_map(|record| match record {
                Record::Line {
                    group,
                    level,
                    message,
                } if group.as_ref() == Some(id) => Some((*level, message.clone())),
                _ => None,
            })
            .collect()
    };

    for run in [0, 3, 5] {
        let bundled = lines_in(&groups[run].0);
        let steps: Vec<&str> = bundled
            .iter()
            .filter(|(level, _)| *level == Level::DEBUG)
            .map(|(_, message)| message.as_str())
            .collect();
        for step in [
            "arguments",
            "config",
            "tools",
            "inno setup",
            "stages",
            "build",
            "built",
            "icon",
            "bundle",
            "installer",
            "zip",
            "publish",
            "zipped",
        ] {
            assert!(
                steps.contains(&step),
                "a debug line for {step}: {bundled:?}"
            );
        }
        let others: Vec<&(Level, String)> = bundled
            .iter()
            .filter(|(level, _)| *level != Level::DEBUG)
            .collect();
        if run == 5 {
            assert_eq!(
                others,
                [&(
                    Level::WARN,
                    "not signed: Windows signing comes with part 5e".to_owned()
                )],
                "the signing table is a warning, and the run goes on"
            );
        } else {
            assert!(
                others.is_empty(),
                "a run that succeeds logs only steps: {bundled:?}"
            );
        }
    }

    let refusal = lines_in(&groups[1].0);
    assert!(
        refusal.iter().any(|(level, message)| *level == Level::INFO
            && message
                == "refused: --env: a Windows review bundle cannot carry environment values, since a shortcut and a URL command cannot set them"),
        "a refusal is an info line with the reason: {refusal:?}"
    );
    assert!(
        !refusal.iter().any(|(level, _)| *level == Level::ERROR),
        "a refusal is never an ERROR: {refusal:?}"
    );

    let failure = lines_in(&groups[2].0);
    assert!(
        failure.iter().any(|(level, message)| *level == Level::ERROR
            && message == "failed at installer: ISCC exited with exit status: 1; Inno Setup printed why above"),
        "a failed step is an ERROR with the reason: {failure:?}"
    );

    let other_system = lines_in(&groups[4].0);
    assert_eq!(
        other_system,
        [
            (Level::DEBUG, "arguments".to_owned()),
            (
                Level::INFO,
                "refused: bundle windows runs only on Windows, and this system is macos".to_owned()
            ),
        ],
        "another system is refused at the start"
    );
}
