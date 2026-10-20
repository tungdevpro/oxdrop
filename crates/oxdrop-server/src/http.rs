use std::convert::Infallible;
use std::io::SeekFrom;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{FromRef, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
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

use crate::engine::{AllRefusal, Refusal, Shared};
use crate::page::{self, ReceiverView};
use crate::range::Requested;
use crate::tracked::{ByteStream, Share, Tracked};
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
        .route("/t/{secret}/all", get(download_all))
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
    method: Method,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    check_secret(&shared, &secret)?;
    let id: ItemId = item_id.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    let (item, cancelled) = match shared.start_download(id) {
        Ok(download) => download,
        Err(Refusal::NoSuchItem) => return Err(StatusCode::NOT_FOUND),
        Err(Refusal::Unavailable(status)) => return Ok(unavailable(status)),
    };
    let withdrawn = cancelled.cancelled_owned();
    let name = item.download_name();
    let etag = item.etag();

    // A file is served as itself; a folder as a zip laid out from its snapshot.
    let ItemKind::Folder(snapshot) = &item.kind else {
        let span = match Span::requested(&headers, item.size, &etag) {
            Ok(span) => span,
            Err(unsatisfiable) => return Ok(unsatisfiable.into_response()),
        };
        let file = match tokio::fs::File::open(&item.path).await {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(%error, item = %item.id, "can't open Item");
                return Ok(unavailable(ItemStatus::Missing));
            }
        };
        let bytes = snapshot_bytes(file, &item, span.start, span.length)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let share = Share {
            item: item.id,
            identity: etag.clone(),
            at: 0,
            total: item.size,
        };
        let download = Download {
            content_type: "application/octet-stream",
            name,
            etag,
            span,
            head: method == Method::HEAD,
        };
        let progress = (shared, vec![share]);
        return Ok(download.respond(Box::pin(bytes), progress, withdrawn, stop));
    };
    let plan = ZipPlan::for_folder(&item.name, &item.path, item.modified, snapshot);
    let share = Share {
        item: item.id,
        identity: etag.clone(),
        at: 0,
        total: plan.len(),
    };
    let zip = Zip {
        plan,
        name,
        etag,
        shares: vec![share],
        head: method == Method::HEAD,
    };
    match serve_zip(&shared, zip, &headers, withdrawn, stop).await {
        Ok(response) => Ok(response),
        Err(error) => {
            tracing::warn!(%error, item = %item.id, "folder changed while preparing zip");
            Ok(unavailable(ItemStatus::Changed))
        }
    }
}

/// "Download all": every Item in one zip, each at its root under its Item Name.
async fn download_all(
    State(Listening { shared, stop, .. }): State<Listening>,
    Path(secret): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    check_secret(&shared, &secret)?;
    let (items, tokens) = match shared.start_download_all() {
        Ok(download) => download,
        Err(AllRefusal::NoItems) => return Err(StatusCode::NOT_FOUND),
        Err(AllRefusal::Unavailable(names)) => return Ok(some_unavailable(&names)),
    };
    // Withdrawing any one of the Items withdraws the whole zip.
    let withdrawn = async move {
        futures_util::future::select_all(
            tokens
                .into_iter()
                .map(|token| Box::pin(token.cancelled_owned())),
        )
        .await;
    };
    let plan = ZipPlan::for_items(&items);
    let etag = all_etag(&items);
    let shares = items
        .iter()
        .zip(plan.item_spans())
        .map(|(item, span)| Share {
            item: item.id,
            identity: etag.clone(),
            at: span.start,
            total: span.end - span.start,
        })
        .collect();
    let zip = Zip {
        plan,
        name: ALL_NAME.to_owned(),
        etag,
        shares,
        head: method == Method::HEAD,
    };
    match serve_zip(&shared, zip, &headers, withdrawn, stop).await {
        Ok(response) => Ok(response),
        Err(error) => {
            tracing::warn!(%error, "an Item changed while preparing the Download all zip");
            Ok(unavailable(ItemStatus::Changed))
        }
    }
}

/// What the Receiver saves "Download all" as.
const ALL_NAME: &str = "Oxdrop.zip";

/// A strong `ETag` for "Download all" that changes whenever any Item is added, removed,
/// renamed or refreshed, so a resumed download never splices two different zips.
fn all_etag(items: &[Item]) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    for item in items {
        (item.id, &item.name, item.etag()).hash(&mut hasher);
    }
    format!("\"a{:x}-{:x}\"", items.len(), hasher.finish())
}

/// A zip to serve: its layout, what the Receiver saves it as and its `ETag`, and where
/// the Item downloads it holds sit in it.
struct Zip {
    plan: ZipPlan,
    name: String,
    etag: String,
    shares: Vec<Share>,
    /// Whether only the headers are wanted.
    head: bool,
}

/// Serves the part of the zip the request asks for. Fails if a file it needs differs
/// from its snapshot.
async fn serve_zip(
    shared: &Arc<Shared>,
    zip: Zip,
    headers: &HeaderMap,
    withdrawn: impl Future<Output = ()> + Send + 'static,
    stop: CancellationToken,
) -> std::io::Result<Response> {
    let Zip {
        plan,
        name,
        etag,
        shares,
        head,
    } = zip;
    let span = match Span::requested(headers, plan.len(), &etag) {
        Ok(span) => span,
        Err(unsatisfiable) => return Ok(unsatisfiable.into_response()),
    };
    let end = span.start + span.length;
    let bytes = zip_range(Arc::new(plan), shared.crcs.clone(), span.start, end).await?;
    let download = Download {
        content_type: "application/zip",
        name,
        etag,
        span,
        head,
    };
    let progress = (shared.clone(), shares);
    Ok(download.respond(Box::pin(bytes), progress, withdrawn, stop))
}

/// Which bytes of a download of `total` bytes to send, and with what status.
struct Span {
    status: StatusCode,
    start: u64,
    length: u64,
    total: u64,
}

impl Span {
    /// The span the request's `Range` (and `If-Range`) headers ask for.
    fn requested(headers: &HeaderMap, total: u64, etag: &str) -> Result<Self, Unsatisfiable> {
        let (status, start, length) = match Requested::from_headers(headers, total, etag) {
            Requested::Full => (StatusCode::OK, 0, total),
            Requested::Partial { start, end } => {
                (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
            }
            Requested::Unsatisfiable => return Err(Unsatisfiable { total }),
        };
        Ok(Self {
            status,
            start,
            length,
            total,
        })
    }
}

/// No part of a download of `total` bytes is what the request's `Range` asks for.
struct Unsatisfiable {
    total: u64,
}

impl IntoResponse for Unsatisfiable {
    fn into_response(self) -> Response {
        let range = format!("bytes */{}", self.total);
        (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, range)],
        )
            .into_response()
    }
}

/// A download response: what it is, what the Receiver saves it as, and which bytes.
struct Download {
    content_type: &'static str,
    name: String,
    etag: String,
    span: Span,
    /// Whether only the headers are wanted, so nothing is served toward any download.
    head: bool,
}

impl Download {
    /// Streams `bytes` as this download, reporting progress toward the Item downloads in
    /// `shares` and cut off if it is withdrawn or the listener stops.
    fn respond(
        self,
        bytes: ByteStream,
        (shared, shares): (Arc<Shared>, Vec<Share>),
        withdrawn: impl Future<Output = ()> + Send + 'static,
        stop: CancellationToken,
    ) -> Response {
        let Span {
            status,
            start,
            length,
            total,
        } = self.span;
        let shares = if self.head { Vec::new() } else { shares };
        let mut response = (
            status,
            [
                (header::CONTENT_TYPE, self.content_type.to_owned()),
                (header::CONTENT_LENGTH, length.to_string()),
                (header::CONTENT_DISPOSITION, content_disposition(&self.name)),
                (header::ETAG, self.etag),
                (header::ACCEPT_RANGES, "bytes".to_owned()),
            ],
            Body::from_stream(abortable(
                Tracked::new(bytes, shared, shares, start..start + length),
                withdrawn,
                stop,
            )),
        )
            .into_response();
        if status == StatusCode::PARTIAL_CONTENT {
            let range = format!("bytes {start}-{}/{total}", start + length - 1);
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&range).expect("ASCII header value"),
            );
        }
        response
    }
}

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

/// `409 Conflict` for "Download all", naming the Items that are Changed or Missing.
fn some_unavailable(names: &[String]) -> Response {
    let reason = format!(
        "Some items changed on the computer or are no longer there: {}. \
         Ask the sender to refresh or remove them, or download the others one at a time.",
        names.join(", ")
    );
    (
        StatusCode::CONFLICT,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        reason,
    )
        .into_response()
}

/// Passes `chunks` through until the download is withdrawn (`withdrawn` completes) or
/// this listener stops (`stop`), then fails the stream so the connection is cut and the
/// Receiver's browser marks the download as failed, rather than ending early as if it
/// were complete.
fn abortable(
    chunks: impl Stream<Item = std::io::Result<Bytes>> + Send + Unpin,
    withdrawn: impl Future<Output = ()> + Send + 'static,
    stop: CancellationToken,
) -> impl Stream<Item = std::io::Result<Bytes>> {
    let withdrawn = Box::pin(withdrawn);
    futures_util::stream::unfold(Some((chunks, withdrawn, stop)), |state| async move {
        let (mut chunks, mut withdrawn, stop) = state?;
        tokio::select! {
            biased;
            () = &mut withdrawn => {
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
