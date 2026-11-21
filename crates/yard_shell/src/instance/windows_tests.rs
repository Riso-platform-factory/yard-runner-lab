//! The Windows transport: a named mutex and a named pipe, each test with
//! names of its own.

use super::{
    Instance, MAX_REQUEST_LEN, decode_request, encode_request,
    windows::{Names, check, send},
};
use ::windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING,
            PIPE_ACCESS_INBOUND, WriteFile,
        },
        System::Pipes::{CreateNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_WAIT},
    },
    core::HSTRING,
};
use futures::StreamExt as _;
use std::time::{Duration, Instant};

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

fn next_request(instance: &mut Instance) -> Vec<u8> {
    let Instance::Listening(listener) = instance else {
        panic!("the first start listens: {instance:?}");
    };
    futures::executor::block_on(listener.inner.requests.next()).unwrap()
}

/// Writes `message` to the pipe as one message, as another program might.
fn write_raw(pipe: &str, message: &[u8]) {
    // SAFETY: the name is a valid string, and the handle is closed below.
    let client = unsafe {
        CreateFileW(
            &HSTRING::from(pipe),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None::<HANDLE>,
        )
    }
    .unwrap();
    let mut written = 0;
    // SAFETY: `client` is an open pipe, and `written` a valid place.
    unsafe { WriteFile(client, Some(message), Some(&mut written), None) }.unwrap();
    // SAFETY: `client` was opened above and is closed once.
    unsafe { CloseHandle(client) }.unwrap();
}

#[test]
fn the_first_start_listens_and_the_second_hands_over() {
    let names = names();
    let mut first = check(&names, &[]);
    assert!(matches!(first, Instance::Listening(_)), "{first:?}");
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
fn a_message_over_the_limit_is_refused_and_the_listener_goes_on() {
    let names = names();
    let mut first = check(&names, &[]);
    assert!(matches!(first, Instance::Listening(_)), "{first:?}");
    write_raw(&names.pipe, &vec![1; MAX_REQUEST_LEN + 10]);
    // The largest request still arrives whole.
    let largest = encode_request(&["a".repeat(MAX_REQUEST_LEN - 2)]).unwrap();
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
fn a_sender_with_no_instance_reports_it_in_time() {
    let names = names();
    let started = Instant::now();
    match send(&names.pipe, &encode_request(&[])) {
        Instance::Refused(reason) => assert!(reason.contains("seconds"), "{reason}"),
        other => panic!("no instance is reported: {other:?}"),
    }
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1900), "{waited:?}");
    assert!(waited < Duration::from_secs(5), "{waited:?}");
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

#[test]
fn an_instance_that_never_reads_is_given_up_in_time() {
    let names = names();
    // A pipe with almost no room, which this test never reads.
    // SAFETY: the name is a valid string, and the pipe is closed below.
    let server = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(&names.pipe),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
            1,
            0,
            1,
            0,
            None,
        )
    };
    assert!(!server.is_invalid());
    let request = encode_request(&["a".repeat(MAX_REQUEST_LEN - 2)]);
    let started = Instant::now();
    match send(&names.pipe, &request) {
        Instance::Refused(reason) => {
            assert!(reason.contains("did not take the request"), "{reason}")
        }
        other => panic!("an instance that never reads is given up: {other:?}"),
    }
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1900), "{waited:?}");
    assert!(waited < Duration::from_millis(2500), "{waited:?}");
    // SAFETY: `server` was created above and is closed once.
    unsafe { CloseHandle(server) }.unwrap();
}
