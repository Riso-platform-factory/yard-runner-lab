//! One instance: the request format, the arguments that are URLs, the
//! socket a second start finds, and the requests the running instance
//! handles.

use gpui::{App, TestAppContext};
use std::{cell::RefCell, rc::Rc};
use yard_shell as shell;

fn strings(arguments: &[&str]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string())
        .collect()
}

#[test]
fn a_request_reads_back_as_it_was_written() {
    let arguments = strings(&["yard://tour/a?b=c", "", "--flag", "file name"]);
    let request = shell::encode_request(&arguments).unwrap();
    assert_eq!(request[0], shell::REQUEST_VERSION);
    assert_eq!(shell::decode_request(&request).unwrap(), arguments);
}

#[test]
fn a_request_without_arguments_is_the_version_alone() {
    let request = shell::encode_request(&[]).unwrap();
    assert_eq!(request, [shell::REQUEST_VERSION]);
    assert_eq!(
        shell::decode_request(&request).unwrap(),
        Vec::<String>::new()
    );
    // An empty argument is not the same as no argument.
    let empty = shell::encode_request(&strings(&[""])).unwrap();
    assert_eq!(shell::decode_request(&empty).unwrap(), [""]);
}

#[test]
fn a_request_larger_than_the_limit_is_refused_on_both_sides() {
    let at_limit = "a".repeat(shell::MAX_REQUEST_LEN - 2);
    let request = shell::encode_request(std::slice::from_ref(&at_limit)).unwrap();
    assert_eq!(request.len(), shell::MAX_REQUEST_LEN);
    assert_eq!(shell::decode_request(&request).unwrap(), [at_limit]);

    let over = "a".repeat(shell::MAX_REQUEST_LEN - 1);
    assert_eq!(
        shell::encode_request(&[over]),
        Err(shell::RequestError::TooLarge(shell::MAX_REQUEST_LEN + 1))
    );
    let mut received = vec![shell::REQUEST_VERSION];
    received.resize(shell::MAX_REQUEST_LEN + 1, b'a');
    assert_eq!(
        shell::decode_request(&received),
        Err(shell::RequestError::TooLarge(shell::MAX_REQUEST_LEN + 1))
    );
}

#[test]
fn a_bad_request_is_refused() {
    assert_eq!(
        shell::decode_request(&[shell::REQUEST_VERSION + 1, b'a', 0]),
        Err(shell::RequestError::Version(shell::REQUEST_VERSION + 1))
    );
    assert_eq!(shell::decode_request(&[]), Err(shell::RequestError::Empty));
    assert_eq!(
        shell::decode_request(&[shell::REQUEST_VERSION, b'a']),
        Err(shell::RequestError::Unterminated)
    );
    assert_eq!(
        shell::decode_request(&[shell::REQUEST_VERSION, 0xff, 0]),
        Err(shell::RequestError::NotUtf8)
    );
    assert_eq!(
        shell::encode_request(&strings(&["a\0b"])),
        Err(shell::RequestError::Nul)
    );
}

#[test]
fn arguments_with_a_scheme_are_urls() {
    let arguments = strings(&[
        "yard://tour/a",
        "mailto:someone@example.com",
        "x-yard+1.2:thing",
        "dev",
        "--flag",
        "/home/me/a:b",
        "notes.txt",
        "C:\\file",
        "1yard://a",
        ":nothing",
    ]);
    assert_eq!(
        shell::argument_urls(&arguments),
        [
            "yard://tour/a",
            "mailto:someone@example.com",
            "x-yard+1.2:thing"
        ]
    );
}

#[cfg(unix)]
mod socket {
    use super::*;
    use std::os::unix::net::UnixDatagram;

    #[test]
    fn the_first_start_makes_the_folder_and_listens() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("data/instance.sock");
        let first = match shell::check_instance(&path, &[]) {
            shell::Instance::First(listener) => listener,
            other => panic!("the first start listens: {other:?}"),
        };
        assert_eq!(first.path(), path);
        assert!(path.exists());
    }

    #[test]
    fn a_second_start_hands_its_arguments_to_the_first() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("instance.sock");
        let listener = UnixDatagram::bind(&path).unwrap();
        let arguments = strings(&["yard://tour/a", "dev"]);
        assert!(matches!(
            shell::check_instance(&path, &arguments),
            shell::Instance::Sent
        ));
        let mut buffer = vec![0; shell::MAX_REQUEST_LEN + 1];
        let len = listener.recv(&mut buffer).unwrap();
        assert_eq!(shell::decode_request(&buffer[..len]).unwrap(), arguments);
    }

    #[test]
    fn a_stale_socket_is_removed_and_bound_again() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("instance.sock");
        // An instance that ended without removing its socket.
        drop(UnixDatagram::bind(&path).unwrap());
        assert!(path.exists());
        let first = match shell::check_instance(&path, &[]) {
            shell::Instance::First(listener) => listener,
            other => panic!("a stale socket is replaced: {other:?}"),
        };
        assert!(path.exists());
        // The new socket listens.
        assert!(matches!(
            shell::check_instance(&path, &[]),
            shell::Instance::Sent
        ));
        drop(first);
    }

    #[test]
    fn a_path_too_long_for_a_socket_runs_alone() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("a".repeat(200)).join("instance.sock");
        assert!(matches!(
            shell::check_instance(&path, &[]),
            shell::Instance::Alone
        ));
        assert!(!path.exists());
    }

    #[test]
    fn a_request_too_large_is_not_sent() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("instance.sock");
        let _listener = UnixDatagram::bind(&path).unwrap();
        let long = "a".repeat(shell::MAX_REQUEST_LEN);
        match shell::check_instance(&path, &[long]) {
            shell::Instance::Refused(reason) => assert!(reason.contains("bytes"), "{reason}"),
            other => panic!("a request too large is refused: {other:?}"),
        }
    }

    // GPUI's test scheduler refuses work from another thread, so no request
    // is sent here; the listener's thread is tested in `src/instance/tests.rs`.
    #[gpui::test]
    fn the_socket_is_removed_on_quit(cx: &mut TestAppContext) {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("instance.sock");
        let listener = match shell::check_instance(&path, &[]) {
            shell::Instance::First(listener) => listener,
            other => panic!("the first start listens: {other:?}"),
        };
        cx.update(|cx| shell::serve_instance_requests(cx, listener, |_, _| {}));
        assert!(path.exists());
        cx.quit();
        assert!(!path.exists(), "the socket is removed on quit");
    }
}

#[gpui::test]
fn a_request_with_urls_opens_its_urls(cx: &mut TestAppContext) {
    let opened = Rc::new(RefCell::new(Vec::<Vec<String>>::new()));
    let request =
        shell::encode_request(&strings(&["dev", "yard://tour/a", "yard://guest/b"])).unwrap();
    cx.update(|cx: &mut App| {
        let opened = opened.clone();
        shell::handle_instance_request(cx, &request, &move |_, urls| {
            opened.borrow_mut().push(urls)
        });
    });
    assert_eq!(
        *opened.borrow(),
        [strings(&["yard://tour/a", "yard://guest/b"])]
    );
}

#[gpui::test]
fn a_request_without_urls_opens_nothing(cx: &mut TestAppContext) {
    let opened = Rc::new(RefCell::new(Vec::<Vec<String>>::new()));
    let request = shell::encode_request(&strings(&["dev"])).unwrap();
    cx.update(|cx: &mut App| {
        let opened = opened.clone();
        shell::handle_instance_request(cx, &request, &move |_, urls| {
            opened.borrow_mut().push(urls)
        });
    });
    assert!(opened.borrow().is_empty());
}
