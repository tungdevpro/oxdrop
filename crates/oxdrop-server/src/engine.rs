use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use oxdrop_core::{
    AddError, Added, DownloadTracker, IdleState, IdleTimer, Item, ItemId, ItemStatus, Progress,
    Transfer, TransferLink, TransferSecret,
};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::clock::{Clock, SystemClock};
use crate::zip_stream::CrcCache;
use crate::{http, network};

/// How often the Sender rechecks Items' files, so Changed and Missing show up
/// without waiting for the Receiver to ask for them.
const CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// How often, at most, progress alone is published while bytes are flowing. Starting,
/// finishing and failing are published straight away.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// The port the Sender listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 48230;

/// Which local address the Sender listens on, and so which address the Transfer Link uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindAddress {
    /// Pick a private IPv4 address the Receiver can most likely reach.
    Auto,
    Ip(IpAddr),
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub bind: BindAddress,
    /// `0` lets the OS choose.
    pub port: u16,
    /// How long a Transfer may go without activity before it ends.
    pub idle_timeout: Duration,
    /// How long before the idle deadline the Sender starts warning.
    pub warning_lead: Duration,
    pub clock: Arc<dyn Clock>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            bind: BindAddress::Auto,
            port: DEFAULT_PORT,
            idle_timeout: Duration::from_secs(30 * 60),
            warning_lead: Duration::from_secs(2 * 60),
            clock: Arc::new(SystemClock),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("no usable local network connection; connect to Wi-Fi or Ethernet")]
    NoNetwork,
    #[error("can't listen on {address}: {source}")]
    Bind {
        address: SocketAddr,
        source: std::io::Error,
    },
    #[error("can't start the server: {0}")]
    Runtime(std::io::Error),
}

/// What the Sender shows: the open Transfer's link and Items, and whether a Receiver is looking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferState {
    pub link: TransferLink,
    pub items: Vec<Item>,
    /// Whether at least one Receiver page is open and listening for updates.
    pub receiver_connected: bool,
    /// Each Item's progress, for Items that have been downloaded at all.
    pub progress: HashMap<ItemId, Progress>,
    /// When the Transfer ends if left alone, and whether to warn about it.
    pub idle: IdleState,
}

impl TransferState {
    /// The Item's progress toward its most recent download.
    pub fn progress_of(&self, id: ItemId) -> Progress {
        self.progress
            .get(&id)
            .copied()
            .unwrap_or(Progress::NotStarted)
    }
}

/// The open Transfer plus a cancel switch for each Item's in-flight downloads, how far
/// each Item's most recent download has got, and how long until it goes idle.
pub(crate) struct Live {
    pub(crate) transfer: Transfer,
    /// Counts the Transfers opened, so a response from an ended one can tell.
    generation: u64,
    /// Cancelled when the Transfer ends (or the engine stops).
    ended: CancellationToken,
    /// The secret of the Transfer that ended last, which gets "ended" rather than 404.
    last_ended: Option<TransferSecret>,
    downloads: HashMap<ItemId, CancellationToken>,
    progress: HashMap<ItemId, DownloadTracker>,
    idle: IdleTimer,
}

impl Live {
    /// A token that is cancelled if the Item is removed or refreshed, the Transfer ends,
    /// or the engine stops.
    fn download_token(&mut self, id: ItemId) -> CancellationToken {
        let ended = &self.ended;
        self.downloads
            .entry(id)
            .or_insert_with(|| ended.child_token())
            .clone()
    }

    /// Aborts the Item's in-flight downloads and forgets their progress.
    fn cancel_downloads(&mut self, id: ItemId) {
        if let Some(downloads) = self.downloads.remove(&id) {
            downloads.cancel();
        }
        self.progress.remove(&id);
    }

    fn remove(&mut self, id: ItemId) -> bool {
        self.cancel_downloads(id);
        self.transfer.remove(id)
    }
}

/// State shared between the engine handle and the HTTP server.
pub(crate) struct Shared {
    /// Where the Transfer Link points: the address of the listener now serving.
    address: Mutex<SocketAddr>,
    live: Mutex<Live>,
    updates: watch::Sender<TransferState>,
    receivers: AtomicUsize,
    pub(crate) shutdown: CancellationToken,
    pub(crate) crcs: Arc<CrcCache>,
    /// When progress alone was last published.
    progress_published: Mutex<Instant>,
    clock: Arc<dyn Clock>,
    idle_timeout: Duration,
    warning_lead: Duration,
    /// Wakes the idle watcher when activity may have moved the deadline.
    activity: Notify,
}

/// What a secret in a request opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// The open Transfer.
    Open,
    /// The Transfer that ended last.
    Ended,
    Unknown,
}

impl Shared {
    fn address(&self) -> SocketAddr {
        *self.address.lock().expect("address lock poisoned")
    }

    pub(crate) fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().expect("transfer lock poisoned")
    }

    fn state(&self) -> TransferState {
        let address = self.address();
        let live = self.live();
        TransferState {
            link: TransferLink {
                address,
                secret: live.transfer.secret().clone(),
            },
            items: live.transfer.items().to_vec(),
            receiver_connected: self.receivers.load(Ordering::SeqCst) > 0,
            progress: live
                .progress
                .iter()
                .map(|(id, tracker)| (*id, tracker.progress()))
                .collect(),
            idle: live.idle.state(self.clock.now()),
        }
    }

    /// Tells subscribers (the Sender window, Receiver pages) that the state changed.
    /// Must not be called while holding the `live` lock.
    fn publish(&self) {
        self.updates.send_replace(self.state());
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<TransferState> {
        self.updates.subscribe()
    }

    /// Rechecks `items` against their snapshots and records any status that changed,
    /// telling subscribers. The files are looked at without holding the `live` lock, so a
    /// large folder doesn't hold up downloads. Returns each Item's status now.
    fn check(&self, items: &[Item]) -> Vec<ItemStatus> {
        let statuses: Vec<ItemStatus> = items.iter().map(Item::current_status).collect();
        let changed = {
            let mut live = self.live();
            items
                .iter()
                .zip(&statuses)
                .filter(|(item, status)| item.status != **status)
                .fold(false, |any, (item, status)| {
                    live.transfer.set_status(item.id, &item.etag(), *status) || any
                })
        };
        if changed {
            self.publish();
        }
        statuses
    }

    /// Rechecks every Item's files against its snapshot, telling subscribers if any
    /// became Changed or Missing (or matches again).
    fn check_items(&self) {
        let items = self.live().transfer.items().to_vec();
        self.check(&items);
    }

    /// Rechecks the Item's files, then hands out what to serve: the Item as snapshotted
    /// and a token that is cancelled if the Item is removed or refreshed, or the engine stops.
    pub(crate) fn start_download(&self, id: ItemId) -> Result<(Item, CancellationToken), Refusal> {
        let item = self
            .live()
            .transfer
            .item(id)
            .cloned()
            .ok_or(Refusal::NoSuchItem)?;
        match self.check(std::slice::from_ref(&item))[0] {
            ItemStatus::Available => {
                let token = self.live().download_token(id);
                Ok((item, token))
            }
            status => Err(Refusal::Unavailable(status)),
        }
    }

    /// Rechecks every Item, then hands out what "Download all" serves: every Item as
    /// snapshotted, and for each a token that is cancelled if that Item is removed or
    /// refreshed, or the engine stops. Refused if any Item is Changed or Missing.
    pub(crate) fn start_download_all(
        &self,
    ) -> Result<(Vec<Item>, Vec<CancellationToken>), AllRefusal> {
        let items = self.live().transfer.items().to_vec();
        if items.is_empty() {
            return Err(AllRefusal::NoItems);
        }
        let unavailable: Vec<String> = items
            .iter()
            .zip(self.check(&items))
            .filter(|(_, status)| *status != ItemStatus::Available)
            .map(|(item, _)| item.name.clone())
            .collect();
        if !unavailable.is_empty() {
            return Err(AllRefusal::Unavailable(unavailable));
        }
        let mut live = self.live();
        let tokens = items
            .iter()
            .map(|item| live.download_token(item.id))
            .collect();
        Ok((items, tokens))
    }

    /// Records what a response did toward the Item's download, telling subscribers when
    /// it starts, finishes or fails, and otherwise every so often.
    /// Ignored if the Transfer it was served from (`generation`) has ended.
    pub(crate) fn report(
        &self,
        generation: u64,
        id: ItemId,
        update: impl FnOnce(&mut DownloadTracker),
    ) {
        let (before, after) = {
            let mut live = self.live();
            if live.generation != generation || live.transfer.item(id).is_none() {
                return;
            }
            let tracker = live.progress.entry(id).or_default();
            let before = tracker.progress();
            update(tracker);
            (before, tracker.progress())
        };
        if before == after {
            return;
        }
        let moved_on = std::mem::discriminant(&before) != std::mem::discriminant(&after);
        let mut published = self
            .progress_published
            .lock()
            .expect("progress lock poisoned");
        if moved_on || published.elapsed() >= PROGRESS_INTERVAL {
            *published = Instant::now();
            drop(published);
            self.publish();
        }
    }

    /// Whether `secret` opens the open Transfer, the one that ended last, or nothing.
    /// Admitting a request to the open Transfer counts as activity.
    pub(crate) fn admit(&self, secret: &str) -> Admission {
        let admission = {
            let live = self.live();
            if live.transfer.secret().matches(secret) {
                Admission::Open
            } else if live
                .last_ended
                .as_ref()
                .is_some_and(|ended| ended.matches(secret))
            {
                Admission::Ended
            } else {
                Admission::Unknown
            }
        };
        if admission == Admission::Open {
            self.touch();
        }
        admission
    }

    /// Something counting as activity happened just now.
    fn touch(&self) {
        let now = self.clock.now();
        self.live().idle.touch(now);
        self.activity.notify_one();
    }

    /// A download response starts streaming; it counts as activity until
    /// [`stream_ended`](Self::stream_ended). Returns the Transfer it belongs to.
    pub(crate) fn stream_started(&self) -> u64 {
        let now = self.clock.now();
        let generation = {
            let mut live = self.live();
            live.idle.stream_started(now);
            live.generation
        };
        self.activity.notify_one();
        generation
    }

    /// A download response of Transfer `generation` stopped streaming.
    pub(crate) fn stream_ended(&self, generation: u64) {
        let now = self.clock.now();
        {
            let mut live = self.live();
            if live.generation != generation {
                return;
            }
            live.idle.stream_ended(now);
        }
        self.activity.notify_one();
    }

    /// A token cancelled when the open Transfer ends, or the engine stops.
    pub(crate) fn ended_token(&self) -> CancellationToken {
        self.live().ended.clone()
    }

    /// Ends the open Transfer, cutting off its downloads and event streams so its
    /// Transfer Link stops working, and opens a fresh, empty one with a new secret.
    fn end_transfer(&self) {
        let now = self.clock.now();
        {
            let mut live = self.live();
            let ended = std::mem::replace(&mut live.transfer, Transfer::open());
            live.last_ended = Some(ended.secret().clone());
            live.generation += 1;
            live.ended.cancel();
            live.ended = self.shutdown.child_token();
            live.downloads.clear();
            live.progress.clear();
            live.idle = IdleTimer::new(now, self.idle_timeout, self.warning_lead);
        }
        tracing::info!("Transfer ended; a fresh one is open");
        self.publish();
        self.activity.notify_one();
    }

    /// Ends the Transfer once its idle deadline passes, telling subscribers when the
    /// warning starts. Runs until the engine stops.
    async fn watch_idle(&self) {
        let mut shown = None;
        loop {
            let now = self.clock.now();
            let (state, next) = {
                let live = self.live();
                (live.idle.state(now), live.idle.next_change(now))
            };
            if state == IdleState::Expired {
                self.end_transfer();
                continue;
            }
            // Publish when the countdown starts, stops or moves; not for every request.
            let warning = matches!(state, IdleState::Warning { .. }).then_some(state);
            if warning != shown {
                shown = warning;
                self.publish();
            }
            let wake = async {
                match next {
                    Some(at) => self.clock.sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = self.shutdown.cancelled() => break,
                () = self.activity.notified() => {}
                () = wake => {}
            }
        }
    }

    /// Marks a Receiver as connected until the returned guard is dropped.
    pub(crate) fn connect_receiver(self: &Arc<Self>) -> ReceiverGuard {
        self.receivers.fetch_add(1, Ordering::SeqCst);
        self.publish();
        ReceiverGuard(self.clone())
    }
}

/// Why an Item can't be downloaded right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    NoSuchItem,
    /// The Item's file is Changed or Missing.
    Unavailable(ItemStatus),
}

/// Why "Download all" can't be served right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AllRefusal {
    NoItems,
    /// These Items (by Item Name) are Changed or Missing, so not all can be sent.
    Unavailable(Vec<String>),
}

pub(crate) struct ReceiverGuard(Arc<Shared>);

impl Drop for ReceiverGuard {
    fn drop(&mut self) {
        self.0.receivers.fetch_sub(1, Ordering::SeqCst);
        self.0.publish();
    }
}

/// The running Sender: one open Transfer served over HTTP on a local address.
/// Dropping it stops the server.
pub struct SenderEngine {
    shared: Arc<Shared>,
    /// The port to try first whenever the Sender binds.
    port: u16,
    runtime: tokio::runtime::Handle,
    /// The listener serving now; replaced when the user switches interface.
    serving: Mutex<Serving>,
    /// Owns the runtime until shutdown.
    thread: Option<JoinHandle<()>>,
}

/// One bound listener serving the Transfer.
struct Serving {
    stop: CancellationToken,
    stopped: mpsc::Receiver<()>,
}

impl Serving {
    /// Spawns a server for `listener` on `runtime`.
    fn start(
        shared: &Arc<Shared>,
        runtime: &tokio::runtime::Handle,
        listener: TcpListener,
        address: SocketAddr,
    ) -> Self {
        let stop = shared.shutdown.child_token();
        let router = http::router(http::Listening {
            shared: shared.clone(),
            address,
            stop: stop.clone(),
        });
        let (done, stopped) = mpsc::channel();
        let shutdown = stop.clone();
        runtime.spawn(async move {
            let listener =
                tokio::net::TcpListener::from_std(listener).expect("listener is non-blocking");
            let serve =
                axum::serve(listener, router).with_graceful_shutdown(shutdown.cancelled_owned());
            if let Err(error) = serve.await {
                tracing::error!(%error, %address, "server stopped");
            }
            let _ = done.send(());
        });
        tracing::info!(%address, "Sender listening");
        Self { stop, stopped }
    }

    /// Stops listening and ends this listener's event streams and downloads,
    /// waiting briefly for its socket to close.
    fn stop(self) {
        self.stop.cancel();
        if self.stopped.recv_timeout(Duration::from_secs(2)).is_err() {
            tracing::warn!("old listener is slow to close");
        }
    }
}

impl SenderEngine {
    /// Binds the listener and opens the first Transfer.
    pub fn start(config: EngineConfig) -> Result<Self, StartError> {
        let ip = match config.bind {
            BindAddress::Ip(ip) => ip,
            BindAddress::Auto => IpAddr::V4(
                network::interface_candidates()
                    .first()
                    .ok_or(StartError::NoNetwork)?
                    .ip,
            ),
        };
        let listener = bind(ip, config.port)?;
        let address = listener.local_addr().map_err(StartError::Runtime)?;

        let transfer = Transfer::open();
        let idle = IdleTimer::new(config.clock.now(), config.idle_timeout, config.warning_lead);
        let (updates, _) = watch::channel(TransferState {
            link: TransferLink {
                address,
                secret: transfer.secret().clone(),
            },
            items: Vec::new(),
            receiver_connected: false,
            progress: HashMap::new(),
            idle: idle.state(config.clock.now()),
        });
        let shutdown = CancellationToken::new();
        let shared = Arc::new(Shared {
            address: Mutex::new(address),
            live: Mutex::new(Live {
                transfer,
                generation: 0,
                ended: shutdown.child_token(),
                last_ended: None,
                downloads: HashMap::new(),
                progress: HashMap::new(),
                idle,
            }),
            updates,
            receivers: AtomicUsize::new(0),
            shutdown,
            crcs: Arc::default(),
            progress_published: Mutex::new(Instant::now()),
            clock: config.clock,
            idle_timeout: config.idle_timeout,
            warning_lead: config.warning_lead,
            activity: Notify::new(),
        });

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("oxdrop-server")
            .enable_all()
            .build()
            .map_err(StartError::Runtime)?;
        let handle = runtime.handle().clone();
        let serving = Serving::start(&shared, &handle, listener, address);
        handle.spawn({
            let shared = shared.clone();
            async move {
                let mut ticks = tokio::time::interval(CHECK_INTERVAL);
                loop {
                    tokio::select! {
                        () = shared.shutdown.cancelled() => break,
                        _ = ticks.tick() => shared.check_items(),
                    }
                }
            }
        });
        handle.spawn({
            let shared = shared.clone();
            async move { shared.watch_idle().await }
        });
        let shutdown = shared.shutdown.clone();
        // The runtime lives on its own thread so the engine can be dropped anywhere,
        // including inside another runtime.
        let thread = std::thread::Builder::new()
            .name("oxdrop-server".into())
            .spawn(move || runtime.block_on(shutdown.cancelled_owned()))
            .map_err(StartError::Runtime)?;

        Ok(Self {
            shared,
            port: config.port,
            runtime: handle,
            serving: Mutex::new(serving),
            thread: Some(thread),
        })
    }

    /// Listens on `ip` instead, so the Transfer Link points there. The Transfer and its
    /// secret are unchanged. If `ip` can't be bound, the Sender keeps listening where it was.
    pub fn switch_interface(&self, ip: IpAddr) -> Result<(), StartError> {
        self.shared.touch();
        let mut serving = self.serving.lock().expect("serving lock poisoned");
        if self.shared.address().ip() == ip {
            return Ok(());
        }
        let listener = bind(ip, self.port)?;
        let address = listener.local_addr().map_err(StartError::Runtime)?;
        let new = Serving::start(&self.shared, &self.runtime, listener, address);
        std::mem::replace(&mut *serving, new).stop();
        *self.shared.address.lock().expect("address lock poisoned") = address;
        drop(serving);
        self.shared.publish();
        Ok(())
    }

    pub fn state(&self) -> TransferState {
        self.shared.state()
    }

    /// A feed of state changes; the current state is always available via `borrow()`.
    pub fn subscribe(&self) -> watch::Receiver<TransferState> {
        self.shared.subscribe()
    }

    /// Adds the file at `path` to the open Transfer as an Item.
    pub fn add_path(&self, path: &Path) -> Result<Added, AddError> {
        self.shared.touch();
        let added = self.shared.live().transfer.add_path(path);
        if matches!(added, Ok(Added::New(_))) {
            self.shared.publish();
        }
        added
    }

    /// Adds several paths at once; the outcomes are in the same order as `paths`.
    pub fn add_paths(&self, paths: &[PathBuf]) -> Vec<Result<Added, AddError>> {
        self.shared.touch();
        let outcomes: Vec<_> = {
            let mut live = self.shared.live();
            paths
                .iter()
                .map(|path| live.transfer.add_path(path))
                .collect()
        };
        if outcomes.iter().any(|o| matches!(o, Ok(Added::New(_)))) {
            self.shared.publish();
        }
        outcomes
    }

    /// Rechecks every Item's file now. The engine also does this every few seconds.
    pub fn check_items(&self) {
        self.shared.check_items();
    }

    /// Snapshots an Item's file again so its current contents are offered, aborting
    /// downloads of the previous version. Returns whether there was such an Item.
    /// If the file can't be read, the Item stays Missing and the error says why.
    pub fn refresh_item(&self, id: ItemId) -> Result<bool, AddError> {
        self.shared.touch();
        let refreshed = {
            let mut live = self.shared.live();
            live.cancel_downloads(id);
            live.transfer.refresh(id)
        };
        if !matches!(refreshed, Ok(false)) {
            self.shared.publish();
        }
        refreshed
    }

    /// Removes an Item and aborts its in-flight downloads. Returns whether it was there.
    pub fn remove_item(&self, id: ItemId) -> bool {
        self.shared.touch();
        let removed = self.shared.live().remove(id);
        if removed {
            self.shared.publish();
        }
        removed
    }
}

impl SenderEngine {
    /// Ends the open Transfer: its Transfer Link stops working, its downloads are cut off
    /// and open Receiver pages are told. A fresh, empty Transfer with a new link opens.
    pub fn end_transfer(&self) {
        self.shared.end_transfer();
    }

    /// Counts as activity, putting the idle deadline off again ("Keep open").
    pub fn keep_open(&self) {
        self.shared.touch();
    }
}

/// Listens on `ip` alone, at `port` if it's free and otherwise at a port the OS picks,
/// so a busy port never stops the Sender from starting.
fn bind(ip: IpAddr, port: u16) -> Result<TcpListener, StartError> {
    let requested = SocketAddr::new(ip, port);
    let listener = match TcpListener::bind(requested) {
        // Windows reports ports reserved by Hyper-V/WSL as access denied.
        Err(error)
            if port != 0
                && matches!(
                    error.kind(),
                    ErrorKind::AddrInUse | ErrorKind::PermissionDenied
                ) =>
        {
            tracing::info!(%requested, %error, "port unavailable; letting the OS pick one");
            TcpListener::bind(SocketAddr::new(ip, 0))
        }
        result => result,
    }
    .map_err(|source| StartError::Bind {
        address: requested,
        source,
    })?;
    listener
        .set_nonblocking(true)
        .map_err(StartError::Runtime)?;
    Ok(listener)
}

impl Drop for SenderEngine {
    fn drop(&mut self) {
        self.shared.shutdown.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
