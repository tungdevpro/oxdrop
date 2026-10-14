//! The Oxdrop domain: a Transfer of Items offered by the Sender, reachable by
//! whoever holds its Transfer Link. See `CONTEXT.md` for the vocabulary.

mod link;
mod secret;
mod size;
mod transfer;

pub use link::TransferLink;
pub use secret::TransferSecret;
pub use size::format_size;
pub use transfer::{AddError, Item, ItemId, Transfer};
