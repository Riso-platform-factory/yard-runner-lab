//! `cargo xtask bundle macos`: makes `<name>.app`, and with `--zip`
//! `<name>-<version>.zip`, from a product config; signs it with the
//! developer's identity, notarizes it, and wraps it in a disk image when the
//! config's `[release.macos]` asks. `cargo xtask bundle linux` is in
//! [`linux`], and `cargo xtask bundle windows` in [`windows`]; what they all
//! do is here.

mod linux;
mod signing;
mod windows;
pub use linux::{LinuxArgs, LinuxBundled, bundle_linux};
pub use signing::Secrets;
pub use windows::{WindowsArgs, WindowsBundled, WindowsHost, bundle_windows, inno_script};

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand};
use signing::{SECRET_VARIABLES, Session, Signing};
use yard_build_config::BuildConfig;
use yard_product::Product;

/// The production product's config, relative to the repository: the default
/// `--config`, and the one a `--dev` bundle must differ from.
const PRODUCTION_CONFIG: &str = "app/yard/product.toml";
/// The default `--out`, relative to the repository.
const DEFAULT_OUT: &str = "dist";

#[derive(Parser)]
#[command(name = "cargo xtask")]
struct Args {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Makes an installable package from a product config.
    #[command(subcommand)]
    Bundle(System),
}

#[derive(Subcommand)]
enum System {
    /// Makes `<name>.app` from production code, signed ad hoc.
    Macos(MacosArgs),
    /// Makes `<name>-<version>-linux-<arch>.tar.gz` from production code, on
    /// Linux.
    Linux(LinuxArgs),
    /// Makes `<Name>-<version>-<arch>-setup.exe` with Inno Setup, and
    /// `<Name>-<version>-windows-<arch>.zip`, from production code, on
    /// Windows.
    Windows(WindowsArgs),
}

/// The options of `cargo xtask bundle macos`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct MacosArgs {
    /// The product config [default: app/yard/product.toml in the repository].
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// The folder the bundle goes into [default: dist in the repository].
    #[arg(long, value_name = "FOLDER")]
    pub out: Option<PathBuf>,
    /// Builds a review bundle, which runs `dev`, with a product other than
    /// the production one.
    #[arg(long)]
    pub dev: bool,
    /// An environment variable of the review bundle (`LSEnvironment`).
    /// Needs `--dev`.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Also writes `<out>/<name>-<version>.zip`, a release, and prints its
    /// sha256.
    #[arg(long)]
    pub zip: bool,
}

/// What `bundle_macos` made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundled {
    pub app: PathBuf,
    /// The zip and its SHA-256 in hex, with `--zip`.
    pub zip: Option<(PathBuf, String)>,
    /// The disk image and its SHA-256 in hex, with `dmg = true`.
    pub dmg: Option<(PathBuf, String)>,
}

/// The reason of a run that stopped because it was interrupted.
const INTERRUPTED: &str = "interrupted";

/// Why `bundle_macos` stopped: the step, and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub step: &'static str,
    pub reason: String,
}

impl Failure {
    fn new(step: &'static str, reason: impl fmt::Display) -> Self {
        Self {
            step,
            reason: reason.to_string(),
        }
    }

    /// Whether the input broke a rule, which is expected, rather than a step
    /// that failed.
    pub fn is_refusal(&self) -> bool {
        matches!(self.step, "arguments" | "config" | "secrets" | "tools")
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.step, self.reason)
    }
}

/// Whether the run was asked to stop: set on SIGINT, SIGTERM, or SIGHUP by
/// the handler [`main`] installs, and read before each step, so the run stops
/// after the current step and still cleans up. Tests set it without a
/// signal. A second signal does not kill xtask either; in a terminal, Ctrl-C
/// also reaches the running child, which stops.
#[derive(Debug, Clone, Default)]
pub struct Interrupt(Arc<AtomicBool>);

impl Interrupt {
    /// Asks the run to stop. The run logs it when it sees it, inside its
    /// group.
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// The exit code of a run that did not succeed: 130 when it was
    /// interrupted, as a shell reports Ctrl-C, else 1.
    pub fn exit_code(&self) -> u8 {
        if self.is_set() { 130 } else { 1 }
    }
}

/// Runs the external commands, so tests can check them without running them.
pub trait Runner {
    /// Runs `command` to its end, its output on stderr.
    fn run(&mut self, command: &mut Command) -> Result<(), String>;
    /// Runs `command` to its end and returns what it wrote on stdout; its
    /// stderr stays on stderr.
    fn output(&mut self, command: &mut Command) -> Result<Vec<u8>, String>;
    /// Runs `command` to its end and returns whether it succeeded and what
    /// it wrote on stdout, which a failed command may still have written
    /// (such as `notarytool`'s report of a rejection); its stderr stays on
    /// stderr.
    fn report(&mut self, command: &mut Command) -> Result<(bool, Vec<u8>), String>;
    /// Runs `command` with `input` on its stdin and returns what it wrote on
    /// stdout. The input may hold a secret, and is never logged.
    fn input(&mut self, command: &mut Command, input: &[u8]) -> Result<Vec<u8>, String>;
}

/// Runs the commands for real. A command that a SIGINT, SIGTERM, or SIGHUP
/// ended sets `interrupt`, as the same signal reaches xtask's handler, so
/// the run is an interrupt whichever of the two is seen first.
pub struct SystemRunner {
    pub interrupt: Interrupt,
}

impl SystemRunner {
    /// Whether the command succeeded; when a signal of an interrupt ended
    /// it, sets the interrupt.
    fn succeeded(&self, command: &Command, status: ExitStatus) -> Result<(), String> {
        if status.success() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            // SIGHUP, SIGINT, and SIGTERM, the same on every Unix.
            if let Some(signal) = status.signal()
                && [1, 2, 15].contains(&signal)
            {
                self.interrupt.set();
            }
        }
        Err(format!("{} exited with {status}", program(command)))
    }
}

impl Runner for SystemRunner {
    fn run(&mut self, command: &mut Command) -> Result<(), String> {
        let status = command
            .stdout(Stdio::from(io::stderr()))
            .status()
            .map_err(|error| could_not_run(command, error))?;
        self.succeeded(command, status)
    }

    fn output(&mut self, command: &mut Command) -> Result<Vec<u8>, String> {
        let output = command
            .stderr(Stdio::inherit())
            .output()
            .map_err(|error| could_not_run(command, error))?;
        self.succeeded(command, output.status)?;
        Ok(output.stdout)
    }

    fn report(&mut self, command: &mut Command) -> Result<(bool, Vec<u8>), String> {
        let output = command
            .stderr(Stdio::inherit())
            .output()
            .map_err(|error| could_not_run(command, error))?;
        // A failure is reported, not returned, but one ended by an
        // interrupt still sets it.
        let _ = self.succeeded(command, output.status);
        Ok((output.status.success(), output.stdout))
    }

    fn input(&mut self, command: &mut Command, input: &[u8]) -> Result<Vec<u8>, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| could_not_run(command, error))?;
        let written = child.stdin.take().expect("stdin is piped").write_all(input);
        let output = child
            .wait_with_output()
            .map_err(|error| could_not_run(command, error))?;
        self.succeeded(command, output.status)?;
        written.map_err(|error| format!("could not write to {}: {error}", program(command)))?;
        Ok(output.stdout)
    }
}

fn program(command: &Command) -> String {
    command.get_program().to_string_lossy().into_owned()
}

fn could_not_run(command: &Command, error: io::Error) -> String {
    format!("could not run {}: {error}", program(command))
}

/// The repository's root, where the cargo commands run and the defaults are.
pub fn repository() -> PathBuf {
    // `tooling/xtask` is two folders below the root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask is in tooling/xtask")
        .to_owned()
}

/// Where xtask's own build output goes: `CARGO_TARGET_DIR` when it is set,
/// else `target` in the repository. xtask's logs are kept there, never in
/// the `--out` folder.
pub fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repository().join("target"))
}

/// The command line: starts both logs of `yard_log` as the product `xtask`
/// (`xtask.log` and `lifecycle.jsonl` in [`target_dir`]), parses the
/// arguments, runs the task, prints the result, and exits 1 on a failure, or
/// 130 when it was interrupted. A signal stops the run after the current
/// step instead of killing xtask, so the keychain is always cleaned up.
pub fn main() -> ExitCode {
    yard_log::start(Product::new(
        "xtask",
        env!("CARGO_PKG_VERSION"),
        "dev.yard.xtask",
    ));
    yard_log::open_files(&target_dir());
    // xtask has no startup of its own: each task is a group of its own.
    yard_log::startup_finished();
    let args = Args::parse();
    let interrupt = Interrupt::default();
    let handler = interrupt.clone();
    if let Err(error) = ctrlc::set_handler(move || handler.set()) {
        log::warn!("could not handle interrupts: {error}");
    }
    match args.task {
        Task::Bundle(System::Macos(options)) => {
            match bundle_macos(
                &options,
                &Secrets::from_env(),
                &interrupt,
                &mut SystemRunner {
                    interrupt: interrupt.clone(),
                },
            ) {
                Ok(bundled) => {
                    if let Some((_, digest)) = &bundled.zip {
                        println!("sha256 {digest}");
                    }
                    if let Some((_, digest)) = &bundled.dmg {
                        println!("dmg-sha256 {digest}");
                    }
                    println!("{}", bundled.app.display());
                    ExitCode::SUCCESS
                }
                Err(failure) => {
                    eprintln!("{failure}");
                    ExitCode::from(interrupt.exit_code())
                }
            }
        }
        Task::Bundle(System::Linux(options)) => {
            match bundle_linux(
                &options,
                std::env::consts::OS,
                &interrupt,
                &mut SystemRunner {
                    interrupt: interrupt.clone(),
                },
            ) {
                Ok(bundled) => {
                    let (archive, digest) = &bundled.archive;
                    println!("sha256 {digest}");
                    println!("{}", archive.display());
                    ExitCode::SUCCESS
                }
                Err(failure) => {
                    eprintln!("{failure}");
                    ExitCode::from(interrupt.exit_code())
                }
            }
        }
        Task::Bundle(System::Windows(options)) => {
            match bundle_windows(
                &options,
                &WindowsHost::from_env(),
                &interrupt,
                &mut SystemRunner {
                    interrupt: interrupt.clone(),
                },
            ) {
                Ok(bundled) => {
                    for (path, digest) in [&bundled.installer, &bundled.zip] {
                        println!("sha256 {digest}");
                        println!("{}", path.display());
                    }
                    ExitCode::SUCCESS
                }
                Err(failure) => {
                    eprintln!("{failure}");
                    ExitCode::from(interrupt.exit_code())
                }
            }
        }
    }
}

/// Makes `<out>/<name>.app` from the config, inside the group
/// `bundle-macos{config, dev, zip}`, printing each step's name on stdout. A
/// refusal is logged at `info`, a failure at `error` in the diagnostic log.
/// The secrets come from the environment through [`Secrets::from_env`].
/// Once `interrupt` is set, the run stops before its next step, and before
/// it publishes anything, with the reason `interrupted`. A run is an
/// interrupt when `interrupt` is set once its clean-up is done, even if
/// every step succeeded or a step failed of the same signal; it is logged at
/// `info`.
pub fn bundle_macos(
    options: &MacosArgs,
    secrets: &Secrets,
    interrupt: &Interrupt,
    runner: &mut dyn Runner,
) -> Result<Bundled, Failure> {
    let config = options
        .config
        .clone()
        .unwrap_or_else(|| repository().join(PRODUCTION_CONFIG));
    let _group = tracing::info_span!(
        "bundle-macos",
        config = %config.display(),
        dev = options.dev,
        zip = options.zip
    )
    .entered();
    let result = bundle(&config, options, secrets, interrupt, runner);
    log_outcome(&result, interrupt);
    result
}

/// Logs how a run ended, inside its group: a refusal at `info`, an
/// interrupt at `info`, a failure at `error` in the diagnostic log.
fn log_outcome<T>(result: &Result<T, Failure>, interrupt: &Interrupt) {
    if let Err(failure) = result {
        if failure.is_refusal() {
            tracing::info!(step = failure.step, "refused: {}", failure.reason);
        } else if interrupt.is_set() {
            tracing::info!(
                step = failure.step,
                reason = %failure.reason,
                "stopped: interrupted"
            );
        } else {
            log::error!("failed at {}: {}", failure.step, failure.reason);
        }
    }
}

/// Checks the arguments every system shares, and makes the config and the
/// `--out` folder absolute.
fn arguments(
    config: &Path,
    out: Option<&Path>,
    dev: bool,
    env: &[String],
) -> Result<(PathBuf, PathBuf), Failure> {
    let arguments = |reason: String| Failure::new("arguments", reason);
    env_pairs(env).map_err(arguments)?;
    if !dev && !env.is_empty() {
        return Err(arguments("--env requires --dev".into()));
    }
    let config = std::path::absolute(config).map_err(|error| arguments(error.to_string()))?;
    let out = out
        .map(Path::to_owned)
        .unwrap_or_else(|| repository().join(DEFAULT_OUT));
    let out = std::path::absolute(out).map_err(|error| arguments(error.to_string()))?;
    Ok((config, out))
}

/// The steps `stages` and `build`: the stages check, then yard built in
/// release with `PRODUCT_CONFIG` and the variables `envs`, production code
/// only unless `dev`. Returns the executable Cargo reports.
fn build(
    config: &Path,
    dev: bool,
    envs: &[(&str, &str)],
    interrupt: &Interrupt,
    runner: &mut dyn Runner,
) -> Result<PathBuf, Failure> {
    step("stages", interrupt)?;
    runner
        .run(
            cargo()
                .args(["test", "-p", "yard", "--test", "stages"])
                .env_remove("YARD_PROMOTE"),
        )
        .map_err(|reason| Failure::new("stages", reason))?;

    step("build", interrupt)?;
    let mut build = cargo();
    build
        .args(["build", "--release", "-p", "yard", "--bin", "yard"])
        .env("PRODUCT_CONFIG", config)
        .envs(envs.iter().copied());
    if !dev {
        build.arg("--no-default-features");
    }
    build.arg("--message-format=json-render-diagnostics");
    let report = runner
        .output(&mut build)
        .map_err(|reason| Failure::new("build", reason))?;
    let binary = reported_executable(&report).map_err(|reason| Failure::new("build", reason))?;
    tracing::debug!(binary = %binary.display(), "built");
    Ok(binary)
}

/// A run that succeeded is still an interrupt when a signal arrived during
/// its last step or its clean-up.
fn interrupted_at_end<T>(result: Result<T, Failure>, interrupt: &Interrupt) -> Result<T, Failure> {
    match result {
        Ok(_) if interrupt.is_set() => {
            tracing::info!("interrupted; stopping after the current step");
            Err(Failure::new("clean-up", INTERRUPTED))
        }
        result => result,
    }
}

fn bundle(
    config: &Path,
    options: &MacosArgs,
    secrets: &Secrets,
    interrupt: &Interrupt,
    runner: &mut dyn Runner,
) -> Result<Bundled, Failure> {
    tracing::debug!("arguments");
    let (config, out) = arguments(config, options.out.as_deref(), options.dev, &options.env)?;

    tracing::debug!("config");
    let product = read_config(&config, options)?;

    tracing::debug!("secrets");
    let signing =
        Signing::new(&product, secrets).map_err(|reason| Failure::new("secrets", reason))?;

    let binary = build(&config, options.dev, &[], interrupt, runner)?;

    let mut session = Session::default();
    let job = Job {
        product: &product,
        options,
        binary: &binary,
        out: &out,
        signing: &signing,
        interrupt,
    };
    // A panic, too, leaves no keychain behind: the clean-up runs, then the
    // panic goes on.
    let result = panic::catch_unwind(AssertUnwindSafe(|| signed(&job, &mut session, runner)));
    if signing.identity().is_some() {
        tracing::debug!("keychain clean-up");
    }
    let cleaned = session.clean_up(runner);
    let result = result.unwrap_or_else(|panic| panic::resume_unwind(panic));
    let result = match (result, cleaned) {
        (Ok(bundled), Ok(())) => Ok(bundled),
        (Err(failure), Ok(())) => Err(failure),
        (Err(mut failure), Err(reason)) => {
            failure.reason = format!("{}; then {reason}", failure.reason);
            Err(failure)
        }
        (Ok(_), Err(reason)) => Err(Failure::new("keychain", reason)),
    };
    interrupted_at_end(result, interrupt)
}

/// What one run bundles, and how.
struct Job<'a> {
    product: &'a BuildConfig,
    options: &'a MacosArgs,
    binary: &'a Path,
    out: &'a Path,
    signing: &'a Signing,
    interrupt: &'a Interrupt,
}

/// The steps from the keychain on, inside the session that
/// [`Session::clean_up`] undoes.
fn signed(job: &Job, session: &mut Session, runner: &mut dyn Runner) -> Result<Bundled, Failure> {
    let Job { out, interrupt, .. } = *job;
    if let Some(identity) = job.signing.identity() {
        step("keychain", interrupt)?;
        session
            .set_up(identity, runner)
            .map_err(|reason| Failure::new("keychain", reason))?;
    }

    step("icon", interrupt)?;
    staged(out, "icon", "macos", |staging| {
        in_temp(staging, job, session, runner)
    })
}

/// Runs `work` in a new [`Staging`] folder of `out`, made at the step
/// `step`, and closes it: what `work` published stays, or, when it fails,
/// every previous file is put back.
fn staged<T>(
    out: &Path,
    step: &'static str,
    system: &str,
    work: impl FnOnce(&mut Staging) -> Result<T, Failure>,
) -> Result<T, Failure> {
    fs::create_dir_all(out).map_err(|error| {
        Failure::new(step, format!("could not create {}: {error}", out.display()))
    })?;
    let mut staging = Staging::new(out, system).map_err(|error| {
        Failure::new(
            step,
            format!(
                "could not create a temporary folder in {}: {error}",
                out.display()
            ),
        )
    })?;
    match work(&mut staging) {
        Ok(made) => {
            staging.close(true);
            Ok(made)
        }
        Err(mut failure) => {
            for previous in staging.close(false) {
                failure.reason = format!(
                    "{}; previous file retained at {}",
                    failure.reason,
                    previous.display()
                );
            }
            Err(failure)
        }
    }
}

/// Prints a step's name and logs it, unless the run was interrupted.
fn step(name: &'static str, interrupt: &Interrupt) -> Result<(), Failure> {
    not_interrupted(name, interrupt)?;
    writeln!(io::stdout(), "{name}")
        .map_err(|error| Failure::new(name, format!("could not print the step: {error}")))?;
    tracing::debug!("{name}");
    Ok(())
}

/// `cargo` in the repository, without the secrets in its environment.
/// Stops the run at `step` once it was interrupted.
fn not_interrupted(step: &'static str, interrupt: &Interrupt) -> Result<(), Failure> {
    if interrupt.is_set() {
        tracing::info!("interrupted; stopping after the current step");
        return Err(Failure::new(step, INTERRUPTED));
    }
    Ok(())
}

fn cargo() -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(repository());
    for name in SECRET_VARIABLES {
        command.env_remove(name);
    }
    command
}

/// Reads the config through `yard_build_config`, and checks what bundling
/// adds: a `--dev` product apart from the production one, and names that are
/// single path components.
fn read_config(config: &Path, options: &MacosArgs) -> Result<BuildConfig, Failure> {
    let refuse = |reason: String| Failure::new("config", reason);
    let product = read_product(config, options.dev, false)?;
    if !is_path_component(&product.name) {
        return Err(refuse(format!(
            "{}: name must be a single path component",
            config.display()
        )));
    }
    if options.zip && !is_path_component(&product.version) {
        return Err(refuse(format!(
            "{}: version must be a single path component with --zip",
            config.display()
        )));
    }
    if product.macos.dmg && !is_path_component(&product.version) {
        return Err(refuse(format!(
            "{}: version must be a single path component with `dmg`",
            config.display()
        )));
    }
    Ok(product)
}

/// Reads the config through `yard_build_config`; with `dev`, refuses a
/// product that takes the production product's name, identifier, or URL
/// scheme, compared without case, and with `linux` also its Linux `<name>`.
fn read_product(config: &Path, dev: bool, linux: bool) -> Result<BuildConfig, Failure> {
    let refuse = |reason: String| Failure::new("config", reason);
    let product = yard_build_config::read(config).map_err(refuse)?;
    if dev {
        // A review bundle must never take the real product's name,
        // directories, identifier, or URL scheme; macOS compares these
        // without case.
        let production =
            yard_build_config::read(&repository().join(PRODUCTION_CONFIG)).map_err(refuse)?;
        let file_names = (
            yard_product::file_name(&product.name),
            yard_product::file_name(&production.name),
        );
        let mut pairs = vec![
            ("name", &product.name, &production.name),
            ("identifier", &product.identifier, &production.identifier),
            ("url_scheme", &product.url_scheme, &production.url_scheme),
        ];
        if linux {
            // Two names that differ only outside their letters and digits
            // share their Linux files and directories.
            pairs.push(("name", &file_names.0, &file_names.1));
        }
        for (key, value, real) in pairs {
            if value.to_lowercase() == real.to_lowercase() {
                return Err(refuse(format!(
                    "{}: --dev requires a `{key}` different from the production product's",
                    config.display()
                )));
            }
        }
    }
    Ok(product)
}

fn is_path_component(value: &str) -> bool {
    !value.contains('/') && value != "." && value != ".."
}

/// The `KEY=VALUE` pairs of `--env`, each KEY shell-style.
fn env_pairs(env: &[String]) -> Result<Vec<(&str, &str)>, String> {
    env.iter()
        .map(|pair| {
            pair.split_once('=')
                .filter(|(key, _)| is_shell_name(key))
                .ok_or_else(|| format!("--env {pair:?} requires KEY=VALUE with a shell-style KEY"))
        })
        .collect()
}

fn is_shell_name(key: &str) -> bool {
    let mut characters = key.chars();
    characters
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The `yard` executable that Cargo's JSON messages report, never a guessed
/// path, so a stale binary of another target is never bundled.
fn reported_executable(report: &[u8]) -> Result<PathBuf, String> {
    String::from_utf8_lossy(report)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message["reason"] == "compiler-artifact" && message["target"]["name"] == "yard"
        })
        .filter_map(|message| message["executable"].as_str().map(PathBuf::from))
        .next_back()
        .ok_or_else(|| "cargo reported no yard executable".to_owned())
}

/// The staging folder `<out>/.bundle-<system>.<pid>-<n>`, inside `out` so what
/// is made there moves into place by a rename. It is closed on every path, a
/// panic too: dropped without [`Staging::close`], it closes as after a
/// failure.
struct Staging {
    path: PathBuf,
    /// What [`Staging::publish`] replaced in `out`, in order.
    replaced: Vec<Replaced>,
    closed: bool,
}

/// A file of `out` that a publish replaced, so a failure can undo it.
struct Replaced {
    target: PathBuf,
    /// Where the file that `target` held waits, in the staging folder.
    previous: Option<PathBuf>,
    /// Whether the new file is at `target`.
    moved: bool,
}

impl Staging {
    fn new(out: &Path, system: &str) -> io::Result<Self> {
        for attempt in 0.. {
            let path = out.join(format!(".bundle-{system}.{}-{attempt}", std::process::id()));
            match fs::create_dir(&path) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                result => {
                    return result.map(|()| Self {
                        path,
                        replaced: Vec::new(),
                        closed: false,
                    });
                }
            }
        }
        unreachable!("an unbounded range ends")
    }

    /// Moves `from` to `to`, keeping what `to` held in the staging folder
    /// until the folder closes.
    fn publish(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        let previous = if exists(to) {
            let previous = self.path.join(format!("previous-{}", self.replaced.len()));
            rename(to, &previous)?;
            Some(previous)
        } else {
            None
        };
        self.replaced.push(Replaced {
            target: to.to_owned(),
            previous,
            moved: false,
        });
        rename(from, to)?;
        if let Some(replaced) = self.replaced.last_mut() {
            replaced.moved = true;
        }
        Ok(())
    }

    /// Removes the folder. After a failure (`published` false), first takes
    /// back what was published, last first, and puts each previous file
    /// back; a previous file that cannot be put back keeps the folder, and
    /// is returned.
    fn close(&mut self, published: bool) -> Vec<PathBuf> {
        self.closed = true;
        let mut retained = Vec::new();
        if !published {
            let replaced = std::mem::take(&mut self.replaced);
            for (index, replaced) in replaced.into_iter().enumerate().rev() {
                let target = &replaced.target;
                if replaced.moved
                    && let Err(reason) =
                        rename(target, &self.path.join(format!("unpublished-{index}")))
                {
                    log::warn!("could not take back {}: {reason}", target.display());
                }
                if let Some(previous) = replaced.previous {
                    if !exists(target) && rename(&previous, target).is_ok() {
                        tracing::debug!(target = %target.display(), "previous file put back");
                    } else {
                        log::warn!(
                            "previous {} retained at {}",
                            target.display(),
                            previous.display()
                        );
                        retained.push(previous);
                    }
                }
            }
        }
        if retained.is_empty()
            && let Err(error) = fs::remove_dir_all(&self.path)
        {
            log::warn!("could not remove {}: {error}", self.path.display());
        }
        retained
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.closed {
            self.close(false);
        }
    }
}

/// The steps that work in the staging folder: the icon, the bundle, the
/// signature, the notarization, the zip, and the disk image; then, unless
/// the run was interrupted, the app, the zip, and the disk image are
/// published together.
fn in_temp(
    staging: &mut Staging,
    job: &Job,
    session: &Session,
    runner: &mut dyn Runner,
) -> Result<Bundled, Failure> {
    let Job {
        product,
        options,
        binary,
        out,
        signing,
        interrupt,
    } = *job;
    let destination = &out.join(format!("{}.app", product.name));
    let temp = staging.path.clone();
    let temp = temp.as_path();
    let icns =
        make_icns(temp, &product.icon, runner).map_err(|reason| Failure::new("icon", reason))?;

    step("bundle", interrupt)?;
    let bundle = lay_out(product, options, binary, temp, Some(&icns)).map_err(|error| {
        Failure::new("bundle", format!("could not lay out the bundle: {error}"))
    })?;
    runner
        .run(
            Command::new("plutil")
                .arg("-lint")
                .arg(bundle.join("Contents/Info.plist")),
        )
        .map_err(|reason| Failure::new("bundle", reason))?;

    step("sign", interrupt)?;
    let sign = |reason| Failure::new("sign", reason);
    match signing.identity() {
        // The entitlements apply ad hoc too, as in Zed's local bundles.
        None => {
            let mut codesign = Command::new("codesign");
            codesign.args(["--force", "--deep"]);
            if let Some(file) = &product.macos.entitlements {
                codesign.arg("--entitlements").arg(file);
            }
            runner
                .run(codesign.args(["--sign", "-"]).arg(&bundle))
                .map_err(sign)?
        }
        Some(identity) => {
            // The binary, then the app, as Zed's `script/bundle-mac` does.
            let suffix = if options.dev { "-bin" } else { "" };
            let binary = bundle
                .join("Contents/MacOS")
                .join(format!("{}{suffix}", product.name));
            runner
                .run(&mut identity.codesign(&binary, true, None))
                .map_err(sign)?;
            runner
                .run(&mut identity.codesign(&bundle, true, None))
                .map_err(sign)?;
        }
    }
    runner
        .run(
            Command::new("codesign")
                .args(["--verify", "--deep", "--strict"])
                .arg(&bundle),
        )
        .map_err(sign)?;

    if let Some(identity) = signing.identity().filter(|identity| identity.notarizes()) {
        step("notarize", interrupt)?;
        let notarize = |reason| Failure::new("notarize", reason);
        let submitted = temp.join("notarize.zip");
        runner
            .run(
                Command::new("ditto")
                    .args(["-c", "-k", "--keepParent"])
                    .arg(&bundle)
                    .arg(&submitted),
            )
            .map_err(notarize)?;
        session
            .notarize(identity, &submitted, runner)
            .map_err(notarize)?;
        fs::remove_file(&submitted).map_err(|error| {
            notarize(format!("could not remove {}: {error}", submitted.display()))
        })?;
        step("staple", interrupt)?;
        staple(&bundle, runner).map_err(|reason| Failure::new("staple", reason))?;
    }

    let zip = if options.zip {
        step("zip", interrupt)?;
        let zip = |reason| Failure::new("zip", reason);
        let staged = temp.join("release.zip");
        runner
            .run(
                Command::new("ditto")
                    .args(["-c", "-k", "--keepParent"])
                    .arg(&bundle)
                    .arg(&staged),
            )
            .map_err(zip)?;
        // The digest comes from the staged zip, so no published zip lacks
        // one.
        let digest = sha256(&["shasum", "-a", "256"], &staged, runner).map_err(zip)?;
        Some((staged, digest))
    } else {
        None
    };
    let dmg = if product.macos.dmg {
        step("dmg", interrupt)?;
        let dmg = |reason| Failure::new("dmg", reason);
        let image = disk_image(temp, product, &bundle, runner).map_err(dmg)?;
        match signing.identity() {
            None => tracing::debug!("the app is signed ad hoc: the disk image is not signed"),
            Some(identity) => {
                runner
                    .run(&mut identity.codesign(
                        &image,
                        false,
                        Some(&format!("{}.dmg", product.identifier)),
                    ))
                    .map_err(dmg)?;
                if identity.notarizes() {
                    step("notarize", interrupt)?;
                    session
                        .notarize(identity, &image, runner)
                        .map_err(|reason| Failure::new("notarize", reason))?;
                    step("staple", interrupt)?;
                    staple(&image, runner).map_err(|reason| Failure::new("staple", reason))?;
                }
            }
        }
        let digest = sha256(&["shasum", "-a", "256"], &image, runner).map_err(dmg)?;
        Some((image, digest))
    } else {
        None
    };

    // Everything is published together, or nothing is: a failure takes back
    // what was moved and puts the previous files back.
    not_interrupted("publish", interrupt)?;
    tracing::debug!(destination = %destination.display(), "publish");
    staging
        .publish(&bundle, destination)
        .map_err(|reason| Failure::new("bundle", reason))?;
    let zip = match zip {
        Some((staged, digest)) => {
            let archive = out.join(format!("{}-{}.zip", product.name, product.version));
            staging
                .publish(&staged, &archive)
                .map_err(|reason| Failure::new("zip", reason))?;
            tracing::debug!(archive = %archive.display(), sha256 = digest, "zipped");
            Some((archive, digest))
        }
        None => None,
    };
    let dmg = match dmg {
        Some((image, digest)) => {
            let published = out.join(format!("{}-{}.dmg", product.name, product.version));
            staging
                .publish(&image, &published)
                .map_err(|reason| Failure::new("dmg", reason))?;
            tracing::debug!(dmg = %published.display(), sha256 = digest, "disk image");
            Some((published, digest))
        }
        None => None,
    };
    Ok(Bundled {
        app: destination.to_owned(),
        zip,
        dmg,
    })
}

/// Makes `<temp>/release.dmg` holding the app and a link to
/// `/Applications`, as Zed's `script/bundle-mac` does.
fn disk_image(
    temp: &Path,
    product: &BuildConfig,
    app: &Path,
    runner: &mut dyn Runner,
) -> Result<PathBuf, String> {
    let staging = temp.join("dmg");
    fs::create_dir(&staging)
        .map_err(|error| format!("could not create {}: {error}", staging.display()))?;
    runner.run(
        Command::new("ditto")
            .arg(app)
            .arg(staging.join(format!("{}.app", product.name))),
    )?;
    symlink("/Applications", &staging.join("Applications"))
        .map_err(|error| format!("could not link /Applications: {error}"))?;
    let image = temp.join("release.dmg");
    runner.run(
        Command::new("hdiutil")
            .args(["create", "-volname", &product.name, "-srcfolder"])
            .arg(&staging)
            .args(["-ov", "-format", "UDZO"])
            .arg(&image),
    )?;
    Ok(image)
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_: &str, _: &Path) -> io::Result<()> {
    Err(io::Error::other("symbolic links need a Unix system"))
}

fn staple(path: &Path, runner: &mut dyn Runner) -> Result<(), String> {
    runner.run(Command::new("xcrun").args(["stapler", "staple"]).arg(path))
}

/// Makes `<temp>/product.icns` from the PNG, at 16 to 512 pixels, each at 1x
/// and 2x.
fn make_icns(temp: &Path, icon: &Path, runner: &mut dyn Runner) -> Result<PathBuf, String> {
    let iconset = temp.join("product.iconset");
    fs::create_dir(&iconset)
        .map_err(|error| format!("could not create {}: {error}", iconset.display()))?;
    for size in [16, 32, 128, 256, 512] {
        for (pixels, suffix) in [(size, ""), (size * 2, "@2x")] {
            let pixels = pixels.to_string();
            runner.output(
                Command::new("sips")
                    .args(["-z", &pixels, &pixels])
                    .arg(icon)
                    .arg("--out")
                    .arg(iconset.join(format!("icon_{size}x{size}{suffix}.png"))),
            )?;
        }
    }
    let icns = temp.join("product.icns");
    runner.run(
        Command::new("iconutil")
            .args(["-c", "icns"])
            .arg(&iconset)
            .arg("-o")
            .arg(&icns),
    )?;
    Ok(icns)
}

/// The SHA-256 of `file` in hex, from `command` (such as `shasum -a 256`)
/// run on it.
fn sha256(command: &[&str], file: &Path, runner: &mut dyn Runner) -> Result<String, String> {
    let output = runner.output(Command::new(command[0]).args(&command[1..]).arg(file))?;
    String::from_utf8_lossy(&output)
        .split_whitespace()
        .next()
        .filter(|digest| digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_owned)
        .ok_or_else(|| format!("{} printed no SHA-256", command[0]))
}

/// Lays out `<folder>/<name>.app`: the binary (behind a wrapper running `dev`
/// with `--dev`), the icon when `icns` is given, and `Info.plist`.
pub fn lay_out(
    product: &BuildConfig,
    options: &MacosArgs,
    binary: &Path,
    folder: &Path,
    icns: Option<&Path>,
) -> io::Result<PathBuf> {
    let bundle = folder.join(format!("{}.app", product.name));
    let contents = bundle.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources"))?;
    let executable = contents.join("MacOS").join(&product.name);
    if options.dev {
        fs::copy(
            binary,
            executable.with_file_name(format!("{}-bin", product.name)),
        )?;
        fs::write(&executable, WRAPPER)?;
        make_executable(&executable)?;
    } else {
        fs::copy(binary, &executable)?;
    }
    if let Some(icns) = icns {
        fs::rename(
            icns,
            contents
                .join("Resources")
                .join(format!("{}.icns", product.name)),
        )?;
    }
    let env = env_pairs(&options.env).map_err(io::Error::other)?;
    fs::write(contents.join("Info.plist"), info_plist(product, &env))?;
    Ok(bundle)
}

/// The executable of a review bundle: it runs the binary beside it with
/// `dev`.
pub const WRAPPER: &str = "#!/bin/sh\nexec \"$0-bin\" dev \"$@\"\n";

fn make_executable(path: &Path) -> io::Result<()> {
    set_mode(path, 0o755)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

/// The text of `Info.plist`, every value XML-escaped.
pub fn info_plist(product: &BuildConfig, env: &[(&str, &str)]) -> String {
    let name = xml_escape(&product.name);
    let version = xml_escape(&product.version);
    let identifier = xml_escape(&product.identifier);
    let scheme = xml_escape(&product.url_scheme);
    let mut text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>{name}</string>
    <key>CFBundleDisplayName</key><string>{name}</string>
    <key>CFBundleIdentifier</key><string>{identifier}</string>
    <key>CFBundleShortVersionString</key><string>{version}</string>
    <key>CFBundleVersion</key><string>{version}</string>
    <key>CFBundleExecutable</key><string>{name}</string>
    <key>CFBundleIconFile</key><string>{name}.icns</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>{identifier}</string>
            <key>CFBundleURLSchemes</key><array><string>{scheme}</string></array>
        </dict>
    </array>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
"#
    );
    if !env.is_empty() {
        text.push_str("    <key>LSEnvironment</key>\n    <dict>\n");
        for (key, value) in env {
            text.push_str(&format!(
                "        <key>{}</key><string>{}</string>\n",
                xml_escape(key),
                xml_escape(value)
            ));
        }
        text.push_str("    </dict>\n");
    }
    text.push_str("</dict>\n</plist>\n");
    text
}

/// Escapes `&`, `<`, `>`, and a carriage return, which XML would otherwise
/// read as a line break.
pub fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\r' => escaped.push_str("&#13;"),
            c => escaped.push(c),
        }
    }
    escaped
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn rename(from: &Path, to: &Path) -> Result<(), String> {
    fs::rename(from, to).map_err(|error| {
        format!(
            "could not move {} to {}: {error}",
            from.display(),
            to.display()
        )
    })
}
