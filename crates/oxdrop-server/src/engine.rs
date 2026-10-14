use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use oxdrop_core::{AddError, Added, Item, ItemId, Transfer, TransferLink};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{http, network};

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
    /// The Item to serve and a token that is cancelled if the Item is removed or the engine stops.
    pub(crate) fn start_download(
        &mut self,
        id: ItemId,
        shutdown: &CancellationToken,
    ) -> Option<(Item, CancellationToken)> {
        let item = self.transfer.item(id)?.clone();
        let token = self
            .downloads
            .entry(id)
            .or_insert_with(|| shutdown.child_token())
            .clone();
        Some((item, token))
    }

    fn remove(&mut self, id: ItemId) -> bool {
        if let Some(downloads) = self.downloads.remove(&id) {
            downloads.cancel();
        }
        self.transfer.remove(id)
    }
}

/// State shared between the engine handle and the HTTP server.
pub(crate) struct Shared {
    address: SocketAddr,
    live: Mutex<Live>,
    updates: watch::Sender<TransferState>,
    receivers: AtomicUsize,
    pub(crate) shutdown: CancellationToken,
}

impl Shared {
    pub(crate) fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().expect("transfer lock poisoned")
    }

    fn state(&self) -> TransferState {
        let live = self.live();
        TransferState {
            link: TransferLink {
                address: self.address,
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

    /// Marks a Receiver as connected until the returned guard is dropped.
    pub(crate) fn connect_receiver(self: &Arc<Self>) -> ReceiverGuard {
        self.receivers.fetch_add(1, Ordering::SeqCst);
        self.publish();
        ReceiverGuard(self.clone())
    }
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
    server: Option<JoinHandle<()>>,
}

impl SenderEngine {
    /// Binds the listener and opens the first Transfer.
    pub fn start(config: EngineConfig) -> Result<Self, StartError> {
        let ip = match config.bind {
            BindAddress::Ip(ip) => ip,
            BindAddress::Auto => network::pick_address().ok_or(StartError::NoNetwork)?,
        };
        let requested = SocketAddr::new(ip, config.port);
        let listener = TcpListener::bind(requested).map_err(|source| StartError::Bind {
            address: requested,
            source,
        })?;
        listener
            .set_nonblocking(true)
            .map_err(StartError::Runtime)?;
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
            address,
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
        let router = http::router(shared.clone());
        let shutdown = shared.shutdown.clone();
        let server = std::thread::Builder::new()
            .name("oxdrop-server".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .expect("listener is non-blocking");
                    let serve = axum::serve(listener, router)
                        .with_graceful_shutdown(shutdown.cancelled_owned());
                    if let Err(error) = serve.await {
                        tracing::error!(%error, "server stopped");
                    }
                });
            })
            .map_err(StartError::Runtime)?;

        tracing::info!(%address, "Sender listening");
        Ok(Self {
            shared,
            server: Some(server),
        })
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

    /// Removes an Item and aborts its in-flight downloads. Returns whether it was there.
    pub fn remove_item(&self, id: ItemId) -> bool {
        let removed = self.shared.live().remove(id);
        if removed {
            self.shared.publish();
        }
        removed
    }
}

impl Drop for SenderEngine {
    fn drop(&mut self) {
        self.shared.shutdown.cancel();
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}
