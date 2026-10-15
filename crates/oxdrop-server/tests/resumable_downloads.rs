//! The Receiver never gets a corrupt or half-old file, and interrupted downloads resume.

mod support;

use std::path::Path;
use std::time::{Duration, SystemTime};

use oxdrop_core::ItemStatus;
use support::Harness;

#[tokio::test]
async fn an_item_edited_after_adding_is_changed_and_refused() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"hello");
    h.file("notes.txt", b"hello, edited");

    let response = h.get(&format!("items/{id}")).await;

    assert_eq!(response.status(), 409);
    let body = response.text().await.unwrap();
    assert!(body.contains("changed"), "{body}");
    assert_eq!(h.engine.state().items[0].status, ItemStatus::Changed);
}

#[tokio::test]
async fn an_item_deleted_after_adding_is_missing_and_refused() {
    let h = Harness::new();
    let path = h.file("notes.txt", b"hello");
    let id = h.engine.add_path(&path).unwrap().id();
    std::fs::remove_file(&path).unwrap();

    let response = h.get(&format!("items/{id}")).await;

    assert_eq!(response.status(), 409);
    let body = response.text().await.unwrap();
    assert!(body.contains("no longer"), "{body}");
    assert_eq!(h.engine.state().items[0].status, ItemStatus::Missing);
}

#[tokio::test]
async fn an_edit_that_keeps_the_size_is_still_changed() {
    let h = Harness::new();
    let path = h.file("notes.txt", b"hello");
    let id = h.engine.add_path(&path).unwrap().id();
    std::fs::write(&path, b"jello").unwrap();
    set_modified(&path, SystemTime::now() + Duration::from_secs(60));

    assert_eq!(h.get(&format!("items/{id}")).await.status(), 409);
    assert_eq!(h.engine.state().items[0].status, ItemStatus::Changed);
}

/// Sets a file's modification time, so tests don't depend on the filesystem's clock resolution.
fn set_modified(path: &Path, time: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// A sparse file far larger than any socket buffer, so a download of it can't finish early.
fn big_file(h: &Harness, name: &str) -> std::path::PathBuf {
    let path = h.file(name, b"");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(256 * 1024 * 1024)
        .unwrap();
    path
}

/// Reads the rest of a response body: its length if it ends cleanly, the error if it's cut off.
async fn finish(mut response: reqwest::Response) -> Result<usize, reqwest::Error> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut received = 0;
        while let Some(chunk) = response.chunk().await? {
            received += chunk.len();
        }
        Ok(received)
    })
    .await
    .expect("the download ends promptly")
}

#[tokio::test]
async fn a_file_that_shrinks_mid_download_aborts_it() {
    let h = Harness::new();
    let path = big_file(&h, "video.mp4");
    let id = h.engine.add_path(&path).unwrap().id();
    let mut response = h.get(&format!("items/{id}")).await;
    assert_eq!(response.status(), 200);
    response.chunk().await.unwrap().expect("first chunk");

    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(1000)
        .unwrap();

    let finished = finish(response).await;
    assert!(
        finished.is_err(),
        "completed instead of aborting: {finished:?}"
    );
}

#[tokio::test]
async fn a_file_that_grows_mid_download_aborts_it() {
    let h = Harness::new();
    let path = big_file(&h, "video.mp4");
    let id = h.engine.add_path(&path).unwrap().id();
    let mut response = h.get(&format!("items/{id}")).await;
    assert_eq!(response.status(), 200);
    response.chunk().await.unwrap().expect("first chunk");

    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_len(file.metadata().unwrap().len() + 1_000_000)
        .unwrap();

    let finished = finish(response).await;
    assert!(
        finished.is_err(),
        "completed instead of aborting: {finished:?}"
    );
}

#[tokio::test]
async fn a_file_edited_in_place_mid_download_aborts_it() {
    let h = Harness::new();
    let path = big_file(&h, "video.mp4");
    let id = h.engine.add_path(&path).unwrap().id();
    let mut response = h.get(&format!("items/{id}")).await;
    assert_eq!(response.status(), 200);
    response.chunk().await.unwrap().expect("first chunk");

    set_modified(&path, SystemTime::now() + Duration::from_secs(60));

    let finished = finish(response).await;
    assert!(
        finished.is_err(),
        "completed instead of aborting: {finished:?}"
    );
}

#[tokio::test]
async fn downloads_carry_a_strong_etag_and_accept_ranges() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"hello");

    let response = h.get(&format!("items/{id}")).await;

    assert_eq!(response.status(), 200);
    let headers = response.headers();
    assert_eq!(headers["accept-ranges"], "bytes");
    let etag = headers["etag"].to_str().unwrap();
    assert!(
        etag.starts_with('"') && etag.ends_with('"') && etag.len() > 2,
        "strong ETag: {etag}"
    );
}

#[tokio::test]
async fn head_returns_the_same_headers_as_get_without_a_body() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"hello");
    let url = format!("{}items/{id}", h.link());

    let get = h.client.get(&url).send().await.unwrap();
    let head = h.client.head(&url).send().await.unwrap();

    assert_eq!(head.status(), 200);
    for name in [
        "content-type",
        "content-length",
        "content-disposition",
        "etag",
        "accept-ranges",
    ] {
        assert_eq!(head.headers().get(name), get.headers().get(name), "{name}");
    }
    assert_eq!(head.headers()["content-length"], "5");
    assert!(head.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn refreshing_a_changed_item_offers_the_new_version_under_a_new_etag() {
    let h = Harness::new();
    let path = h.file("notes.txt", b"hello");
    let id = h.engine.add_path(&path).unwrap().id();
    let old_etag = h.get(&format!("items/{id}")).await.headers()["etag"].clone();
    std::fs::write(&path, b"hello, edited").unwrap();
    assert_eq!(h.get(&format!("items/{id}")).await.status(), 409);

    h.engine.refresh_item(id).unwrap();

    let item = &h.engine.state().items[0];
    assert_eq!(item.status, ItemStatus::Available);
    assert_eq!(item.size, 13);
    let response = h.get(&format!("items/{id}")).await;
    assert_eq!(response.status(), 200);
    assert_ne!(response.headers()["etag"], old_etag);
    assert_eq!(&response.bytes().await.unwrap()[..], b"hello, edited");
}

#[tokio::test]
async fn refreshing_a_missing_item_fails_and_leaves_it_missing() {
    let h = Harness::new();
    let path = h.file("notes.txt", b"hello");
    let id = h.engine.add_path(&path).unwrap().id();
    std::fs::remove_file(&path).unwrap();

    assert!(h.engine.refresh_item(id).is_err());

    assert_eq!(h.engine.state().items[0].status, ItemStatus::Missing);
    assert_eq!(h.get(&format!("items/{id}")).await.status(), 409);
}

fn patterned(len: u32) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

#[tokio::test]
async fn a_download_resumed_with_range_is_byte_identical() {
    let h = Harness::new();
    let contents = patterned(3_000_000);
    let id = h.add_file("video.mp4", &contents);
    let url = format!("{}items/{id}", h.link());

    let first = h
        .client
        .get(&url)
        .header("range", "bytes=0-999")
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 206);
    assert_eq!(first.headers()["content-range"], "bytes 0-999/3000000");
    assert_eq!(first.headers()["content-length"], "1000");
    let etag = first.headers()["etag"].clone();
    let mut received = first.bytes().await.unwrap().to_vec();

    let rest = h
        .client
        .get(&url)
        .header("range", "bytes=1000-")
        .header("if-range", etag)
        .send()
        .await
        .unwrap();
    assert_eq!(rest.status(), 206);
    assert_eq!(
        rest.headers()["content-range"],
        "bytes 1000-2999999/3000000"
    );
    assert_eq!(rest.headers()["content-length"], "2999000");
    received.extend_from_slice(&rest.bytes().await.unwrap());

    assert!(received == contents, "resumed download differs");
}

#[tokio::test]
async fn range_requests_follow_http_semantics() {
    let h = Harness::new();
    let contents = patterned(1000);
    let id = h.add_file("notes.bin", &contents);
    let url = format!("{}items/{id}", h.link());
    let etag = h.get(&format!("items/{id}")).await.headers()["etag"]
        .to_str()
        .unwrap()
        .to_owned();

    let full = 0..1000;
    let partial = |range: &str, if_range: Option<&str>, content_range: &str, served| {
        (
            range.to_owned(),
            if_range.map(str::to_owned),
            206,
            Some(content_range.to_owned()),
            Some(served),
        )
    };
    let whole = |range: &str, if_range: Option<&str>| {
        (
            range.to_owned(),
            if_range.map(str::to_owned),
            200,
            None,
            Some(full.clone()),
        )
    };
    let cases = [
        partial("bytes=-100", None, "bytes 900-999/1000", 900..1000),
        partial("bytes=-5000", None, "bytes 0-999/1000", 0..1000),
        partial("bytes=990-5000", None, "bytes 990-999/1000", 990..1000),
        partial(
            "bytes=999-999",
            Some(&etag),
            "bytes 999-999/1000",
            999..1000,
        ),
        (
            "bytes=1000-".to_owned(),
            None,
            416,
            Some("bytes */1000".to_owned()),
            None,
        ),
        // Several ranges, nonsense, another unit, or an older version: serve it all.
        whole("bytes=0-9,20-29", None),
        whole("bytes=9-0", None),
        whole("items=0-9", None),
        whole("bytes=0-9", Some("\"some-older-version\"")),
    ];
    for (range, if_range, status, content_range, served) in cases {
        let mut request = h.client.get(&url).header("range", &range);
        if let Some(if_range) = &if_range {
            request = request.header("if-range", if_range);
        }
        let response = request.send().await.unwrap();
        let case = format!("Range: {range}, If-Range: {if_range:?}");
        assert_eq!(response.status(), status, "{case}");
        assert_eq!(
            response
                .headers()
                .get("content-range")
                .map(|v| v.to_str().unwrap().to_owned()),
            content_range,
            "{case}"
        );
        if let Some(served) = served {
            let body = response.bytes().await.unwrap();
            assert!(body[..] == contents[served], "{case}: wrong bytes");
        }
    }
}

#[tokio::test]
async fn the_receiver_hears_live_when_an_item_changes_and_is_refreshed() {
    let h = Harness::new();
    let path = h.file("notes.txt", b"hello");
    let id = h.engine.add_path(&path).unwrap().id();
    let mut events = h.events().await;
    assert_eq!(events.next().await["items"][0]["status"], "available");

    std::fs::write(&path, b"hello, edited").unwrap();
    h.engine.check_items();
    assert_eq!(events.next().await["items"][0]["status"], "changed");

    h.engine.refresh_item(id).unwrap();
    let item = &events.next().await["items"][0];
    assert_eq!(item["status"], "available");
    assert_eq!(item["size"], "13 B");

    std::fs::remove_file(&path).unwrap();
    h.engine.check_items();
    assert_eq!(events.next().await["items"][0]["status"], "missing");
}

#[tokio::test]
async fn the_page_shows_unavailable_items_without_a_download_link() {
    let h = Harness::new();
    let edited = h.file("edited.txt", b"hello");
    let deleted = h.file("deleted.txt", b"hello");
    let edited_id = h.engine.add_path(&edited).unwrap().id();
    let deleted_id = h.engine.add_path(&deleted).unwrap().id();
    let fine_id = h.add_file("fine.txt", b"hello");
    std::fs::write(&edited, b"hello, edited").unwrap();
    std::fs::remove_file(&deleted).unwrap();
    h.engine.check_items();

    let page = h.get("").await.text().await.unwrap();

    assert!(
        page.contains(&format!("href=\"items/{fine_id}\"")),
        "{page}"
    );
    for id in [edited_id, deleted_id] {
        assert!(!page.contains(&format!("href=\"items/{id}\"")), "{page}");
    }
    assert!(page.contains("Changed on the computer"), "{page}");
    assert!(page.contains("No longer on the computer"), "{page}");
}
