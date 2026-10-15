//! The Oxdrop domain: a Transfer of Items offered by the Sender, reachable by
//! whoever holds its Transfer Link. See `CONTEXT.md` for the vocabulary.

mod interfaces;
mod link;
mod names;
mod secret;
mod size;
mod transfer;

pub use interfaces::{Candidate, NetworkInterface, rank_interfaces};
pub use link::TransferLink;
pub use names::android_safe_name;
pub use secret::TransferSecret;
pub use size::format_size;
pub use transfer::{AddError, Added, Item, ItemId, Transfer};
