//! The Oxdrop domain: a Transfer of Items offered by the Sender, reachable by
//! whoever holds its Transfer Link. See `CONTEXT.md` for the vocabulary.

mod folder;
mod idle;
mod interfaces;
mod link;
mod names;
mod progress;
mod secret;
mod size;
mod transfer;
mod zip_plan;

pub use folder::{EntryKind, FolderEntry, FolderSnapshot, Skipped, walk_folder};
pub use idle::{IdleState, IdleTimer};
pub use interfaces::{Candidate, NetworkInterface, rank_interfaces};
pub use link::TransferLink;
pub use names::android_safe_name;
pub use progress::{DownloadTracker, Progress};
pub use secret::TransferSecret;
pub use size::format_size;
pub use transfer::{AddError, Added, Item, ItemId, ItemKind, ItemStatus, Transfer};
pub use zip_plan::{Part, Segment, ZipEntry, ZipFile, ZipPlan};
