//! A Transfer left alone ends after 30 minutes, with a countdown in the last 2.

mod support;

use std::time::Duration;

use oxdrop_core::IdleState;
use support::Harness;

const MINUTE: Duration = Duration::from_secs(60);

/// Lets the engine react to the clock moving, so "nothing happened" can be checked.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

async fn ended(h: &Harness, old_link: &str) {
    h.eventually("the Transfer ends", |state| {
        state.link.to_string() != old_link
    })
    .await;
    let old = h.client.get(old_link).send().await.unwrap();
    assert_eq!(old.status(), 410);
}

async fn still_open(h: &Harness, link: &str) {
    settle().await;
    assert_eq!(h.link(), link, "the Transfer is still open");
}

#[tokio::test]
async fn left_alone_it_warns_two_minutes_before_ending() {
    let h = Harness::new();
    h.add_file("notes.txt", b"my notes");
    let link = h.link();
    assert!(matches!(h.engine.state().idle, IdleState::Open { .. }));

    h.clock.advance(28 * MINUTE);

    h.eventually("the countdown starts", |state| {
        matches!(state.idle, IdleState::Warning { .. })
    })
    .await;
    still_open(&h, &link).await;

    h.clock.advance(2 * MINUTE);

    ended(&h, &link).await;
    assert!(h.engine.state().items.is_empty());
    assert!(matches!(h.engine.state().idle, IdleState::Open { .. }));
}

#[tokio::test]
async fn keep_open_puts_the_deadline_off_again() {
    let h = Harness::new();
    let link = h.link();
    h.clock.advance(29 * MINUTE);
    h.eventually("the countdown starts", |state| {
        matches!(state.idle, IdleState::Warning { .. })
    })
    .await;

    h.engine.keep_open();

    h.eventually("the countdown stops", |state| {
        matches!(state.idle, IdleState::Open { .. })
    })
    .await;
    h.clock.advance(20 * MINUTE);
    still_open(&h, &link).await;
    h.clock.advance(10 * MINUTE);
    ended(&h, &link).await;
}

#[tokio::test]
async fn a_request_from_the_receiver_counts_as_activity() {
    let h = Harness::new();
    let link = h.link();
    h.clock.advance(20 * MINUTE);

    assert_eq!(h.get("").await.status(), 200);

    h.clock.advance(20 * MINUTE);
    still_open(&h, &link).await;
    h.clock.advance(10 * MINUTE);
    ended(&h, &link).await;
}

#[tokio::test]
async fn a_request_with_a_wrong_secret_is_not_activity() {
    let h = Harness::new();
    let link = h.link();
    h.clock.advance(20 * MINUTE);
    let address = h.engine.state().link.address;

    let wrong = format!("http://{address}/t/{}/", "A".repeat(22));
    assert_eq!(h.client.get(wrong).send().await.unwrap().status(), 404);

    h.clock.advance(10 * MINUTE);
    ended(&h, &link).await;
}

#[tokio::test]
async fn a_sender_command_counts_as_activity() {
    let h = Harness::new();
    let link = h.link();
    h.clock.advance(20 * MINUTE);

    h.add_file("notes.txt", b"my notes");

    h.clock.advance(20 * MINUTE);
    still_open(&h, &link).await;
    h.clock.advance(10 * MINUTE);
    ended(&h, &link).await;
}

#[tokio::test]
async fn a_download_in_progress_keeps_it_open_until_it_stops() {
    let h = Harness::new();
    let path = h.file("big.bin", b"");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    let id = h.engine.add_path(&path).unwrap().id();
    let link = h.link();
    let mut response = h.get(&format!("items/{id}")).await;
    response.chunk().await.unwrap().expect("first chunk");

    h.clock.advance(45 * MINUTE);

    still_open(&h, &link).await;
    assert_eq!(h.engine.state().idle, IdleState::Busy);

    drop(response);

    h.eventually("the download stops", |state| {
        matches!(state.idle, IdleState::Open { .. })
    })
    .await;
    h.clock.advance(29 * MINUTE);
    still_open(&h, &link).await;
    h.clock.advance(MINUTE);
    ended(&h, &link).await;
}

#[tokio::test]
async fn an_open_receiver_page_alone_does_not_keep_it_open() {
    let h = Harness::new();
    let link = h.link();
    let mut events = h.events().await;
    events.next().await;

    h.clock.advance(30 * MINUTE);

    ended(&h, &link).await;
    assert_eq!(events.next_event().await.unwrap().0, "ended");
}
