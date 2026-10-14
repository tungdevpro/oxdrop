mod support;

use percent_encoding::percent_decode_str;
use support::Harness;

#[tokio::test]
async fn page_lists_the_item_with_its_name_and_size() {
    let h = Harness::new();
    h.add_file("holiday.jpg", &[7u8; 2_500]);

    let response = h.get("").await;

    assert_eq!(response.status(), 200);
    let content_type = response.headers()["content-type"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(content_type.starts_with("text/html"), "{content_type}");
    let page = response.text().await.unwrap();
    assert!(page.contains("holiday.jpg"), "{page}");
    assert!(page.contains("2.5 KB"), "{page}");
}

#[tokio::test]
async fn page_links_to_the_item_download() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"hello");

    let page = h.get("").await.text().await.unwrap();

    assert!(page.contains(&format!("href=\"items/{id}\"")), "{page}");
}

#[tokio::test]
async fn transfer_link_without_trailing_slash_still_opens_the_page() {
    let h = Harness::new();
    h.add_file("notes.txt", b"hello");
    let link = h.link();

    let response = h
        .client
        .get(link.trim_end_matches('/'))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("notes.txt"));
}

#[tokio::test]
async fn download_is_byte_identical_with_exact_length() {
    let h = Harness::new();
    let contents: Vec<u8> = (0..3_000_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let id = h.add_file("video.mp4", &contents);

    let response = h.get(&format!("items/{id}")).await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-length"],
        contents.len().to_string().as_str()
    );
    let body = response.bytes().await.unwrap();
    assert_eq!(body.len(), contents.len());
    assert!(
        body.as_ref() == contents.as_slice(),
        "downloaded bytes differ"
    );
}

#[tokio::test]
async fn download_is_an_attachment_named_after_the_item() {
    let h = Harness::new();
    let id = h.add_file("report.pdf", b"%PDF-1.7");

    let response = h.get(&format!("items/{id}")).await;

    let disposition = response.headers()["content-disposition"].to_str().unwrap();
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(
        disposition.contains("filename=\"report.pdf\""),
        "{disposition}"
    );
}

#[tokio::test]
async fn non_ascii_item_name_round_trips_in_content_disposition() {
    let h = Harness::new();
    let name = "Ảnh kỷ niệm 📷.png";
    let id = h.add_file(name, b"png");

    let response = h.get(&format!("items/{id}")).await;

    let disposition = response.headers()["content-disposition"].to_str().unwrap();
    let encoded = disposition
        .split("filename*=UTF-8''")
        .nth(1)
        .unwrap_or_else(|| panic!("no filename* in {disposition}"));
    let decoded = percent_decode_str(encoded).decode_utf8().unwrap();
    assert_eq!(decoded, name);
}

#[tokio::test]
async fn wrong_secret_is_not_found() {
    let h = Harness::new();
    let id = h.add_file("notes.txt", b"hello");
    let link = h.link();
    let origin = link.split("/t/").next().unwrap();

    for path in [
        "/t/not-the-secret/",
        &format!("/t/not-the-secret/items/{id}"),
    ] {
        let response = h
            .client
            .get(format!("{origin}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{path}");
    }
}

#[tokio::test]
async fn unknown_item_is_not_found() {
    let h = Harness::new();
    h.add_file("notes.txt", b"hello");

    for path in ["items/999", "items/not-an-id", "items/../../etc/passwd"] {
        assert_eq!(h.get(path).await.status(), 404, "{path}");
    }
}

#[tokio::test]
async fn each_transfer_link_has_a_fresh_unguessable_secret() {
    let a = Harness::new();
    let b = Harness::new();
    let secret = |link: &str| {
        link.split("/t/")
            .nth(1)
            .unwrap()
            .trim_end_matches('/')
            .to_owned()
    };

    let (sa, sb) = (secret(&a.link()), secret(&b.link()));

    assert_ne!(sa, sb);
    // 128 bits, base64url without padding.
    assert_eq!(sa.len(), 22, "{sa}");
    assert!(
        sa.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{sa}"
    );
}
