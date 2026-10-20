//! "Download all": the whole Transfer in one zip, each Item at its root under its Item Name.

mod support;

use support::Harness;

/// Opens a zip with an independent reader (the `zip` crate), never our own writer.
fn open_zip(bytes: Vec<u8>) -> zip::ZipArchive<std::io::Cursor<Vec<u8>>> {
    zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a valid zip")
}

fn read_entry(archive: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>, name: &str) -> Vec<u8> {
    let mut contents = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_name(name).expect(name), &mut contents)
        .expect("entry reads and its CRC matches");
    contents
}

fn names(archive: &zip::ZipArchive<std::io::Cursor<Vec<u8>>>) -> Vec<String> {
    let mut names: Vec<String> = archive
        .file_names()
        .map(|name| name.unwrap().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn download_all_zips_every_item_at_the_root_under_its_item_name() {
    let h = Harness::new();
    h.add_file("notes.txt", b"my notes");
    h.file_in("Photos", "beach.jpg", &[1; 3000]);
    h.file_in("Photos/2024", "cake.jpg", &[2; 200]);
    h.engine.add_path(&h.path("Photos")).unwrap();
    // Collides with notes.txt, so its Item Name is numbered.
    h.file_in("elsewhere", "notes.txt", b"other notes");
    h.engine.add_path(&h.path("elsewhere/notes.txt")).unwrap();

    let response = h.get("all").await;

    assert_eq!(response.status(), 200);
    let headers = response.headers().clone();
    assert_eq!(headers["content-type"], "application/zip");
    assert_eq!(headers["accept-ranges"], "bytes");
    assert!(headers.contains_key("etag"));
    let disposition = headers["content-disposition"].to_str().unwrap();
    assert!(
        disposition.starts_with("attachment;") && disposition.contains("filename=\"Oxdrop.zip\""),
        "{disposition}"
    );
    let body = response.bytes().await.unwrap().to_vec();
    assert_eq!(headers["content-length"], body.len().to_string().as_str());

    let mut archive = open_zip(body);
    assert_eq!(
        names(&archive),
        [
            "Photos/",
            "Photos/2024/",
            "Photos/2024/cake.jpg",
            "Photos/beach.jpg",
            "notes (2).txt",
            "notes.txt",
        ]
    );
    assert_eq!(read_entry(&mut archive, "notes.txt"), b"my notes");
    assert_eq!(read_entry(&mut archive, "notes (2).txt"), b"other notes");
    assert_eq!(read_entry(&mut archive, "Photos/beach.jpg"), [1; 3000]);
    assert_eq!(read_entry(&mut archive, "Photos/2024/cake.jpg"), [2; 200]);
}

#[tokio::test]
async fn head_of_download_all_gives_the_same_headers_and_no_body() {
    let h = Harness::new();
    h.add_file("notes.txt", b"my notes");
    h.file_in("Photos", "beach.jpg", &[1; 3000]);
    h.engine.add_path(&h.path("Photos")).unwrap();
    let get = h.get("all").await;
    let (get_length, get_etag) = (
        get.headers()["content-length"].clone(),
        get.headers()["etag"].clone(),
    );

    let head = h
        .client
        .head(format!("{}all", h.link()))
        .send()
        .await
        .unwrap();

    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-length"], get_length);
    assert_eq!(head.headers()["etag"], get_etag);
    assert_eq!(head.headers()["content-type"], "application/zip");
    assert!(head.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn download_all_of_an_empty_transfer_is_not_found() {
    let h = Harness::new();

    assert_eq!(h.get("all").await.status(), 404);
}

#[tokio::test]
async fn download_all_is_refused_naming_the_items_that_changed_or_went_missing() {
    let h = Harness::new();
    h.add_file("fine.txt", b"fine");
    let edited = h.file("edited.txt", b"before");
    h.engine.add_path(&edited).unwrap();
    let deleted = h.file("deleted.txt", b"gone soon");
    h.engine.add_path(&deleted).unwrap();
    let inside = h.file_in("Docs", "plan.txt", b"plan");
    h.engine.add_path(&h.path("Docs")).unwrap();

    std::fs::write(&edited, b"after!!").unwrap();
    std::fs::remove_file(&deleted).unwrap();
    std::fs::write(&inside, b"new plan").unwrap();
    let response = h.get("all").await;

    assert_eq!(response.status(), 409);
    let body = response.text().await.unwrap();
    for name in ["edited.txt", "deleted.txt", "Docs"] {
        assert!(body.contains(name), "{name} not named in: {body}");
    }
    assert!(!body.contains("fine.txt"), "{body}");
}

fn patterned(len: u32, seed: u32) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + seed) % 251) as u8).collect()
}

async fn get_range(h: &Harness, range: &str, if_range: Option<&str>) -> reqwest::Response {
    let mut request = h
        .client
        .get(format!("{}all", h.link()))
        .header("range", range);
    if let Some(etag) = if_range {
        request = request.header("if-range", etag);
    }
    request.send().await.unwrap()
}

/// Adds the same Items, in the same order, to a Sender.
fn add_items(h: &Harness, files: &Harness) {
    for path in ["first.bin", "Album", "last.bin"] {
        h.engine.add_path(&files.path(path)).unwrap();
    }
}

#[tokio::test]
async fn download_all_resumed_mid_file_without_cached_crcs_is_byte_identical() {
    let files = Harness::new();
    files.file("first.bin", &patterned(400_000, 1));
    files.file_in("Album", "a.jpg", &patterned(300_000, 2));
    files.file_in("Album/sub", "b.mov", &patterned(1_000_000, 3));
    files.file("last.bin", &patterned(5_000, 4));
    add_items(&files, &files);
    let total: u64 = files.get("all").await.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    // A fresh Sender for the same Items: nothing has been read, so no CRC is cached.
    let fresh = Harness::new();
    add_items(&fresh, &files);
    // Somewhere inside b.mov, past first.bin and a.jpg whose CRCs the tail needs.
    let split = total * 2 / 3;

    let second = get_range(&fresh, &format!("bytes={split}-"), None).await;
    assert_eq!(second.status(), 206);
    assert_eq!(
        second.headers()["content-range"],
        format!("bytes {split}-{}/{total}", total - 1).as_str()
    );
    let second = second.bytes().await.unwrap();
    let first = get_range(&fresh, &format!("bytes=0-{}", split - 1), None).await;
    assert_eq!(first.status(), 206);
    let mut resumed = first.bytes().await.unwrap().to_vec();
    resumed.extend_from_slice(&second);

    let whole = fresh.get("all").await.bytes().await.unwrap();
    assert_eq!(resumed.len() as u64, total);
    assert!(resumed == whole, "resumed zip differs from a full download");
    let mut archive = open_zip(resumed);
    assert_eq!(read_entry(&mut archive, "first.bin"), patterned(400_000, 1));
    assert_eq!(
        read_entry(&mut archive, "Album/a.jpg"),
        patterned(300_000, 2)
    );
    assert_eq!(
        read_entry(&mut archive, "Album/sub/b.mov"),
        patterned(1_000_000, 3)
    );
    assert_eq!(read_entry(&mut archive, "last.bin"), patterned(5_000, 4));
}

#[tokio::test]
async fn resuming_download_all_after_the_items_change_starts_over() {
    let h = Harness::new();
    h.add_file("notes.txt", b"my notes");
    let old_etag = h.get("all").await.headers()["etag"]
        .to_str()
        .unwrap()
        .to_owned();

    h.add_file("late.txt", b"added later");
    let response = get_range(&h, "bytes=10-", Some(&old_etag)).await;

    assert_eq!(response.status(), 200, "a different zip, so all of it");
    assert_ne!(response.headers()["etag"], old_etag.as_str());
    let mut archive = open_zip(response.bytes().await.unwrap().to_vec());
    assert_eq!(read_entry(&mut archive, "late.txt"), b"added later");
}

#[tokio::test]
async fn removing_any_item_aborts_download_all_in_progress_and_later_ones_leave_it_out() {
    let h = Harness::new();
    let small = h.add_file("small.txt", b"already sent");
    let big = h.file("big.bin", b"");
    // Sparse and far larger than any socket buffer, so the download can't finish early.
    std::fs::File::options()
        .write(true)
        .open(&big)
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    h.engine.add_path(&big).unwrap();
    let mut response = h.get("all").await;
    assert_eq!(response.status(), 200);
    let old_etag = response.headers()["etag"].clone();
    response.chunk().await.unwrap().expect("first chunk");

    // Not the Item being streamed now: one already sent.
    h.engine.remove_item(small);

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
    assert!(finished.is_err(), "completed instead of aborting");

    let head = h
        .client
        .head(format!("{}all", h.link()))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_ne!(head.headers()["etag"], old_etag);
    let length: u64 = head.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    // The tail of the zip is its central directory, which names every entry.
    let tail = get_range(&h, &format!("bytes={}-", length - 200), None)
        .await
        .bytes()
        .await
        .unwrap();
    let tail = String::from_utf8_lossy(&tail);
    assert!(tail.contains("big.bin"), "{tail}");
    assert!(!tail.contains("small.txt"), "{tail}");
}

#[tokio::test]
async fn the_receiver_page_offers_download_all_once_there_are_items() {
    let h = Harness::new();
    let empty = h.get("").await.text().await.unwrap();
    assert!(
        empty.contains(
            r#"<a id="all" class="all" href="all" download="Oxdrop.zip" hidden=true>Download all</a>"#
        ),
        "{empty}"
    );

    h.add_file("notes.txt", b"my notes");
    let page = h.get("").await.text().await.unwrap();

    assert!(
        page.contains(
            r#"<a id="all" class="all" href="all" download="Oxdrop.zip">Download all</a>"#
        ),
        "{page}"
    );
}
