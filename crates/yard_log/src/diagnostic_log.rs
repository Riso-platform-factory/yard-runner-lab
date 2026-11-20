//! The diagnostic log, and the start of both logs: the application writes persistent,
//! rotating diagnostics to its product-named log and, when enabled, the current run's
//! lifecycle to stderr and lifecycle.jsonl.

use super::lifecycle_trace::{KNOWN_ERRORS, Lifecycle, LifecycleLayer, Message, Phase, writer};
use gpui::App;
use std::{
    backtrace::Backtrace,
    collections::HashMap,
    fs::{self, File},
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use tracing_subscriber::{EnvFilter, prelude::*};
use yard_product::Product;

static PRODUCT: OnceLock<Product> = OnceLock::new();
static DIAGNOSTIC_PATH: OnceLock<PathBuf> = OnceLock::new();
static ROTATED_PATH: OnceLock<PathBuf> = OnceLock::new();
static LIFECYCLE_PATH: OnceLock<PathBuf> = OnceLock::new();
static LIFECYCLE: OnceLock<Arc<Lifecycle>> = OnceLock::new();

/// A line of the diagnostic log written before its file is open.
struct Early {
    level: log::Level,
    target: String,
    module_path: Option<String>,
    file: Option<String>,
    line: Option<u32>,
    message: String,
}

/// Lines kept until the diagnostic log's file is open. `None` once it is.
static EARLY: Mutex<Option<Vec<Early>>> = Mutex::new(Some(Vec::new()));
/// Whether [`open_files`] has run.
static OPENED: AtomicBool = AtomicBool::new(false);

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        zlog::Zlog {}.enabled(metadata) || LIFECYCLE.get().is_some()
    }

    fn log(&self, record: &log::Record<'_>) {
        if !keep(record) {
            zlog::Zlog {}.log(record);
        }
        if LIFECYCLE.get().is_some() {
            let _ = tracing_log::format_trace(record);
        }
    }

    fn flush(&self) {
        zlog::flush();
    }
}

/// Whether lines are still kept for the diagnostic log's file.
static EARLY_OPEN: AtomicBool = AtomicBool::new(true);

/// Keeps a line for the diagnostic log's file while it is not open, and
/// says whether it did. The line is formatted before the lock is taken, so
/// a message whose formatting logs, or panics, does not wait for itself.
fn keep(record: &log::Record<'_>) -> bool {
    if !EARLY_OPEN.load(Ordering::Acquire) {
        return false;
    }
    let line = Early {
        level: record.level(),
        target: record.target().to_owned(),
        module_path: record.module_path().map(str::to_owned),
        file: record.file().map(str::to_owned),
        line: record.line(),
        message: record.args().to_string(),
    };
    let mut early = EARLY.lock().unwrap_or_else(|poison| poison.into_inner());
    match early.as_mut() {
        Some(lines) => {
            lines.push(line);
            true
        }
        None => false,
    }
}

/// Ends the run in the lifecycle trace when the process exits, after every
/// quit handler has run: groups still open end as interrupted, then
/// `run-end`, and both files are flushed.
extern "C" fn at_exit() {
    if let Some(lifecycle) = LIFECYCLE.get() {
        lifecycle.finish();
    }
    flush();
}

/// Starts both logs, before any other work of the app: the lifecycle trace
/// prints from here on, and what is logged is kept until [`open_files`]
/// knows where to write it, since the application's directories are worked out after
/// this.
pub fn start(product: Product) {
    PRODUCT
        .set(product)
        .unwrap_or_else(|_| panic!("{} logger initialized twice", product.name));
    let name = product.name;
    let app_version = product.version;
    let identifier = product.identifier;
    let prefix = product.environment_prefix();
    let lifecycle_variable = product.lifecycle_variable();
    // An empty filter is the same as none.
    let lifecycle_filter = std::env::var(&lifecycle_variable)
        .ok()
        .filter(|filter| !filter.trim().is_empty());
    let mut invalid_filter = None;
    if lifecycle_filter.is_some() || cfg!(debug_assertions) {
        let filter = lifecycle_filter.unwrap_or_else(|| "info".into());
        let filter = EnvFilter::try_new(&filter).unwrap_or_else(|error| {
            invalid_filter = Some(format!("{filter:?}: {error}"));
            EnvFilter::new("info")
        });
        let (tx, rx) = mpsc::channel();
        let color = std::io::stderr().is_terminal();
        std::thread::Builder::new()
            .name(format!("{name}-lifecycle-writer"))
            .spawn(move || writer::run(rx, color, name))
            .expect("could not spawn lifecycle writer");
        let lifecycle = Arc::new(Lifecycle::new(tx, KNOWN_ERRORS));
        LIFECYCLE
            .set(lifecycle.clone())
            .unwrap_or_else(|_| panic!("{name} logger initialized twice"));
        lifecycle.start_run(product.name, app_version, identifier);
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry()
                .with(filter)
                .with(LifecycleLayer(lifecycle)),
        )
        .expect("could not install lifecycle subscriber");
        // SAFETY: `at_exit` only reads statics that live as long as the
        // process.
        if unsafe { libc::atexit(at_exit) } != 0 {
            eprintln!("could not register the end of the lifecycle trace");
        }
    } else {
        // With `tracing`'s `log` feature on (calloop turns it on for Linux),
        // events are sent to `log` until a dispatcher has been set, so
        // without one the lifecycle trace would end up in the diagnostic log.
        tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::default())
            .expect("could not install the silent subscriber");
    }

    log::set_logger(&Logger)
        .unwrap_or_else(|error| panic!("could not install {name} logger: {error}"));
    log::set_max_level(log::LevelFilter::max());
    zlog::process_env(std::env::var(format!("{prefix}_LOG")).ok());
    zlog::filter::refresh_from_settings(&HashMap::default());

    let start_thread = std::thread::current().id();
    let previous = std::panic::take_hook();
    // TODO(panic-before-open-files): A run ending in a panic before open_files leaves kept lines only on stderr because no logs directory is known. Persist them when the paths engine can supply a directory to the panic hook.
    std::panic::set_hook(Box::new(move |panic| {
        log::error!("{panic}\n{}", Backtrace::force_capture());
        if !OPENED.load(Ordering::Acquire) && LIFECYCLE.get().is_none() {
            // Nothing else shows the lines kept for a file that never
            // opened.
            for line in EARLY
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .flatten()
            {
                eprintln!("{} [{}] {}", line.level, line.target, line.message);
            }
        }
        if std::thread::current().id() == start_thread
            && let Some(lifecycle) = LIFECYCLE.get()
        {
            lifecycle.finish_failed();
        }
        flush();
        previous(panic);
    }));
    log::info!("{name} {app_version} ({identifier}) starting");
    if let Some(problem) = invalid_filter {
        log::warn!("invalid {lifecycle_variable} filter {problem}; using info");
    }
}

/// Opens the files of both logs in the application's logs directory, and writes there
/// what was logged since [`start`]. Only the first call opens them.
pub fn open_files(logs: &Path) {
    let _span = tracing::info_span!("log-open").entered();
    if OPENED.swap(true, Ordering::AcqRel) {
        log::warn!("the log files are already open; a second call changes nothing");
        return;
    }
    let name = PRODUCT.get().expect("start called before open_files").name;
    let mut problems = Vec::new();
    if let Err(error) = fs::create_dir_all(logs) {
        problems.push(format!("could not create {name}'s logs directory: {error}"));
    }
    let diagnostic = DIAGNOSTIC_PATH.get_or_init(|| logs.join(format!("{name}.log")));
    let rotated = ROTATED_PATH.get_or_init(|| logs.join(format!("{name}.log.old")));
    let lifecycle_path = LIFECYCLE_PATH.get_or_init(|| logs.join("lifecycle.jsonl"));
    match LIFECYCLE.get() {
        Some(lifecycle) => {
            let file = File::create(lifecycle_path)
                .map_err(|error| {
                    problems.push(format!("could not create the lifecycle log: {error}"))
                })
                .ok();
            let _ = lifecycle.tx.send(Message::File(file));
        }
        // With the trace off, a file from an earlier run would look like
        // this one's.
        // TODO(trace-off-test): No test covers this trace-off branch. Add a test with debug assertions disabled and no lifecycle filter to verify removal of an old lifecycle file.
        None => match fs::remove_file(lifecycle_path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                problems.push(format!("could not remove an old lifecycle log: {error}"))
            }
            _ => {}
        },
    }
    let previous_size = fs::metadata(diagnostic).ok().map(|m| m.len());
    let rotated_before = fs::metadata(rotated)
        .ok()
        .map(|m| (m.len(), m.modified().ok()));
    if let Err(error) = zlog::init_output_file(diagnostic, Some(rotated)) {
        problems.push(format!("could not open the diagnostic log: {error}"));
        if LIFECYCLE.get().is_none() {
            zlog::init_output_stderr();
        }
    }
    // zlog copies then truncates, even when the copy fails. Confirm both
    // effects before reporting a rotation, rather than assuming its size threshold.
    if let (Some(previous_size), Ok(moved), Ok(current)) = (
        previous_size,
        fs::metadata(rotated),
        fs::metadata(diagnostic),
    ) && previous_size > 0
        && current.len() == 0
        && moved.len() == previous_size
        && rotated_before != Some((moved.len(), moved.modified().ok()))
    {
        tracing::debug!(
            path = %rotated.display(),
            bytes = moved.len(),
            "rotated previous diagnostic log"
        );
    }
    // The kept lines, in the order they were logged, also those other
    // threads log meanwhile.
    let mut kept = 0;
    loop {
        let lines = {
            let mut early = EARLY.lock().unwrap_or_else(|poison| poison.into_inner());
            let lines = std::mem::take(early.as_mut().expect("kept until now"));
            if lines.is_empty() {
                *early = None;
                EARLY_OPEN.store(false, Ordering::Release);
                break;
            }
            lines
        };
        kept += lines.len();
        for line in &lines {
            use log::Log as _;
            zlog::Zlog {}.log(
                &log::Record::builder()
                    .level(line.level)
                    .target(&line.target)
                    .module_path(line.module_path.as_deref())
                    .file(line.file.as_deref())
                    .line(line.line)
                    .args(format_args!("{}", line.message))
                    .build(),
            );
        }
    }
    for problem in &problems {
        log::error!("{problem}");
    }
    tracing::debug!(
        diagnostic = %diagnostic.display(),
        lifecycle = %lifecycle_path.display(),
        kept,
        "opened"
    );
}

/// Starts the shutdown phase when the app begins to quit. The run ends when
/// the process exits, after every quit handler.
pub fn init_app(cx: &mut App) {
    if let Some(lifecycle) = LIFECYCLE.get() {
        cx.on_app_quit(move |_| {
            lifecycle.set_phase(Phase::Shutdown);
            async {}
        })
        .detach();
        tracing::debug!("quit handling registered");
    }
}

/// Ends startup and begins the running phase.
pub fn startup_finished() {
    if let Some(lifecycle) = LIFECYCLE.get() {
        lifecycle.set_phase(Phase::Running);
    }
}

/// Flushes diagnostics and waits up to 100 ms for queued lifecycle records.
pub(crate) fn flush() {
    zlog::flush();
    if let Some(lifecycle) = LIFECYCLE.get() {
        let (tx, rx) = mpsc::sync_channel(0);
        if lifecycle.tx.send(Message::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_millis(100));
        }
    }
}
