//! The Unix transport, on every Unix, although only Linux checks for the
//! instance at startup.

use super::{
    Instance, decode_request,
    unix::{
        SocketId, check, check_with, data_folder_is_safe, is_own_socket, mark_socket, own_egid,
        own_uid, private_folder, receive, remove_own_socket, take_lock,
    },
};
use futures::StreamExt as _;
use std::{
    fs,
    os::unix::{
        fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
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
    // The data folder as a normal home has it, whatever the umask.
    fs::create_dir(temp.path().join("data")).unwrap();
    fs::set_permissions(temp.path().join("data"), fs::Permissions::from_mode(0o755)).unwrap();
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
    let Instance::Listening(mut first) = check_in(&home, &[]) else {
        panic!("the first start listens");
    };
    assert_eq!(mode(&home.dir), 0o700);
    assert_eq!(mode(&home.socket), 0o600);
    assert!(matches!(
        check_in(&home, &["yard://tour/a", "dev"]),
        Instance::Forwarded
    ));
    let request = futures::executor::block_on(first.inner.requests_for_tests().next()).unwrap();
    assert_eq!(decode_request(&request).unwrap(), ["yard://tour/a", "dev"]);
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
    let first_id = mark_socket(&home.socket).unwrap();
    drop(first);
    fs::remove_file(&home.socket).unwrap();
    // Another instance bound the path since, at once, so that Linux may give
    // it the same inode in the same clock tick.
    let _second = UnixDatagram::bind(&home.socket).unwrap();
    let second_id = mark_socket(&home.socket).unwrap();
    assert_ne!(first_id, second_id);
    remove_own_socket(&home.socket, &home.lock, first_id);
    assert!(home.socket.exists());

    remove_own_socket(&home.socket, &home.lock, second_id);
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

#[test]
fn a_data_folder_others_may_write_to_runs_alone() {
    let home = home();
    let data = home.dir.parent().unwrap();
    fs::set_permissions(data, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(matches!(check_in(&home, &[]), Instance::Alone(_)));
    assert!(!home.dir.exists());
    fs::set_permissions(data, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(private_folder(&home.dir, own_uid() + 1).is_err());
}

#[test]
fn a_group_may_write_to_the_data_folder_only_when_it_is_the_users_own() {
    let (uid, egid) = (own_uid(), own_egid());
    assert!(data_folder_is_safe(0o40755, uid, egid + 1, uid, egid).is_ok());
    assert!(data_folder_is_safe(0o40775, uid, egid, uid, egid).is_ok());
    assert!(data_folder_is_safe(0o40775, uid, egid + 1, uid, egid).is_err());
    assert!(data_folder_is_safe(0o40757, uid, egid, uid, egid).is_err());
    assert!(data_folder_is_safe(0o40700, uid + 1, egid, uid, egid).is_err());

    // A real folder of the user's own group, group-writable, is used.
    let home = home();
    let data = home.dir.parent().unwrap();
    if fs::metadata(data).unwrap().gid() == egid {
        fs::set_permissions(data, fs::Permissions::from_mode(0o775)).unwrap();
        let Instance::Listening(_first) = check_in(&home, &[]) else {
            panic!("a data folder of the user's own group is used");
        };
    }
}

#[test]
fn a_data_folder_the_start_makes_is_private() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("fresh/data/instance");
    let Instance::Listening(_first) = check(
        &dir,
        &dir.join("instance.sock"),
        &dir.join("instance.lock"),
        &[],
    ) else {
        panic!("the first start listens");
    };
    assert_eq!(mode(dir.parent().unwrap()), 0o700);
    assert_eq!(mode(&temp.path().join("fresh")), 0o700);
}

#[test]
fn a_fifo_or_a_link_at_the_lock_path_never_holds_up_the_start() {
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    let path = std::ffi::CString::new(home.lock.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `path` is a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(matches!(check_in(&home, &[]), Instance::Alone(_)));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(
        fs::symlink_metadata(&home.lock)
            .unwrap()
            .file_type()
            .is_fifo()
    );

    fs::remove_file(&home.lock).unwrap();
    let target = home.dir.join("target");
    fs::write(&target, "").unwrap();
    std::os::unix::fs::symlink(&target, &home.lock).unwrap();
    assert!(take_lock(&home.lock, own_uid()).is_err());
    assert!(matches!(check_in(&home, &[]), Instance::Alone(_)));

    fs::remove_file(&home.lock).unwrap();
    fs::write(&home.lock, "").unwrap();
    assert!(take_lock(&home.lock, own_uid() + 1).is_err());
    assert!(take_lock(&home.lock, own_uid()).is_ok());
}

#[test]
fn a_listener_that_cannot_start_leaves_no_socket() {
    let home = home();
    let instance = check_with(&home.dir, &home.socket, &home.lock, &[], |_| {
        Err(std::io::Error::other("no thread"))
    });
    assert!(matches!(instance, Instance::Alone(_)));
    assert!(!home.socket.exists());
    // The next start is the one instance.
    let Instance::Listening(_first) = check_in(&home, &[]) else {
        panic!("the next start listens");
    };
}

#[test]
fn a_full_queue_refuses_the_hand_off_in_time() {
    let home = home();
    fs::create_dir_all(&home.dir).unwrap();
    // An instance that does not read: its queue fills up.
    let _listener = UnixDatagram::bind(&home.socket).unwrap();
    let filler = UnixDatagram::unbound().unwrap();
    filler.connect(&home.socket).unwrap();
    filler.set_nonblocking(true).unwrap();
    while filler.send(b"\x01").is_ok() {}
    let started = std::time::Instant::now();
    match check_in(&home, &["yard://tour/a"]) {
        Instance::Refused(_) => {}
        other => panic!("a full queue refuses the hand-off: {other:?}"),
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn quitting_waits_for_a_start_that_holds_the_lock() {
    let home = home();
    let Instance::Listening(first) = check_in(&home, &[]) else {
        panic!("the first start listens");
    };
    let id = SocketId::of(&fs::symlink_metadata(&home.socket).unwrap());
    drop(first);
    let held = take_lock(&home.lock, own_uid()).unwrap();
    std::thread::scope(|scope| {
        let quit = scope.spawn(|| remove_own_socket(&home.socket, &home.lock, id));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            home.socket.exists(),
            "the socket is removed only under the lock"
        );
        drop(held);
        quit.join().unwrap();
    });
    assert!(!home.socket.exists());
}
