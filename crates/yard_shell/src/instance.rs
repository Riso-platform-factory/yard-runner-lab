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

/// Asks the system to bring the first window, the one that keeps
/// `window.json`, to the front.
fn activate_main_window(cx: &mut App) {
    let Some(window) = cx.windows().into_iter().next() else {
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
    };

    #[derive(Debug)]
    pub(super) struct Listener {
        socket: UnixDatagram,
        path: PathBuf,
        id: (u64, u64),
    }

    impl Listener {
        #[cfg(test)]
        pub(super) fn socket_for_tests(&self) -> &UnixDatagram {
            &self.socket
        }
    }

    /// The user this process runs as.
    pub(super) fn own_uid() -> u32 {
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// Looks for the instance on `socket` and hands it the arguments, or,
    /// when none listens, binds `socket`. The lock is held from the second
    /// look to the bind, so two starts never both bind, and none removes
    /// the socket of another that just bound it.
    pub(super) fn check(dir: &Path, socket: &Path, lock: &Path, arguments: &[String]) -> Instance {
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
        let lock = match fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(lock)
        {
            Ok(lock) => lock,
            Err(error) => return alone(format!("the instance lock cannot be opened ({error})")),
        };
        if let Err(error) = lock.lock() {
            return alone(format!("the instance lock cannot be taken ({error})"));
        }
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
            .and_then(|()| fs::symlink_metadata(socket))
        {
            Ok(metadata) => (metadata.dev(), metadata.ino()),
            Err(error) => {
                drop(listener);
                let _ = fs::remove_file(socket);
                return alone(format!(
                    "the instance socket cannot be made private ({error})"
                ));
            }
        };
        tracing::debug!("listening");
        Instance::Listening(InstanceListener {
            inner: Listener {
                socket: listener,
                path: socket.to_owned(),
                id,
            },
        })
    }

    fn alone(reason: String) -> Instance {
        log::warn!("{reason}; running without a single instance");
        Instance::Alone(reason)
    }

    /// Sends the request to the instance listening on `socket`, and says
    /// what came of it, or `None` when no instance listens there.
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
        let sender = match UnixDatagram::unbound() {
            Ok(sender) => sender,
            Err(error) => {
                return Some(alone(format!(
                    "no socket to reach another instance ({error})"
                )));
            }
        };
        match sender.connect(socket) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConnectionRefused | ErrorKind::NotFound
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

    /// Makes `dir` a folder of `uid` that only `uid` may enter.
    pub(super) fn private_folder(dir: &Path, uid: u32) -> Result<(), String> {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("the data folder cannot be created ({error})"))?;
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
        let Listener { socket, path, id } = listener;
        match receive(socket) {
            Ok(requests) => handle_instance_requests(cx, requests, scheme),
            Err(error) => log::warn!(
                "the instance listener did not start ({error}); later starts are not received"
            ),
        }
        cx.on_app_quit(move |_| {
            remove_own_socket(&path, id);
            async {}
        })
        .detach();
    }

    /// Removes the socket at `path` if it is still the one with `id`.
    pub(super) fn remove_own_socket(path: &Path, id: (u64, u64)) {
        match fs::symlink_metadata(path) {
            Ok(metadata) if (metadata.dev(), metadata.ino()) == id => {
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
    pub(super) fn receive(
        socket: UnixDatagram,
    ) -> std::io::Result<futures::channel::mpsc::UnboundedReceiver<Vec<u8>>> {
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
