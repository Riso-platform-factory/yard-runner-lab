//! Signing with the developer's identity: the secrets from the environment,
//! the temporary keychain, and notarization.
//!
//! A secret (the certificate, its password, the notarization key, and the
//! temporary keychain's password) never goes into a command's arguments, a
//! log line, or an error. `security` reads the passwords from its standard
//! input in interactive mode (`security -i`); the certificate and the key are
//! written to files in a folder only the user can read, removed at the end.

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use yard_build_config::BuildConfig;

use super::Runner;

/// Every variable that holds a secret of signing or notarization, on any
/// system. None of them reaches a `cargo` command.
pub const SECRET_VARIABLES: [&str; 7] = [
    "MACOS_CERTIFICATE",
    "MACOS_CERTIFICATE_PASSWORD",
    "APPLE_NOTARIZATION_KEY",
    "APPLE_NOTARIZATION_KEY_ID",
    "APPLE_NOTARIZATION_ISSUER_ID",
    "WINDOWS_CERTIFICATE",
    "WINDOWS_CERTIFICATE_PASSWORD",
];

/// The secrets of signing and notarization, from the environment only.
#[derive(Clone, Default)]
pub struct Secrets {
    /// `MACOS_CERTIFICATE`: the identity's `.p12`, base64.
    pub certificate: Option<String>,
    /// `MACOS_CERTIFICATE_PASSWORD`.
    pub certificate_password: Option<String>,
    /// `APPLE_NOTARIZATION_KEY`: the App Store Connect API key's `.p8`,
    /// base64.
    pub notarization_key: Option<String>,
    /// `APPLE_NOTARIZATION_KEY_ID`.
    pub notarization_key_id: Option<String>,
    /// `APPLE_NOTARIZATION_ISSUER_ID`.
    pub notarization_issuer_id: Option<String>,
}

impl Secrets {
    /// Reads the variables; an empty one counts as absent.
    pub fn from_env() -> Self {
        let var = |name| std::env::var(name).ok().filter(|value| !value.is_empty());
        Self {
            certificate: var("MACOS_CERTIFICATE"),
            certificate_password: var("MACOS_CERTIFICATE_PASSWORD"),
            notarization_key: var("APPLE_NOTARIZATION_KEY"),
            notarization_key_id: var("APPLE_NOTARIZATION_KEY_ID"),
            notarization_issuer_id: var("APPLE_NOTARIZATION_ISSUER_ID"),
        }
    }
}

/// Says which secrets are set, never their values.
impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("certificate", &self.certificate.is_some())
            .field("certificate_password", &self.certificate_password.is_some())
            .field("notarization_key", &self.notarization_key.is_some())
            .field("notarization_key_id", &self.notarization_key_id.is_some())
            .field(
                "notarization_issuer_id",
                &self.notarization_issuer_id.is_some(),
            )
            .finish()
    }
}

/// How the app is signed, checked before anything is built.
pub enum Signing {
    /// No `[release.macos.signing]`: signed ad hoc, as without signing.
    AdHoc,
    Identity(Identity),
}

pub struct Identity {
    pub name: String,
    pub entitlements: Option<PathBuf>,
    /// The `.p12` and its password, when `MACOS_CERTIFICATE` is set.
    certificate: Option<(Vec<u8>, String)>,
    notarization: Option<Notarization>,
}

struct Notarization {
    key: Vec<u8>,
    key_id: String,
    issuer_id: String,
}

impl Signing {
    /// Checks that the secrets the config needs are set and readable.
    pub fn new(product: &BuildConfig, secrets: &Secrets) -> Result<Self, String> {
        let Some(signing) = &product.macos.signing else {
            tracing::debug!("no signing identity: signed ad hoc");
            return Ok(Self::AdHoc);
        };
        let certificate = match &secrets.certificate {
            None => None,
            Some(certificate) => {
                let password = secrets.certificate_password.clone().ok_or(
                    "MACOS_CERTIFICATE is set without MACOS_CERTIFICATE_PASSWORD in the environment",
                )?;
                one_word(&password)
                    .map_err(|reason| format!("MACOS_CERTIFICATE_PASSWORD {reason}"))?;
                let p12 =
                    base64_decode(certificate).ok_or("MACOS_CERTIFICATE is not valid base64")?;
                Some((p12, password))
            }
        };
        let notarization = if signing.notarize {
            let missing: Vec<&str> = [
                ("APPLE_NOTARIZATION_KEY", &secrets.notarization_key),
                ("APPLE_NOTARIZATION_KEY_ID", &secrets.notarization_key_id),
                (
                    "APPLE_NOTARIZATION_ISSUER_ID",
                    &secrets.notarization_issuer_id,
                ),
            ]
            .into_iter()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name)
            .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "notarize = true needs {} in the environment",
                    missing.join(", ")
                ));
            }
            let key = base64_decode(secrets.notarization_key.as_deref().unwrap_or_default())
                .ok_or("APPLE_NOTARIZATION_KEY is not valid base64")?;
            Some(Notarization {
                key,
                key_id: secrets.notarization_key_id.clone().unwrap_or_default(),
                issuer_id: secrets.notarization_issuer_id.clone().unwrap_or_default(),
            })
        } else {
            None
        };
        tracing::debug!(
            identity = signing.identity,
            certificate = certificate.is_some(),
            notarize = notarization.is_some(),
            "signing identity"
        );
        Ok(Self::Identity(Identity {
            name: signing.identity.clone(),
            entitlements: product.macos.entitlements.clone(),
            certificate,
            notarization,
        }))
    }

    pub fn identity(&self) -> Option<&Identity> {
        match self {
            Self::AdHoc => None,
            Self::Identity(identity) => Some(identity),
        }
    }
}

impl Identity {
    pub fn notarizes(&self) -> bool {
        self.notarization.is_some()
    }

    /// `codesign` with the identity, the hardened runtime, a timestamp, the
    /// entitlements when `entitlements`, and the signature's identifier when
    /// one is given.
    pub fn codesign(&self, path: &Path, entitlements: bool, identifier: Option<&str>) -> Command {
        let mut command = Command::new("codesign");
        command.args(["--force", "--options", "runtime", "--timestamp"]);
        if entitlements && let Some(file) = &self.entitlements {
            command.arg("--entitlements").arg(file);
        }
        if let Some(identifier) = identifier {
            command.args(["--identifier", identifier]);
        }
        command.arg("--sign").arg(&self.name).arg(path);
        command
    }
}

/// What the run set up for signing, undone by [`Session::clean_up`] on every
/// path: a private folder, and the temporary keychain, which `security
/// delete-keychain` also takes out of the search list.
#[derive(Default)]
pub struct Session {
    private: Option<PathBuf>,
    keychain: Option<PathBuf>,
    /// Whether `create-keychain` ran, even if it did not finish.
    created: bool,
    key_file: Option<PathBuf>,
}

impl Session {
    /// With the certificate, makes the temporary keychain and puts it first
    /// in the user's search list; without it, uses the existing keychains,
    /// read only. Writes the notarization key to a private file.
    pub fn set_up(&mut self, identity: &Identity, runner: &mut dyn Runner) -> Result<(), String> {
        if identity.certificate.is_none() && identity.notarization.is_none() {
            tracing::debug!("the identity comes from the existing keychains");
            return Ok(());
        }
        let private = private_folder()
            .map_err(|error| format!("could not create a private temporary folder: {error}"))?;
        self.private = Some(private.clone());
        if let Some(notarization) = &identity.notarization {
            let key_file = private.join("notarization-key.p8");
            write_private(&key_file, &notarization.key)
                .map_err(|error| format!("could not write the notarization key: {error}"))?;
            self.key_file = Some(key_file);
        }
        let Some((p12, password)) = &identity.certificate else {
            tracing::debug!("the identity comes from the existing keychains");
            return Ok(());
        };

        let keychain = private.join("signing.keychain");
        let keychain_password = random_password()
            .map_err(|error| format!("could not make the keychain's password: {error}"))?;
        let path = keychain.to_string_lossy();
        let path = Word::Value("the keychain's path (in TMPDIR)", &path);
        let secret = Word::Value("the keychain's password", &keychain_password);
        let create = command_line(&[Word::Literal("create-keychain -p"), secret, path])?;
        self.keychain = Some(keychain.clone());
        // Marked before it runs, so a `create-keychain` that is stopped
        // half-way is still deleted.
        self.created = true;
        interactive(runner, &create)?;
        tracing::debug!(keychain = %keychain.display(), "keychain created");
        // The search list is read and set at once, so a change made by
        // something else in between is as unlikely as can be.
        let search_list = read_search_list(runner)?;
        if search_list.is_empty() {
            // Setting a list without them would drop the user's keychains.
            return Err("security list-keychains printed no keychain".into());
        }
        runner.run(
            security()
                .args(["list-keychains", "-d", "user", "-s"])
                .arg(&keychain)
                .args(&search_list),
        )?;
        tracing::debug!(
            keychains = search_list.len(),
            "keychain first in the search list"
        );
        // Without `-t`, the keychain does not lock itself during a long
        // notarization.
        runner.run(security().arg("set-keychain-settings").arg(&keychain))?;
        interactive(
            runner,
            &command_line(&[Word::Literal("unlock-keychain -p"), secret, path])?,
        )?;
        let certificate = private.join("certificate.p12");
        let import = command_line(&[
            Word::Literal("import"),
            Word::Value(
                "the certificate's path (in TMPDIR)",
                &certificate.to_string_lossy(),
            ),
            Word::Literal("-k"),
            path,
            Word::Literal("-f pkcs12 -P"),
            Word::Value("MACOS_CERTIFICATE_PASSWORD", password),
            Word::Literal("-T /usr/bin/codesign"),
        ])?;
        write_private(&certificate, p12)
            .map_err(|error| format!("could not write the certificate: {error}"))?;
        let imported = interactive(runner, &import);
        let removed = fs::remove_file(&certificate);
        imported?;
        removed.map_err(|error| format!("could not remove the certificate's file: {error}"))?;
        interactive(
            runner,
            &command_line(&[
                Word::Literal("set-key-partition-list -S apple-tool:,apple:,codesign: -s -k"),
                secret,
                path,
            ])?,
        )?;
        tracing::debug!("certificate imported");
        Ok(())
    }

    /// Notarizes `file` (a zip of the app, or the disk image) and waits; on
    /// a rejection prints the notarization log and fails.
    pub fn notarize(
        &self,
        identity: &Identity,
        file: &Path,
        runner: &mut dyn Runner,
    ) -> Result<(), String> {
        let (Some(notarization), Some(key_file)) = (&identity.notarization, &self.key_file) else {
            return Err("notarization is not set up".into());
        };
        let notarytool = |subcommand: &str| {
            let mut command = Command::new("xcrun");
            command.args(["notarytool", subcommand]);
            command
        };
        let credentials = |command: &mut Command| {
            command
                .arg("--key")
                .arg(key_file)
                .args(["--key-id", &notarization.key_id])
                .args(["--issuer", &notarization.issuer_id]);
        };
        let mut submit = notarytool("submit");
        submit.arg(file);
        credentials(&mut submit);
        submit.args(["--wait", "--output-format", "json"]);
        // A rejection may end `notarytool` with a failure; its report still
        // names the submission whose log is printed.
        let (succeeded, report) = runner.report(&mut submit)?;
        let report: serde_json::Value = serde_json::from_slice(&report).map_err(|error| {
            if succeeded {
                format!("notarytool printed no JSON: {error}")
            } else {
                "notarytool submit failed and printed no report".to_owned()
            }
        })?;
        let status = report["status"].as_str().unwrap_or("unknown");
        let id = report["id"].as_str().unwrap_or_default();
        tracing::debug!(file = %file.display(), id, status, "notarization");
        if status == "Accepted" {
            return Ok(());
        }
        if !id.is_empty() {
            let mut log = notarytool("log");
            log.arg(id);
            credentials(&mut log);
            if let Err(reason) = runner.run(&mut log) {
                log::warn!("could not print the notarization log: {reason}");
            }
        }
        Err(format!(
            "notarization of {} ended {status}; its log is above",
            file.display()
        ))
    }

    /// Deletes the temporary keychain, which also takes it out of the
    /// user's search list and leaves every other keychain there, and removes
    /// the private folder, whatever happened before. Each step is logged at
    /// `debug`, a failed one at `error` in the diagnostic log; a keychain
    /// that cannot be deleted is named with the command that deletes it.
    pub fn clean_up(&mut self, runner: &mut dyn Runner) -> Result<(), String> {
        let mut failures = Vec::new();
        if let Some(keychain) = self.keychain.take()
            && std::mem::take(&mut self.created)
        {
            match runner.run(security().arg("delete-keychain").arg(&keychain)) {
                Ok(()) => tracing::debug!(keychain = %keychain.display(), "keychain deleted"),
                // `create-keychain` was stopped before it made the keychain.
                Err(_) if !keychain_exists(&keychain) => {
                    tracing::debug!(keychain = %keychain.display(), "keychain not found")
                }
                Err(reason) => failures.push(format!(
                    "could not delete the keychain {}: {reason}; delete it with: \
                     security delete-keychain {}",
                    keychain.display(),
                    shell_quote(&keychain.to_string_lossy())
                )),
            }
        }
        self.key_file = None;
        if let Some(private) = self.private.take() {
            match fs::remove_dir_all(&private) {
                Ok(()) => tracing::debug!("private folder removed"),
                Err(error) => {
                    failures.push(format!("could not remove {}: {error}", private.display()))
                }
            }
        }
        for failure in &failures {
            log::error!("{failure}");
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

/// The user's keychain search list, as `security list-keychains` prints it.
fn read_search_list(runner: &mut dyn Runner) -> Result<Vec<String>, String> {
    let list = runner.output(security().args(["list-keychains", "-d", "user"]))?;
    parse_search_list(&list)
}

/// Whether the keychain's file is on disk, under its name or with the `-db`
/// that `security` adds.
fn keychain_exists(keychain: &Path) -> bool {
    let mut db = keychain.as_os_str().to_owned();
    db.push("-db");
    keychain.exists() || Path::new(&db).exists()
}

/// `security`, in a process group of its own, so a Ctrl-C in the terminal
/// reaches only xtask, which stops between steps, and never kills the
/// keychain's set-up or clean-up half-way.
fn security() -> Command {
    let mut command = Command::new("security");
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
}

/// Refuses a word that `security -i` would read as more than one line or cut
/// short: one with a line break or a NUL. Says so without the word.
fn one_word(word: &str) -> Result<(), String> {
    if word.contains(['\n', '\r', '\0']) {
        Err("holds a line break or a NUL, which `security -i` cannot take".into())
    } else {
        Ok(())
    }
}

/// The longest line `security -i` reads, with its line break: a longer one
/// is cut, and its rest read as another command.
const LINE_LIMIT: usize = 4096;

/// A word of a `security -i` command: as it is, or a value, quoted, and
/// named in an error without its content.
#[derive(Clone, Copy)]
enum Word<'a> {
    Literal(&'a str),
    Value(&'static str, &'a str),
}

/// The command line of `security -i`, once every value is one word and the
/// line fits; an error names the value at fault, never its content.
fn command_line(words: &[Word]) -> Result<String, String> {
    let mut line = Vec::new();
    for word in words {
        match *word {
            Word::Literal(text) => line.push(text.to_owned()),
            Word::Value(name, value) => {
                one_word(value).map_err(|reason| format!("{name} {reason}"))?;
                line.push(quote(value));
            }
        }
    }
    let line = line.join(" ");
    if line.len() + 1 >= LINE_LIMIT {
        let longest = words
            .iter()
            .filter_map(|word| match *word {
                Word::Value(name, value) => Some((value.len(), name)),
                Word::Literal(_) => None,
            })
            .max()
            .map_or("", |(_, name)| name);
        return Err(format!(
            "a `security -i` command must be shorter than {LINE_LIMIT} bytes; \
             its longest value is {longest}"
        ));
    }
    Ok(line)
}

/// Runs one command of `security`'s interactive mode, which reads it from
/// standard input, so the passwords in it are in no process's arguments.
fn interactive(runner: &mut dyn Runner, line: &str) -> Result<(), String> {
    let mut command = security();
    command.arg("-i");
    runner.input(&mut command, format!("{line}\n").as_bytes())?;
    Ok(())
}

/// A word of `security -i`, in double quotes.
fn quote(word: &str) -> String {
    format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A word of a POSIX shell, in single quotes.
pub(super) fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The paths `security list-keychains` prints, each verbatim in double
/// quotes on a line of its own. A line that is not one whole quoted path,
/// such as a path with a line break or a double quote in it, is refused, so
/// the list is never set from a misread one.
fn parse_search_list(output: &[u8]) -> Result<Vec<String>, String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.strip_prefix('"')
                .and_then(|line| line.strip_suffix('"'))
                .filter(|path| !path.is_empty() && !path.contains('"'))
                .map(str::to_owned)
                .ok_or_else(|| {
                    "security list-keychains printed a line that is not one quoted path, \
                     as a keychain path with a line break or a double quote prints; \
                     the search list is left as it is"
                        .to_owned()
                })
        })
        .collect()
}

/// 32 random bytes, in hex.
#[cfg(unix)]
fn random_password() -> io::Result<String> {
    use std::io::Read as _;
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The temporary keychain is macOS's only.
#[cfg(not(unix))]
fn random_password() -> io::Result<String> {
    Err(io::Error::other("a temporary keychain needs macOS"))
}

/// A new folder under the system's temporary folder that only the user can
/// read, under a name this process never used before.
fn private_folder() -> io::Result<PathBuf> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let base = std::env::temp_dir();
    loop {
        let attempt = NEXT.fetch_add(1, Ordering::Relaxed);
        let folder = base.join(format!("yard-signing.{}-{attempt}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&folder) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            result => return result.map(|()| folder),
        }
    }
}

/// Writes a file only the user can read.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
}

/// Decodes standard base64, ignoring whitespace; `None` when it is not.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    let mut padding = 0;
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            _ => return None,
        };
        if padding > 0 {
            return None;
        }
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
        }
    }
    (padding <= 2 && bits < 6 && !bytes.is_empty()).then_some(bytes)
}
