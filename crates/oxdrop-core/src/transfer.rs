use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use crate::TransferSecret;
use crate::names::unique_name;

/// Identifies an Item within its Transfer. Opaque to the Receiver; stable while the Transfer is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemId(u64);

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ItemId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(ItemId)
    }
}

/// What adding a path did to the Transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    New(ItemId),
    /// The path was already an Item; nothing changed.
    AlreadyPresent(ItemId),
}

impl Added {
    pub fn id(self) -> ItemId {
        match self {
            Added::New(id) | Added::AlreadyPresent(id) => id,
        }
    }
}

/// Whether an Item's file still matches the snapshot taken when it was added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStatus {
    Available,
    /// The file was edited after it was added.
    Changed,
    /// The file was deleted or moved after it was added.
    Missing,
}

/// One file within a Transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    /// The Item Name: what both the Sender and the Receiver call this Item.
    pub name: String,
    pub path: PathBuf,
    /// The file's size when snapshotted; exactly what a download serves.
    pub size: u64,
    /// The file's modification time when snapshotted.
    pub modified: SystemTime,
    pub status: ItemStatus,
}

impl Item {
    /// A strong `ETag` naming this snapshot of the file, so a browser resuming a
    /// download never splices bytes from two different versions.
    pub fn etag(&self) -> String {
        let modified = self
            .modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("\"{:x}-{modified:x}\"", self.size)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AddError {
    #[error("{} can't be read: {source}", path.display())]
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} is a folder; folders can't be sent yet", path.display())]
    Folder { path: PathBuf },
    #[error("{} is not a regular file", path.display())]
    NotAFile { path: PathBuf },
}

/// The metadata of the regular file at `path`, or why it can't be an Item.
fn file_metadata(path: &Path) -> Result<std::fs::Metadata, AddError> {
    let metadata = std::fs::metadata(path).map_err(|source| AddError::Unreadable {
        path: path.to_owned(),
        source,
    })?;
    if metadata.is_dir() {
        return Err(AddError::Folder {
            path: path.to_owned(),
        });
    }
    if !metadata.is_file() {
        return Err(AddError::NotAFile {
            path: path.to_owned(),
        });
    }
    Ok(metadata)
}

/// The modification time to snapshot; filesystems that don't record one all read as the epoch.
fn modified(metadata: &std::fs::Metadata) -> SystemTime {
    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}

/// A set of Items the Sender offers to the Receiver, reachable with its secret.
#[derive(Debug)]
pub struct Transfer {
    secret: TransferSecret,
    items: Vec<Item>,
    next_id: u64,
}

impl Transfer {
    /// Opens a new, empty Transfer with a fresh secret.
    pub fn open() -> Self {
        Self {
            secret: TransferSecret::generate(),
            items: Vec::new(),
            next_id: 1,
        }
    }

    pub fn secret(&self) -> &TransferSecret {
        &self.secret
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Adds the file at `path` as a new Item with a unique Item Name.
    /// Adding a path that is already an Item changes nothing.
    pub fn add_path(&mut self, path: &Path) -> Result<Added, AddError> {
        let path = &std::path::absolute(path).map_err(|source| AddError::Unreadable {
            path: path.to_owned(),
            source,
        })?;
        if let Some(existing) = self.items.iter().find(|item| item.path == *path) {
            return Ok(Added::AlreadyPresent(existing.id));
        }
        let metadata = file_metadata(path)?;
        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy(),
            None => {
                return Err(AddError::NotAFile {
                    path: path.to_owned(),
                });
            }
        };
        let name = unique_name(&file_name, self.items.iter().map(|item| item.name.as_str()));
        let id = ItemId(self.next_id);
        self.next_id += 1;
        self.items.push(Item {
            id,
            name,
            path: path.to_owned(),
            size: metadata.len(),
            modified: modified(&metadata),
            status: ItemStatus::Available,
        });
        Ok(Added::New(id))
    }

    /// Snapshots an Item's file again, so its current contents are what's offered.
    /// Returns whether there was such an Item; if its file can't be read it becomes Missing.
    pub fn refresh(&mut self, id: ItemId) -> Result<bool, AddError> {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return Ok(false);
        };
        match file_metadata(&item.path) {
            Ok(metadata) => {
                item.size = metadata.len();
                item.modified = modified(&metadata);
                item.status = ItemStatus::Available;
                Ok(true)
            }
            Err(error) => {
                item.status = ItemStatus::Missing;
                Err(error)
            }
        }
    }

    /// Rechecks an Item's file against its snapshot and records the result.
    /// Returns the Item's status, or `None` if there's no such Item.
    pub fn check(&mut self, id: ItemId) -> Option<ItemStatus> {
        let item = self.items.iter_mut().find(|item| item.id == id)?;
        item.status = match std::fs::metadata(&item.path) {
            Ok(metadata) if !metadata.is_file() => ItemStatus::Missing,
            Ok(metadata) if metadata.len() == item.size && modified(&metadata) == item.modified => {
                ItemStatus::Available
            }
            Ok(_) => ItemStatus::Changed,
            Err(_) => ItemStatus::Missing,
        };
        Some(item.status)
    }

    /// Removes an Item, freeing its Item Name. Returns whether it was there.
    pub fn remove(&mut self, id: ItemId) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id != id);
        self.items.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_a_file_creates_an_item_named_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        let mut transfer = Transfer::open();

        let id = transfer.add_path(&path).unwrap().id();

        let item = transfer.item(id).unwrap();
        assert_eq!(item.name, "notes.txt");
        assert_eq!(item.size, 5);
        assert_eq!(item.path, path);
    }

    #[test]
    fn item_ids_are_distinct_and_round_trip_through_text() {
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = Transfer::open();
        let ids: Vec<ItemId> = ["a", "b"]
            .iter()
            .map(|n| {
                let p = dir.path().join(n);
                std::fs::write(&p, b"x").unwrap();
                transfer.add_path(&p).unwrap().id()
            })
            .collect();

        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[0].to_string().parse::<ItemId>().unwrap(), ids[0]);
    }

    #[test]
    fn folders_and_missing_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = Transfer::open();

        assert!(matches!(
            transfer.add_path(dir.path()),
            Err(AddError::Folder { .. })
        ));
        assert!(matches!(
            transfer.add_path(&dir.path().join("nope")),
            Err(AddError::Unreadable { .. })
        ));
        assert!(transfer.items().is_empty());
    }
}
