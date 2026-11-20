use super::unix::receive;
use futures::StreamExt as _;
use std::os::unix::net::UnixDatagram;

#[test]
fn each_datagram_reaches_the_receiver_whole() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("instance.sock");
    let mut requests = receive(UnixDatagram::bind(&path).unwrap()).unwrap();
    let sender = UnixDatagram::unbound().unwrap();
    sender.connect(&path).unwrap();
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
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn the_largest_request_arrives_and_a_larger_one_shows_its_size() {
    use super::MAX_REQUEST_LEN;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("instance.sock");
    let mut requests = receive(UnixDatagram::bind(&path).unwrap()).unwrap();
    let sender = UnixDatagram::unbound().unwrap();
    sender.connect(&path).unwrap();
    sender.send(&vec![1; MAX_REQUEST_LEN]).unwrap();
    sender.send(&vec![1; MAX_REQUEST_LEN + 10]).unwrap();
    let largest = futures::executor::block_on(requests.next()).unwrap();
    assert_eq!(largest.len(), MAX_REQUEST_LEN);
    let larger = futures::executor::block_on(requests.next()).unwrap();
    assert!(larger.len() > MAX_REQUEST_LEN);
}
