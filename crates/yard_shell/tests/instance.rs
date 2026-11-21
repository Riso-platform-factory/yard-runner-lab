//! One instance: the request format, the arguments that are URLs, the
//! socket a second start finds, and the requests the running instance
//! handles.

use gpui::{App, TestAppContext};
use std::{cell::RefCell, rc::Rc};
use yard_shell as shell;

/// The product's identifier in these tests.
const ID: &str = "test.yard.instance";

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
fn only_urls_of_the_product_scheme_are_urls() {
    let arguments = strings(&[
        "yard://tour/a",
        "YARD://tour/b",
        "yard:tour",
        "mailto:someone@example.com",
        "yard-check://tour/c",
        "dev",
        "--flag",
        "/home/me/yard:b",
        "notes.txt",
    ]);
    assert_eq!(
        shell::argument_urls(&arguments, "yard"),
        ["yard://tour/a", "YARD://tour/b", "yard:tour"]
    );
    assert!(shell::argument_urls(&arguments, "").is_empty());
}

#[test]
fn the_instance_name_is_the_installers_without_a_home() {
    // No test sets this product's home variable, so its directories are
    // the platform's.
    let product = yard_product::Product {
        name: "Instance Name Test",
        version: "0",
        identifier: ID,
    };
    let platform = yard_paths::Paths::from_env(&product);
    assert_eq!(platform.origin(), yard_paths::Origin::Platform);
    assert_eq!(
        shell::instance_name(&platform, ID),
        yard_product::instance_name(ID)
    );
    assert_eq!(
        yard_product::instance_name(ID),
        "test.yard.instance-instance"
    );
}

#[test]
fn each_home_has_an_instance_name_of_its_own() {
    let one = yard_paths::Paths::under("one");
    let other = yard_paths::Paths::under("other");
    let name = shell::instance_name(&one, ID);
    assert!(name.starts_with("test.yard.instance-instance-"), "{name}");
    assert_eq!(
        name,
        shell::instance_name(&yard_paths::Paths::under("one"), ID)
    );
    assert_ne!(name, shell::instance_name(&other, ID));
    assert_ne!(
        shell::instance_name(&one, "test.yard.other"),
        shell::instance_name(&one, ID)
    );
}

#[cfg(windows)]
#[test]
fn on_windows_the_first_start_listens_and_the_second_hands_over() {
    let home = tempfile::tempdir().unwrap();
    let paths = yard_paths::Paths::under(home.path());
    let shell::Instance::Listening(_first) = shell::check_instance(&paths, ID, &[]) else {
        panic!("the first start listens");
    };
    assert!(matches!(
        shell::check_instance(&paths, ID, &strings(&["yard://tour/a"])),
        shell::Instance::Forwarded
    ));
    // Another home is another instance.
    let other = tempfile::tempdir().unwrap();
    let other = yard_paths::Paths::under(other.path());
    let shell::Instance::Listening(_second) = shell::check_instance(&other, ID, &[]) else {
        panic!("another home listens on its own");
    };
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    // A home whose data folder is as a normal home has it, whatever the umask.
    fn home() -> (tempfile::TempDir, yard_paths::Paths) {
        let home = tempfile::tempdir().unwrap();
        let paths = yard_paths::Paths::under(home.path());
        std::fs::create_dir(paths.data_dir()).unwrap();
        std::fs::set_permissions(paths.data_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
        (home, paths)
    }

    #[test]
    fn a_second_start_sends_only_its_urls() {
        use std::os::unix::ffi::OsStringExt as _;
        let (_home, paths) = home();
        std::fs::create_dir(paths.instance_dir()).unwrap();
        std::fs::set_permissions(paths.instance_dir(), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let running = std::os::unix::net::UnixDatagram::bind(paths.instance_socket()).unwrap();
        let arguments = vec![
            "dev".into(),
            "yard://tour/a?token=secret".into(),
            "--flag=secret".into(),
            std::ffi::OsString::from_vec(vec![b'y', b':', 0xff]),
        ];
        let (instance, urls) = shell::start_instance(&paths, ID, arguments, "yard");
        assert!(matches!(instance, shell::Instance::Forwarded));
        assert_eq!(urls, ["yard://tour/a?token=secret"]);
        let mut buffer = vec![0; 1024];
        let len = running.recv(&mut buffer).unwrap();
        assert_eq!(
            shell::decode_request(&buffer[..len]).unwrap(),
            ["yard://tour/a?token=secret"]
        );

        // Without URLs the request is empty: the running instance comes to
        // the front.
        let (instance, urls) = shell::start_instance(&paths, ID, vec!["dev".into()], "yard");
        assert!(matches!(instance, shell::Instance::Forwarded));
        assert!(urls.is_empty());
        let len = running.recv(&mut buffer).unwrap();
        assert!(shell::decode_request(&buffer[..len]).unwrap().is_empty());
    }

    #[test]
    fn the_first_start_listens_and_the_second_hands_over() {
        let (_home, paths) = home();
        let shell::Instance::Listening(_first) = shell::check_instance(&paths, ID, &[]) else {
            panic!("the first start listens");
        };
        assert!(paths.instance_socket().exists());
        assert!(matches!(
            shell::check_instance(&paths, ID, &strings(&["yard://tour/a"])),
            shell::Instance::Forwarded
        ));
    }

    // GPUI's test scheduler refuses work from another thread, so no request
    // is sent here; the listener's thread is tested in `src/instance/tests.rs`.
    #[gpui::test]
    fn the_socket_is_removed_on_quit(cx: &mut TestAppContext) {
        let (_home, paths) = home();
        let shell::Instance::Listening(listener) = shell::check_instance(&paths, ID, &[]) else {
            panic!("the first start listens");
        };
        cx.update(|cx| shell::serve_instance_requests(cx, listener, "yard"));
        assert!(paths.instance_socket().exists());
        cx.quit();
        assert!(
            !paths.instance_socket().exists(),
            "the socket is removed on quit"
        );
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
#[test]
fn other_systems_run_without_one_instance() {
    let home = tempfile::tempdir().unwrap();
    let paths = yard_paths::Paths::under(home.path());
    assert!(matches!(
        shell::check_instance(&paths, ID, &[]),
        shell::Instance::Alone(_)
    ));
    assert!(!paths.instance_dir().exists());
}

type Opened = Rc<RefCell<Vec<String>>>;

// The product's scheme is `test`, and the extension `tour` records the URLs
// it gets, as the startup sets them up.
fn app_with_tour(cx: &mut TestAppContext) -> Opened {
    let opened = Opened::default();
    cx.update(|cx: &mut App| {
        yard_ui::init(cx);
        shell::set_url_scheme(cx, "test");
        let tour = shell::Extension::new(cx, "tour").unwrap();
        let opened = opened.clone();
        tour.on_open_url(cx, move |url, _| opened.borrow_mut().push(url.to_owned()));
        shell::extensions_loaded(cx);
    });
    opened
}

#[gpui::test]
fn each_request_opens_its_urls_and_brings_the_window_to_the_front(cx: &mut TestAppContext) {
    let opened = app_with_tour(cx);
    let (requests, received) = futures::channel::mpsc::unbounded();
    cx.update(|cx| shell::handle_instance_requests(cx, received, "test"));
    let request = shell::encode_request(&strings(&["dev", "test://tour/a", "other://x"])).unwrap();
    requests.unbounded_send(request).unwrap();
    requests
        .unbounded_send(shell::encode_request(&[]).unwrap())
        .unwrap();
    requests.unbounded_send(vec![9]).unwrap();
    cx.run_until_parked();
    assert_eq!(*opened.borrow(), ["test://tour/a"]);
}
