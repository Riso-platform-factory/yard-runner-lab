//! One instance of the application: a second start hands its arguments to the
//! instance already running, then exits. The running instance opens the URLs
//! among them, or, when there are none, brings its main window to the front.
//!
//! The request format and its handling are the same on every system; only the
//! transport differs. On Linux and FreeBSD it is a Unix datagram socket, as in
//! Zed's `listen_for_cli_connections`.

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
/// argument in UTF-8 followed by a NUL. No arguments asks the running
/// instance to come to the front.
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

/// The arguments that are URLs: a scheme of two or more characters, then
/// `:`, as Zed's `parse_url_arg` takes them. The others are logged and left
/// out.
pub fn argument_urls(arguments: &[String]) -> Vec<String> {
    arguments
        .iter()
        .filter(|argument| {
            let url = is_url(argument);
            if !url {
                tracing::debug!(argument = argument.as_str(), "not a URL, ignored");
            }
            url
        })
        .cloned()
        .collect()
}

fn is_url(argument: &str) -> bool {
    let Some((scheme, _)) = argument.split_once(':') else {
        return false;
    };
    let mut bytes = scheme.bytes();
    // One letter before `:` is a drive, not a scheme.
    scheme.len() > 1
        && bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
}

/// Handles one request from a second start: the URLs among its arguments go
/// to `open_urls`, and a request without URLs brings the main window to the
/// front.
pub fn handle_instance_request(
    cx: &mut App,
    request: &[u8],
    open_urls: &dyn Fn(&mut App, Vec<String>),
) {
    let arguments = match decode_request(request) {
        Ok(arguments) => arguments,
        Err(error) => {
            let _group = tracing::info_span!("instance-request").entered();
            log::warn!("a request from another start was refused: {error}");
            return;
        }
    };
    let urls = arguments.iter().filter(|argument| is_url(argument)).count();
    let _group = tracing::info_span!("instance-request", urls).entered();
    let urls = argument_urls(&arguments);
    if urls.is_empty() {
        activate_main_window(cx);
    } else {
        tracing::debug!("opening the URLs");
        open_urls(cx, urls);
    }
}

/// Brings the first window to the front, the one that keeps `window.json`.
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
    cx.activate(true);
    tracing::debug!("activated");
}

#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use super::{MAX_REQUEST_LEN, encode_request, handle_instance_request};
    use futures::StreamExt as _;
    use gpui::App;
    use std::{
        io::ErrorKind,
        os::unix::net::{SocketAddr, UnixDatagram},
        path::{Path, PathBuf},
    };

    /// What [`check_instance`] found.
    #[derive(Debug)]
    pub enum Instance {
        /// The running instance took the arguments; this start exits.
        Sent,
        /// An instance runs but did not take the arguments, for the reason
        /// given; this start exits with an error.
        Refused(String),
        /// This start is the one instance, and listens for the next.
        First(InstanceListener),
        /// This start runs without a single instance, because its socket
        /// cannot be used.
        Alone,
    }

    /// The bound socket of the one instance, until [`serve_instance_requests`]
    /// listens on it.
    #[derive(Debug)]
    pub struct InstanceListener {
        socket: UnixDatagram,
        path: PathBuf,
    }

    impl InstanceListener {
        /// The socket's path.
        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    /// Hands the arguments to the instance listening on `socket`, or, when
    /// none listens, binds `socket` so this start is the one instance. A
    /// socket left by an instance that ended without removing it is removed.
    pub fn check_instance(socket: &Path, arguments: &[String]) -> Instance {
        let _group = tracing::info_span!("instance-check", socket = %socket.display()).entered();
        if let Err(error) = SocketAddr::from_pathname(socket) {
            log::warn!(
                "the instance socket {} cannot be used ({error}); running without a single instance",
                socket.display()
            );
            return Instance::Alone;
        }
        let sender = match UnixDatagram::unbound() {
            Ok(sender) => sender,
            Err(error) => {
                log::warn!(
                    "no socket to reach another instance ({error}); running without a single instance"
                );
                return Instance::Alone;
            }
        };
        match sender.connect(socket) {
            Ok(()) => return send(&sender, arguments),
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
                if let Err(error) = std::fs::remove_file(socket) {
                    log::warn!(
                        "the stale instance socket cannot be removed ({error}); running without a single instance"
                    );
                    return Instance::Alone;
                }
                tracing::debug!("stale socket removed");
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                log::warn!(
                    "the instance socket cannot be reached ({error}); running without a single instance"
                );
                return Instance::Alone;
            }
        }
        if let Some(folder) = socket.parent()
            && let Err(error) = std::fs::create_dir_all(folder)
        {
            log::warn!(
                "the instance socket's folder cannot be created ({error}); running without a single instance"
            );
            return Instance::Alone;
        }
        match UnixDatagram::bind(socket) {
            Ok(listener) => {
                tracing::debug!("listening");
                Instance::First(InstanceListener {
                    socket: listener,
                    path: socket.to_owned(),
                })
            }
            Err(error) => {
                log::warn!(
                    "the instance socket cannot be bound ({error}); running without a single instance"
                );
                Instance::Alone
            }
        }
    }

    fn send(sender: &UnixDatagram, arguments: &[String]) -> Instance {
        let request = match encode_request(arguments) {
            Ok(request) => request,
            Err(error) => {
                let reason = format!("the running instance was not reached: {error}");
                log::warn!("{reason}");
                return Instance::Refused(reason);
            }
        };
        match sender.send(&request) {
            Ok(_) => {
                tracing::debug!(bytes = request.len(), "sent to the running instance");
                Instance::Sent
            }
            Err(error) => {
                let reason = format!("the running instance was not reached: {error}");
                log::warn!("{reason}");
                Instance::Refused(reason)
            }
        }
    }

    /// Receives the requests of later starts off the main thread, and hands
    /// each to the main thread: its URLs to `open_urls`, or, without URLs,
    /// the main window to the front. The socket is removed when the app
    /// quits.
    pub fn serve_instance_requests(
        cx: &mut App,
        listener: InstanceListener,
        open_urls: impl Fn(&mut App, Vec<String>) + 'static,
    ) {
        let InstanceListener { socket, path } = listener;
        let mut rx = match receive(socket) {
            Ok(rx) => rx,
            Err(error) => {
                log::warn!(
                    "the instance listener did not start ({error}); later starts are not received"
                );
                return;
            }
        };
        cx.spawn(async move |cx| {
            while let Some(request) = rx.next().await {
                cx.update(|cx| handle_instance_request(cx, &request, &open_urls));
            }
        })
        .detach();
        cx.on_app_quit(move |_| {
            if let Err(error) = std::fs::remove_file(&path) {
                log::warn!("the instance socket was not removed: {error}");
            }
            async {}
        })
        .detach();
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
