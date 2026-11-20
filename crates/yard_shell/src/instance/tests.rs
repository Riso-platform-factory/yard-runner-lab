//! The Unix transport, on every Unix, although only Linux checks for the
//! instance at startup.

use super::{
    Instance, decode_request,
    unix::{check, is_own_socket, own_uid, private_folder, receive, remove_own_socket},
};
use futures::StreamExt as _;
use std::{
    fs,
    os::unix::{
        fs::{MetadataExt as _, PermissionsExt as _},
        net::UnixDatagram,
    },
    path::{Path, PathBuf},
};

struct Home {
    _temp: tempfile::TempDir,
    dir: PathBuf,
    socket: PathBuf,
    lock: PathBuf,
}

fn home() -> Home {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("data/instance");
    Home {
        socket: dir.join("instance.sock"),
        lock: dir.join("instance.lock"),
        dir,
        _temp: temp,
    }
}

fn check_in(home: &Home, arguments: &[&str]) -> Instance {
    let arguments: Vec<String> = arguments
        .iter()
        .map(|argument| argument.to_string())
        .collect();
    check(&home.dir, &home.socket, &home.lock, &arguments)
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

#[test]
fn the_first_start_listens_in_a_private_folder_and_the_second_hands_over() {
    let home = home();
    let Instance::Listening(first) = check_in(&home, &[]) else {
        panic!("the first start listens");
    };
    assert_eq!(mode(&home.dir), 0o700);
    assert_eq!(mode(&home.socket), 0o600);
    assert!(matches!(
        check_in(&home, &["yard://tour/a", "dev"]),
        Instance::Forwarded
    ));
    let mut buffer = vec![0; 1024];
    let len = first.inner.socket_for_tests().recv(&mut buffer).unwrap();
    assert_eq!(
        decode_request(&buffer[..len]).unwrap(),
        ["yard://tour/a", "dev"]
    );
}

#[test]
fn two_starts_at_once_end_with_exactly_one_listener() {
    for _ in 0..50 {
        let home = home();
        let barrier = std::sync::Barrier::new(2);
        let results: Vec<Instance> = std::thread::scope(|scope| {
            let starts: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        check_in(&home, &[])
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
fn a_stale_socket_is_removed_and_bound_again() {
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    // An instance that ended without removing its socket.
    drop(UnixDatagram::bind(&home.socket).unwrap());
    let Instance::Listening(_first) = check_in(&home, &[]) else {
        panic!("a stale socket is replaced");
    };
    assert!(matches!(check_in(&home, &[]), Instance::Forwarded));
}

#[test]
fn a_regular_file_or_a_link_at_the_socket_path_is_left_alone() {
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    fs::write(&home.socket, "keep").unwrap();
    assert!(matches!(check_in(&home, &[]), Instance::Alone(_)));
    assert_eq!(fs::read_to_string(&home.socket).unwrap(), "keep");

    fs::remove_file(&home.socket).unwrap();
    let target = home.dir.join("target");
    fs::write(&target, "keep").unwrap();
    std::os::unix::fs::symlink(&target, &home.socket).unwrap();
    assert!(matches!(check_in(&home, &[]), Instance::Alone(_)));
    assert!(fs::symlink_metadata(&home.socket).unwrap().is_symlink());
}

#[test]
fn a_socket_of_another_user_is_not_ours() {
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    let _socket = UnixDatagram::bind(&home.socket).unwrap();
    let metadata = fs::symlink_metadata(&home.socket).unwrap();
    assert!(is_own_socket(&metadata, own_uid()));
    assert!(!is_own_socket(&metadata, own_uid() + 1));
    let file = home.dir.join("file");
    fs::write(&file, "").unwrap();
    assert!(!is_own_socket(
        &fs::symlink_metadata(&file).unwrap(),
        own_uid()
    ));
}

#[test]
fn the_instance_folder_is_made_private_and_must_be_ours() {
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    fs::set_permissions(&home.dir, fs::Permissions::from_mode(0o755)).unwrap();
    private_folder(&home.dir, own_uid()).unwrap();
    assert_eq!(mode(&home.dir), 0o700);
    assert!(private_folder(&home.dir, own_uid() + 1).is_err());

    let link = home.dir.with_file_name("link");
    std::os::unix::fs::symlink(&home.dir, &link).unwrap();
    assert!(private_folder(&link, own_uid()).is_err());
}

#[test]
fn a_path_too_long_for_a_socket_runs_alone() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("a".repeat(200));
    let instance = check(
        &dir,
        &dir.join("instance.sock"),
        &dir.join("instance.lock"),
        &[],
    );
    assert!(matches!(instance, Instance::Alone(_)));
    assert!(!dir.exists());
}

#[test]
fn a_request_too_large_is_refused_by_the_sender() {
    let home = home();
    let Instance::Listening(_first) = check_in(&home, &[]) else {
        panic!("the first start listens");
    };
    let long = "a".repeat(super::MAX_REQUEST_LEN);
    match check_in(&home, &[&long]) {
        Instance::Refused(reason) => assert!(reason.contains("bytes"), "{reason}"),
        other => panic!("a request too large is refused: {other:?}"),
    }
}

#[test]
fn only_the_same_socket_is_removed_on_quit() {
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    let first = UnixDatagram::bind(&home.socket).unwrap();
    let metadata = fs::symlink_metadata(&home.socket).unwrap();
    let id = (metadata.dev(), metadata.ino());
    drop(first);
    fs::remove_file(&home.socket).unwrap();
    // Another instance bound the path since.
    let _second = UnixDatagram::bind(&home.socket).unwrap();
    remove_own_socket(&home.socket, id);
    assert!(home.socket.exists());

    let metadata = fs::symlink_metadata(&home.socket).unwrap();
    remove_own_socket(&home.socket, (metadata.dev(), metadata.ino()));
    assert!(!home.socket.exists());
}

#[test]
fn each_datagram_reaches_the_receiver_whole() {
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    let mut requests = receive(UnixDatagram::bind(&home.socket).unwrap()).unwrap();
    let sender = UnixDatagram::unbound().unwrap();
    sender.connect(&home.socket).unwrap();
    sender.send(b"\x01first\0").unwrap();
    sender.send(b"\x01").unwrap();
    assert_eq!(
        futures::executor::block_on(requests.next()).unwrap(),
        b"\x01first\0"
    );
    assert_eq!(
        futures::executor::block_on(requests.next()).unwrap(),
        b"\x01"
    );
}

// macOS keeps a Unix datagram to 2048 bytes; Linux takes the largest request.
#[cfg(target_os = "linux")]
#[test]
fn the_largest_request_arrives_and_a_larger_one_shows_its_size() {
    use super::MAX_REQUEST_LEN;
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    let mut requests = receive(UnixDatagram::bind(&home.socket).unwrap()).unwrap();
    let sender = UnixDatagram::unbound().unwrap();
    sender.connect(&home.socket).unwrap();
    sender.send(&vec![1; MAX_REQUEST_LEN]).unwrap();
    sender.send(&vec![1; MAX_REQUEST_LEN + 10]).unwrap();
    let largest = futures::executor::block_on(requests.next()).unwrap();
    assert_eq!(largest.len(), MAX_REQUEST_LEN);
    let larger = futures::executor::block_on(requests.next()).unwrap();
    assert!(larger.len() > MAX_REQUEST_LEN);
}
