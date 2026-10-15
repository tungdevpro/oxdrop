//! Walking a folder Item: what goes into its zip, and what is left out.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One file or directory inside a folder Item, as it was when snapshotted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderEntry {
    /// Path relative to the folder.
    pub relative: PathBuf,
    pub kind: EntryKind,
    pub modified: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File { size: u64 },
    Directory,
}

/// A file or directory inside a folder Item that won't be sent, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub relative: PathBuf,
    pub reason: String,
}

/// Everything a folder Item will send, in zip order, plus what it leaves out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderSnapshot {
    pub entries: Vec<FolderEntry>,
    pub skipped: Vec<Skipped>,
}

/// Files and folders the OS creates for itself, never worth sending. Lowercase.
const JUNK: [&str; 8] = [
    ".ds_store",
    "thumbs.db",
    "desktop.ini",
    "$recycle.bin",
    ".spotlight-v100",
    ".trashes",
    ".fseventsd",
    ".temporaryitems",
];

/// Whether a file or folder named `name` is OS junk. `._*` files are macOS
/// AppleDouble companions holding another file's metadata.
fn is_junk(name: &str) -> bool {
    name.starts_with("._") || JUNK.contains(&name.to_lowercase().as_str())
}

/// How a macOS alias file starts: bookmark data, which only Finder can follow.
const ALIAS_MAGIC: &[u8] = b"book\0\0\0\0mark";

/// Whether the file at `path` is a macOS alias.
fn is_alias(path: &Path) -> std::io::Result<bool> {
    let mut start = [0u8; ALIAS_MAGIC.len()];
    let read = std::fs::File::open(path)?.read(&mut start)?;
    Ok(start[..read] == *ALIAS_MAGIC)
}

/// Walks the folder at `root`, depth first with names in order. Fails only if
/// `root` itself can't be read; anything inside that can't be read is skipped.
pub fn walk_folder(root: &Path) -> std::io::Result<FolderSnapshot> {
    let mut snapshot = FolderSnapshot::default();
    let children = read_children(root)?;
    walk(root, Path::new(""), children, &mut snapshot);
    Ok(snapshot)
}

/// The names in the directory at `path`, sorted.
fn read_children(path: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    let mut children: Vec<_> = std::fs::read_dir(path)?
        .map(|child| child.map(|child| child.file_name()))
        .collect::<Result<_, _>>()?;
    children.sort();
    Ok(children)
}

fn walk(
    root: &Path,
    relative: &Path,
    children: Vec<std::ffi::OsString>,
    snapshot: &mut FolderSnapshot,
) {
    for name in children {
        if is_junk(&name.to_string_lossy()) {
            continue;
        }
        let relative = relative.join(&name);
        let path = root.join(&relative);
        let skip = |snapshot: &mut FolderSnapshot, reason: String| {
            snapshot.skipped.push(Skipped {
                relative: relative.clone(),
                reason,
            });
        };
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                skip(snapshot, format!("can't be read: {error}"));
                continue;
            }
        };
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        // Links (and Windows junctions) could point outside the folder or back into it.
        if metadata.file_type().is_symlink() {
            skip(snapshot, "is a link".to_owned());
        } else if metadata.is_dir() {
            // Kept even if its contents can't be read, so the structure survives.
            snapshot.entries.push(FolderEntry {
                relative: relative.clone(),
                kind: EntryKind::Directory,
                modified,
            });
            match read_children(&path) {
                Ok(children) => walk(root, &relative, children, snapshot),
                Err(error) => skip(snapshot, format!("can't be read: {error}")),
            }
        } else {
            match is_alias(&path) {
                Err(error) => skip(snapshot, format!("can't be read: {error}")),
                Ok(true) => skip(snapshot, "is an alias".to_owned()),
                Ok(false) => snapshot.entries.push(FolderEntry {
                    relative,
                    kind: EntryKind::File {
                        size: metadata.len(),
                    },
                    modified,
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, contents: &[u8]) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Each entry as `path` for directories (with a trailing `/`) or `path (size)` for files.
    fn listed(snapshot: &FolderSnapshot) -> Vec<String> {
        snapshot
            .entries
            .iter()
            .map(|entry| {
                let path = entry.relative.to_string_lossy().replace('\\', "/");
                match entry.kind {
                    EntryKind::Directory => format!("{path}/"),
                    EntryKind::File { size } => format!("{path} ({size})"),
                }
            })
            .collect()
    }

    #[test]
    fn walks_nested_files_hidden_files_and_empty_directories_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "b.txt", b"bb");
        write(root, "a/z.txt", b"zzz");
        write(root, "a/deeper/y.txt", b"y");
        write(root, ".env", b"SECRET=1");
        std::fs::create_dir_all(root.join("empty")).unwrap();

        let snapshot = walk_folder(root).unwrap();

        assert_eq!(
            listed(&snapshot),
            [
                ".env (8)",
                "a/",
                "a/deeper/",
                "a/deeper/y.txt (1)",
                "a/z.txt (3)",
                "b.txt (2)",
                "empty/",
            ]
        );
        assert!(snapshot.skipped.is_empty());
    }

    #[test]
    fn os_junk_is_left_out_silently_at_any_depth() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "photo.jpg", b"p");
        write(root, ".DS_Store", b"x");
        write(root, "Thumbs.db", b"x");
        write(root, "Desktop.ini", b"x");
        write(root, "._photo.jpg", b"x");
        write(root, "$RECYCLE.BIN/old.txt", b"x");
        write(root, ".Spotlight-V100/store.db", b"x");
        write(root, ".Trashes/501/old.txt", b"x");
        write(root, ".fseventsd/log", b"x");
        write(root, "trip/.DS_Store", b"x");
        write(root, "trip/._beach.jpg", b"x");
        write(root, "trip/beach.jpg", b"b");
        write(root, "trip/.hidden", b"h");

        let snapshot = walk_folder(root).unwrap();

        assert_eq!(
            listed(&snapshot),
            [
                "photo.jpg (1)",
                "trip/",
                "trip/.hidden (1)",
                "trip/beach.jpg (1)"
            ]
        );
        assert!(snapshot.skipped.is_empty());
    }

    fn skipped(snapshot: &FolderSnapshot) -> Vec<String> {
        snapshot
            .skipped
            .iter()
            .map(|s| s.relative.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    /// The start of a macOS alias file: bookmark data.
    const ALIAS_HEADER: &[u8] = b"book\0\0\0\0mark\0\0\0\0";

    #[test]
    fn macos_aliases_are_skipped_without_following_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "notes.txt", b"n");
        write(root, "Report alias", &[ALIAS_HEADER, &[0u8; 64]].concat());
        // Not an alias: just happens to start with "book".
        write(root, "bookmarks.txt", b"book list");

        let snapshot = walk_folder(root).unwrap();

        assert_eq!(listed(&snapshot), ["bookmarks.txt (9)", "notes.txt (1)"]);
        assert_eq!(skipped(&snapshot), ["Report alias"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped_without_following_them() {
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "private.txt", b"secret");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "notes.txt", b"n");
        std::os::unix::fs::symlink(outside.path().join("private.txt"), root.join("escape"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("elsewhere")).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        // Would loop forever if followed.
        std::os::unix::fs::symlink(root, root.join("sub/loop")).unwrap();

        let snapshot = walk_folder(root).unwrap();

        assert_eq!(listed(&snapshot), ["notes.txt (1)", "sub/"]);
        assert_eq!(skipped(&snapshot), ["elsewhere", "escape", "sub/loop"]);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_and_folders_are_skipped_with_a_reason() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "notes.txt", b"n");
        write(root, "locked.txt", b"l");
        write(root, "vault/inside.txt", b"i");
        let lock = |relative: &str, locked: bool| {
            let mode = if locked { 0o000 } else { 0o755 };
            std::fs::set_permissions(root.join(relative), std::fs::Permissions::from_mode(mode))
                .unwrap();
        };
        lock("locked.txt", true);
        lock("vault", true);

        let snapshot = walk_folder(root);

        lock("locked.txt", false);
        lock("vault", false);
        let snapshot = snapshot.unwrap();
        assert_eq!(listed(&snapshot), ["notes.txt (1)", "vault/"]);
        assert_eq!(skipped(&snapshot), ["locked.txt", "vault"]);
        assert!(
            snapshot.skipped.iter().all(|s| !s.reason.is_empty()),
            "{:?}",
            snapshot.skipped
        );
    }
}
