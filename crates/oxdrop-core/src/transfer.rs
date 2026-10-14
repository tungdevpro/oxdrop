use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

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

/// One file within a Transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    /// The Item Name: what both the Sender and the Receiver call this Item.
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
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
        let metadata = std::fs::metadata(path).map_err(|source| AddError::Unreadable {
            path: path.to_owned(),
            source,
        })?;
        if metadata.is_dir() {
            return Err(AddError::Folder {
                path: path.to_owned(),
            });
        }
        let file_name = match path.file_name() {
            Some(name) if metadata.is_file() => name.to_string_lossy(),
            _ => {
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
        });
        Ok(Added::New(id))
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
