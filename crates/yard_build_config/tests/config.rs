//! `read` checks the product config: a missing or invalid identifier, icon,
//! URL scheme, or update address, or an unknown key, fails with the file and
//! the key. The configs under test in `tests/configs/` are each a different
//! app.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A folder of its own under the system's temporary folder, removed when it
/// drops.
struct TempDir(PathBuf);
impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn tempdir() -> TempDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "yard_build_config-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

/// PNG's CRC-32, written here apart from the build script's.
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

/// A chunk: length, kind, data, and the CRC of kind and data.
fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut bytes = (data.len() as u32).to_be_bytes().to_vec();
    bytes.extend(kind);
    bytes.extend(data);
    bytes.extend(crc32(&bytes[4..]).to_be_bytes());
    bytes
}

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// The signature and a valid `IHDR` chunk.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut data = width.to_be_bytes().to_vec();
    data.extend(height.to_be_bytes());
    data.extend([8, 6, 0, 0, 0]);
    let mut bytes = SIGNATURE.to_vec();
    bytes.extend(chunk(b"IHDR", &data));
    bytes
}

/// A PNG's chunk structure: the header, then `IEND`.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = png_header(width, height);
    bytes.extend(chunk(b"IEND", &[]));
    bytes
}

/// Writes `product.toml` with `keys` and, when given, `icon.png`, in a
/// folder removed when the returned guard drops.
fn config(keys: &str, icon: Option<&[u8]>) -> (TempDir, PathBuf) {
    config_with_scheme(keys, icon, "url_scheme = \"sample\"\n")
}

fn config_with_scheme(keys: &str, icon: Option<&[u8]>, scheme: &str) -> (TempDir, PathBuf) {
    let folder = tempdir();
    let path = folder.path().join("product.toml");
    std::fs::write(
        &path,
        format!("name = \"sample\"\nversion = \"1.0\"\n{keys}{scheme}"),
    )
    .unwrap();
    if let Some(icon) = icon {
        std::fs::write(folder.path().join("icon.png"), icon).unwrap();
    }
    (folder, path)
}

/// Writes `product.toml` as `config` does, with `version` instead of `1.0`.
fn config_with_version(keys: &str, icon: &[u8], version: &str) -> (TempDir, PathBuf) {
    let (folder, path) = config(keys, Some(icon));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replacen("version = \"1.0\"", &format!("version = {version:?}"), 1),
    )
    .unwrap();
    (folder, path)
}

/// The yard application's folder, which holds its own config.
fn yard() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/yard")
}

const IDENTIFIER: &str = "identifier = \"com.example.sample\"\n";
const ICON: &str = "identifier = \"com.example.sample\"\nicon = \"icon.png\"\n";

/// The error for the config, which names the file and the key.
fn refused((_folder, path): (TempDir, PathBuf), key: &str) -> String {
    let error = yard_build_config::read(&path).unwrap_err();
    assert!(
        error.contains(&path.display().to_string()) && error.contains(&format!("`{key}`")),
        "{error}"
    );
    error
}

#[test]
fn a_valid_config_gives_the_identifier_and_the_icon_s_absolute_path() {
    let icon = std::fs::read(yard().join("icon.png")).unwrap();
    let (folder, path) = config(ICON, Some(&icon));
    let config = yard_build_config::read(&path).unwrap();
    assert_eq!(config.name, "sample");
    assert_eq!(config.version, "1.0");
    assert_eq!(config.identifier, "com.example.sample");
    assert!(config.icon.is_absolute());
    assert_eq!(
        config.icon,
        folder.path().join("icon.png").canonicalize().unwrap()
    );
}

#[test]
fn a_square_png_of_exactly_512_is_accepted() {
    let (_folder, path) = config(ICON, Some(&png(512, 512)));
    yard_build_config::read(&path).unwrap();
}

#[test]
fn the_app_s_own_config_is_valid() {
    let path = yard().join("product.toml");
    let config = yard_build_config::read(&path).unwrap();
    assert_eq!(config.identifier, "vn.risotech.yard");
    assert_eq!(config.url_scheme, "yard");
    assert!(config.icon.ends_with("icon.png"));
}

#[test]
fn a_url_scheme_accepts_lowercase_ascii_letters_digits_and_punctuation() {
    let icon = png(512, 512);
    for scheme in ["a", "sample2+preview-1.test"] {
        let (_folder, path) =
            config_with_scheme(ICON, Some(&icon), &format!("url_scheme = {scheme:?}\n"));
        let config = yard_build_config::read(&path).unwrap();
        assert_eq!(config.url_scheme, scheme);
    }
}

#[test]
fn a_missing_or_invalid_url_scheme_is_refused() {
    let icon = png(512, 512);
    for scheme in [
        "",
        "url_scheme = 3",
        "url_scheme = \"\"",
        "url_scheme = \"Yard\"",
        "url_scheme = \"1yard\"",
        "url_scheme = \"+yard\"",
        "url_scheme = \"yard_app\"",
        "url_scheme = \"yard user\"",
        "url_scheme = \"yard:\"",
        "url_scheme = \"risó\"",
    ] {
        refused(config_with_scheme(ICON, Some(&icon), scheme), "url_scheme");
    }
}

#[test]
fn a_missing_or_invalid_identifier_is_refused() {
    let icon = png(1024, 1024);
    refused(config("icon = \"icon.png\"\n", Some(&icon)), "identifier");
    for identifier in ["sample", "com..example", "-com.example", "com.exa_mple"] {
        refused(
            config(
                &format!("identifier = \"{identifier}\"\nicon = \"icon.png\"\n"),
                Some(&icon),
            ),
            "identifier",
        );
    }
}

#[test]
fn a_missing_icon_key_or_file_is_refused_as_missing() {
    refused(config(IDENTIFIER, Some(&png(1024, 1024))), "icon");
    let error = refused(config(ICON, None), "icon");
    assert!(error.contains("missing or unreadable"), "{error}");
}

#[test]
fn an_absolute_icon_path_is_refused() {
    let (folder, _) = config("", Some(&png(1024, 1024)));
    let absolute = folder.path().join("icon.png");
    let error = refused(
        config(
            &format!("{IDENTIFIER}icon = {:?}\n", absolute.display().to_string()),
            None,
        ),
        "icon",
    );
    assert!(error.contains("relative"), "{error}");
}

#[test]
fn an_icon_that_is_not_a_valid_square_png_of_512_is_refused() {
    let mut bad_crc = png(1024, 1024);
    bad_crc[29] ^= 0xff;
    let mut old_stub = SIGNATURE.to_vec();
    old_stub.extend(13u32.to_be_bytes());
    old_stub.extend(b"IHDR");
    old_stub.extend(1024u32.to_be_bytes());
    old_stub.extend(1024u32.to_be_bytes());
    for icon in [
        b"not a png at all, just some text".to_vec(),
        png(1024, 1024)[..20].to_vec(),
        bad_crc,
        png_header(1024, 1024),
        old_stub,
        png(0, 0),
        png(1 << 31, 1 << 31),
    ] {
        let error = refused(config(ICON, Some(&icon)), "icon");
        assert!(error.contains("not a valid PNG"), "{error}");
    }
    for icon in [png(1024, 512), png(256, 256)] {
        let error = refused(config(ICON, Some(&icon)), "icon");
        assert!(error.contains("square and at least 512"), "{error}");
    }
}

#[test]
fn an_absent_update_url_gives_none() {
    let (_folder, path) = config(ICON, Some(&png(512, 512)));
    let config = yard_build_config::read(&path).unwrap();
    assert_eq!(config.update_url, None);
    let path = yard().join("product.toml");
    let config = yard_build_config::read(&path).unwrap();
    assert_eq!(config.update_url, None, "yard's own config names none");
}

#[test]
fn an_https_or_local_http_update_url_is_accepted() {
    let icon = png(512, 512);
    for url in [
        "https://releases.example.com/latest.json",
        "https://releases.example.com:8443/sample/latest.json?channel=stable",
        "http://127.0.0.1:8765/latest.json",
        "http://localhost/latest.json",
    ] {
        let (_folder, path) =
            config_with_version(&format!("{ICON}update_url = {url:?}\n"), &icon, "0.1.0");
        let config = yard_build_config::read(&path).unwrap();
        assert_eq!(config.update_url.as_deref(), Some(url));
    }
}

#[test]
fn an_update_url_that_is_not_https_or_local_http_is_refused() {
    let icon = png(512, 512);
    for url in [
        "\"http://example.com/latest.json\"",
        "\"http://127.0.0.2/latest.json\"",
        "\"ftp://releases.example.com/latest.json\"",
    ] {
        let error = refused(
            config(&format!("{ICON}update_url = {url}\n"), Some(&icon)),
            "update_url",
        );
        assert!(error.contains("https://"), "{error}");
    }
}

#[test]
fn an_unreadable_update_url_is_refused() {
    let icon = png(512, 512);
    for url in [
        "\"\"",
        "3",
        "\"releases.example.com/latest.json\"",
        "\"https://\"",
        "\"https:///latest.json\"",
        "\"https://releases example.com/latest.json\"",
        "\"https://user@releases.example.com/latest.json\"",
        "\"https://releases.example.com:port/latest.json\"",
    ] {
        refused(
            config(&format!("{ICON}update_url = {url}\n"), Some(&icon)),
            "update_url",
        );
    }
}

#[test]
fn a_version_that_is_not_semantic_is_refused_only_with_an_update_url() {
    let icon = png(512, 512);
    let url = "update_url = \"https://releases.example.com/latest.json\"\n";
    let error = refused(config(&format!("{ICON}{url}"), Some(&icon)), "version");
    assert!(error.contains("semantic version"), "{error}");
    let (_folder, path) = config(ICON, Some(&icon));
    assert_eq!(yard_build_config::read(&path).unwrap().version, "1.0");
    let (_folder, path) = config_with_version(&format!("{ICON}{url}"), &icon, "0.1.0");
    let config = yard_build_config::read(&path).unwrap();
    assert_eq!(config.version, "0.1.0");
    assert_eq!(
        config.update_url.as_deref(),
        Some("https://releases.example.com/latest.json")
    );
}

#[test]
fn an_update_url_to_another_host_or_an_impossible_port_is_refused() {
    let icon = png(512, 512);
    for url in [
        "http://127.0.0.1.evil.com/latest.json",
        "http://localhost@evil.com/latest.json",
        "https://releases.example.com:65536/latest.json",
        "https://releases.example.com:0/latest.json",
        "http://127.0.0.1:+80/latest.json",
    ] {
        refused(
            config_with_version(&format!("{ICON}update_url = {url:?}\n"), &icon, "0.1.0"),
            "update_url",
        );
    }
    let (_folder, path) = config_with_version(
        &format!("{ICON}update_url = \"https://releases.example.com:65535/latest.json\"\n"),
        &icon,
        "0.1.0",
    );
    yard_build_config::read(&path).unwrap();
}

/// A config under test in `tests/configs/`.
fn under_test(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/configs")
        .join(name)
        .join("product.toml")
}

#[test]
fn an_unknown_key_is_refused_naming_it() {
    let made = config(&format!("{ICON}colour = \"red\"\n"), Some(&png(512, 512)));
    let path = made.1.clone();
    let error = refused(made, "colour");
    assert_eq!(
        error,
        format!("product config {}: unknown key `colour`", path.display())
    );
}

#[test]
fn of_several_unknown_keys_the_first_in_sorted_order_is_named() {
    let made = config(
        &format!("{ICON}zebra = 1\ncolour = \"red\"\n"),
        Some(&png(512, 512)),
    );
    let path = made.1.clone();
    let error = refused(made, "colour");
    assert_eq!(
        error,
        format!("product config {}: unknown key `colour`", path.display())
    );
}

#[test]
fn a_config_that_is_not_toml_is_refused_as_invalid() {
    let (_folder, path) = config("name = \n", None);
    let error = yard_build_config::read(&path).unwrap_err();
    assert!(
        error.starts_with(&format!("invalid product config {}: ", path.display())),
        "{error}"
    );
}

#[test]
fn the_configs_under_test_read_to_their_apps() {
    for (name, app, version, identifier, url_scheme) in [
        ("plain", "Lab", "0.1.0", "test.yard.lab", "yardlab"),
        (
            "spaced",
            "Lab Notes Pro",
            "0.2.0",
            "test.yard.labnotes",
            "yardlabnotes",
        ),
        (
            "other",
            "Other Lab",
            "1.0.0",
            "test.yard.other",
            "yardother",
        ),
    ] {
        let path = under_test(name);
        let config = yard_build_config::read(&path).unwrap();
        assert_eq!(
            config,
            yard_build_config::BuildConfig {
                name: app.to_owned(),
                version: version.to_owned(),
                identifier: identifier.to_owned(),
                url_scheme: url_scheme.to_owned(),
                icon: path.with_file_name("icon.png").canonicalize().unwrap(),
                update_url: None,
                macos: Default::default(),
                windows: Default::default(),
            },
            "{name}"
        );
    }
}

#[test]
fn the_configs_under_test_have_different_icons() {
    let icons: std::collections::BTreeSet<Vec<u8>> = ["plain", "spaced", "other"]
        .into_iter()
        .map(|name| std::fs::read(under_test(name).with_file_name("icon.png")).unwrap())
        .collect();
    assert_eq!(icons.len(), 3);
}

#[test]
fn each_bad_config_under_test_is_refused_naming_the_file_and_the_key() {
    for (name, key, reason) in [
        ("bad-missing-name", "name", "must be a non-empty string"),
        ("bad-name-for-linux", "name", "gives no file name for Linux"),
        ("bad-identifier", "identifier", "at least two labels"),
        ("bad-scheme", "url_scheme", "lowercase ASCII letter"),
        ("bad-icon-not-square", "icon", "is 512 × 256"),
        ("bad-icon-small", "icon", "is 256 × 256"),
        ("bad-unknown-key", "colour", "unknown key"),
        ("bad-update-url", "update_url", "must use https://"),
        ("bad-release-key", "release.macos.colour", "unknown key"),
    ] {
        let path = under_test(name);
        let error = yard_build_config::read(&path).unwrap_err();
        assert!(
            error.contains(&path.display().to_string())
                && error.contains(&format!("`{key}`"))
                && error.contains(reason),
            "{name}: {error}"
        );
    }
}

#[test]
fn the_signed_config_reads_its_release_tables() {
    let path = under_test("signed");
    let config = yard_build_config::read(&path).unwrap();
    assert_eq!(
        config.macos,
        yard_build_config::MacosRelease {
            dmg: true,
            entitlements: Some(
                path.with_file_name("entitlements.plist")
                    .canonicalize()
                    .unwrap()
            ),
            signing: Some(yard_build_config::MacosSigning {
                identity: "Developer ID Application: YARD Test (YARDTEST01)".into(),
                notarize: true,
            }),
        }
    );
    assert_eq!(
        config.windows.signing,
        Some(yard_build_config::WindowsSigning {
            timestamp_url: "http://timestamp.digicert.com".into()
        })
    );
}

#[test]
fn release_tables_are_optional_with_their_defaults() {
    let icon = png(512, 512);
    let (_folder, path) = config(ICON, Some(&icon));
    let config = yard_build_config::read(&path).unwrap();
    assert_eq!(config.macos, yard_build_config::MacosRelease::default());
    assert_eq!(config.windows, yard_build_config::WindowsRelease::default());

    let (_folder, path) = config_with_scheme(
        ICON,
        Some(&icon),
        "url_scheme = \"sample\"\n[release.linux]\n[release.macos]\n\
         [release.macos.signing]\nidentity = \"Developer ID Application: A (B)\"\n\
         [release.windows.signing]\n",
    );
    let config = yard_build_config::read(&path).unwrap();
    assert!(!config.macos.dmg);
    assert_eq!(config.macos.entitlements, None);
    assert_eq!(
        config.macos.signing,
        Some(yard_build_config::MacosSigning {
            identity: "Developer ID Application: A (B)".into(),
            notarize: true,
        }),
        "notarize defaults to true"
    );
    assert_eq!(
        config.windows.signing.unwrap().timestamp_url,
        yard_build_config::DEFAULT_TIMESTAMP_URL
    );
}

#[test]
fn a_timestamp_url_may_be_http_or_https() {
    let icon = png(512, 512);
    for url in [
        "http://timestamp.digicert.com",
        "https://ts.example.com:8443/tsa",
    ] {
        let (_folder, path) = config_with_scheme(
            ICON,
            Some(&icon),
            &format!(
                "url_scheme = \"sample\"\n[release.windows.signing]\ntimestamp_url = {url:?}\n"
            ),
        );
        let config = yard_build_config::read(&path).unwrap();
        assert_eq!(config.windows.signing.unwrap().timestamp_url, url);
    }
}

/// The error for a config whose release tables are `release`, which names
/// the file and `key`, and holds `reason`.
fn release_refused(release: &str, key: &str, reason: &str) {
    let made = config_with_scheme(
        ICON,
        Some(&png(512, 512)),
        &format!("url_scheme = \"sample\"\n{release}"),
    );
    let error = refused(made, key);
    assert!(error.contains(reason), "{release}: {error}");
}

#[test]
fn a_bad_release_table_is_refused_naming_the_key() {
    for (release, key, reason) in [
        ("release = 3\n", "release", "must be a table"),
        ("[release]\nios = 1\n", "release.ios", "unknown key"),
        (
            "[release.linux]\nformat = \"deb\"\n",
            "release.linux.format",
            "unknown key",
        ),
        (
            "[release.macos]\ncolour = \"red\"\n",
            "release.macos.colour",
            "unknown key",
        ),
        (
            "[release.macos]\ndmg = \"yes\"\n",
            "release.macos.dmg",
            "true or false",
        ),
        (
            "[release.macos]\nentitlements = \"\"\n",
            "release.macos.entitlements",
            "non-empty",
        ),
        (
            "[release.macos]\nentitlements = \"missing.plist\"\n",
            "release.macos.entitlements",
            "missing or not a file",
        ),
        (
            "[release.macos.signing]\n",
            "release.macos.signing.identity",
            "non-empty",
        ),
        (
            "[release.macos.signing]\nidentity = \"A\\nB\"\n",
            "release.macos.signing.identity",
            "line breaks",
        ),
        (
            "[release.macos.signing]\nidentity = \"A\"\nnotarize = 1\n",
            "release.macos.signing.notarize",
            "true or false",
        ),
        (
            "[release.macos.signing]\nidentity = \"A\"\nteam = \"B\"\n",
            "release.macos.signing.team",
            "unknown key",
        ),
        (
            "[release.windows]\nmsi = true\n",
            "release.windows.msi",
            "unknown key",
        ),
        (
            "[release.windows.signing]\ntimestamp_url = \"ftp://ts.example.com\"\n",
            "release.windows.signing.timestamp_url",
            "http:// or https://",
        ),
        (
            "[release.windows.signing]\ntimestamp_url = \"not a url\"\n",
            "release.windows.signing.timestamp_url",
            "readable URL",
        ),
    ] {
        release_refused(release, key, reason);
    }
    // An absolute path to a file that exists on every system, in a TOML
    // basic string, with `\` and `"` escaped, so any checkout path reads back.
    let absolute = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let escaped = absolute
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    release_refused(
        &format!("[release.macos]\nentitlements = \"{escaped}\"\n"),
        "release.macos.entitlements",
        "relative",
    );
}

#[test]
fn an_entitlements_file_is_made_absolute_like_the_icon() {
    let (folder, path) = config_with_scheme(
        ICON,
        Some(&png(512, 512)),
        "url_scheme = \"sample\"\n[release.macos]\nentitlements = \"app.entitlements\"\n",
    );
    std::fs::write(folder.path().join("app.entitlements"), "<plist/>").unwrap();
    let config = yard_build_config::read(&path).unwrap();
    assert_eq!(
        config.macos.entitlements,
        Some(
            folder
                .path()
                .join("app.entitlements")
                .canonicalize()
                .unwrap()
        )
    );
}
