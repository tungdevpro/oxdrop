//! Folder Items reach the Receiver as a single zip that keeps their structure.

mod support;

use oxdrop_core::{ItemKind, ItemStatus};
use support::Harness;

#[tokio::test]
async fn adding_a_folder_creates_one_item_named_after_it() {
    let h = Harness::new();
    h.file_in("Photos", "beach.jpg", &[1; 300]);
    h.file_in("Photos/2024", "cake.jpg", &[2; 200]);
    h.file_in("Photos", ".DS_Store", b"junk");
    let folder = h.path("Photos");

    h.engine.add_path(&folder).unwrap();

    let state = h.engine.state();
    let item = &state.items[0];
    assert_eq!(item.name, "Photos");
    assert_eq!(item.size, 500, "total of the files sent, junk excluded");
    assert!(matches!(item.kind, ItemKind::Folder(_)), "{:?}", item.kind);
}

#[tokio::test]
async fn colliding_folder_names_are_numbered_at_the_end() {
    let h = Harness::new();
    let first = h.path("Desktop/my.photos");
    let second = h.path("Pictures/my.photos");
    let file = h.file_in("Downloads", "my.photos", b"not a folder");
    for path in [&first, &second] {
        std::fs::create_dir_all(path).unwrap();
    }

    for outcome in h.engine.add_paths(&[first, second, file]) {
        outcome.unwrap();
    }

    assert_eq!(
        h.item_names(),
        ["my.photos", "my.photos (2)", "my (2).photos"]
    );
}

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

#[tokio::test]
async fn a_folder_downloads_as_a_zip_that_keeps_its_structure() {
    let h = Harness::new();
    h.file_in("Photos", "beach.jpg", &[1; 3000]);
    h.file_in("Photos/2024/Tết", "bánh chưng.jpg", "ảnh".as_bytes());
    h.file_in("Photos", ".env", b"hidden but included");
    h.file_in("Photos", ".DS_Store", b"junk");
    h.file_in("Photos", "Thumbs.db", b"junk");
    std::fs::create_dir_all(h.path("Photos/empty")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(h.path("Photos/beach.jpg"), h.path("Photos/link.jpg")).unwrap();
    let id = h.engine.add_path(&h.path("Photos")).unwrap().id();

    let response = h.get(&format!("items/{id}")).await;

    assert_eq!(response.status(), 200);
    let headers = response.headers().clone();
    assert_eq!(headers["content-type"], "application/zip");
    let disposition = headers["content-disposition"].to_str().unwrap();
    assert!(
        disposition.contains("filename=\"Photos.zip\""),
        "{disposition}"
    );
    let body = response.bytes().await.unwrap().to_vec();
    assert_eq!(headers["content-length"], body.len().to_string().as_str());

    let mut archive = open_zip(body);
    let mut names: Vec<String> = archive
        .file_names()
        .map(|name| name.unwrap().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "Photos/",
            "Photos/.env",
            "Photos/2024/",
            "Photos/2024/Tết/",
            "Photos/2024/Tết/bánh chưng.jpg",
            "Photos/beach.jpg",
            "Photos/empty/",
        ]
    );
    assert_eq!(read_entry(&mut archive, "Photos/beach.jpg"), [1; 3000]);
    assert_eq!(
        read_entry(&mut archive, "Photos/2024/Tết/bánh chưng.jpg"),
        "ảnh".as_bytes()
    );
    assert_eq!(
        read_entry(&mut archive, "Photos/.env"),
        b"hidden but included"
    );
    assert!(archive.by_name("Photos/empty/").unwrap().is_dir());
}

#[tokio::test]
async fn zip_entries_keep_the_snapshot_modification_times() {
    let h = Harness::new();
    let path = h.file_in("Docs", "plan.txt", b"plan");
    // 2021-06-15 12:34:56 UTC.
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_623_760_496);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(when)
        .unwrap();
    let id = h.engine.add_path(&h.path("Docs")).unwrap().id();

    let body = h.get(&format!("items/{id}")).await.bytes().await.unwrap();

    let mut archive = open_zip(body.to_vec());
    let entry = archive.by_name("Docs/plan.txt").unwrap();
    let modified = entry.last_modified().expect("a modification time");
    assert_eq!(
        (
            modified.year(),
            modified.month(),
            modified.day(),
            modified.hour(),
            modified.minute(),
            modified.second()
        ),
        (2021, 6, 15, 12, 34, 56)
    );
}

fn patterned(len: u32, seed: u32) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + seed) % 251) as u8).collect()
}

async fn get_range(h: &Harness, path: &str, range: &str) -> reqwest::Response {
    h.client
        .get(format!("{}{path}", h.link()))
        .header("range", range)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_zip_resumed_mid_file_without_cached_crcs_is_byte_identical() {
    let h = Harness::new();
    h.file_in("Album", "a.jpg", &patterned(300_000, 1));
    h.file_in("Album", "b.mov", &patterned(1_000_000, 2));
    h.file_in("Album/sub", "c.txt", &patterned(2_000, 3));
    let id = h.engine.add_path(&h.path("Album")).unwrap().id();
    let item = format!("items/{id}");
    let total: u64 = h.get(&item).await.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    // A fresh Sender for the same folder: nothing has been read, so no CRC is cached.
    let fresh = Harness::new();
    let id = fresh.engine.add_path(&h.path("Album")).unwrap().id();
    let item = format!("items/{id}");
    // Somewhere inside b.mov's bytes.
    let split = total / 2;

    let second = get_range(&fresh, &item, &format!("bytes={split}-")).await;
    assert_eq!(second.status(), 206);
    assert_eq!(
        second.headers()["content-range"],
        format!("bytes {split}-{}/{total}", total - 1).as_str()
    );
    let second = second.bytes().await.unwrap();
    let first = get_range(&fresh, &item, &format!("bytes=0-{}", split - 1)).await;
    assert_eq!(first.status(), 206);
    let mut resumed = first.bytes().await.unwrap().to_vec();
    resumed.extend_from_slice(&second);

    let whole = fresh.get(&item).await.bytes().await.unwrap();
    assert_eq!(resumed.len() as u64, total);
    assert!(resumed == whole, "resumed zip differs from a full download");
    let mut archive = open_zip(resumed);
    assert_eq!(
        read_entry(&mut archive, "Album/b.mov"),
        patterned(1_000_000, 2)
    );
    assert_eq!(
        read_entry(&mut archive, "Album/a.jpg"),
        patterned(300_000, 1)
    );
    assert_eq!(
        read_entry(&mut archive, "Album/sub/c.txt"),
        patterned(2_000, 3)
    );
}

#[tokio::test]
async fn a_folder_with_an_edited_or_deleted_file_is_changed_and_refused() {
    for (what, edit) in [
        (
            "edited",
            (|path: &std::path::Path| std::fs::write(path, b"new!!").unwrap()) as fn(&_),
        ),
        ("deleted", |path| std::fs::remove_file(path).unwrap()),
    ] {
        let h = Harness::new();
        let inside = h.file_in("Docs/deep", "plan.txt", b"plan");
        h.file_in("Docs", "other.txt", b"other");
        let id = h.engine.add_path(&h.path("Docs")).unwrap().id();

        edit(&inside);

        assert_eq!(h.get(&format!("items/{id}")).await.status(), 409, "{what}");
        assert_eq!(
            h.engine.state().items[0].status,
            ItemStatus::Changed,
            "{what}"
        );
    }
}

#[tokio::test]
async fn a_deleted_folder_is_missing() {
    let h = Harness::new();
    h.file_in("Docs", "plan.txt", b"plan");
    let id = h.engine.add_path(&h.path("Docs")).unwrap().id();

    std::fs::remove_dir_all(h.path("Docs")).unwrap();

    assert_eq!(h.get(&format!("items/{id}")).await.status(), 409);
    assert_eq!(h.engine.state().items[0].status, ItemStatus::Missing);
}

#[tokio::test]
async fn new_files_join_a_folder_only_when_it_is_refreshed() {
    let h = Harness::new();
    h.file_in("Docs", "plan.txt", b"plan");
    let id = h.engine.add_path(&h.path("Docs")).unwrap().id();
    let item = format!("items/{id}");
    let before = h.get(&item).await;
    let old_etag = before.headers()["etag"].clone();
    h.file_in("Docs", "late.txt", b"late");

    h.engine.check_items();
    let unchanged = h.get(&item).await;
    assert_eq!(unchanged.status(), 200);
    assert_eq!(unchanged.headers()["etag"], old_etag);
    assert!(
        open_zip(unchanged.bytes().await.unwrap().to_vec())
            .by_name("Docs/late.txt")
            .is_err()
    );

    h.engine.refresh_item(id).unwrap();

    let refreshed = h.get(&item).await;
    assert_ne!(refreshed.headers()["etag"], old_etag);
    let mut archive = open_zip(refreshed.bytes().await.unwrap().to_vec());
    assert_eq!(read_entry(&mut archive, "Docs/late.txt"), b"late");
}

#[tokio::test]
async fn a_folder_file_changing_mid_download_aborts_it() {
    let h = Harness::new();
    let big = h.file_in("Videos", "long.mp4", b"");
    std::fs::File::options()
        .write(true)
        .open(&big)
        .unwrap()
        .set_len(256 * 1024 * 1024)
        .unwrap();
    h.file_in("Videos", "short.mp4", b"short");
    let id = h.engine.add_path(&h.path("Videos")).unwrap().id();
    let mut response = h.get(&format!("items/{id}")).await;
    assert_eq!(response.status(), 200);
    response.chunk().await.unwrap().expect("first chunk");

    std::fs::File::options()
        .write(true)
        .open(&big)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        .unwrap();

    let finished = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match response.chunk().await {
                Ok(Some(_)) => {}
                Ok(None) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .expect("the download ends promptly");
    assert!(finished.is_err(), "completed instead of aborting");
}

/// A zip served by the Sender, read on demand with `Range` requests in large blocks,
/// so an independent reader can open a multi-gigabyte zip without holding it in memory.
/// Must be used off the async runtime (in `spawn_blocking`).
struct RemoteZip {
    client: reqwest::Client,
    url: String,
    len: u64,
    position: u64,
    block_start: u64,
    block: Vec<u8>,
}

impl RemoteZip {
    const BLOCK: u64 = 8 * 1024 * 1024;
}

impl std::io::Read for RemoteZip {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.len {
            return Ok(0);
        }
        let block_end = self.block_start + self.block.len() as u64;
        if !(self.block_start..block_end).contains(&self.position) {
            let end = (self.position + Self::BLOCK).min(self.len) - 1;
            let request = self
                .client
                .get(&self.url)
                .header("range", format!("bytes={}-{end}", self.position));
            let response = tokio::runtime::Handle::current()
                .block_on(async { request.send().await?.error_for_status()?.bytes().await })
                .map_err(std::io::Error::other)?;
            assert_eq!(
                response.len() as u64,
                end - self.position + 1,
                "short range"
            );
            self.block_start = self.position;
            self.block = response.to_vec();
        }
        let offset = (self.position - self.block_start) as usize;
        let n = out.len().min(self.block.len() - offset);
        out[..n].copy_from_slice(&self.block[offset..offset + n]);
        self.position += n as u64;
        Ok(n)
    }
}

impl std::io::Seek for RemoteZip {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.position = match to {
            std::io::SeekFrom::Start(at) => at,
            std::io::SeekFrom::End(back) => self.len.checked_add_signed(back).unwrap(),
            std::io::SeekFrom::Current(by) => self.position.checked_add_signed(by).unwrap(),
        };
        Ok(self.position)
    }
}

/// The CRC-32 of `len` zero bytes, without hashing them all.
fn crc_of_zeros(len: u64) -> u32 {
    const MIB: u64 = 1024 * 1024;
    let mut mib = crc32fast::Hasher::new();
    mib.update(&vec![0; MIB as usize]);
    let mut total = crc32fast::Hasher::new();
    for _ in 0..len / MIB {
        total.combine(&mib);
    }
    total.update(&vec![0; (len % MIB) as usize]);
    total.finalize()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_with_a_file_over_4_gb_unzips_correctly() {
    let h = Harness::new();
    let big_len = u64::from(u32::MAX) + 17;
    let big = h.file_in("Disk", "a.img", b"");
    // Sparse: takes no real space, reads back as zeros.
    std::fs::File::options()
        .write(true)
        .open(&big)
        .unwrap()
        .set_len(big_len)
        .unwrap();
    // Sorted after a.img, so its local header sits past the 4 GB mark.
    h.file_in("Disk", "z.txt", b"after the big one");
    let id = h.engine.add_path(&h.path("Disk")).unwrap().id();
    let url = format!("{}items/{id}", h.link());
    let head = h.client.head(&url).send().await.unwrap();
    let len: u64 = head.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(len > big_len);
    let remote = RemoteZip {
        client: h.client.clone(),
        url,
        len,
        position: 0,
        block_start: 0,
        block: Vec::new(),
    };

    let started = std::time::Instant::now();
    tokio::task::spawn_blocking(move || {
        let mut archive = zip::ZipArchive::new(remote).expect("a valid Zip64 archive");
        {
            let big = archive.by_name("Disk/a.img").unwrap();
            assert_eq!(big.size(), big_len);
            assert_eq!(big.compressed_size(), big_len);
            assert_eq!(big.crc32(), crc_of_zeros(big_len));
        }
        let mut small = Vec::new();
        std::io::Read::read_to_end(&mut archive.by_name("Disk/z.txt").unwrap(), &mut small)
            .expect("z.txt reads and its CRC matches");
        assert_eq!(small, b"after the big one");
    })
    .await
    .unwrap();
    eprintln!("Zip64 check took {:?}", started.elapsed());
}

#[tokio::test]
async fn the_receiver_sees_a_folder_as_a_zip_to_download() {
    let h = Harness::new();
    h.file_in("Photos", "beach.jpg", &[1; 2500]);
    let id = h.engine.add_path(&h.path("Photos")).unwrap().id();

    let page = h.get("").await.text().await.unwrap();
    assert!(
        page.contains(&format!("href=\"items/{id}\" download=\"Photos.zip\"")),
        "{page}"
    );
    assert!(page.contains("Folder"), "{page}");

    let mut events = h.events().await;
    let item = &events.next().await["items"][0];
    assert_eq!(item["kind"], "folder");
    assert_eq!(item["download"], "Photos.zip");
    assert_eq!(item["size"], "2.5 KB");
}
