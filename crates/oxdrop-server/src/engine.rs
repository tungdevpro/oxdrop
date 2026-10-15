use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use oxdrop_core::{AddError, Added, Item, ItemId, ItemStatus, Transfer, TransferLink};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{http, network};

/// How often the Sender rechecks Items' files, so Changed and Missing show up
/// without waiting for the Receiver to ask for them.
const CHECK_INTERVAL: Duration = Duration::from_secs(2);

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
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            bind: BindAddress::Auto,
            port: DEFAULT_PORT,
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
}

/// The open Transfer plus a cancel switch for each Item's in-flight downloads.
pub(crate) struct Live {
    pub(crate) transfer: Transfer,
    downloads: HashMap<ItemId, CancellationToken>,
}

impl Live {
    /// Rechecks an Item's file against its snapshot. Returns its status and whether
    /// that differs from what was recorded before, or `None` if there's no such Item.
    fn check(&mut self, id: ItemId) -> Option<(ItemStatus, bool)> {
        let before = self.transfer.item(id)?.status;
        let after = self.transfer.check(id)?;
        Some((after, after != before))
    }

    /// The Item to serve and a token that is cancelled if the Item is removed or the engine stops.
    fn start_download(
        &mut self,
        id: ItemId,
        shutdown: &CancellationToken,
    ) -> Result<(Item, CancellationToken), Refusal> {
        let item = self.transfer.item(id).ok_or(Refusal::NoSuchItem)?.clone();
        let token = self
            .downloads
            .entry(id)
            .or_insert_with(|| shutdown.child_token())
            .clone();
        Ok((item, token))
    }

    /// Aborts the Item's in-flight downloads.
    fn cancel_downloads(&mut self, id: ItemId) {
        if let Some(downloads) = self.downloads.remove(&id) {
            downloads.cancel();
        }
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

    /// Rechecks every Item's file against its snapshot, telling subscribers if any
    /// became Changed or Missing (or matches again).
    fn check_items(&self) {
        let any_changed = {
            let mut live = self.live();
            let ids: Vec<ItemId> = live.transfer.items().iter().map(|item| item.id).collect();
            ids.into_iter()
                .filter_map(|id| live.check(id))
                .fold(false, |any, (_, changed)| any || changed)
        };
        if any_changed {
            self.publish();
        }
    }

    /// Rechecks the Item's file, then hands out what to serve: the Item as snapshotted
    /// and a token that is cancelled if the Item is removed or the engine stops.
    pub(crate) fn start_download(&self, id: ItemId) -> Result<(Item, CancellationToken), Refusal> {
        let (outcome, status_changed) = {
            let mut live = self.live();
            match live.check(id) {
                None => (Err(Refusal::NoSuchItem), false),
                Some((ItemStatus::Available, changed)) => {
                    (live.start_download(id, &self.shutdown), changed)
                }
                Some((status, changed)) => (Err(Refusal::Unavailable(status)), changed),
            }
        };
        if status_changed {
            self.publish();
        }
        outcome
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
        let (updates, _) = watch::channel(TransferState {
            link: TransferLink {
                address,
                secret: transfer.secret().clone(),
            },
            items: Vec::new(),
            receiver_connected: false,
        });
        let shared = Arc::new(Shared {
            address: Mutex::new(address),
            live: Mutex::new(Live {
                transfer,
                downloads: HashMap::new(),
            }),
            updates,
            receivers: AtomicUsize::new(0),
            shutdown: CancellationToken::new(),
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
        let added = self.shared.live().transfer.add_path(path);
        if matches!(added, Ok(Added::New(_))) {
            self.shared.publish();
        }
        added
    }

    /// Adds several paths at once; the outcomes are in the same order as `paths`.
    pub fn add_paths(&self, paths: &[PathBuf]) -> Vec<Result<Added, AddError>> {
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
        let removed = self.shared.live().remove(id);
        if removed {
            self.shared.publish();
        }
        removed
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
