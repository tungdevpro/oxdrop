use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::TransferSecret;

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

    /// Adds the file at `path` as a new Item.
    pub fn add_path(&mut self, path: &Path) -> Result<ItemId, AddError> {
        let metadata = std::fs::metadata(path).map_err(|source| AddError::Unreadable {
            path: path.to_owned(),
            source,
        })?;
        if metadata.is_dir() {
            return Err(AddError::Folder {
                path: path.to_owned(),
            });
        }
        let name = match path.file_name() {
            Some(name) if metadata.is_file() => name.to_string_lossy().into_owned(),
            _ => {
                return Err(AddError::NotAFile {
                    path: path.to_owned(),
                });
            }
        };
        let id = ItemId(self.next_id);
        self.next_id += 1;
        self.items.push(Item {
            id,
            name,
            path: path.to_owned(),
            size: metadata.len(),
        });
        Ok(id)
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

        let id = transfer.add_path(&path).unwrap();

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
                transfer.add_path(&p).unwrap()
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
