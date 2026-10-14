use std::convert::Infallible;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use futures_util::{Stream, StreamExt};
use oxdrop_core::{ItemId, android_safe_name};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use tokio_stream::wrappers::WatchStream;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use crate::engine::Shared;
use crate::page::{self, ReceiverView};

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/t/{secret}", get(add_trailing_slash))
        .route("/t/{secret}/", get(receiver_page))
        .route("/t/{secret}/events", get(events))
        .route("/t/{secret}/items/{item_id}", get(download_item))
        .with_state(shared)
}

/// Rejects anything not carrying the open Transfer's secret with a plain 404,
/// so a wrong secret is indistinguishable from a path that doesn't exist.
fn check_secret(shared: &Shared, secret: &str) -> Result<(), StatusCode> {
    if shared.live().transfer.secret().matches(secret) {
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
    let items = shared.live().transfer.items().to_vec();
    Ok(Html(page::render(&items)))
}

/// Server-sent `transfer` events carrying the Receiver's view of the Transfer: the
/// current view first, then a new one whenever it changes. An open stream is what
/// "Receiver connected" means.
async fn events(
    State(shared): State<Arc<Shared>>,
    Path(secret): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    check_secret(&shared, &secret)?;
    let connected = shared.connect_receiver();
    let mut last_sent = None;
    let stream = WatchStream::new(shared.subscribe())
        .filter_map(move |state| {
            let _connected = &connected;
            let json = serde_json::to_string(&ReceiverView::new(&state.items))
                .expect("Receiver view serializes");
            let event = (last_sent.as_ref() != Some(&json)).then(|| {
                last_sent = Some(json.clone());
                Ok(Event::default().event("transfer").data(json))
            });
            std::future::ready(event)
        })
        .take_until(shared.shutdown.clone().cancelled_owned());
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn download_item(
    State(shared): State<Arc<Shared>>,
    Path((secret, item_id)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
    check_secret(&shared, &secret)?;
    let id: ItemId = item_id.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    let (item, cancelled) = shared
        .live()
        .start_download(id, &shared.shutdown)
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
        Body::from_stream(abortable(ReaderStream::new(file), cancelled)),
    )
        .into_response())
}

/// Passes `chunks` through until `cancelled` fires, then fails the stream so the
/// connection is cut and the Receiver's browser marks the download as failed,
/// rather than ending early as if it were complete.
fn abortable(
    chunks: ReaderStream<tokio::fs::File>,
    cancelled: CancellationToken,
) -> impl Stream<Item = std::io::Result<Bytes>> {
    futures_util::stream::unfold(Some((chunks, cancelled)), |state| async move {
        let (mut chunks, cancelled) = state?;
        tokio::select! {
            biased;
            () = cancelled.cancelled() => {
                Some((Err(std::io::Error::other("Item is no longer offered")), None))
            }
            chunk = chunks.next() => chunk.map(|chunk| (chunk, Some((chunks, cancelled)))),
        }
    })
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

/// An attachment `Content-Disposition` (RFC 6266) naming the file the Receiver saves:
/// the Item Name made Android-safe, as an ASCII `filename` for old clients and an
/// exact UTF-8 `filename*` for everyone else.
fn content_disposition(item_name: &str) -> String {
    let name = android_safe_name(item_name);
    let fallback: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' => '_',
            ' ' => ' ',
            c if c.is_ascii_graphic() => c,
            _ => '_',
        })
        .collect();
    let encoded = utf8_percent_encode(&name, ATTR_CHAR_ESCAPES);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}
