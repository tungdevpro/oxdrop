//! The Sender sees each Item's progress: bytes served toward its most recent download.

mod support;

use oxdrop_core::Progress;
use support::Harness;

#[tokio::test]
async fn a_full_download_is_done() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", &[7; 300_000]);
    assert_eq!(h.engine.state().progress_of(id), Progress::NotStarted);

    let body = h.get(&format!("items/{id}")).await.bytes().await.unwrap();

    assert_eq!(body.len(), 300_000);
    h.eventually("the download is done", |state| {
        state.progress_of(id) == Progress::Done
    })
    .await;
}

/// A sparse file far larger than any socket buffer, so its download can't finish early.
fn big_file(h: &Harness, name: &str, len: u64) -> std::path::PathBuf {
    let path = h.file(name, b"");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(len)
        .unwrap();
    path
}

#[tokio::test]
async fn a_download_the_receiver_breaks_off_fails_and_resuming_it_finishes_it() {
    let h = Harness::new();
    let total = 256 * 1024 * 1024;
    let id = h
        .engine
        .add_path(&big_file(&h, "movie.mp4", total))
        .unwrap()
        .id();
    let mut response = h.get(&format!("items/{id}")).await;
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    let mut received = 0u64;
    while received < 1024 * 1024 {
        received += response.chunk().await.unwrap().expect("a chunk").len() as u64;
    }

    drop(response);

    h.eventually("the download fails", |state| {
        matches!(state.progress_of(id), Progress::Failed { served, total: t }
            if served >= received && served < t && t == total)
    })
    .await;
    let Progress::Failed { served, .. } = h.engine.state().progress_of(id) else {
        unreachable!()
    };

    let rest = h
        .client
        .get(format!("{}items/{id}", h.link()))
        .header("range", format!("bytes={served}-"))
        .header("if-range", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(rest.status(), 206);
    let rest = rest.bytes().await.unwrap();

    assert_eq!(rest.len() as u64, total - served);
    h.eventually("the resumed download is done", |state| {
        state.progress_of(id) == Progress::Done
    })
    .await;
}

#[tokio::test]
async fn a_head_request_is_not_a_download() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", &[7; 300_000]);

    let head = h
        .client
        .head(format!("{}items/{id}", h.link()))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);

    // Give a wrongly counted response time to show up.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(h.engine.state().progress_of(id), Progress::NotStarted);
}

#[tokio::test]
async fn download_all_moves_each_items_progress() {
    let h = Harness::new();
    let notes = h.add_file("notes.txt", &[7; 300_000]);
    h.file_in("Photos", "beach.jpg", &[1; 500_000]);
    h.file_in("Photos/2024", "cake.jpg", &[2; 200_000]);
    let photos = h.engine.add_path(&h.path("Photos")).unwrap().id();
    let empty = h.add_file("empty.txt", b"");
    let mut updates = h.engine.subscribe();

    let body = h.get("all").await.bytes().await.unwrap();
    assert!(!body.is_empty());

    for (what, id) in [
        ("notes.txt", notes),
        ("Photos", photos),
        ("empty.txt", empty),
    ] {
        h.eventually(what, |state| state.progress_of(id) == Progress::Done)
            .await;
    }
    // The state stream carried it too, so the Sender window sees it without polling.
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates.borrow_and_update().progress_of(photos),
        Progress::Done
    );
}

#[tokio::test]
async fn download_all_resumed_part_way_only_moves_the_items_it_serves() {
    let h = Harness::new();
    let first = h.add_file("first.bin", &[1; 400_000]);
    let second = h.add_file("second.bin", &[2; 400_000]);
    let head = h
        .client
        .head(format!("{}all", h.link()))
        .send()
        .await
        .unwrap();
    let length: u64 = head.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();

    // Only the back half of the zip: the end of second.bin, then the central directory.
    let tail = h
        .client
        .get(format!("{}all", h.link()))
        .header("range", format!("bytes={}-", length - 1000))
        .send()
        .await
        .unwrap();
    assert_eq!(tail.status(), 206);
    tail.bytes().await.unwrap();

    h.eventually("second.bin moves", |state| {
        matches!(state.progress_of(second), Progress::Receiving { served, .. } if served > 0)
    })
    .await;
    assert_eq!(h.engine.state().progress_of(first), Progress::NotStarted);
}

#[tokio::test]
async fn downloading_an_empty_file_is_done() {
    let h = Harness::new();
    let id = h.add_file("empty.txt", b"");

    let body = h.get(&format!("items/{id}")).await.bytes().await.unwrap();

    assert!(body.is_empty());
    h.eventually("the download is done", |state| {
        state.progress_of(id) == Progress::Done
    })
    .await;
}

#[tokio::test]
async fn a_head_request_for_an_empty_file_is_not_a_download_either() {
    let h = Harness::new();
    let id = h.add_file("empty.txt", b"");

    let head = h
        .client
        .head(format!("{}items/{id}", h.link()))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(h.engine.state().progress_of(id), Progress::NotStarted);
}

#[tokio::test]
async fn refreshing_or_removing_an_item_forgets_its_progress() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"my notes");
    h.get(&format!("items/{id}")).await.bytes().await.unwrap();
    h.eventually("done", |state| state.progress_of(id) == Progress::Done)
        .await;

    h.engine.refresh_item(id).unwrap();
    assert_eq!(h.engine.state().progress_of(id), Progress::NotStarted);

    h.get(&format!("items/{id}")).await.bytes().await.unwrap();
    h.eventually("done again", |state| {
        state.progress_of(id) == Progress::Done
    })
    .await;
    h.engine.remove_item(id);
    assert!(h.engine.state().progress.is_empty());
}
