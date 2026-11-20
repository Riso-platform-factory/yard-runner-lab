//! The update install: downloads a release, checks it, and swaps it with the
//! running bundle.

use http_client::HttpClient;
use std::{path::PathBuf, sync::Arc};
// The swap, `ditto`, `codesign`, and the bundle's checks are macOS's.
#[cfg(target_os = "macos")]
use {
    super::check::{host, without_addresses},
    futures::{AsyncReadExt as _, FutureExt as _},
    http_client::{AsyncBody, HttpRequestExt as _, RedirectPolicy, Request},
    sha2::{Digest as _, Sha256},
    std::{
        cmp::Ordering,
        future::Future,
        io::Write as _,
        path::{Component, Path},
        process::Command,
        time::Duration,
    },
    tempfile::TempDir,
    tracing::Instrument as _,
};

/// The largest release downloaded.
#[cfg(target_os = "macos")]
const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;
/// The largest release once unpacked.
#[cfg(target_os = "macos")]
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;
/// The size of one read of the download.
#[cfg(target_os = "macos")]
const CHUNK: usize = 64 * 1024;
/// The longest a download may take.
#[cfg(target_os = "macos")]
pub(super) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[cfg(target_os = "macos")]
const DITTO: &str = "/usr/bin/ditto";
#[cfg(target_os = "macos")]
const CODESIGN: &str = "/usr/bin/codesign";
#[cfg(target_os = "macos")]
const PLUTIL: &str = "/usr/bin/plutil";
#[cfg(target_os = "macos")]
const ZIPINFO: &str = "/usr/bin/zipinfo";

/// The bundle to replace, from the running executable, or why there is none.
#[cfg(target_os = "macos")]
pub fn running_bundle() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("the running executable is unknown: {error}"))?;
    let executable = executable.canonicalize().unwrap_or(executable);
    let not_in_bundle = || {
        format!(
            "{} is not inside <name>.app/Contents/MacOS/",
            executable.display()
        )
    };
    let macos = executable.parent().ok_or_else(not_in_bundle)?;
    let contents = macos.parent().ok_or_else(not_in_bundle)?;
    let bundle = contents.parent().ok_or_else(not_in_bundle)?;
    if macos.file_name().is_none_or(|name| name != "MacOS")
        || contents.file_name().is_none_or(|name| name != "Contents")
        || bundle
            .extension()
            .is_none_or(|extension| extension != "app")
    {
        return Err(not_in_bundle());
    }
    let folder = bundle.parent().ok_or_else(not_in_bundle)?;
    // Creating a folder beside the bundle, removed when dropped, is the one
    // reliable test that an install can stage and swap there.
    tempfile::Builder::new()
        .prefix(".probe-")
        .tempdir_in(folder)
        .map_err(|error| format!("the folder {} cannot be written: {error}", folder.display()))?;
    Ok(bundle.to_path_buf())
}

/// The bundle to replace: none on this system.
#[cfg(not(target_os = "macos"))]
pub fn running_bundle() -> Result<PathBuf, String> {
    Err(String::from(super::check::NOT_ON_THIS_SYSTEM))
}

/// Downloads, checks, and installs a release over `bundle`, as the running
/// application does after it finds a newer version; tests call it directly.
#[cfg(target_os = "macos")]
pub async fn install(
    client: Arc<dyn HttpClient>,
    bundle: PathBuf,
    name: &str,
    identifier: &str,
    version: &str,
    release_url: &str,
    sha256: &str,
) -> Result<(), String> {
    // Called directly there is no executor to time the download with; the
    // running application goes through `install_within` with its timer.
    let release = ReleaseFile {
        name,
        identifier,
        version,
        url: release_url,
        sha256,
    };
    install_within(client, bundle, release, futures::future::pending()).await
}

/// Installs nothing on this system, saying why inside `update-install`.
#[cfg(not(target_os = "macos"))]
pub async fn install(
    _client: Arc<dyn HttpClient>,
    _bundle: PathBuf,
    _name: &str,
    _identifier: &str,
    version: &str,
    _release_url: &str,
    _sha256: &str,
) -> Result<(), String> {
    tracing::info_span!("update-install", version).in_scope(|| {
        tracing::debug!("{}", super::check::NOT_ON_THIS_SYSTEM);
    });
    Err(String::from(super::check::NOT_ON_THIS_SYSTEM))
}

/// What the release file and the product say the release must be.
#[cfg(target_os = "macos")]
pub(super) struct ReleaseFile<'a> {
    pub name: &'a str,
    pub identifier: &'a str,
    pub version: &'a str,
    pub url: &'a str,
    pub sha256: &'a str,
}

/// `install`, with the download given up when `limit` ends first.
#[cfg(target_os = "macos")]
pub(super) async fn install_within(
    client: Arc<dyn HttpClient>,
    bundle: PathBuf,
    release: ReleaseFile<'_>,
    limit: impl Future<Output = ()>,
) -> Result<(), String> {
    let version = release.version;
    let host = host(release.url);
    let download_group = tracing::info_span!("update-download", version, host = %host);
    let temp = async {
        let downloaded = futures::select_biased! {
            downloaded = download(client, &bundle, &release).fuse() => downloaded,
            _ = limit.fuse() => Err(Refusal::Failed(format!(
                "the download did not end within {} minutes",
                DOWNLOAD_TIMEOUT.as_secs() / 60
            ))),
        };
        downloaded.map_err(|refusal| match refusal {
            Refusal::Expected(reason) => {
                tracing::info!("not downloaded: {reason}");
                reason
            }
            Refusal::Failed(reason) => {
                let reason = without_addresses(&reason, &host);
                log::warn!("update download of {version} from {host} failed: {reason}");
                reason
            }
        })
    }
    .instrument(download_group)
    .await?;
    tracing::info_span!("update-install", version).in_scope(|| {
        replace(&temp, &bundle, &release).map_err(|reason| {
            log::warn!("update install of {version} failed: {reason}");
            reason
        })
    })
}

/// Why nothing was downloaded: an expected refusal, or a failure.
#[cfg(target_os = "macos")]
enum Refusal {
    /// The bundle on disk is already the release or newer.
    Expected(String),
    Failed(String),
}

#[cfg(target_os = "macos")]
impl From<String> for Refusal {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

/// Whether `url` may be downloaded from: `https://`, or `http://` only to
/// `127.0.0.1` or `localhost`, as the build's `update_url`.
#[cfg(target_os = "macos")]
fn check_url(url: &str) -> Result<(), String> {
    let unreadable = || String::from("the release address is not a readable URL");
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(unreadable());
    }
    let (scheme, rest) = url.split_once("://").ok_or_else(unreadable)?;
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
        return Err(unreadable());
    }
    match scheme {
        "https" => Ok(()),
        "http" if matches!(host, "127.0.0.1" | "localhost") => Ok(()),
        "http" => Err(String::from(
            "the release address must use https://; http:// is allowed only to 127.0.0.1 or localhost",
        )),
        _ => Err(String::from("the release address must be an https:// URL")),
    }
}

/// Streams the release into a private temporary folder beside `bundle`,
/// computing its digest, and checks the digest.
#[cfg(target_os = "macos")]
async fn download(
    client: Arc<dyn HttpClient>,
    bundle: &Path,
    release: &ReleaseFile<'_>,
) -> Result<TempDir, Refusal> {
    check_url(release.url)?;
    let wanted = semver::Version::parse(release.version)
        .map_err(|error| format!("the version {:?} is unreadable: {error}", release.version))?;
    let on_disk = plist_value(
        &bundle.join("Contents/Info.plist"),
        "CFBundleShortVersionString",
    )
    .map_err(|reason| format!("the bundle on disk: {reason}"))?;
    let on_disk = semver::Version::parse(&on_disk).map_err(|error| {
        format!("the bundle on disk's version {on_disk:?} is unreadable: {error}")
    })?;
    if on_disk.cmp_precedence(&wanted) != Ordering::Less {
        return Err(Refusal::Expected(format!(
            "the bundle on disk is already version {on_disk}"
        )));
    }
    tracing::debug!(on_disk = %on_disk, "bundle on disk is older");
    let folder = bundle
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", bundle.display()))?;
    let temp = tempfile::Builder::new()
        .prefix(&format!(".{}-update-", release.name))
        .tempdir_in(folder)
        .map_err(|error| {
            format!(
                "creating a temporary folder in {} failed: {error}",
                folder.display()
            )
        })?;
    tracing::debug!(folder = %temp.path().display(), "temporary folder");
    let request = Request::get(release.url)
        .follow_redirects(RedirectPolicy::NoFollow)
        .body(AsyncBody::default())
        .map_err(|error| format!("invalid request: {error}"))?;
    let mut response = client
        .send(request)
        .await
        .map_err(|error| format!("request failed: {error:#}"))?;
    let status = response.status();
    if status.is_redirection() {
        return Err(
            format!("the server answered {status}, a redirect, which is not followed").into(),
        );
    }
    if !status.is_success() {
        return Err(format!("the server answered {status}").into());
    }
    let zip = temp.path().join("release.zip");
    let mut file = std::fs::File::create(&zip)
        .map_err(|error| format!("creating {} failed: {error}", zip.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    let mut bytes: u64 = 0;
    let body = response.body_mut();
    loop {
        let read = body
            .read(&mut buffer)
            .await
            .map_err(|error| format!("reading the release failed: {error}"))?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        if bytes > MAX_DOWNLOAD {
            return Err(format!(
                "the release is larger than {} GiB",
                MAX_DOWNLOAD / (1024 * 1024 * 1024)
            )
            .into());
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|error| format!("writing {} failed: {error}", zip.display()))?;
    }
    file.sync_all()
        .map_err(|error| format!("writing {} failed: {error}", zip.display()))?;
    tracing::debug!(bytes, "downloaded");
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !digest.eq_ignore_ascii_case(release.sha256.trim()) {
        return Err(format!(
            "the release's digest is {digest}, not the expected {}",
            release.sha256
        )
        .into());
    }
    tracing::debug!("digest matches");
    Ok(temp)
}

/// Checks the downloaded release's entries, unpacks it, checks the bundle,
/// and swaps it with `bundle`.
#[cfg(target_os = "macos")]
fn replace(temp: &TempDir, bundle: &Path, release: &ReleaseFile<'_>) -> Result<(), String> {
    let zip = temp.path().join("release.zip");
    let expected = format!("{}.app", release.name);
    check_entries(&zip, &expected)?;
    tracing::debug!(app = expected, "entries checked");

    let unpacked = temp.path().join("unpacked");
    run(Command::new(DITTO)
        .args(["-x", "-k"])
        .arg(&zip)
        .arg(&unpacked))?;
    tracing::debug!("unpacked");

    let entries: Vec<_> = std::fs::read_dir(&unpacked)
        .map_err(|error| format!("reading {} failed: {error}", unpacked.display()))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("reading {} failed: {error}", unpacked.display()))?;
    let new = match entries.as_slice() {
        [entry]
            if entry.file_name() == expected.as_str()
                && entry.file_type().is_ok_and(|kind| kind.is_dir()) =>
        {
            entry.path()
        }
        _ => {
            return Err(format!(
                "the release unpacks to {} entries, not one folder {expected}",
                entries.len()
            ));
        }
    };
    check_links(&new, &new)?;
    tracing::debug!("one application, no link outside it");

    let plist = new.join("Contents/Info.plist");
    let found = plist_value(&plist, "CFBundleIdentifier")?;
    if found != release.identifier {
        return Err(format!(
            "the release's identifier is {found:?}, not {:?}",
            release.identifier
        ));
    }
    tracing::debug!(identifier = release.identifier, "identifier matches");
    let found = plist_value(&plist, "CFBundleShortVersionString")?;
    if found != release.version {
        return Err(format!(
            "the release's bundle says version {found:?}, not {:?}",
            release.version
        ));
    }
    tracing::debug!(version = release.version, "version matches");

    run(Command::new(CODESIGN)
        .args(["--verify", "--deep", "--strict"])
        .arg(&new))?;
    tracing::debug!("signature valid");

    swap(&new, bundle)?;
    tracing::debug!(bundle = %bundle.display(), "exchanged");
    tracing::debug!("installed");
    Ok(())
}

/// Lists the zip's entries and refuses an absolute path, a `..` component,
/// an entry outside `<app>/`, or more than 2 GiB once unpacked.
#[cfg(target_os = "macos")]
fn check_entries(zip: &Path, app: &str) -> Result<(), String> {
    let listing = output(Command::new(ZIPINFO).arg("-1").arg(zip))?;
    let prefix = format!("{app}/");
    for entry in String::from_utf8_lossy(&listing).lines() {
        if entry.starts_with('/') {
            return Err(format!("the release holds an absolute path {entry:?}"));
        }
        if entry.split('/').any(|component| component == "..") {
            return Err(format!("the release holds a path with `..`: {entry:?}"));
        }
        if !entry.starts_with(&prefix) {
            return Err(format!("the release holds {entry:?}, outside {prefix}"));
        }
    }
    let totals = output(Command::new(ZIPINFO).arg("-t").arg(zip))?;
    let totals = String::from_utf8_lossy(&totals);
    let unpacked = totals
        .split(',')
        .find_map(|part| {
            part.trim()
                .strip_suffix("bytes uncompressed")
                .and_then(|bytes| bytes.trim().parse::<u64>().ok())
        })
        .ok_or_else(|| format!("zipinfo gave no unpacked size: {}", totals.trim()))?;
    if unpacked > MAX_UNPACKED {
        return Err(format!(
            "the release is larger than {} GiB once unpacked",
            MAX_UNPACKED / (1024 * 1024 * 1024)
        ));
    }
    Ok(())
}

/// Refuses a symbolic link under `dir` whose target leaves `root`.
#[cfg(target_os = "macos")]
fn check_links(root: &Path, dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("reading {} failed: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("reading {} failed: {error}", dir.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("reading {} failed: {error}", path.display()))?;
        if kind.is_symlink() {
            let target = std::fs::read_link(&path)
                .map_err(|error| format!("reading {} failed: {error}", path.display()))?;
            let inside = path
                .parent()
                .and_then(|parent| parent.strip_prefix(root).ok())
                .is_some_and(|parent| stays_inside(&parent.join(&target)));
            if !inside {
                return Err(format!(
                    "the link {} points outside the bundle, to {}",
                    path.strip_prefix(root).unwrap_or(&path).display(),
                    target.display()
                ));
            }
        } else if kind.is_dir() {
            check_links(root, &path)?;
        }
    }
    Ok(())
}

/// Whether `relative`, a path from the bundle's root, stays inside it.
#[cfg(target_os = "macos")]
fn stays_inside(relative: &Path) -> bool {
    let mut depth: usize = 0;
    for component in relative.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => match depth.checked_sub(1) {
                Some(up) => depth = up,
                None => return false,
            },
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Swaps `new` and `bundle` in one atomic step, so that `bundle` is a
/// complete bundle at every moment; the old one ends where `new` was.
#[cfg(target_os = "macos")]
fn swap(new: &Path, bundle: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = |path: &Path| {
        std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| format!("{} holds a NUL byte", path.display()))
    };
    let (from, to) = (c_path(new)?, c_path(bundle)?);
    // SAFETY: both are NUL-terminated paths that live across the call.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "exchanging the release with {} failed: {}",
            bundle.display(),
            std::io::Error::last_os_error()
        ))
    }
}

/// `key`'s string value in the property list at `plist`.
#[cfg(target_os = "macos")]
fn plist_value(plist: &Path, key: &str) -> Result<String, String> {
    let value = output(
        Command::new(PLUTIL)
            .args(["-extract", key, "raw", "-expect", "string", "-o", "-"])
            .arg(plist),
    )
    .map_err(|reason| format!("Info.plist has no string {key}: {reason}"))?;
    Ok(String::from_utf8_lossy(&value).trim().to_owned())
}

/// Runs `command` to success, or says how it failed.
#[cfg(target_os = "macos")]
fn run(command: &mut Command) -> Result<(), String> {
    output(command).map(drop)
}

/// The standard output of `command` run to success, or how it failed.
#[cfg(target_os = "macos")]
fn output(command: &mut Command) -> Result<Vec<u8>, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|error| format!("running {program} failed: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "{program} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
