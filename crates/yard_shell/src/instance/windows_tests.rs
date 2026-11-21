//! The Windows transport: a named mutex and a named pipe, each test with
//! names of its own. Nothing here waits without a limit.

use super::{
    Instance, MAX_REQUEST_LEN, decode_request, encode_request,
    windows::{Handle, Names, Sent, check, create_pipe, hand_over, receive, send},
};
use ::windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING,
            PIPE_ACCESS_INBOUND, WriteFile,
        },
        System::{
            Pipes::{
                CreateNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_WAIT,
                WaitNamedPipeW,
            },
            Threading::{CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE},
        },
    },
    core::HSTRING,
};
use std::time::{Duration, Instant};

/// The longest any step of these tests waits before it fails.
const LIMIT: Duration = Duration::from_secs(5);

/// Names that no other test, and no other run, uses.
fn names() -> Names {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    Names::new(&format!(
        "test.yard.instance-{}-{nanos}-{next}",
        std::process::id()
    ))
    .unwrap()
}

fn strings(arguments: &[&str]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string())
        .collect()
}

/// The next request the listener received, within [`LIMIT`].
fn next_from(requests: &mut futures::channel::mpsc::UnboundedReceiver<Vec<u8>>) -> Vec<u8> {
    let started = Instant::now();
    loop {
        match requests.try_recv() {
            Ok(request) => return request,
            Err(futures::channel::mpsc::TryRecvError::Closed) => panic!("the listener ended"),
            Err(_) if started.elapsed() < LIMIT => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => panic!("no request within {LIMIT:?}"),
        }
    }
}

fn next_request(instance: &mut Instance) -> Vec<u8> {
    let Instance::Listening(listener) = instance else {
        panic!("the first start listens: {instance:?}");
    };
    next_from(&mut listener.inner.requests)
}

/// Opens the pipe as another program might, waiting while it is busy or
/// not there yet.
fn open_raw(pipe: &str) -> HANDLE {
    let name = HSTRING::from(pipe);
    let started = Instant::now();
    loop {
        // SAFETY: the name is a valid string; the caller closes the handle.
        let opened = unsafe {
            CreateFileW(
                &name,
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        match opened {
            Ok(client) => return client,
            Err(error) if started.elapsed() > LIMIT => panic!("the pipe did not open: {error}"),
            Err(error) if error.code() == ERROR_PIPE_BUSY.to_hresult() => {
                // SAFETY: the name is a valid string.
                let _ = unsafe { WaitNamedPipeW(&name, 100) };
            }
            Err(error) if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("the pipe did not open: {error}"),
        }
    }
}

/// Writes `message` to the pipe as one message, as another program might,
/// on a thread of its own so that a write that blocks fails the test, and
/// says whether the whole message was written.
fn try_write_raw(pipe: &str, message: &[u8]) -> bool {
    let len = message.len();
    let (pipe, message) = (pipe.to_owned(), message.to_vec());
    let (done, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let client = open_raw(&pipe);
        let mut written = 0;
        // SAFETY: `client` is an open pipe, and `written` a valid place.
        let result = unsafe { WriteFile(client, Some(&message), Some(&mut written), None) };
        close(client);
        let _ = done.send(result.map(|()| written as usize));
    });
    let written = wait.recv_timeout(LIMIT).expect("the raw write did not end");
    written.is_ok_and(|written| written == len)
}

fn write_raw(pipe: &str, message: &[u8]) {
    assert!(try_write_raw(pipe, message), "the raw write failed");
}

/// Holds the instance mutex, as a running instance does.
fn hold_mutex(names: &Names) -> HANDLE {
    let name = HSTRING::from(&names.mutex);
    // SAFETY: the name is a valid string; the caller closes the handle.
    unsafe { CreateMutexW(None, false, &name) }.unwrap()
}

fn mutex_exists(names: &Names) -> bool {
    let name = HSTRING::from(&names.mutex);
    // SAFETY: the name is a valid string, and the handle is closed below.
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &name) } {
        Ok(mutex) => {
            // SAFETY: opened above, closed once.
            unsafe { CloseHandle(mutex) }.unwrap();
            true
        }
        Err(error) => {
            assert_eq!(error.code(), ERROR_FILE_NOT_FOUND.to_hresult(), "{error}");
            false
        }
    }
}

/// A pipe of the given name that this test creates and never reads.
fn raw_server(pipe: &str, in_buffer: u32) -> HANDLE {
    let name = HSTRING::from(pipe);
    // SAFETY: the name is a valid string; the caller closes the handle.
    let server = unsafe {
        CreateNamedPipeW(
            &name,
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
            1,
            0,
            in_buffer,
            0,
            None,
        )
    };
    assert!(!server.is_invalid());
    server
}

fn close(handle: HANDLE) {
    // SAFETY: the handle was opened by the test and is closed once.
    unsafe { CloseHandle(handle) }.unwrap();
}

#[test]
fn the_first_start_listens_and_the_second_hands_over() {
    let names = names();
    let mut first = check(&names, &[]);
    assert!(matches!(first, Instance::Listening(_)), "{first:?}");
    // At once, before the listener's thread may have connected the pipe.
    assert!(matches!(
        check(&names, &strings(&["yard://tour/a", "dev"])),
        Instance::Forwarded
    ));
    assert_eq!(
        decode_request(&next_request(&mut first)).unwrap(),
        ["yard://tour/a", "dev"]
    );
    // With no URL, the request is empty: the instance comes to the front.
    assert!(matches!(check(&names, &[]), Instance::Forwarded));
    assert!(
        decode_request(&next_request(&mut first))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_start_that_wrote_and_closed_before_the_listener_connected_is_read() {
    let names = names();
    let pipe = create_pipe(&names.pipe).unwrap();
    write_raw(&names.pipe, b"\x01first\0");
    let mut requests = receive(pipe, Handle(hold_mutex(&names))).unwrap();
    assert_eq!(next_from(&mut requests), b"\x01first\0");
    // The listener goes on.
    write_raw(&names.pipe, b"\x01second\0");
    assert_eq!(next_from(&mut requests), b"\x01second\0");
}

#[test]
fn two_starts_at_once_end_with_exactly_one_listener() {
    for _ in 0..20 {
        let names = names();
        let barrier = std::sync::Barrier::new(2);
        let results: Vec<Instance> = std::thread::scope(|scope| {
            let starts: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        check(&names, &[])
                    })
                })
                .collect();
            starts
                .into_iter()
                .map(|start| start.join().unwrap())
                .collect()
        });
        let listening = results
            .iter()
            .filter(|result| matches!(result, Instance::Listening(_)))
            .count();
        let forwarded = results
            .iter()
            .filter(|result| matches!(result, Instance::Forwarded))
            .count();
        assert_eq!((listening, forwarded), (1, 1), "{results:?}");
    }
}

#[test]
fn the_listener_takes_what_the_codec_takes_and_no_more() {
    let names = names();
    let mut first = check(&names, &[]);
    assert!(matches!(first, Instance::Listening(_)), "{first:?}");
    // The listener may drop the pipe before the writer is done.
    let _ = try_write_raw(&names.pipe, &vec![1; MAX_REQUEST_LEN + 1]);
    // The largest request the codec writes arrives whole.
    let largest = encode_request(&["a".repeat(MAX_REQUEST_LEN - 2)]).unwrap();
    assert_eq!(largest.len(), MAX_REQUEST_LEN);
    write_raw(&names.pipe, &largest);
    assert_eq!(next_request(&mut first), largest);
    assert!(matches!(
        check(&names, &strings(&["yard://tour/a"])),
        Instance::Forwarded
    ));
    assert_eq!(
        decode_request(&next_request(&mut first)).unwrap(),
        ["yard://tour/a"]
    );
}

#[test]
fn a_start_that_connects_and_sends_nothing_is_given_up() {
    let names = names();
    let mut first = check(&names, &[]);
    assert!(matches!(first, Instance::Listening(_)), "{first:?}");
    let silent = open_raw(&names.pipe);
    let opened = Instant::now();
    // The listener gives the silent start two seconds, then serves again,
    // within the next start's own two seconds.
    std::thread::sleep(Duration::from_secs(1));
    assert!(matches!(
        check(&names, &strings(&["yard://tour/a"])),
        Instance::Forwarded
    ));
    let blocked = opened.elapsed();
    assert!(blocked < Duration::from_millis(2600), "{blocked:?}");
    assert_eq!(
        decode_request(&next_request(&mut first)).unwrap(),
        ["yard://tour/a"]
    );
    close(silent);
}

#[test]
fn a_request_too_large_is_refused_by_the_sender() {
    let names = names();
    let _first = check(&names, &[]);
    let long = "a".repeat(MAX_REQUEST_LEN);
    match check(&names, &[long]) {
        Instance::Refused(reason) => assert!(reason.contains("bytes"), "{reason}"),
        other => panic!("a request too large is refused: {other:?}"),
    }
}

#[test]
fn an_instance_without_a_pipe_is_reported_in_time() {
    let names = names();
    let mutex = hold_mutex(&names);
    let started = Instant::now();
    match check(&names, &[]) {
        Instance::Refused(reason) => assert!(reason.contains("seconds"), "{reason}"),
        other => panic!("an instance without a pipe is reported: {other:?}"),
    }
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1900), "{waited:?}");
    assert!(waited < Duration::from_millis(2500), "{waited:?}");
    close(mutex);
}

#[test]
fn an_instance_that_ends_before_its_pipe_leaves_the_place_to_this_start() {
    let names = names();
    let mutex = hold_mutex(&names);
    let closer = {
        let mutex = mutex.0 as usize;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            close(HANDLE(mutex as _));
        })
    };
    // The sender's side of a later start: it finds the mutex gone, whether
    // before or while it waits for the pipe, and says so.
    let started = Instant::now();
    let deadline = started + Duration::from_secs(2);
    let sent = send(
        &names.pipe,
        &HSTRING::from(&names.mutex),
        &encode_request(&[]),
        deadline,
    );
    assert!(matches!(sent, Sent::Gone), "the sender saw the mutex go");
    assert!(started.elapsed() < Duration::from_millis(1500));
    closer.join().unwrap();
    // Running the election again makes this start the instance.
    let first = check(&names, &[]);
    assert!(matches!(first, Instance::Listening(_)), "{first:?}");
}

#[test]
fn an_instance_that_ends_after_the_pipe_opened_leaves_the_place_to_this_start() {
    let names = names();
    let mutex = hold_mutex(&names);
    let server = raw_server(&names.pipe, 1024);
    let client = open_raw(&names.pipe);
    // The instance ends between the open and the hand-over.
    close(server);
    close(mutex);
    let deadline = Instant::now() + Duration::from_secs(2);
    let sent = hand_over(
        Handle(client),
        &HSTRING::from(&names.mutex),
        &encode_request(&[]).unwrap(),
        deadline,
    );
    assert!(matches!(sent, Sent::Gone), "the sender saw the instance go");
    let first = check(&names, &[]);
    assert!(matches!(first, Instance::Listening(_)), "{first:?}");
}

#[test]
fn an_instance_that_never_reads_is_given_up_in_time() {
    let names = names();
    let mutex = hold_mutex(&names);
    // A pipe with almost no room, which this test never reads.
    let server = raw_server(&names.pipe, 1);
    let request = ["a".repeat(MAX_REQUEST_LEN - 2)];
    let started = Instant::now();
    match check(&names, &request) {
        Instance::Refused(reason) => {
            assert!(reason.contains("did not take the request"), "{reason}")
        }
        other => panic!("an instance that never reads is given up: {other:?}"),
    }
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1900), "{waited:?}");
    assert!(waited < Duration::from_millis(2500), "{waited:?}");
    close(server);
    close(mutex);
}

#[test]
fn a_pipe_name_taken_by_another_runs_alone_and_releases_the_mutex() {
    let names = names();
    let squatter = raw_server(&names.pipe, 1024);
    assert!(matches!(check(&names, &[]), Instance::Alone(_)));
    assert!(!mutex_exists(&names), "the mutex is released");
    close(squatter);
}

#[test]
fn names_hold_the_session_and_the_mutex_is_per_session() {
    let names = Names::new("test.yard.lab-instance").unwrap();
    assert_eq!(names.mutex, r"Local\test.yard.lab-instance");
    let session = names
        .pipe
        .strip_prefix(r"\\.\pipe\test.yard.lab-instance-")
        .unwrap();
    assert!(session.parse::<u32>().is_ok(), "{}", names.pipe);
}
