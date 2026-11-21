//! What the tests of `bundle macos` share: temporary folders, the configs
//! under test, a runner that records commands instead of running them, and a
//! reader of the `Info.plist` text.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A folder of its own under the system's temporary folder, removed when it
/// drops.
pub struct TempDir(PathBuf);
impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn tempdir() -> TempDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "xtask-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path.canonicalize().unwrap())
}

/// A config under test of `yard_build_config`.
pub fn under_test(name: &str) -> PathBuf {
    xtask::repository()
        .join("crates/yard_build_config/tests/configs")
        .join(name)
        .join("product.toml")
        .canonicalize()
        .unwrap()
}

/// Writes `<folder>/product.toml` with these values and the icon of `plain`.
pub fn config_in(
    folder: &Path,
    name: &str,
    version: &str,
    identifier: &str,
    scheme: &str,
) -> PathBuf {
    std::fs::copy(
        under_test("plain").with_file_name("icon.png"),
        folder.join("icon.png"),
    )
    .unwrap();
    let path = folder.join("product.toml");
    std::fs::write(
        &path,
        format!(
            "name = {name:?}\nversion = {version:?}\nidentifier = {identifier:?}\n\
             url_scheme = {scheme:?}\nicon = \"icon.png\"\n"
        ),
    )
    .unwrap();
    path
}

/// A command as the runner was given it.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The program's file name without `.exe`.
    pub program: String,
    /// The program as it was given, a full path when it is one.
    pub path: PathBuf,
    pub args: Vec<String>,
    /// Set variables with their value, removed ones with `None`.
    pub envs: Vec<(String, Option<String>)>,
    pub dir: Option<PathBuf>,
    /// What was written to its stdin, for `Runner::input`.
    pub input: Option<String>,
}

/// The SHA-256 the fake `shasum` prints.
pub const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// What runs on the bundle being verified.
pub type Hook = Box<dyn FnMut(&Path)>;

/// What runs as a command starts.
pub type CommandHook = Box<dyn FnMut(&Recorded)>;

/// The banner the fake `ISCC.exe /?` prints, as Inno Setup 6.4.3 does.
pub const ISCC_BANNER: &str = "Inno Setup 6.4.3 Command-Line Compiler\r\nCopyright (C) 1997-2025 Jordan Russell. All rights reserved.\r\n";

/// The search list the fake `security list-keychains` prints.
pub const LOGIN_KEYCHAIN: &str = "/Users/test/Library/Keychains/login.keychain-db";

/// Records every command and does only what the next step needs: `cargo
/// build` reports `binary`, `iconutil`, `ditto`, `hdiutil`, `tar -czf`,
/// Windows' `tar -a -c -f`, and `ISCC` write their output file, `shasum`,
/// `sha256sum`, and `certutil` print [`DIGEST`], `security list-keychains`
/// prints and sets `search_list` (at first [`LOGIN_KEYCHAIN`]),
/// `create-keychain` writes the keychain's file as `<path>-db`,
/// `delete-keychain` takes the keychain out of `search_list`, and
/// `notarytool submit` reports `notarization` and fails unless it is
/// `Accepted`, and `ISCC.exe /?` prints `iscc_banner`.
/// `on_command` sees every command as it starts. `fail` and `also_fail` make
/// a program, or a program and its first argument (such as `security
/// delete-keychain`), fail, after `on_fail` runs; `on_verify` runs on the
/// bundle when `codesign --verify` is asked for.
pub struct FakeRunner {
    pub commands: Vec<Recorded>,
    pub binary: PathBuf,
    pub fail: Option<&'static str>,
    pub also_fail: Option<&'static str>,
    /// What runs when a command fails, as a signal would arrive during it.
    pub on_fail: Option<Box<dyn FnMut()>>,
    /// What runs as each command starts.
    pub on_command: Option<CommandHook>,
    pub on_verify: Option<Hook>,
    /// What `ISCC.exe /?` prints.
    pub iscc_banner: &'static str,
    /// Whether `certutil` prints the digest in pairs separated by spaces,
    /// as older Windows does.
    pub certutil_spaced: bool,
    /// The status `notarytool submit` reports.
    pub notarization: &'static str,
    /// The notarization key's file as `notarytool submit` found it.
    pub key_seen: Option<Vec<u8>>,
    /// The certificate's file as `security import` found it, with its mode.
    pub certificate_seen: Option<(Vec<u8>, u32)>,
    /// Whether `security list-keychains` prints nothing.
    pub no_keychains: bool,
    /// The user's keychain search list, shared so a hook can change it
    /// during the run.
    pub search_list: Arc<Mutex<Vec<String>>>,
}

impl FakeRunner {
    /// A runner whose build reports a binary in `folder`.
    pub fn new(folder: &Path) -> Self {
        let binary = folder.join("yard-binary");
        std::fs::write(&binary, b"the yard binary").unwrap();
        Self {
            commands: Vec::new(),
            binary,
            fail: None,
            also_fail: None,
            on_fail: None,
            on_command: None,
            on_verify: None,
            iscc_banner: ISCC_BANNER,
            certutil_spaced: false,
            notarization: "Accepted",
            key_seen: None,
            certificate_seen: None,
            no_keychains: false,
            search_list: Arc::new(Mutex::new(vec![LOGIN_KEYCHAIN.to_owned()])),
        }
    }

    pub fn programs(&self) -> Vec<&str> {
        self.commands.iter().map(|c| c.program.as_str()).collect()
    }

    fn record(&mut self, command: &Command, input: Option<&[u8]>) -> Result<Recorded, String> {
        let recorded = Recorded {
            // `cargo.exe` reads as `cargo`, and `ISCC.exe` as `ISCC`, on
            // every system.
            program: {
                let name = Path::new(command.get_program())
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                match name.strip_suffix(".exe") {
                    Some(stem) => stem.to_owned(),
                    None => name,
                }
            },
            path: PathBuf::from(command.get_program()),
            args: command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
            envs: command
                .get_envs()
                .map(|(key, value)| {
                    (
                        key.to_string_lossy().into_owned(),
                        value.map(|value| value.to_string_lossy().into_owned()),
                    )
                })
                .collect(),
            dir: command.get_current_dir().map(Path::to_path_buf),
            input: input.map(|input| String::from_utf8(input.to_vec()).unwrap()),
        };
        self.commands.push(recorded.clone());
        if let Some(hook) = &mut self.on_command {
            hook(&recorded);
        }
        let first = recorded.args.first().cloned().unwrap_or_default();
        let with_first = format!("{} {first}", recorded.program);
        if [self.fail, self.also_fail]
            .into_iter()
            .flatten()
            .any(|fail| fail == recorded.program || fail == with_first)
        {
            if let Some(hook) = &mut self.on_fail {
                hook();
            }
            return Err(format!("{} exited with exit status: 1", recorded.program));
        }
        Ok(recorded)
    }
}

impl xtask::Runner for FakeRunner {
    fn run(&mut self, command: &mut Command) -> Result<(), String> {
        let recorded = self.record(command, None)?;
        let last = recorded.args.last().map(PathBuf::from);
        match recorded.program.as_str() {
            "iconutil" | "ditto" | "hdiutil" => std::fs::write(last.unwrap(), b"made").unwrap(),
            "tar" if recorded.args[0] == "-czf" => {
                std::fs::write(&recorded.args[1], b"made").unwrap()
            }
            // Windows' `tar -a -c -f <zip>`.
            "tar" => {
                let file = recorded.args.iter().position(|arg| arg == "-f").unwrap();
                std::fs::write(&recorded.args[file + 1], b"made").unwrap()
            }
            // Inno Setup writes `<OutputDir>/<OutputBaseFilename>.exe`,
            // relative to the script.
            "ISCC" => {
                let script = last.unwrap();
                let text = std::fs::read_to_string(&script).unwrap();
                let value = |key: &str| {
                    text.lines()
                        .find_map(|line| line.strip_prefix(&format!("{key}=")))
                        .unwrap()
                        .to_owned()
                };
                let setup = script
                    .parent()
                    .unwrap()
                    .join(value("OutputDir"))
                    .join(format!("{}.exe", value("OutputBaseFilename")));
                std::fs::write(setup, b"setup").unwrap();
            }
            "codesign" if recorded.args[0] == "--verify" => {
                if let Some(hook) = &mut self.on_verify {
                    hook(&last.unwrap());
                }
            }
            "security" if recorded.args[0] == "list-keychains" => {
                let set = recorded.args.iter().position(|arg| arg == "-s").unwrap();
                *self.search_list.lock().unwrap() = recorded.args[set + 1..].to_vec();
            }
            "security" if recorded.args[0] == "delete-keychain" => {
                let keychain = &recorded.args[1];
                self.search_list
                    .lock()
                    .unwrap()
                    .retain(|entry| entry != keychain);
            }
            _ => {}
        }
        Ok(())
    }

    fn input(&mut self, command: &mut Command, input: &[u8]) -> Result<Vec<u8>, String> {
        let recorded = self.record(command, Some(input))?;
        let line = recorded.input.unwrap();
        if line.starts_with("create-keychain ") {
            let path = line.split('"').nth(3).unwrap();
            std::fs::write(format!("{path}-db"), b"keychain").unwrap();
        }
        if line.starts_with("import ") {
            let path = line.split('"').nth(1).unwrap();
            let bytes = std::fs::read(path).unwrap();
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777
            };
            #[cfg(not(unix))]
            let mode = 0;
            self.certificate_seen = Some((bytes, mode));
        }
        Ok(Vec::new())
    }

    fn report(&mut self, command: &mut Command) -> Result<(bool, Vec<u8>), String> {
        let recorded = self.record(command, None)?;
        if recorded.program == "ISCC" {
            assert_eq!(recorded.args, ["/?"]);
            // ISCC ends `/?` with a code that says nothing of the version.
            return Ok((false, self.iscc_banner.as_bytes().to_vec()));
        }
        assert_eq!(recorded.args[..2], ["notarytool", "submit"]);
        let key = recorded.args.iter().position(|arg| arg == "--key").unwrap();
        self.key_seen = Some(std::fs::read(&recorded.args[key + 1]).unwrap());
        let report = serde_json::json!({
            "id": "submission-1",
            "status": self.notarization,
            "message": "Processing complete",
        });
        Ok((
            self.notarization == "Accepted",
            report.to_string().into_bytes(),
        ))
    }

    fn output(&mut self, command: &mut Command) -> Result<Vec<u8>, String> {
        let recorded = self.record(command, None)?;
        Ok(match recorded.program.as_str() {
            "security" if self.no_keychains => Vec::new(),
            "security" => self
                .search_list
                .lock()
                .unwrap()
                .iter()
                .map(|keychain| format!("    \"{keychain}\"\n"))
                .collect::<String>()
                .into_bytes(),
            "shasum" | "sha256sum" => format!("{DIGEST}  {}\n", recorded.args.last().unwrap()).into_bytes(),
            "certutil" => {
                let digest = if self.certutil_spaced {
                    DIGEST
                        .as_bytes()
                        .chunks(2)
                        .map(|pair| std::str::from_utf8(pair).unwrap().to_uppercase())
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    DIGEST.to_owned()
                };
                format!(
                    "SHA256 hash of {}:\r\n{digest}\r\nCertUtil: -hashfile command completed successfully.\r\n",
                    recorded.args[1]
                )
                .into_bytes()
            }
            "cargo" | "cargo.exe" => format!(
                "{}\n{}\n{}\n",
                serde_json::json!({"reason": "compiler-artifact", "target": {"name": "yard_log"}, "executable": null}),
                serde_json::json!({"reason": "compiler-artifact", "target": {"name": "yard"}, "executable": self.binary}),
                serde_json::json!({"reason": "build-finished", "success": true}),
            )
            .into_bytes(),
            _ => Vec::new(),
        })
    }
}

/// A value of a property list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plist {
    String(String),
    Bool(bool),
    Array(Vec<Plist>),
    Dict(Vec<(String, Plist)>),
}

impl Plist {
    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn keys(&self) -> Vec<&str> {
        match self {
            Plist::Dict(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    pub fn string(value: &str) -> Plist {
        Plist::String(value.to_owned())
    }
}

/// Reads an XML property list made of `dict`, `key`, `string`, `array`, and
/// `true`/`false`: the root value inside `<plist>`.
pub fn parse_plist(text: &str) -> Plist {
    let mut rest = text.trim_start();
    for prefix in ["<?xml", "<!DOCTYPE", "<plist"] {
        assert!(rest.starts_with(prefix), "expected {prefix} at {rest:?}");
        rest = rest[rest.find('>').unwrap() + 1..].trim_start();
    }
    let (value, rest) = value(rest);
    assert_eq!(rest.trim(), "</plist>", "after the root value");
    value
}

fn unescape(text: &str) -> String {
    assert!(!text.contains('<'), "markup in text {text:?}");
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#13;", "\r")
        .replace("&amp;", "&")
}

/// The text up to `</tag>`, and what follows it.
fn text<'a>(rest: &'a str, tag: &str) -> (String, &'a str) {
    let close = format!("</{tag}>");
    let end = rest.find(&close).unwrap_or_else(|| panic!("no {close}"));
    (
        unescape(&rest[..end]),
        rest[end + close.len()..].trim_start(),
    )
}

fn value(rest: &str) -> (Plist, &str) {
    let rest = rest.trim_start();
    if let Some(rest) = rest.strip_prefix("<string>") {
        let (text, rest) = text(rest, "string");
        (Plist::String(text), rest)
    } else if let Some(rest) = rest.strip_prefix("<true/>") {
        (Plist::Bool(true), rest.trim_start())
    } else if let Some(rest) = rest.strip_prefix("<false/>") {
        (Plist::Bool(false), rest.trim_start())
    } else if let Some(mut rest) = rest.strip_prefix("<array>") {
        let mut items = Vec::new();
        loop {
            rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix("</array>") {
                return (Plist::Array(items), after.trim_start());
            }
            let (item, after) = value(rest);
            items.push(item);
            rest = after;
        }
    } else if let Some(mut rest) = rest.strip_prefix("<dict>") {
        let mut entries = Vec::new();
        loop {
            rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix("</dict>") {
                return (Plist::Dict(entries), after.trim_start());
            }
            let after = rest
                .strip_prefix("<key>")
                .unwrap_or_else(|| panic!("expected <key> at {rest:?}"));
            let (key, after) = text(after, "key");
            let (item, after) = value(after);
            entries.push((key, item));
            rest = after;
        }
    } else {
        panic!("unexpected value at {rest:?}")
    }
}

/// Encodes standard base64, as the secrets in the environment are.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let number = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        for index in 0..4 {
            if index <= chunk.len() {
                text.push(ALPHABET[(number >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// A copy of the `signed` config in `folder`, with `notarize` and `dmg` as
/// given.
pub fn signed_config(folder: &Path, notarize: bool, dmg: bool) -> PathBuf {
    let signed = under_test("signed");
    for file in ["icon.png", "entitlements.plist"] {
        std::fs::copy(signed.with_file_name(file), folder.join(file)).unwrap();
    }
    let text = std::fs::read_to_string(&signed)
        .unwrap()
        .replace("notarize = true", &format!("notarize = {notarize}"))
        .replace("dmg = true", &format!("dmg = {dmg}"));
    let path = folder.join("product.toml");
    std::fs::write(&path, text).unwrap();
    path
}
