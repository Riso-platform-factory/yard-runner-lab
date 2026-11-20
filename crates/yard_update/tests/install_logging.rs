// The check and the install run only on macOS until the release-updates slice.
#![cfg(target_os = "macos")]

use gpui::TestAppContext;
use http_client::{AsyncBody, FakeHttpClient, Response};
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use yard_product::Product;
use yard_shell::Extension;

// The diagnostic log's records are re-emitted as tracing events under this
// target, so one subscriber sees both logs.
const PROBE: &str = "update_install_probe";

const NAME: &str = "yard-check";
const IDENTIFIER: &str = "vn.risotech.yard";
const URL: &str = "http://127.0.0.1:8765/latest.json";
// The path and query make it obvious when a log leaks them: only the host may
// appear in the logs.
const ZIP_URL: &str = "https://releases.example.com/download/yard-check-0.2.0.zip?channel=alpha";
const WRONG_SHA256: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const FIRST_CHECK: Duration = Duration::from_secs(5);
const NEXT_CHECK: Duration = Duration::from_secs(24 * 60 * 60);

static RECORDS: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

// The one process-wide diagnostic logger this file installs once; alone in
// its binary, it cannot collide with another test's logger.
struct Diagnostics;

impl log::Log for Diagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("yard_update")
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        RECORDS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((record.level(), record.args().to_string()));
        match record.level() {
            log::Level::Error => tracing::error!(target: PROBE, "{}", record.args()),
            log::Level::Warn => tracing::warn!(target: PROBE, "{}", record.args()),
            log::Level::Info => tracing::info!(target: PROBE, "{}", record.args()),
            log::Level::Debug => tracing::debug!(target: PROBE, "{}", record.args()),
            log::Level::Trace => tracing::trace!(target: PROBE, "{}", record.args()),
        }
    }
    fn flush(&self) {}
}

fn errors() -> Vec<String> {
    RECORDS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter(|(level, _)| *level == log::Level::Error)
        .map(|(_, message)| message.clone())
        .collect()
}

// The groups the code opens and the lines it logs inside them.
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
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
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
        let target = event.metadata().target();
        if !(target.starts_with("yard_update") || target == PROBE) {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut captured = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let stack = if captured.entered.is_empty() {
            "none".to_string()
        } else {
            captured
                .entered
                .iter()
                .map(|id| captured.spans[id].as_str())
                .collect::<Vec<_>>()
                .join(" > ")
        };
        captured
            .lines
            .push(format!("{stack}: {}{}", event.metadata().level(), fields.0));
    }
    fn enter(&self, id: &tracing::span::Id) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entered
            .push(id.into_u64());
    }
    fn exit(&self, id: &tracing::span::Id) {
        assert_eq!(
            self.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entered
                .pop(),
            Some(id.into_u64())
        );
    }
}

// The spans ever created whose name starts with `prefix`, quotes stripped for
// field-value checks.
fn spans_named(trace: &Trace, prefix: &str) -> Vec<String> {
    trace
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .spans
        .values()
        .map(|span| span.replace('"', ""))
        .filter(|span| span.starts_with(prefix))
        .collect()
}

// The logged lines so far, quotes stripped.
fn lines(trace: &Trace) -> Vec<String> {
    trace
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .lines
        .iter()
        .map(|line| line.replace('"', ""))
        .collect()
}

// The groups enclosing `line`, innermost last, or "none".
fn stack_of(line: &str) -> &str {
    line.split_once(": ").map(|(stack, _)| stack).unwrap_or("")
}

// The line after its enclosing groups.
fn body_of(line: &str) -> &str {
    line.split_once(": ").map(|(_, body)| body).unwrap_or(line)
}

// Whether one of the groups enclosing `line` starts with `prefix`.
fn in_group(line: &str, prefix: &str) -> bool {
    stack_of(line)
        .split(" > ")
        .any(|group| group.starts_with(prefix))
}

// Whether the line sits inside `update-download` or `update-install`.
fn in_update_group(line: &str) -> bool {
    in_group(line, "update-download") || in_group(line, "update-install")
}

// The product the check and the install run against: `yard-check` 0.1.0.
fn product() -> Product {
    Product::new(NAME, "0.1.0", IDENTIFIER)
}

// The release file `start` reads: `url` and `sha256` name the zip to download.
fn release(version: &str, sha256: &str) -> String {
    format!("{{\"version\":\"{version}\",\"url\":\"{ZIP_URL}\",\"sha256\":\"{sha256}\"}}")
}

// A minimal `Info.plist` holding the keys an install checks.
fn info_plist(identifier: &str, version: &str, executable: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>CFBundleIdentifier</key>\n\
         \t<string>{identifier}</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\
         \t<string>{version}</string>\n\
         \t<key>CFBundleExecutable</key>\n\
         \t<string>{executable}</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

// `command` run to success.
fn run(command: &mut Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?} failed: {status}");
}

// Builds `<dir>/<name>.app`: the `Info.plist`, an executable script in
// `Contents/MacOS/` holding `marker`, signed ad hoc as `cargo xtask bundle macos`.
fn make_bundle(dir: &Path, name: &str, identifier: &str, version: &str, marker: &str) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        info_plist(identifier, version, name),
    )
    .unwrap();
    let executable = macos.join(name);
    std::fs::write(&executable, format!("#!/bin/sh\n# {marker}\nexit 0\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    run(Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(&app));
    app
}

// `ditto -c -k --keepParent` of `source` into `zip`.
fn ditto_zip(source: &Path, zip: &Path) {
    run(Command::new("ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(source)
        .arg(zip));
}

// `shasum -a 256` of `file`, the digest only.
fn sha256(file: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(file)
        .output()
        .unwrap();
    assert!(output.status.success(), "shasum failed: {output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

// A release for `version`: `<name>.app` built in `dir`, zipped as
// `ditto -c -k --keepParent` makes it; the zip's bytes and its digest.
fn release_zip(dir: &Path, identifier: &str, version: &str) -> (Vec<u8>, String) {
    let app = make_bundle(
        dir,
        NAME,
        identifier,
        version,
        &format!("version {version}"),
    );
    let zip = dir.join(format!("{NAME}-{version}.zip"));
    ditto_zip(&app, &zip);
    (std::fs::read(&zip).unwrap(), sha256(&zip))
}

// A client that answers every request with `body`.
fn client(body: Vec<u8>) -> Arc<http_client::HttpClientWithUrl> {
    FakeHttpClient::create(move |_request| {
        let body = body.clone();
        async move {
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(body))
                .unwrap())
        }
    })
}

// A client that answers the release file at `URL` with `json` and every
// other request with `zip`.
fn client_with_release(json: String, zip: Vec<u8>) -> Arc<http_client::HttpClientWithUrl> {
    FakeHttpClient::create(move |request| {
        let uri = request.uri().to_string();
        let json = json.clone();
        let zip = zip.clone();
        async move {
            let body = if uri == URL {
                AsyncBody::from(json)
            } else {
                AsyncBody::from(zip)
            };
            Ok(Response::builder().status(200).body(body).unwrap())
        }
    })
}

// Advances the test clock by `duration` and runs everything it unblocks. The
// first park lets a just-spawned timer register its deadline before the clock
// moves.
fn advance(app: &mut TestAppContext, duration: Duration) {
    app.run_until_parked();
    app.executor().advance_clock(duration);
    app.run_until_parked();
}

// Alone in this binary: every captured callsite runs under one scoped
// subscriber, and no other test reaches the install's lines beside it.
#[gpui::test]
fn update_install_logging(cx: &mut TestAppContext) {
    log::set_logger(&Diagnostics).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let trace = Trace::default();
    tracing::subscriber::with_default(trace.clone(), || {
        // A release downloaded and installed in place:
        // `update-download{version, host}` holding `downloaded`, and
        // `update-install{version}` holding `installed`.
        let temp = tempfile::tempdir().unwrap();
        let bundle = make_bundle(
            temp.path(),
            NAME,
            IDENTIFIER,
            "0.1.0",
            "the running version",
        );
        let release_dir = tempfile::tempdir().unwrap();
        let (zip, sha256) = release_zip(release_dir.path(), IDENTIFIER, "0.2.0");
        let installed = futures::executor::block_on(yard_update::install(
            client(zip),
            bundle,
            NAME,
            IDENTIFIER,
            "0.2.0",
            ZIP_URL,
            &sha256,
        ));
        assert!(installed.is_ok(), "the release installs: {installed:?}");
        assert!(
            spans_named(&trace, "update-download")
                .iter()
                .any(|span| span.contains("version=0.2.0")
                    && span.contains("host=releases.example.com")),
            "`update-download{{version=0.2.0, host=releases.example.com}}`: {:?}",
            spans_named(&trace, "update-download")
        );
        assert!(
            lines(&trace).iter().any(|line| {
                line.contains(": DEBUG")
                    && in_group(line, "update-download")
                    && body_of(line).contains("downloaded")
            }),
            "`downloaded` inside `update-download{{version=0.2.0, host}}`: {:#?}",
            lines(&trace)
        );
        assert!(
            spans_named(&trace, "update-install")
                .iter()
                .any(|span| span.contains("version=0.2.0")),
            "`update-install{{version=0.2.0}}`: {:?}",
            spans_named(&trace, "update-install")
        );
        assert!(
            lines(&trace).iter().any(|line| {
                line.contains(": DEBUG")
                    && in_group(line, "update-install")
                    && body_of(line).contains("installed")
            }),
            "`installed` inside `update-install{{version=0.2.0}}`: {:#?}",
            lines(&trace)
        );

        // A release refused on a wrong digest: the WARN inside its group.
        let refused_dir = tempfile::tempdir().unwrap();
        let refused_bundle = make_bundle(
            refused_dir.path(),
            NAME,
            IDENTIFIER,
            "0.1.0",
            "the running version",
        );
        let refused_release = tempfile::tempdir().unwrap();
        let (zip, _) = release_zip(refused_release.path(), IDENTIFIER, "0.2.0");
        let refused = futures::executor::block_on(yard_update::install(
            client(zip),
            refused_bundle,
            NAME,
            IDENTIFIER,
            "0.2.0",
            ZIP_URL,
            WRONG_SHA256,
        ));
        assert!(refused.is_err(), "a wrong digest is refused");
        assert!(
            lines(&trace)
                .iter()
                .any(|line| line.contains(": WARN") && in_update_group(line)),
            "the WARN inside its group: {:#?}",
            lines(&trace)
        );

        // Outside a bundle there is nothing to replace:
        // `not installable {reason}`, once.
        let mut app = cx.new_app();
        app.update(yard_ui::init);
        let extension = app.update(|cx| {
            // The startup binds the default keys in `app-init`, before extensions run.
            yard_shell::bind_default_keys(cx);
            Extension::new(cx, "update").unwrap()
        });
        let window = app.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), yard_shell::new_shell)
                .unwrap()
        });
        app.simulate_window_resize(window.into(), gpui::size(gpui::px(800.), gpui::px(600.)));
        let check = client_with_release(release("0.2.0", WRONG_SHA256), Vec::new());
        app.update(move |cx| yard_update::start(cx, &extension, product(), URL, check));
        advance(&mut app, FIRST_CHECK);
        advance(&mut app, NEXT_CHECK);
        let not_installable: Vec<String> = lines(&trace)
            .into_iter()
            .filter(|line| line.contains(": DEBUG") && body_of(line).contains("not installable"))
            .collect();
        assert_eq!(
            not_installable.len(),
            1,
            "`not installable {{reason}}` is logged once: {not_installable:#?}"
        );

        assert!(errors().is_empty(), "{:?}", errors());
        assert!(
            lines(&trace).iter().all(|line| !line.contains(": ERROR")),
            "never an ERROR: {:#?}",
            lines(&trace)
        );

        // The release URL's host may appear; its path and query never do, in
        // the lines or in the groups' fields.
        let exposed: Vec<String> = lines(&trace)
            .into_iter()
            .chain(spans_named(&trace, "update"))
            .filter(|line| line.contains("channel") || line.contains("download/"))
            .collect();
        assert!(
            exposed.is_empty(),
            "the release URL's path and query are never exposed: {exposed:?}"
        );
    });
    assert!(
        trace
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entered
            .is_empty(),
        "every group closed"
    );
}
