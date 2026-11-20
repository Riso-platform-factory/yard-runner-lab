//! Reads and checks the product config, `product.toml`, into a
//! [`BuildConfig`].

use std::path::{Path, PathBuf};

mod release;
pub use release::*;

/// The product config's values, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildConfig {
    pub name: String,
    pub version: String,
    /// The reverse-domain identifier, as macOS asks of a bundle identifier.
    pub identifier: String,
    pub url_scheme: String,
    /// The icon's absolute, canonical path.
    pub icon: PathBuf,
    /// The address of the newest release's file, `None` when the config
    /// names none.
    pub update_url: Option<String>,
    /// How the macOS package is signed and wrapped: `[release.macos]`.
    pub macos: MacosRelease,
    /// How the Windows package is signed: `[release.windows]`.
    pub windows: WindowsRelease,
}

/// The keys a product config may hold. Any other key is refused; with
/// several, the first in sorted order is named, so the message does not
/// depend on how the table orders its keys.
const KEYS: &[&str] = &[
    "name",
    "version",
    "identifier",
    "url_scheme",
    "icon",
    "update_url",
    "release",
];

/// Reads the product config at `path`, inside the group
/// `build-config-read{file}`. An error names the file and, when one value
/// or key is at fault, that key in backticks. A refusal is logged at `info`,
/// because a config that breaks a rule is expected.
pub fn read(path: &Path) -> Result<BuildConfig, String> {
    let _group = tracing::info_span!("build-config-read", file = %path.display()).entered();
    check(path).inspect_err(|reason| tracing::info!("refused: {reason}"))
}

fn check(path: &Path) -> Result<BuildConfig, String> {
    let file = path.display();
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read product config {file}: {error}"))?;
    let config: toml::Table =
        toml::from_str(&text).map_err(|error| format!("invalid product config {file}: {error}"))?;
    if let Some(key) = config
        .keys()
        .filter(|key| !KEYS.contains(&key.as_str()))
        .min()
    {
        return Err(format!("product config {file}: unknown key `{key}`"));
    }
    let string = |key: &str| -> Result<String, String> {
        let value = config
            .get(key)
            .and_then(toml::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("product config {file}: `{key}` must be a non-empty string"))?;
        if value.contains(['\r', '\n']) {
            return Err(format!(
                "product config {file}: `{key}` must not contain line breaks"
            ));
        }
        Ok(value.to_owned())
    };
    let name = string("name")?;
    if yard_product::file_name(&name).is_empty() {
        return Err(format!(
            "product config {file}: `name` {name:?} gives no file name for Linux: it must hold \
             at least one ASCII letter or digit"
        ));
    }
    tracing::debug!(name, "name");
    let version = string("version")?;
    tracing::debug!(version, "version");
    let identifier = string("identifier")?;
    if !yard_product::is_identifier(&identifier) {
        return Err(format!(
            "product config {file}: `identifier` {identifier:?} must be at least two labels \
             separated by dots, each of ASCII letters, digits, and `-`, not starting or ending \
             with `-`"
        ));
    }
    tracing::debug!(identifier, "identifier");
    let url_scheme = string("url_scheme")?;
    let mut characters = url_scheme.chars();
    if !characters.next().is_some_and(|c| c.is_ascii_lowercase())
        || !characters
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
    {
        return Err(format!(
            "product config {file}: `url_scheme` {url_scheme:?} is invalid. It must \
             begin with a lowercase ASCII letter and contain only lowercase ASCII letters, \
             digits, '+', '-', or '.'"
        ));
    }
    tracing::debug!(url_scheme, "url scheme");
    let icon = string("icon")?;
    if Path::new(&icon).is_absolute() {
        return Err(format!(
            "product config {file}: `icon` {icon:?} must be relative to the config file"
        ));
    }
    let icon = path.parent().unwrap_or(Path::new("")).join(&icon);
    let bytes = std::fs::read(&icon).map_err(|error| {
        format!(
            "product config {file}: `icon` {} is missing or unreadable: {error}",
            icon.display()
        )
    })?;
    let icon = icon.canonicalize().map_err(|error| {
        format!(
            "product config {file}: `icon` {} is missing or unreadable: {error}",
            icon.display()
        )
    })?;
    let (width, height) = png_size(&bytes).ok_or_else(|| {
        format!(
            "product config {file}: `icon` {} is not a valid PNG",
            icon.display()
        )
    })?;
    if width != height || width < 512 {
        return Err(format!(
            "product config {file}: `icon` {} is {width} × {height}; it must be square and at \
             least 512 × 512",
            icon.display()
        ));
    }
    tracing::debug!(icon = %icon.display(), width, height, "icon");
    let update_url = match config.get("update_url") {
        None => None,
        Some(_) => {
            let update_url = string("update_url")?;
            if let Err(reason) = check_update_url(&update_url) {
                return Err(format!(
                    "product config {file}: `update_url` {update_url:?} {reason}"
                ));
            }
            if let Err(error) = semver::Version::parse(&version) {
                return Err(format!(
                    "product config {file}: `version` {version:?} must be a semantic version \
                     when `update_url` is set: {error}"
                ));
            }
            Some(update_url)
        }
    };
    tracing::debug!(present = update_url.is_some(), "update url");
    let (macos, windows) = release::read(path, config.get("release"))?;
    tracing::debug!(
        dmg = macos.dmg,
        macos_signed = macos.signing.is_some(),
        windows_signed = windows.signing.is_some(),
        "release"
    );
    Ok(BuildConfig {
        name,
        version,
        identifier,
        url_scheme,
        icon,
        update_url,
        macos,
        windows,
    })
}

/// Accepts an `https://` URL, or an `http://` one only to `127.0.0.1` or
/// `localhost`, for checks on this machine.
fn check_update_url(url: &str) -> Result<(), &'static str> {
    let (scheme, host) = split_url(url)?;
    match scheme {
        "https" => Ok(()),
        "http" if matches!(host, "127.0.0.1" | "localhost") => Ok(()),
        "http" => Err("must use https://; http:// is allowed only to 127.0.0.1 or localhost"),
        _ => Err("must be an https:// URL"),
    }
}

/// The scheme and the host of a URL whose host is ASCII letters, digits,
/// `.`, and `-`, with an optional port from 1 to 65535.
fn split_url(url: &str) -> Result<(&str, &str), &'static str> {
    const UNREADABLE: &str = "is not a readable URL";
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(UNREADABLE);
    }
    let (scheme, rest) = url.split_once("://").ok_or(UNREADABLE)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        || port.is_some_and(|port| {
            !port.chars().all(|c| c.is_ascii_digit())
                || !port.parse::<u16>().is_ok_and(|port| port >= 1)
        })
    {
        return Err(UNREADABLE);
    }
    Ok((scheme, host))
}

/// The width and height of a PNG, or `None` when `bytes` are not one: the
/// signature, a first chunk `IHDR` of 13 bytes whose CRC matches and whose
/// sizes are within PNG's range, and chunks that run whole up to `IEND`.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    const MAX: u32 = (1 << 31) - 1;
    let number = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(
            bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    if bytes.get(..8)? != SIGNATURE || number(8)? != 13 || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    if crc32(bytes.get(12..29)?) != number(29)? {
        return None;
    }
    let (width, height) = (number(16)?, number(20)?);
    if !(1..=MAX).contains(&width) || !(1..=MAX).contains(&height) {
        return None;
    }
    let mut at = 8;
    loop {
        let length = usize::try_from(number(at)?).ok()?;
        let kind = bytes.get(at + 4..at + 8)?;
        let end = at.checked_add(12)?.checked_add(length)?;
        if end > bytes.len() {
            return None;
        }
        if kind == b"IEND" {
            return Some((width, height));
        }
        at = end;
    }
}

/// The CRC-32 that PNG chunks carry (ISO-HDLC, polynomial `0xEDB88320`).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
