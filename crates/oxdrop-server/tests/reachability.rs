//! The Sender is reachable only the way we intend: at the bound address, by
//! whoever holds the Transfer Link.

mod support;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use oxdrop_server::{BindAddress, EngineConfig};
use support::Harness;

#[tokio::test]
async fn requests_naming_another_host_are_rejected() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"hello");
    let port = h.engine.state().link.address.port();

    // A DNS-rebinding page reaches us under its own host name, never our IP literal.
    for host in [
        format!("evil.example:{port}"),
        format!("localhost:{port}"),
        "127.0.0.1".to_owned(),
        format!("127.0.0.1:{}", port.wrapping_add(1)),
    ] {
        for path in ["", "events", &format!("items/{id}")] {
            let response = h
                .client
                .get(format!("{}{path}", h.link()))
                .header(reqwest::header::HOST, &host)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 421, "Host: {host}, path: {path:?}");
        }
    }
}

#[tokio::test]
async fn item_ids_cannot_reach_outside_the_transfer() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"hello");
    h.file("secret.txt", b"not offered");

    // Percent-encoded, so the client sends them as-is rather than normalizing `..` away.
    for path in [
        "items/..%2Fsecret.txt",
        "items/..%2F..%2F..%2Fetc%2Fpasswd",
        "items/%2E%2E%2Fsecret.txt",
        &format!("items/{id}%2F..%2Fsecret.txt"),
        &format!("items/{id}%00"),
        "items/..%5Csecret.txt",
        "items/-1",
        "items/18446744073709551616",
    ] {
        let response = h.get(path).await;
        assert_eq!(response.status(), 404, "{path}");
        let body = response.bytes().await.unwrap();
        assert!(!body.windows(11).any(|w| w == b"not offered"), "{path}");
    }
}

#[tokio::test]
async fn a_taken_port_falls_back_to_a_free_one_shown_in_the_link() {
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = squatter.local_addr().unwrap().port();

    let h = Harness::start(EngineConfig {
        bind: BindAddress::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        port: taken,
        ..EngineConfig::default()
    });

    let port = h.engine.state().link.address.port();
    assert_ne!(port, taken);
    assert_ne!(port, 0);
    h.add_file("notes.txt", b"hello");
    assert!(h.get("").await.text().await.unwrap().contains("notes.txt"));
}

#[tokio::test]
async fn a_free_port_is_used_as_asked() {
    let free = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let h = Harness::start(EngineConfig {
        bind: BindAddress::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        port: free,
        ..EngineConfig::default()
    });

    assert_eq!(h.engine.state().link.address.port(), free);
}

#[tokio::test]
async fn switching_interface_moves_the_link_and_keeps_the_secret() {
    let h = Harness::start(EngineConfig {
        bind: BindAddress::Ip(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        port: 0,
        ..EngineConfig::default()
    });
    h.add_file("notes.txt", b"hello");
    let before = h.engine.state().link;
    let updates = h.engine.subscribe();

    h.engine
        .switch_interface(IpAddr::V4(Ipv4Addr::LOCALHOST))
        .unwrap();

    let after = h.engine.state().link;
    assert_eq!(after.address.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(after.secret, before.secret);
    assert!(
        updates.has_changed().unwrap(),
        "subscribers hear about the new link"
    );
    assert!(h.get("").await.text().await.unwrap().contains("notes.txt"));
    // Only the chosen interface is listened on: the old address is gone.
    let old = h.client.get(before.to_string()).send().await;
    assert!(old.is_err(), "old address still answers: {old:?}");
}
