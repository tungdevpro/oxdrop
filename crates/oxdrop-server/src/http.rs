use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use oxdrop_core::ItemId;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use tokio_util::io::ReaderStream;

use crate::engine::Shared;
use crate::page;

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/t/{secret}", get(add_trailing_slash))
        .route("/t/{secret}/", get(receiver_page))
        .route("/t/{secret}/items/{item_id}", get(download_item))
        .with_state(shared)
}

/// Rejects anything not carrying the open Transfer's secret with a plain 404,
/// so a wrong secret is indistinguishable from a path that doesn't exist.
fn check_secret(shared: &Shared, secret: &str) -> Result<(), StatusCode> {
    if shared.transfer().secret().matches(secret) {
        Ok(())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn add_trailing_slash(
    State(shared): State<Arc<Shared>>,
    Path(secret): Path<String>,
) -> Result<Redirect, StatusCode> {
    check_secret(&shared, &secret)?;
    Ok(Redirect::permanent(&format!("/t/{secret}/")))
}

async fn receiver_page(
    State(shared): State<Arc<Shared>>,
    Path(secret): Path<String>,
) -> Result<Html<String>, StatusCode> {
    check_secret(&shared, &secret)?;
    let items = shared.transfer().items().to_vec();
    Ok(Html(page::render(&items)))
}

async fn download_item(
    State(shared): State<Arc<Shared>>,
    Path((secret, item_id)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
    check_secret(&shared, &secret)?;
    let id: ItemId = item_id.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    let item = shared
        .transfer()
        .item(id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    let file = tokio::fs::File::open(&item.path).await.map_err(|error| {
        tracing::warn!(%error, item = %item.id, "can't open Item");
        StatusCode::NOT_FOUND
    })?;
    // Content-Length must describe exactly the bytes we stream, so take it from the open handle.
    let length = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len();

    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (header::CONTENT_LENGTH, length.to_string()),
            (header::CONTENT_DISPOSITION, content_disposition(&item.name)),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

/// RFC 5987 `attr-char`: everything else is percent-encoded in `filename*`.
const ATTR_CHAR_ESCAPES: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

/// An attachment `Content-Disposition` (RFC 6266) carrying the Item Name: an ASCII
/// `filename` for old clients and an exact UTF-8 `filename*` for everyone else.
fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' => '_',
            ' ' => ' ',
            c if c.is_ascii_graphic() => c,
            _ => '_',
        })
        .collect();
    let encoded = utf8_percent_encode(name, ATTR_CHAR_ESCAPES);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}
