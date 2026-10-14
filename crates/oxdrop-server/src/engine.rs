use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use oxdrop_core::{AddError, Item, ItemId, Transfer, TransferLink};
use tokio::sync::oneshot;

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

/// What the Sender shows: the open Transfer's link and Items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferState {
    pub link: TransferLink,
    pub items: Vec<Item>,
}

/// State shared between the engine handle and the HTTP server.
pub(crate) struct Shared {
    transfer: Mutex<Transfer>,
}

impl Shared {
    pub(crate) fn transfer(&self) -> MutexGuard<'_, Transfer> {
        self.transfer.lock().expect("transfer lock poisoned")
    }
}

/// The running Sender: one open Transfer served over HTTP on a local address.
/// Dropping it stops the server.
pub struct SenderEngine {
    shared: Arc<Shared>,
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
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

        let shared = Arc::new(Shared {
            transfer: Mutex::new(Transfer::open()),
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("oxdrop-server")
            .enable_all()
            .build()
            .map_err(StartError::Runtime)?;
        let (shutdown, shutdown_rx) = oneshot::channel();
        let router = http::router(shared.clone());
        let server = std::thread::Builder::new()
            .name("oxdrop-server".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .expect("listener is non-blocking");
                    let serve = axum::serve(listener, router).with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    });
                    if let Err(error) = serve.await {
                        tracing::error!(%error, "server stopped");
                    }
                });
            })
            .map_err(StartError::Runtime)?;

        tracing::info!(%address, "Sender listening");
        Ok(Self {
            shared,
            address,
            shutdown: Some(shutdown),
            server: Some(server),
        })
    }

    pub fn state(&self) -> TransferState {
        let transfer = self.shared.transfer();
        TransferState {
            link: TransferLink {
                address: self.address,
                secret: transfer.secret().clone(),
            },
            items: transfer.items().to_vec(),
        }
    }

    /// Adds the file at `path` to the open Transfer as an Item.
    pub fn add_path(&self, path: &Path) -> Result<ItemId, AddError> {
        self.shared.transfer().add_path(path)
    }
}

impl Drop for SenderEngine {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}
