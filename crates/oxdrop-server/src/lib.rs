//! The Sender engine: owns the open Transfer and serves it to the Receiver over HTTP.
//! The desktop app drives Oxdrop only through [`SenderEngine`].

mod engine;
mod http;
mod network;
mod page;

pub use engine::{BindAddress, EngineConfig, SenderEngine, StartError, TransferState};
