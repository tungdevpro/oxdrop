//! The Sender engine: owns the open Transfer and serves it to the Receiver over HTTP.
//! The desktop app drives Oxdrop only through [`SenderEngine`].

mod engine;
mod http;
mod network;
mod page;
mod range;

pub use engine::{BindAddress, EngineConfig, SenderEngine, StartError, TransferState};
pub use network::interface_candidates;
pub use oxdrop_core::Candidate;
