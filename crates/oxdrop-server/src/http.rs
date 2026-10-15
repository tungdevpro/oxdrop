use std::convert::Infallible;
use std::io::SeekFrom;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{FromRef, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use futures_util::{Stream, StreamExt};
use oxdrop_core::{Item, ItemId, ItemKind, ItemStatus, ZipPlan, android_safe_name};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_stream::wrappers::WatchStream;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use crate::engine::{Refusal, Shared};
use crate::page::{self, ReceiverView};
use crate::range::Requested;
use crate::zip_stream::zip_range;

/// What one bound listener serves: the shared Transfer, plus the address it answers
/// to and the switch that stops it.
#[derive(Clone)]
pub(crate) struct Listening {
    pub(crate) shared: Arc<Shared>,
    pub(crate) address: SocketAddr,
    pub(crate) stop: CancellationToken,
}

impl FromRef<Listening> for Arc<Shared> {
    fn from_ref(listening: &Listening) -> Self {
        listening.shared.clone()
    }
}

pub(crate) fn router(listening: Listening) -> Router {
    Router::new()
        .route("/t/{secret}", get(add_trailing_slash))
        .route("/t/{secret}/", get(receiver_page))
        .route("/t/{secret}/events", get(events))
        .route("/t/{secret}/items/{item_id}", get(download_item))
        .layer(middleware::from_fn_with_state(
            listening.clone(),
            require_bound_host,
        ))
        .with_state(listening)
}

/// Serves only requests addressed to the bound IP literal and port. A page on
/// some other site that rebinds its own host name to our address can make the
/// phone's (or this computer's) browser connect to us, but its requests still
/// carry that host name, so they're turned away here.
async fn require_bound_host(
    State(listening): State<Listening>,
    request: Request,
    next: Next,
) -> Response {
    let expected = listening.address.to_string();
    let host = request.headers().get(header::HOST);
    if host.is_some_and(|host| host.as_bytes() == expected.as_bytes()) {
        next.run(request).await
    } else {
        StatusCode::MISDIRECTED_REQUEST.into_response()
    }
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
    State(Listening { shared, stop, .. }): State<Listening>,
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
        .take_until(stop.cancelled_owned());
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn download_item(
    State(Listening { shared, stop, .. }): State<Listening>,
    Path((secret, item_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    check_secret(&shared, &secret)?;
    let id: ItemId = item_id.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    let (item, cancelled) = match shared.start_download(id) {
        Ok(download) => download,
        Err(Refusal::NoSuchItem) => return Err(StatusCode::NOT_FOUND),
        Err(Refusal::Unavailable(status)) => return Ok(unavailable(status)),
    };

    // A file is served as itself; a folder as a zip laid out from its snapshot.
    let zip = match &item.kind {
        ItemKind::File => None,
        ItemKind::Folder(snapshot) => Some(Arc::new(ZipPlan::for_folder(
            &item.name,
            &item.path,
            item.modified,
            snapshot,
        ))),
    };
    let (total, content_type) = match &zip {
        None => (item.size, "application/octet-stream"),
        Some(plan) => (plan.len(), "application/zip"),
    };

    let etag = item.etag();
    let (status, start, length) = match Requested::from_headers(&headers, total, &etag) {
        Requested::Full => (StatusCode::OK, 0, total),
        Requested::Partial { start, end } => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
        Requested::Unsatisfiable => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{total}"))],
            )
                .into_response());
        }
    };

    let bytes: ByteStream = match zip {
        None => {
            let file = match tokio::fs::File::open(&item.path).await {
                Ok(file) => file,
                Err(error) => {
                    tracing::warn!(%error, item = %item.id, "can't open Item");
                    return Ok(unavailable(ItemStatus::Missing));
                }
            };
            let bytes = snapshot_bytes(file, &item, start, length)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Box::pin(bytes)
        }
        Some(plan) => match zip_range(plan, shared.crcs.clone(), start, start + length).await {
            Ok(bytes) => Box::pin(bytes),
            Err(error) => {
                tracing::warn!(%error, item = %item.id, "folder changed while preparing zip");
                return Ok(unavailable(ItemStatus::Changed));
            }
        },
    };

    let mut response = (
        status,
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (header::CONTENT_LENGTH, length.to_string()),
            (
                header::CONTENT_DISPOSITION,
                content_disposition(&item.download_name()),
            ),
            (header::ETAG, etag),
            (header::ACCEPT_RANGES, "bytes".to_owned()),
        ],
        Body::from_stream(abortable(bytes, cancelled, stop)),
    )
        .into_response();
    if status == StatusCode::PARTIAL_CONTENT {
        let range = format!("bytes {start}-{}/{total}", start + length - 1);
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&range).expect("ASCII header value"),
        );
    }
    Ok(response)
}

/// The body of a download, whichever kind of Item it is.
type ByteStream = std::pin::Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>;

/// Streams `length` bytes of the Item's `file` from `start`. Before the final chunk goes
/// out, the file is checked against the snapshot again; if it changed, the stream fails
/// instead, so the response never completes and the Receiver's browser marks the download
/// failed rather than keeping a half-old file. (A file that shrinks fails by ending early.)
async fn snapshot_bytes(
    mut file: tokio::fs::File,
    item: &Item,
    start: u64,
    length: u64,
) -> std::io::Result<impl Stream<Item = std::io::Result<Bytes>> + Send + Unpin + use<>> {
    file.seek(SeekFrom::Start(start)).await?;
    let probe = file.try_clone().await?;
    let (size, modified) = (item.size, item.modified);
    let unchanged = move |metadata: std::fs::Metadata| {
        metadata.len() == size && metadata.modified().ok() == Some(modified)
    };
    let chunks = ReaderStream::new(file.take(length));
    Ok(Box::pin(futures_util::stream::unfold(
        Some((chunks, probe, length)),
        move |state| async move {
            let (mut chunks, probe, remaining) = state?;
            let chunk = match chunks.next().await? {
                Ok(chunk) => chunk,
                Err(error) => return Some((Err(error), None)),
            };
            let remaining = remaining - chunk.len() as u64;
            if remaining == 0 && !probe.metadata().await.is_ok_and(unchanged) {
                let error = std::io::Error::other("the file changed while being sent");
                return Some((Err(error), None));
            }
            Some((Ok(chunk), Some((chunks, probe, remaining))))
        },
    )))
}

/// `409 Conflict` with a short explanation the Receiver's browser can show.
fn unavailable(status: ItemStatus) -> Response {
    let reason = match status {
        ItemStatus::Missing => {
            "This file is no longer on the computer. Ask the sender to add it again."
        }
        _ => "This file changed on the computer after it was added. Ask the sender to refresh it.",
    };
    (
        StatusCode::CONFLICT,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        reason,
    )
        .into_response()
}

/// Passes `chunks` through until the Item is withdrawn (`withdrawn`) or this listener
/// stops (`stop`), then fails the stream so the connection is cut and the Receiver's
/// browser marks the download as failed, rather than ending early as if it were complete.
fn abortable(
    chunks: impl Stream<Item = std::io::Result<Bytes>> + Send + Unpin,
    withdrawn: CancellationToken,
    stop: CancellationToken,
) -> impl Stream<Item = std::io::Result<Bytes>> {
    futures_util::stream::unfold(Some((chunks, withdrawn, stop)), |state| async move {
        let (mut chunks, withdrawn, stop) = state?;
        tokio::select! {
            biased;
            () = withdrawn.cancelled() => {
                Some((Err(std::io::Error::other("Item is no longer offered")), None))
            }
            () = stop.cancelled() => {
                Some((Err(std::io::Error::other("the Sender stopped listening here")), None))
            }
            chunk = chunks.next() => chunk.map(|chunk| (chunk, Some((chunks, withdrawn, stop)))),
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
