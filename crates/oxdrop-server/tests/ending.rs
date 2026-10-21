//! Ending a Transfer kills its Transfer Link and opens a fresh one.

mod support;

use support::Harness;

#[tokio::test]
async fn ending_a_transfer_kills_its_link_and_opens_a_fresh_empty_one() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"my notes");
    let old_link = h.link();

    h.engine.end_transfer();

    let new_link = h.link();
    assert_ne!(new_link, old_link);
    assert!(h.engine.state().items.is_empty());
    let old_page = h.client.get(&old_link).send().await.unwrap();
    assert_eq!(old_page.status(), 410);
    assert!(
        old_page
            .text()
            .await
            .unwrap()
            .contains("This Transfer has ended"),
    );
    let old_item = h
        .client
        .get(format!("{old_link}items/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(old_item.status(), 410);
    assert_eq!(h.get("").await.status(), 200, "the new link works");
    let address = h.engine.state().link.address;
    let unknown = format!("http://{address}/t/{}/", "A".repeat(22));
    assert_eq!(h.client.get(unknown).send().await.unwrap().status(), 404);
}

#[tokio::test]
async fn ending_tells_open_pages_and_closes_their_streams() {
    let h = Harness::new();
    h.add_file("notes.txt", b"my notes");
    let mut events = h.events().await;
    assert_eq!(events.next_names().await, ["notes.txt"]);

    h.engine.end_transfer();

    let (event, _) = events.next_event().await.expect("an event before closing");
    assert_eq!(event, "ended");
    assert_eq!(events.next_event().await, None, "the stream closes");
    h.eventually("the Receiver counts as gone", |state| {
        !state.receiver_connected
    })
    .await;
}

#[tokio::test]
async fn ending_cuts_off_downloads_in_progress() {
    let h = Harness::new();
    let path = h.file("big.bin", b"");
    // Sparse and far larger than any socket buffer, so the download can't finish early.
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    let id = h.engine.add_path(&path).unwrap().id();
    let mut file = h.get(&format!("items/{id}")).await;
    let mut all = h.get("all").await;
    for response in [&mut file, &mut all] {
        assert_eq!(response.status(), 200);
        response.chunk().await.unwrap().expect("first chunk");
    }

    h.engine.end_transfer();

    for (what, mut response) in [("file", file), ("all", all)] {
        let finished = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match response.chunk().await {
                    Ok(Some(_)) => {}
                    Ok(None) => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .expect("the download stops promptly");
        assert!(finished.is_err(), "{what} completed instead of aborting");
    }
}
