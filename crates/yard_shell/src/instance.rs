//! One instance of the application: a second start hands its arguments to the
//! instance already running, then exits. The running instance opens the URLs
//! among them and brings its main window to the front.
//!
//! The request format and its handling are the same on every system; only the
//! transport differs. On Linux it is a Unix datagram socket in a private
//! folder, as in Zed's `listen_for_cli_connections`. On Windows it is a named
//! mutex and a named pipe, as in Zed's `windows_only_instance`. Other systems
//! run without one instance.

use futures::StreamExt as _;
use gpui::App;

/// The first byte of every request, which says how the rest is written.
pub const REQUEST_VERSION: u8 = 1;

/// The largest request, in bytes, version included.
pub const MAX_REQUEST_LEN: usize = 64 * 1024;

/// Why a request cannot be written or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// The request holds more bytes than [`MAX_REQUEST_LEN`].
    TooLarge(usize),
    /// The request has no version byte.
    Empty,
    /// The request is written in a version this build does not read.
    Version(u8),
    /// The last argument has no NUL after it.
    Unterminated,
    /// An argument is not UTF-8.
    NotUtf8,
    /// An argument holds a NUL, which ends arguments in a request.
    Nul,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(len) => write!(
                f,
                "the request is {len} bytes, more than the {MAX_REQUEST_LEN} the running instance reads"
            ),
            Self::Empty => write!(f, "the request is empty"),
            Self::Version(version) => write!(
                f,
                "the request is version {version}, and this build reads version {REQUEST_VERSION}"
            ),
            Self::Unterminated => write!(f, "the request's last argument is not terminated"),
            Self::NotUtf8 => write!(f, "an argument of the request is not UTF-8"),
            Self::Nul => write!(f, "an argument holds a NUL"),
        }
    }
}

impl std::error::Error for RequestError {}

/// Writes the arguments as one request: [`REQUEST_VERSION`], then each
/// argument in UTF-8 followed by a NUL.
pub fn encode_request(arguments: &[String]) -> Result<Vec<u8>, RequestError> {
    let len = 1 + arguments
        .iter()
        .map(|argument| argument.len() + 1)
        .sum::<usize>();
    if len > MAX_REQUEST_LEN {
        return Err(RequestError::TooLarge(len));
    }
    let mut request = Vec::with_capacity(len);
    request.push(REQUEST_VERSION);
    for argument in arguments {
        if argument.contains('\0') {
            return Err(RequestError::Nul);
        }
        request.extend_from_slice(argument.as_bytes());
        request.push(0);
    }
    Ok(request)
}

/// Reads the arguments of a request that [`encode_request`] wrote.
pub fn decode_request(request: &[u8]) -> Result<Vec<String>, RequestError> {
    if request.len() > MAX_REQUEST_LEN {
        return Err(RequestError::TooLarge(request.len()));
    }
    let (&version, rest) = request.split_first().ok_or(RequestError::Empty)?;
    if version != REQUEST_VERSION {
        return Err(RequestError::Version(version));
    }
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let rest = rest.strip_suffix(&[0]).ok_or(RequestError::Unterminated)?;
    rest.split(|&byte| byte == 0)
        .map(|argument| String::from_utf8(argument.to_vec()).map_err(|_| RequestError::NotUtf8))
        .collect()
}

/// The arguments that are URLs of the product's own `scheme`, in any case:
/// only those reach an extension's `on_open_url`, as on macOS. The others
/// are counted in the log, never shown, since they may hold secrets.
pub fn argument_urls(arguments: &[String], scheme: &str) -> Vec<String> {
    let urls: Vec<String> = arguments
        .iter()
        .filter(|argument| is_url(argument, scheme))
        .cloned()
        .collect();
    let ignored = arguments.len() - urls.len();
    if ignored > 0 {
        tracing::debug!(ignored, "not a URL, ignored");
    }
    urls
}

fn is_url(argument: &str, scheme: &str) -> bool {
    !scheme.is_empty()
        && argument
            .split_once(':')
            .is_some_and(|(prefix, _)| prefix.eq_ignore_ascii_case(scheme))
}

/// The name of the one instance's mutex and pipe on Windows: the
/// installer's [`yard_product::instance_name`] when the directories are the
/// platform's, and otherwise that name with a hash of the data folder, so
/// that a run with a home of its own never meets the installed app, nor a
/// run with another home.
pub fn instance_name(paths: &yard_paths::Paths, identifier: &str) -> String {
    let name = yard_product::instance_name(identifier);
    if paths.origin() == yard_paths::Origin::Platform {
        return name;
    }
    // FNV-1a, which gives the same hash in every build.
    let hash = paths
        .data_dir()
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{name}-{hash:016x}")
}

/// What [`check_instance`] found.
#[derive(Debug)]
pub enum Instance {
    /// The running instance took the arguments; this start exits.
    Forwarded,
    /// An instance runs but did not take the arguments, for the reason
    /// given; this start exits with an error.
    Refused(String),
    /// This start is the one instance, and listens for the next.
    Listening(InstanceListener),
    /// This start runs without one instance, for the reason given.
    Alone(String),
}

/// What the one instance listens on, until [`serve_instance_requests`]
/// receives from it.
#[derive(Debug)]
pub struct InstanceListener {
    #[cfg(unix)]
    inner: unix::Listener,
    #[cfg(windows)]
    inner: windows::Listener,
    #[cfg(not(any(unix, windows)))]
    never: std::convert::Infallible,
}

/// Hands the command line's URLs of the product's `scheme` to the instance
/// already running, or, when none runs, makes this start the one instance.
/// The other arguments are never sent; with no URL, the running instance
/// only comes to the front. Returns the URLs too, for this start to open
/// when it is the one instance. Windows names the instance from the
/// product's `identifier`, as [`instance_name`] does.
pub fn start_instance(
    paths: &yard_paths::Paths,
    identifier: &str,
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
    scheme: &str,
) -> (Instance, Vec<String>) {
    // An argument that is not UTF-8 is no URL; it stays to be counted.
    let arguments: Vec<String> = arguments
        .into_iter()
        .map(|argument| argument.into_string().unwrap_or_default())
        .collect();
    let urls = argument_urls(&arguments, scheme);
    (check_instance(paths, identifier, &urls), urls)
}

/// Hands the arguments to the instance already running, or, when none runs,
/// makes this start the one instance.
pub fn check_instance(
    paths: &yard_paths::Paths,
    identifier: &str,
    arguments: &[String],
) -> Instance {
    #[cfg(windows)]
    {
        // The group names the pipe, as it names the socket on Linux.
        let name = instance_name(paths, identifier);
        match windows::Names::new(&name) {
            Ok(names) => {
                let _group = tracing::info_span!("instance-check", socket = %names.pipe).entered();
                windows::check(&names, arguments)
            }
            Err(reason) => {
                let _group = tracing::info_span!("instance-check", socket = %name).entered();
                windows::alone(reason)
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = identifier;
        let socket = paths.instance_socket();
        let _group = tracing::info_span!("instance-check", socket = %socket.display()).entered();
        #[cfg(target_os = "linux")]
        {
            unix::check(
                &paths.instance_dir(),
                &socket,
                &paths.instance_lock(),
                arguments,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = arguments;
            let reason = "no single instance on this system";
            tracing::debug!(reason);
            Instance::Alone(reason.into())
        }
    }
}

/// Receives the requests of later starts off the main thread and handles
/// each on it, as [`handle_instance_requests`] does. The socket is removed
/// when the app quits, if it is still this instance's.
pub fn serve_instance_requests(cx: &mut App, listener: InstanceListener, scheme: &str) {
    #[cfg(unix)]
    unix::serve(cx, listener.inner, scheme);
    #[cfg(windows)]
    windows::serve(cx, listener.inner, scheme);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (cx, scheme);
        match listener.never {}
    }
}

/// Handles each request a transport receives, on the main thread, in the
/// group `instance-request`: the URLs of the product's `scheme` among its
/// arguments go to [`open_urls`](crate::open_urls), then the main window
/// comes to the front. A request that cannot be read is refused.
pub fn handle_instance_requests(
    cx: &mut App,
    mut requests: futures::channel::mpsc::UnboundedReceiver<Vec<u8>>,
    scheme: &str,
) {
    let scheme = scheme.to_owned();
    cx.spawn(async move |cx| {
        while let Some(request) = requests.next().await {
            cx.update(|cx| handle_request(cx, &request, &scheme));
        }
    })
    .detach();
}

fn handle_request(cx: &mut App, request: &[u8], scheme: &str) {
    let _group = tracing::info_span!("instance-request").entered();
    let arguments = match decode_request(request) {
        Ok(arguments) => arguments,
        Err(error) => {
            log::warn!("a request from another start was refused: {error}");
            return;
        }
    };
    let urls = argument_urls(&arguments, scheme);
    tracing::debug!(urls = urls.len(), "received");
    if !urls.is_empty() {
        crate::open_urls(cx, urls);
    }
    activate_main_window(cx);
}

/// Asks the system to bring to the front the window the user last used, or
/// else the first.
fn activate_main_window(cx: &mut App) {
    let Some(window) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        tracing::debug!("no window to activate");
        return;
    };
    if window
        .update(cx, |_, window, _| window.activate_window())
        .is_err()
    {
        tracing::debug!("the main window closed meanwhile");
        return;
    }
    // GPUI's `activate` does nothing on Linux or Windows; there,
    // `activate_window` alone asks for the focus. On Windows it restores a
    // minimized window and asks for the foreground. The later start called
    // `AllowSetForegroundWindow` for this process, so Windows usually grants
    // it, but it may still refuse.
    // TODO(linux-app-activate): on Linux the window manager or compositor
    // decides whether `activate_window` takes the focus: on X11 it sends
    // `_NET_ACTIVE_WINDOW`, and on Wayland it asks for an `xdg_activation_v1`
    // token, granted only with a recent input serial. A GPUI that implements
    // `activate` on Linux unblocks it.
    cx.activate(true);
    tracing::debug!("activation requested");
}

#[cfg(unix)]
// Only Linux checks for the instance; the rest is built on every Unix so
// that its tests run there too.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod unix {
    use super::{
        Instance, InstanceListener, MAX_REQUEST_LEN, RequestError, encode_request,
        handle_instance_requests,
    };
    use futures::channel::mpsc::UnboundedReceiver;
    use gpui::App;
    use std::{
        fs,
        io::ErrorKind,
        os::unix::{
            fs::{
                DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _,
                PermissionsExt as _,
            },
            net::{SocketAddr, UnixDatagram},
        },
        path::{Path, PathBuf},
        time::Duration,
    };

    /// How long a second start waits for a full queue of the running
    /// instance before it gives up.
    const SEND_TIMEOUT: Duration = Duration::from_secs(2);

    #[derive(Debug)]
    pub(super) struct Listener {
        requests: UnboundedReceiver<Vec<u8>>,
        path: PathBuf,
        lock: PathBuf,
        id: SocketId,
    }

    impl Listener {
        #[cfg(test)]
        pub(super) fn requests_for_tests(&mut self) -> &mut UnboundedReceiver<Vec<u8>> {
            &mut self.requests
        }
    }

    /// What tells one socket at the instance path from another: its device
    /// and inode, which Linux reuses as soon as a socket is removed, and the
    /// modification time [`mark_socket`] gave it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct SocketId {
        dev: u64,
        ino: u64,
        mtime: i64,
        mtime_nsec: i64,
    }

    impl SocketId {
        pub(super) fn of(metadata: &fs::Metadata) -> Self {
            Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
                mtime: metadata.mtime(),
                mtime_nsec: metadata.mtime_nsec(),
            }
        }
    }

    /// Gives the socket at `path` a modification time of its own, which no
    /// other socket bound there gets, even with the same inode within the
    /// same clock tick, and returns its identity.
    pub(super) fn mark_socket(path: &Path) -> std::io::Result<SocketId> {
        use std::os::unix::ffi::OsStrExt as _;
        static MARKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let mark = MARKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = now
            .subsec_nanos()
            .wrapping_add(std::process::id().wrapping_mul(2_654_435_761))
            .wrapping_add(mark.wrapping_mul(7_919))
            % 1_000_000_000;
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            },
            libc::timespec {
                tv_sec: now.as_secs() as libc::time_t,
                tv_nsec: nanos as _,
            },
        ];
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `path_c` is NUL-terminated and `times` holds two values.
        let result = unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                path_c.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        fs::symlink_metadata(path).map(|metadata| SocketId::of(&metadata))
    }

    /// The user this process runs as.
    pub(super) fn own_uid() -> u32 {
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// Looks for the instance on `socket` and hands it the arguments, or,
    /// when none listens, binds `socket` and receives on a thread of its
    /// own. The lock is held from the second look to the bind, so two
    /// starts never both bind, and none removes the socket of another that
    /// just bound it.
    pub(super) fn check(dir: &Path, socket: &Path, lock: &Path, arguments: &[String]) -> Instance {
        check_with(dir, socket, lock, arguments, receive)
    }

    pub(super) fn check_with(
        dir: &Path,
        socket: &Path,
        lock: &Path,
        arguments: &[String],
        spawn: impl FnOnce(UnixDatagram) -> std::io::Result<UnboundedReceiver<Vec<u8>>>,
    ) -> Instance {
        let uid = own_uid();
        if let Err(error) = SocketAddr::from_pathname(socket) {
            return alone(format!(
                "the instance socket's path cannot be used ({error})"
            ));
        }
        if let Err(reason) = private_folder(dir, uid) {
            return alone(reason);
        }
        let request = encode_request(arguments);
        if let Some(instance) = send(socket, &request, uid) {
            return instance;
        }
        let _lock = match take_lock(lock, uid) {
            Ok(lock) => lock,
            Err(reason) => return alone(reason),
        };
        // Another start may have bound the socket while this one waited.
        if let Some(instance) = send(socket, &request, uid) {
            return instance;
        }
        match fs::symlink_metadata(socket) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return alone(format!("the instance socket cannot be read ({error})"));
            }
            Ok(metadata) if !is_own_socket(&metadata, uid) => {
                return alone(
                    "the instance socket's path holds something else, left as it is".into(),
                );
            }
            Ok(_) => {
                if let Err(error) = fs::remove_file(socket) {
                    return alone(format!(
                        "the stale instance socket cannot be removed ({error})"
                    ));
                }
                tracing::debug!("stale socket removed");
            }
        }
        let listener = match UnixDatagram::bind(socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                if let Some(instance) = send(socket, &request, uid) {
                    return instance;
                }
                return alone(format!("the instance socket cannot be bound ({error})"));
            }
            Err(error) => return alone(format!("the instance socket cannot be bound ({error})")),
        };
        let id = match fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
            .and_then(|()| mark_socket(socket))
        {
            Ok(id) => id,
            Err(error) => {
                drop(listener);
                let _ = fs::remove_file(socket);
                return alone(format!(
                    "the instance socket cannot be made private ({error})"
                ));
            }
        };
        let requests = match spawn(listener) {
            Ok(requests) => requests,
            Err(error) => {
                let _ = fs::remove_file(socket);
                let reason = format!("the instance listener did not start ({error})");
                log::error!("{reason}; running without a single instance");
                return Instance::Alone(reason);
            }
        };
        tracing::debug!("listening");
        Instance::Listening(InstanceListener {
            inner: Listener {
                requests,
                path: socket.to_owned(),
                lock: lock.to_owned(),
                id,
            },
        })
    }

    fn alone(reason: String) -> Instance {
        log::warn!("{reason}; running without a single instance");
        Instance::Alone(reason)
    }

    /// Opens and takes the lock at `path`, which must be a file of `uid`.
    /// It never follows a link, and never waits on a FIFO left there.
    pub(super) fn take_lock(path: &Path, uid: u32) -> Result<fs::File, String> {
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| format!("the instance lock cannot be opened ({error})"))?;
        let metadata = lock
            .metadata()
            .map_err(|error| format!("the instance lock cannot be read ({error})"))?;
        if !metadata.is_file() || metadata.uid() != uid {
            return Err("the instance lock's path holds something else".into());
        }
        lock.lock()
            .map_err(|error| format!("the instance lock cannot be taken ({error})"))?;
        Ok(lock)
    }

    /// Sends the request to the instance listening on `socket`, and says
    /// what came of it, or `None` when no instance listens there, or one
    /// may be starting.
    fn send(socket: &Path, request: &Result<Vec<u8>, RequestError>, uid: u32) -> Option<Instance> {
        match fs::symlink_metadata(socket) {
            Err(error) if error.kind() == ErrorKind::NotFound => return None,
            Err(error) => {
                return Some(alone(format!(
                    "the instance socket cannot be read ({error})"
                )));
            }
            // URLs may hold secrets: they go only to this user's socket.
            Ok(metadata) if !is_own_socket(&metadata, uid) => {
                return Some(alone(
                    "the instance socket's path holds something else, left as it is".into(),
                ));
            }
            Ok(_) => {}
        }
        let sender = match UnixDatagram::unbound().and_then(|sender| {
            sender
                .set_write_timeout(Some(SEND_TIMEOUT))
                .map(|()| sender)
        }) {
            Ok(sender) => sender,
            Err(error) => {
                return Some(alone(format!(
                    "no socket to reach another instance ({error})"
                )));
            }
        };
        match sender.connect(socket) {
            Ok(()) => {}
            // Refused: a stale socket. Denied: a socket bound but not yet
            // made private, so its instance is starting.
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConnectionRefused
                        | ErrorKind::NotFound
                        | ErrorKind::PermissionDenied
                ) =>
            {
                return None;
            }
            Err(error) => {
                return Some(alone(format!(
                    "the instance socket cannot be reached ({error})"
                )));
            }
        }
        let request = match request {
            Ok(request) => request,
            Err(error) => return Some(refused(error.to_string())),
        };
        match sender.send(request) {
            Ok(_) => {
                tracing::debug!(bytes = request.len(), "sent to the running instance");
                Some(Instance::Forwarded)
            }
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => None,
            Err(error) => Some(refused(error.to_string())),
        }
    }

    fn refused(error: String) -> Instance {
        let reason = format!("the running instance was not reached: {error}");
        log::warn!("{reason}");
        Instance::Refused(reason)
    }

    /// Whether `metadata` is a socket that belongs to `uid`.
    pub(super) fn is_own_socket(metadata: &fs::Metadata, uid: u32) -> bool {
        metadata.file_type().is_socket() && metadata.uid() == uid
    }

    /// The group this process runs as.
    pub(super) fn own_egid() -> u32 {
        // SAFETY: `getegid` has no preconditions and cannot fail.
        unsafe { libc::getegid() }
    }

    /// Whether no one but `uid` can swap what the data folder holds: it
    /// belongs to `uid`, others may not write to it, and its group may write
    /// to it only when that group is `egid`, a group of the user's own as
    /// Ubuntu gives each user.
    pub(super) fn data_folder_is_safe(
        mode: u32,
        owner: u32,
        group: u32,
        uid: u32,
        egid: u32,
    ) -> Result<(), String> {
        if owner != uid {
            return Err("the data folder belongs to another user".into());
        }
        if mode & 0o002 != 0 {
            return Err("others may write to the data folder".into());
        }
        if mode & 0o020 != 0 && group != egid {
            return Err("another group may write to the data folder".into());
        }
        Ok(())
    }

    /// Makes `dir` a folder of `uid` that only `uid` may enter, in a folder
    /// of `uid` that no one else may write to, so no one can swap it.
    pub(super) fn private_folder(dir: &Path, uid: u32) -> Result<(), String> {
        if let Some(parent) = dir.parent() {
            if !parent.exists() {
                // A data folder this start makes is private whatever the
                // umask, so a fresh install never has it group-writable.
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .and_then(|()| fs::set_permissions(parent, fs::Permissions::from_mode(0o700)))
                    .map_err(|error| format!("the data folder cannot be created ({error})"))?;
            }
            let metadata = fs::metadata(parent)
                .map_err(|error| format!("the data folder cannot be read ({error})"))?;
            data_folder_is_safe(
                metadata.mode(),
                metadata.uid(),
                metadata.gid(),
                uid,
                own_egid(),
            )?;
        }
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!("the instance folder cannot be created ({error})"));
            }
        }
        let metadata = fs::symlink_metadata(dir)
            .map_err(|error| format!("the instance folder cannot be read ({error})"))?;
        if !metadata.is_dir() {
            return Err("the instance folder's path is not a folder".into());
        }
        if metadata.uid() != uid {
            return Err("the instance folder belongs to another user".into());
        }
        if metadata.mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("the instance folder cannot be made private ({error})"))?;
            tracing::debug!("instance folder made private");
        }
        Ok(())
    }

    pub(super) fn serve(cx: &mut App, listener: Listener, scheme: &str) {
        let Listener {
            requests,
            path,
            lock,
            id,
        } = listener;
        handle_instance_requests(cx, requests, scheme);
        cx.on_app_quit(move |_| {
            remove_own_socket(&path, &lock, id);
            async {}
        })
        .detach();
    }

    /// Removes the socket at `path` if it is still the one with `id`, under
    /// the lock, so that no start binds it between the check and the removal.
    pub(super) fn remove_own_socket(path: &Path, lock: &Path, id: SocketId) {
        let _lock = match take_lock(lock, own_uid()) {
            Ok(lock) => lock,
            Err(reason) => {
                log::warn!("the instance socket was left: {reason}");
                return;
            }
        };
        match fs::symlink_metadata(path) {
            Ok(metadata) if SocketId::of(&metadata) == id => {
                if let Err(error) = fs::remove_file(path) {
                    log::warn!("the instance socket was not removed: {error}");
                }
            }
            Ok(_) => tracing::debug!("the instance socket is another's, left"),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => log::warn!("the instance socket cannot be read: {error}"),
        }
    }

    /// Receives each datagram on a thread of its own, and sends it on.
    pub(super) fn receive(socket: UnixDatagram) -> std::io::Result<UnboundedReceiver<Vec<u8>>> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        std::thread::Builder::new()
            .name("instance-listener".into())
            .spawn(move || {
                // One byte more than a request may hold shows a request that
                // is too large, which the datagram would cut.
                let mut buffer = vec![0; MAX_REQUEST_LEN + 1];
                loop {
                    match socket.recv(&mut buffer) {
                        Ok(len) => {
                            if tx.unbounded_send(buffer[..len].to_vec()).is_err() {
                                return;
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => {}
                        Err(error) => {
                            log::warn!(
                                "the instance socket stopped receiving ({error}); later starts run on their own"
                            );
                            return;
                        }
                    }
                }
            })?;
        Ok(rx)
    }
}

#[cfg(windows)]
mod windows {
    use super::{
        Instance, InstanceListener, MAX_REQUEST_LEN, RequestError, encode_request,
        handle_instance_requests,
    };
    use ::windows::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE,
                ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_NO_DATA,
                ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GetLastError,
                HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0, WIN32_ERROR,
            },
            Security::{
                Authorization::{
                    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                    SDDL_REVISION_1,
                },
                GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
                TOKEN_USER, TokenUser,
            },
            Storage::FileSystem::{
                CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
                FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING,
                PIPE_ACCESS_INBOUND, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
                WriteFile,
            },
            System::{
                IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
                Pipes::{
                    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                    GetNamedPipeServerProcessId, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_TYPE_MESSAGE, PIPE_WAIT, WaitNamedPipeW,
                },
                RemoteDesktop::ProcessIdToSessionId,
                Threading::{
                    CreateEventW, CreateMutexW, GetCurrentProcess, GetCurrentProcessId, OpenMutexW,
                    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
                    SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
                },
            },
            UI::WindowsAndMessaging::AllowSetForegroundWindow,
        },
        core::{HSTRING, PWSTR},
    };
    use futures::channel::mpsc::UnboundedReceiver;
    use gpui::App;
    use std::time::{Duration, Instant};

    /// How long a second start waits for the running instance's pipe before
    /// it gives up.
    const SEND_TIMEOUT: Duration = Duration::from_secs(2);

    /// How long a second start waits before it looks again for a pipe that
    /// the starting instance has not created yet.
    const RETRY: Duration = Duration::from_millis(50);

    /// The names of the instance's mutex and pipe.
    #[derive(Debug, Clone)]
    pub(super) struct Names {
        /// The mutex, per logon session, that the installer looks for too.
        pub(super) mutex: String,
        /// The pipe, named with the session, since pipe names are shared by
        /// every user of the machine.
        pub(super) pipe: String,
    }

    impl Names {
        pub(super) fn new(name: &str) -> Result<Self, String> {
            let mut session = 0;
            // SAFETY: `session` is a valid place for the id, and the process
            // id is this process's.
            unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }
                .map_err(|error| format!("this process's session is unknown ({error})"))?;
            Ok(Self {
                mutex: format!(r"Local\{name}"),
                pipe: format!(r"\\.\pipe\{name}-{session}"),
            })
        }
    }

    #[derive(Debug)]
    pub(super) struct Listener {
        pub(super) requests: UnboundedReceiver<Vec<u8>>,
    }

    /// A handle this module opened, closed when dropped.
    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by this module and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// The server end of the instance pipe, which the listener's thread owns.
    pub(super) struct Pipe(Handle);

    // SAFETY: a pipe handle may be used from any thread, and only the
    // listener's thread uses this one.
    unsafe impl Send for Pipe {}

    fn is(error: &::windows::core::Error, code: WIN32_ERROR) -> bool {
        error.code() == code.to_hresult()
    }

    /// Creates the instance mutex: if it is new, this start is the one
    /// instance and listens on the pipe; if it exists, an instance runs, and
    /// this start hands it the arguments. If that instance ends before it
    /// is reached, the mutex is created again, within the same
    /// [`SEND_TIMEOUT`].
    pub(super) fn check(names: &Names, arguments: &[String]) -> Instance {
        let request = encode_request(arguments);
        let deadline = Instant::now() + SEND_TIMEOUT;
        let mutex_name = HSTRING::from(&names.mutex);
        loop {
            // SAFETY: the name is a valid string, alive until after the
            // last error is read, and the mutex takes the default security
            // of this process.
            let created = unsafe { CreateMutexW(None, false, &mutex_name) };
            // SAFETY: reads the error `CreateMutexW` left on this thread.
            let status = unsafe { GetLastError() };
            let sent = match created {
                Ok(mutex) if status != ERROR_ALREADY_EXISTS => {
                    return listen(names, Handle(mutex));
                }
                Ok(mutex) => {
                    drop(Handle(mutex));
                    send(&names.pipe, &mutex_name, &request, deadline)
                }
                // A mutex of a process this one may not open, such as the
                // same user's elevated instance: it runs, so the pipe is
                // tried.
                Err(error) if is(&error, ERROR_ACCESS_DENIED) => {
                    send(&names.pipe, &mutex_name, &request, deadline)
                }
                Err(error) => {
                    return alone(format!("the instance mutex cannot be created ({error})"));
                }
            };
            match sent {
                Sent::Done(instance) => return instance,
                Sent::Gone => tracing::debug!("the running instance ended, looking again"),
            }
        }
    }

    /// Makes this start, which created `mutex`, the one instance.
    fn listen(names: &Names, mutex: Handle) -> Instance {
        let pipe = match create_pipe(&names.pipe) {
            Ok(pipe) => pipe,
            Err(reason) => {
                // Without a pipe the mutex would only make later starts wait
                // for nothing.
                drop(mutex);
                tracing::debug!("instance mutex released");
                return alone(reason);
            }
        };
        let requests = match receive(pipe) {
            Ok(requests) => requests,
            Err(error) => {
                drop(mutex);
                tracing::debug!("instance mutex released");
                let reason = format!("the instance listener did not start ({error})");
                log::error!("{reason}; running without a single instance");
                return Instance::Alone(reason);
            }
        };
        // The mutex handle is never closed, so the mutex lives as long as
        // this process, which the installer's `AppMutex` checks.
        std::mem::forget(mutex);
        tracing::debug!("listening");
        Instance::Listening(InstanceListener {
            inner: Listener { requests },
        })
    }

    pub(super) fn alone(reason: String) -> Instance {
        log::warn!("{reason}; running without a single instance");
        Instance::Alone(reason)
    }

    fn refused(error: String) -> Instance {
        let reason = format!("the running instance was not reached: {error}");
        log::warn!("{reason}");
        Instance::Refused(reason)
    }

    /// The user `process` runs as, as a SID string.
    fn user_of(process: HANDLE) -> ::windows::core::Result<String> {
        let mut token = HANDLE::default();
        // SAFETY: `process` may be queried, and `token` receives a handle
        // that is closed below.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }?;
        let token = Handle(token);
        let mut len = 0;
        // SAFETY: asks only for the size, which fails with that size.
        let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut len) };
        // `u64`s, so the `TOKEN_USER` at the start is aligned.
        let mut buffer = vec![0_u64; (len as usize).div_ceil(8)];
        // SAFETY: `buffer` holds at least `len` writable bytes.
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                len,
                &mut len,
            )
        }?;
        // SAFETY: `GetTokenInformation` wrote a `TOKEN_USER` at the start of
        // `buffer`, whose SID points into `buffer`, still alive below.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut string = PWSTR::null();
        // SAFETY: `sid` is valid, and `string` receives a string freed below.
        unsafe { ConvertSidToStringSidW(sid, &mut string) }?;
        // SAFETY: `ConvertSidToStringSidW` wrote a NUL-terminated string.
        let user = String::from_utf16_lossy(unsafe { string.as_wide() });
        // SAFETY: `ConvertSidToStringSidW` allocated the string with
        // `LocalAlloc`, and nothing uses it after this.
        unsafe { LocalFree(Some(HLOCAL(string.0.cast()))) };
        Ok(user)
    }

    /// The user this process runs as.
    fn own_user() -> ::windows::core::Result<String> {
        // SAFETY: `GetCurrentProcess` returns a pseudo handle, which needs no
        // closing.
        user_of(unsafe { GetCurrentProcess() })
    }

    /// Creates the first and only instance of the pipe, which only this
    /// user and the system may open, and no other machine.
    pub(super) fn create_pipe(name: &str) -> Result<Pipe, String> {
        let user = own_user()
            .map_err(|error| format!("the instance pipe cannot be made private ({error})"))?;
        // A protected DACL, so that nothing is inherited into it.
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{user})(A;;GA;;;SY)"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid string, and `descriptor` receives memory
        // freed below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &sddl,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|error| format!("the instance pipe cannot be made private ({error})"))?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let name = HSTRING::from(name);
        // SAFETY: the name is a valid string, alive until after the last
        // error is read, and `attributes` points to a descriptor alive until
        // after the call.
        let pipe = unsafe {
            CreateNamedPipeW(
                &name,
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                MAX_REQUEST_LEN as u32,
                0,
                Some(&attributes),
            )
        };
        let error = ::windows::core::Error::from_thread();
        // SAFETY: the descriptor was allocated with `LocalAlloc`, and the
        // pipe keeps a copy of its own.
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        if pipe.is_invalid() {
            return Err(format!("the instance pipe cannot be created ({error})"));
        }
        Ok(Pipe(Handle(pipe)))
    }

    /// What came of [`send`].
    pub(super) enum Sent {
        Done(Instance),
        /// The mutex is gone: the instance ended before it had a pipe.
        Gone,
    }

    /// Whether the mutex named `name` exists.
    fn mutex_exists(name: &HSTRING) -> bool {
        // SAFETY: the name is a valid string, and the handle is closed when
        // dropped.
        match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, name) } {
            Ok(mutex) => {
                drop(Handle(mutex));
                true
            }
            Err(error) => !is(&error, ERROR_FILE_NOT_FOUND),
        }
    }

    fn timed_out() -> Instance {
        refused(format!(
            "it did not answer within {} seconds",
            SEND_TIMEOUT.as_secs()
        ))
    }

    /// Hands the request to the instance listening on `pipe` before
    /// `deadline`: opening the pipe, while it does not exist yet or is busy,
    /// checking its user, and writing.
    pub(super) fn send(
        pipe: &str,
        mutex: &HSTRING,
        request: &Result<Vec<u8>, RequestError>,
        deadline: Instant,
    ) -> Sent {
        let request = match request {
            Ok(request) => request,
            Err(error) => return Sent::Done(refused(error.to_string())),
        };
        let name = HSTRING::from(pipe);
        let client = loop {
            if Instant::now() >= deadline {
                return Sent::Done(timed_out());
            }
            // SAFETY: the name is a valid string, and the handle is closed
            // when `client` drops. The server may only identify this
            // process, never act as it.
            let opened = unsafe {
                CreateFileW(
                    &name,
                    (FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES).0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION | FILE_FLAG_OVERLAPPED,
                    None,
                )
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            match opened {
                Ok(client) => break Handle(client),
                Err(error) if is(&error, ERROR_FILE_NOT_FOUND) => {
                    if !mutex_exists(mutex) {
                        return Sent::Gone;
                    }
                    tracing::debug!("no pipe yet, the instance may be starting");
                    std::thread::sleep(RETRY.min(remaining));
                }
                Err(error) if is(&error, ERROR_PIPE_BUSY) => {
                    tracing::debug!("the pipe is busy with another start");
                    let wait = remaining.as_millis().clamp(1, u32::MAX as u128) as u32;
                    // SAFETY: the name is a valid string.
                    if let Err(error) = unsafe { WaitNamedPipeW(&name, wait) }.ok() {
                        tracing::debug!("the pipe stayed busy ({error})");
                    }
                }
                Err(error) => {
                    return Sent::Done(refused(format!("the pipe cannot be opened ({error})")));
                }
            }
        };
        Sent::Done(hand_over(client, request, deadline))
    }

    /// Checks that the server of `client` runs as this user, lets it take
    /// the foreground, and writes the request, all before `deadline`.
    fn hand_over(client: Handle, request: &[u8], deadline: Instant) -> Instance {
        let mut server = 0;
        // SAFETY: `client` is an open pipe, and `server` a valid place.
        if let Err(error) = unsafe { GetNamedPipeServerProcessId(client.0, &mut server) } {
            return refused(format!("the pipe's process is unknown ({error})"));
        }
        // URLs may hold secrets: they go only to a process of this user.
        let own = match own_user() {
            Ok(own) => own,
            Err(error) => return refused(format!("this process's user is unknown ({error})")),
        };
        match server_user(server) {
            Ok(user) if user == own => {}
            Ok(_) => return refused("the pipe belongs to another user".into()),
            // An elevated process, for one, does not let others read its
            // token.
            Err(error) => {
                return refused(format!("the pipe's owner could not be checked ({error})"));
            }
        }
        if Instant::now() >= deadline {
            return timed_out();
        }
        // Windows lets a process take the foreground only from the one that
        // has it, or one that this allowed, and may refuse it even then.
        // SAFETY: takes a process id only.
        if let Err(error) = unsafe { AllowSetForegroundWindow(server) } {
            tracing::debug!("the running instance may not take the foreground ({error})");
        }
        match write(client, request, deadline) {
            Ok(written) if written == request.len() => {
                tracing::debug!(bytes = request.len(), "sent to the running instance");
                Instance::Forwarded
            }
            Ok(written) => refused(format!(
                "only {written} of {} bytes were sent",
                request.len()
            )),
            Err(reason) => refused(reason),
        }
    }

    /// An overlapped write, with what it uses kept together on the heap, so
    /// that a write still pending at the deadline can be left to the system.
    struct Write {
        overlapped: OVERLAPPED,
        buffer: Vec<u8>,
        event: Handle,
    }

    /// Writes `request` to `client`, opened for overlapped writes, as one
    /// message, and gives up at `deadline`, so that an instance that does
    /// not read never holds this start up.
    fn write(client: Handle, request: &[u8], deadline: Instant) -> Result<usize, String> {
        // SAFETY: a manual-reset event without a name, closed when dropped.
        let event = unsafe { CreateEventW(None, true, false, None) }
            .map(Handle)
            .map_err(|error| format!("no event to wait on ({error})"))?;
        let mut write = Box::new(Write {
            overlapped: OVERLAPPED {
                hEvent: event.0,
                ..Default::default()
            },
            buffer: request.to_vec(),
            event,
        });
        let wait = |deadline: Instant| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            remaining.as_millis().min(u32::MAX as u128) as u32
        };
        // SAFETY: `write` holds the buffer and the `OVERLAPPED` until the
        // write ends, or for ever if it may not have ended.
        let started = unsafe {
            WriteFile(
                client.0,
                Some(&write.buffer),
                None,
                Some(&raw mut write.overlapped),
            )
        };
        match started {
            Ok(()) => {}
            Err(error) if is(&error, ERROR_IO_PENDING) => {
                // SAFETY: the event is a valid handle.
                if unsafe { WaitForSingleObject(write.event.0, wait(deadline)) } != WAIT_OBJECT_0 {
                    // SAFETY: cancels only this write.
                    let _ = unsafe { CancelIoEx(client.0, Some(&raw const write.overlapped)) };
                    // The deadline has passed, so the cancel is not waited
                    // for. Unless the write ended already, the system may
                    // still use the buffer and the `OVERLAPPED`: they and
                    // both handles are leaked on purpose, in a start that is
                    // about to exit.
                    // SAFETY: the event is a valid handle.
                    if unsafe { WaitForSingleObject(write.event.0, 0) } != WAIT_OBJECT_0 {
                        Box::leak(write);
                        std::mem::forget(client);
                        return Err(format!(
                            "the running instance did not take the request within {} seconds",
                            SEND_TIMEOUT.as_secs()
                        ));
                    }
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        let mut written = 0;
        // SAFETY: the write has ended, and `written` is a valid place.
        unsafe { GetOverlappedResult(client.0, &write.overlapped, &mut written, false) }.map_err(
            |error| {
                if is(&error, ERROR_OPERATION_ABORTED) {
                    format!(
                        "the running instance did not take the request within {} seconds",
                        SEND_TIMEOUT.as_secs()
                    )
                } else {
                    error.to_string()
                }
            },
        )?;
        Ok(written as usize)
    }

    /// The user the process `id` runs as.
    fn server_user(id: u32) -> ::windows::core::Result<String> {
        // SAFETY: opens the process for queries only; closed when dropped.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id) }?;
        let process = Handle(process);
        user_of(process.0)
    }

    pub(super) fn serve(cx: &mut App, listener: Listener, scheme: &str) {
        handle_instance_requests(cx, listener.requests, scheme);
    }

    /// How long the running instance waits for a start that connected to
    /// send its request.
    const READ_TIMEOUT: Duration = SEND_TIMEOUT;

    /// Receives each message on the pipe on a thread of its own, and sends
    /// it on. A message larger than a request may be, or one that does not
    /// come in time, is refused there, in the group `instance-request`.
    pub(super) fn receive(pipe: Pipe) -> std::io::Result<UnboundedReceiver<Vec<u8>>> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        std::thread::Builder::new()
            .name("instance-listener".into())
            .spawn(move || {
                let pipe = pipe;
                let handle = (pipe.0).0;
                let stopped = |error: &dyn std::fmt::Display| {
                    log::warn!(
                        "the instance pipe stopped receiving ({error}); later starts are refused"
                    );
                };
                // SAFETY: a manual-reset event without a name, closed when
                // dropped.
                let event = match unsafe { CreateEventW(None, true, false, None) } {
                    Ok(event) => Handle(event),
                    Err(error) => return stopped(&error),
                };
                let mut buffer = vec![0; MAX_REQUEST_LEN];
                loop {
                    if let Err(error) = connect(handle, &event) {
                        // Whatever state the pipe is in, disconnecting
                        // makes it ready to listen again.
                        // SAFETY: `handle` is the server end of the pipe,
                        // which only this thread uses.
                        if let Err(error) = unsafe { DisconnectNamedPipe(handle) } {
                            return stopped(&error);
                        }
                        log::warn!("a start could not connect to the instance pipe ({error})");
                        std::thread::sleep(RETRY);
                        continue;
                    }
                    let request = read_request(handle, &event, &mut buffer);
                    // SAFETY: as above.
                    if let Err(error) = unsafe { DisconnectNamedPipe(handle) } {
                        return stopped(&error);
                    }
                    match request {
                        Ok(Some(request)) => {
                            if tx.unbounded_send(request).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(reason) => {
                            let _group = tracing::info_span!("instance-request").entered();
                            log::warn!("a request from another start was refused: {reason}");
                        }
                    }
                }
            })?;
        Ok(rx)
    }

    /// Waits for the next start to connect. A start that connected, and
    /// even one that wrote and closed, before this call counts as connected:
    /// what it wrote is still there to read.
    fn connect(pipe: HANDLE, event: &Handle) -> ::windows::core::Result<()> {
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        // SAFETY: `overlapped` stays alive until the connection is made,
        // which is waited for below.
        match unsafe { ConnectNamedPipe(pipe, Some(&raw mut overlapped)) } {
            Ok(()) => Ok(()),
            Err(error) if is(&error, ERROR_PIPE_CONNECTED) || is(&error, ERROR_NO_DATA) => Ok(()),
            Err(error) if is(&error, ERROR_IO_PENDING) => {
                let mut transferred = 0;
                // SAFETY: waits until the connection is made.
                unsafe { GetOverlappedResult(pipe, &overlapped, &mut transferred, true) }
            }
            Err(error) => Err(error),
        }
    }

    /// Reads one message within [`READ_TIMEOUT`], or `None` when the start
    /// closed the pipe without writing. A message larger than `buffer`,
    /// which holds the largest request, is refused; disconnecting drops the
    /// rest of it.
    fn read_request(
        pipe: HANDLE,
        event: &Handle,
        buffer: &mut [u8],
    ) -> Result<Option<Vec<u8>>, String> {
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        // SAFETY: `buffer` and `overlapped` stay alive until the read ends,
        // which is waited for below, also after a cancel.
        let started = unsafe { ReadFile(pipe, Some(buffer), None, Some(&raw mut overlapped)) };
        match started {
            Ok(()) => {}
            Err(error) if is(&error, ERROR_IO_PENDING) => {
                let wait = READ_TIMEOUT.as_millis() as u32;
                // SAFETY: the event is a valid handle.
                if unsafe { WaitForSingleObject(event.0, wait) } != WAIT_OBJECT_0 {
                    // SAFETY: cancels only this read, whose end is waited
                    // for below.
                    let _ = unsafe { CancelIoEx(pipe, Some(&raw const overlapped)) };
                }
            }
            Err(error) => return read_error(error),
        }
        let mut len = 0;
        // SAFETY: waits until the read ends, then reads its result.
        match unsafe { GetOverlappedResult(pipe, &overlapped, &mut len, true) } {
            Ok(()) => Ok(Some(buffer[..len as usize].to_vec())),
            Err(error) if is(&error, ERROR_OPERATION_ABORTED) => Err(format!(
                "nothing came within {} seconds",
                READ_TIMEOUT.as_secs()
            )),
            Err(error) => read_error(error),
        }
    }

    fn read_error(error: ::windows::core::Error) -> Result<Option<Vec<u8>>, String> {
        if is(&error, ERROR_BROKEN_PIPE) || is(&error, ERROR_NO_DATA) {
            Ok(None)
        } else if is(&error, ERROR_MORE_DATA) {
            Err(format!(
                "the request is more than the {MAX_REQUEST_LEN} bytes the running instance reads"
            ))
        } else {
            Err(format!("the request cannot be read ({error})"))
        }
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests;
