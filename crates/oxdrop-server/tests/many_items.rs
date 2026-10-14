mod support;

use std::time::Duration;

use oxdrop_core::{AddError, Added};
use support::Harness;

#[tokio::test]
async fn adding_several_paths_reports_each_outcome_in_order() {
    let h = Harness::new();
    let first = h.file("a.txt", b"a");
    let second = h.file("b.txt", b"b");
    h.engine.add_path(&first).unwrap();
    let missing = h.file("gone.txt", b"x");
    std::fs::remove_file(&missing).unwrap();

    let outcomes = h.engine.add_paths(&[first, second, missing]);

    assert!(
        matches!(outcomes[0], Ok(Added::AlreadyPresent(_))),
        "{outcomes:?}"
    );
    assert!(matches!(outcomes[1], Ok(Added::New(_))), "{outcomes:?}");
    assert!(
        matches!(outcomes[2], Err(AddError::Unreadable { .. })),
        "{outcomes:?}"
    );
    assert_eq!(h.item_names(), ["a.txt", "b.txt"]);
}

#[tokio::test]
async fn adding_the_same_path_twice_keeps_one_item() {
    let h = Harness::new();
    let path = h.file("notes.txt", b"hello");

    let first = h.engine.add_path(&path).unwrap();
    let second = h.engine.add_path(&path).unwrap();

    assert_eq!(second, Added::AlreadyPresent(first.id()));
    assert_eq!(h.item_names(), ["notes.txt"]);
}

#[tokio::test]
async fn colliding_names_get_numbered_before_the_extension() {
    let h = Harness::new();
    let paths = [
        h.file_in("Desktop", "photo.jpg", b"1"),
        h.file_in("Pictures", "photo.jpg", b"2"),
        // Android storage is case-insensitive, so this collides too.
        h.file_in("Camera", "Photo.JPG", b"3"),
        h.file_in("Desktop", "archive.tar.gz", b"4"),
        h.file_in("Pictures", "archive.tar.gz", b"5"),
        h.file_in("Desktop", ".env", b"6"),
        h.file_in("Pictures", ".env", b"7"),
    ];

    for outcome in h.engine.add_paths(&paths) {
        outcome.unwrap();
    }

    assert_eq!(
        h.item_names(),
        [
            "photo.jpg",
            "photo (2).jpg",
            "Photo (3).JPG",
            "archive.tar.gz",
            "archive (2).tar.gz",
            ".env",
            ".env (2)",
        ]
    );
}

#[tokio::test]
async fn a_numbered_item_downloads_under_its_item_name() {
    let h = Harness::new();
    h.engine
        .add_path(&h.file_in("Desktop", "photo.jpg", b"1"))
        .unwrap();
    let second = h
        .engine
        .add_path(&h.file_in("Pictures", "photo.jpg", b"2"))
        .unwrap()
        .id();

    let response = h.get(&format!("items/{second}")).await;

    let disposition = response.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        disposition.contains("filename=\"photo (2).jpg\""),
        "{disposition}"
    );
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"2");
}

#[tokio::test]
async fn removing_a_name_frees_it_for_the_next_item() {
    let h = Harness::new();
    let first = h
        .engine
        .add_path(&h.file_in("Desktop", "photo.jpg", b"1"))
        .unwrap();
    h.engine.remove_item(first.id());

    h.engine
        .add_path(&h.file_in("Pictures", "photo.jpg", b"2"))
        .unwrap();

    assert_eq!(h.item_names(), ["photo.jpg"]);
}

#[cfg(unix)] // Windows can't create these names at all.
#[tokio::test]
async fn names_are_made_android_safe_only_in_the_download() {
    let h = Harness::new();
    let id = h.add_file("draft: v2?.txt", b"x");

    let response = h.get(&format!("items/{id}")).await;

    let disposition = response.headers()["content-disposition"].to_str().unwrap();
    assert!(
        disposition.contains("filename=\"draft_ v2_.txt\""),
        "{disposition}"
    );
    assert!(
        disposition.contains("filename*=UTF-8''draft_%20v2_.txt"),
        "{disposition}"
    );
    assert_eq!(
        h.item_names(),
        ["draft: v2?.txt"],
        "the Sender shows the real name"
    );
}

#[tokio::test]
async fn a_removed_item_is_gone_for_the_receiver() {
    let h = Harness::new();
    let kept = h.add_file("kept.txt", b"k");
    let removed = h.add_file("removed.txt", b"r");

    assert!(h.engine.remove_item(removed));

    assert_eq!(h.item_names(), ["kept.txt"]);
    assert_eq!(h.get(&format!("items/{removed}")).await.status(), 404);
    assert_eq!(h.get(&format!("items/{kept}")).await.status(), 200);
    let page = h.get("").await.text().await.unwrap();
    assert!(!page.contains("removed.txt"), "{page}");
    assert!(
        !h.engine.remove_item(removed),
        "removing twice reports nothing removed"
    );
}

#[tokio::test]
async fn removing_an_item_aborts_its_download_in_progress() {
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
    let mut response = h.get(&format!("items/{id}")).await;
    assert_eq!(response.status(), 200);
    response.chunk().await.unwrap().expect("first chunk");

    h.engine.remove_item(id);

    let finished = tokio::time::timeout(Duration::from_secs(10), async {
        let mut received = 0usize;
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => received += chunk.len(),
                Ok(None) => return Ok(received),
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .expect("the download stops promptly");
    assert!(
        finished.is_err(),
        "download completed instead of aborting: {finished:?}"
    );
}

#[tokio::test]
async fn an_item_can_be_downloaded_more_than_once() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"hello");

    for _ in 0..2 {
        let response = h.get(&format!("items/{id}")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"hello");
    }
}

#[tokio::test]
async fn events_start_with_the_current_items() {
    let h = Harness::new();
    h.add_file("notes.txt", b"hello");

    let mut events = h.events().await;
    let first = events.next().await;

    let item = &first["items"][0];
    assert_eq!(item["name"], "notes.txt");
    assert_eq!(item["size"], "5 B");
    assert!(item["id"].is_string());
}

#[tokio::test]
async fn adding_and_removing_items_produce_events() {
    let h = Harness::new();
    let mut events = h.events().await;
    assert!(events.next_names().await.is_empty());

    let id = h.add_file("notes.txt", b"hello");
    assert_eq!(events.next_names().await, ["notes.txt"]);

    h.engine.remove_item(id);
    assert!(events.next_names().await.is_empty());
}

#[tokio::test]
async fn the_engine_publishes_state_changes_to_subscribers() {
    let h = Harness::new();
    let mut updates = h.engine.subscribe();

    h.add_file("notes.txt", b"hello");

    tokio::time::timeout(Duration::from_secs(5), updates.changed())
        .await
        .expect("an update arrives")
        .unwrap();
    let names: Vec<String> = updates
        .borrow()
        .items
        .iter()
        .map(|i| i.name.clone())
        .collect();
    assert_eq!(names, ["notes.txt"]);
}

#[tokio::test]
async fn receiver_is_connected_while_the_event_stream_is_open() {
    let h = Harness::new();
    assert!(!h.engine.state().receiver_connected);

    let mut events = h.events().await;
    events.next().await;
    h.eventually("Receiver connected", |s| s.receiver_connected)
        .await;

    drop(events);
    h.eventually("Receiver disconnected", |s| !s.receiver_connected)
        .await;
}

#[tokio::test]
async fn page_shows_every_item_and_subscribes_to_events() {
    let h = Harness::new();
    h.add_file("a.txt", b"a");
    h.add_file("b.txt", b"b");

    let page = h.get("").await.text().await.unwrap();

    assert!(page.contains("a.txt") && page.contains("b.txt"), "{page}");
    assert!(page.contains("new EventSource(\"events\")"), "{page}");
}
