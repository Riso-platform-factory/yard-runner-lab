//! One instance of the application: a second start hands its arguments to the
//! instance already running, then exits. The running instance opens the URLs
//! among them and brings its main window to the front.
//!
//! The request format and its handling are the same on every system; only the
//! transport differs. On Linux it is a Unix datagram socket in a private
//! folder, as in Zed's `listen_for_cli_connections`. Other systems run without
//! one instance until they have a transport.

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
    #[cfg(not(unix))]
    never: std::convert::Infallible,
}

/// Hands the command line's URLs of the product's `scheme` to the instance
/// already running, or, when none runs, makes this start the one instance.
/// The other arguments are never sent; with no URL, the running instance
/// only comes to the front. Returns the URLs too, for this start to open
/// when it is the one instance.
pub fn start_instance(
    paths: &yard_paths::Paths,
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
    scheme: &str,
) -> (Instance, Vec<String>) {
    // An argument that is not UTF-8 is no URL; it stays to be counted.
    let arguments: Vec<String> = arguments
        .into_iter()
        .map(|argument| argument.into_string().unwrap_or_default())
        .collect();
    let urls = argument_urls(&arguments, scheme);
    (check_instance(paths, &urls), urls)
}

/// Hands the arguments to the instance already running, or, when none runs,
/// makes this start the one instance.
pub fn check_instance(paths: &yard_paths::Paths, arguments: &[String]) -> Instance {
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

/// Receives the requests of later starts off the main thread and handles
/// each on it, as [`handle_instance_requests`] does. The socket is removed
/// when the app quits, if it is still this instance's.
pub fn serve_instance_requests(cx: &mut App, listener: InstanceListener, scheme: &str) {
    #[cfg(unix)]
    unix::serve(cx, listener.inner, scheme);
    #[cfg(not(unix))]
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
    // TODO(linux-app-activate): GPUI's `activate` does nothing on Linux, and
    // the window manager or compositor decides whether `activate_window`
    // takes the focus: on X11 it sends `_NET_ACTIVE_WINDOW`, and on Wayland
    // it asks for an `xdg_activation_v1` token, granted only with a recent
    // input serial. A GPUI that implements `activate` on Linux unblocks it.
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

#[cfg(all(test, unix))]
mod tests;
