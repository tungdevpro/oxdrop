//! Producing any byte range of a planned zip, reading files as it goes.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::body::Bytes;
use futures_util::Stream;
use oxdrop_core::{Part, ZipPlan};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// How much of a file is read at a time.
const CHUNK: usize = 256 * 1024;

/// A file as snapshotted: the key its CRC-32 is cached under.
type FileKey = (PathBuf, u64, SystemTime);

/// CRC-32s of files already read, per snapshot, so a resumed download needn't reread them.
#[derive(Default)]
pub(crate) struct CrcCache(Mutex<HashMap<FileKey, u32>>);

impl CrcCache {
    fn get(&self, key: &FileKey) -> Option<u32> {
        self.0
            .lock()
            .expect("CRC cache lock poisoned")
            .get(key)
            .copied()
    }

    fn insert(&self, key: FileKey, crc: u32) {
        self.0
            .lock()
            .expect("CRC cache lock poisoned")
            .insert(key, crc);
    }
}

fn key(plan: &ZipPlan, entry: usize) -> FileKey {
    let entry = &plan.entries()[entry];
    let file = entry.file.as_ref().expect("a file entry");
    (file.path.clone(), file.size, entry.modified)
}

/// The file changed (or vanished) since it was snapshotted.
fn changed() -> std::io::Error {
    std::io::Error::other("a file changed while being sent")
}

/// A CRC-32 needed for a descriptor or the central directory wasn't worked out first.
fn missing_crc() -> std::io::Error {
    std::io::Error::other("a CRC-32 the zip needs is unknown")
}

/// Whether the open file still matches the snapshot it is being sent as.
async fn unchanged(file: &tokio::fs::File, size: u64, modified: SystemTime) -> bool {
    file.metadata()
        .await
        .is_ok_and(|m| m.len() == size && m.modified().ok() == Some(modified))
}

/// Reads the whole file of `entry` to find its CRC-32, checking it is still as snapshotted.
async fn compute_crc(plan: &ZipPlan, entry: usize) -> std::io::Result<u32> {
    let modified = plan.entries()[entry].modified;
    let file_entry = plan.entries()[entry].file.as_ref().expect("a file entry");
    let mut file = tokio::fs::File::open(&file_entry.path).await?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = vec![0; CHUNK];
    let mut remaining = file_entry.size;
    while remaining > 0 {
        let want = remaining.min(CHUNK as u64) as usize;
        let read = file.read(&mut buffer[..want]).await?;
        if read == 0 {
            return Err(changed());
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    if !unchanged(&file, file_entry.size, modified).await {
        return Err(changed());
    }
    Ok(hasher.finalize())
}

/// Bytes `start..end` of the zip. CRC-32s this range needs for files that begin before
/// `start` are found first (from the cache, or by reading those files); the rest are
/// computed while streaming. If any file differs from its snapshot, the stream fails
/// before that file's last bytes go out, so the response can never complete.
pub(crate) async fn zip_range(
    plan: Arc<ZipPlan>,
    cache: Arc<CrcCache>,
    start: u64,
    end: u64,
) -> std::io::Result<impl Stream<Item = std::io::Result<Bytes>> + Send + Unpin + use<>> {
    let mut crcs = HashMap::new();
    for segment in plan.segments() {
        let Part::Data { entry } = segment.part else {
            continue;
        };
        if segment.len == 0 {
            crcs.insert(entry, 0); // the CRC-32 of nothing
            continue;
        }
        let begins_before_range = segment.offset < start;
        let crc_needed = end > segment.offset + segment.len;
        if let Some(crc) = cache.get(&key(&plan, entry)) {
            crcs.insert(entry, crc);
        } else if begins_before_range && crc_needed {
            let crc = compute_crc(&plan, entry).await?;
            cache.insert(key(&plan, entry), crc);
            crcs.insert(entry, crc);
        }
    }

    let (sender, receiver) = mpsc::channel(4);
    tokio::spawn(async move {
        if let Err(error) = produce(&plan, &cache, crcs, start, end, &sender).await {
            let _ = sender.send(Err(error)).await;
        }
    });
    Ok(ReceiverStream::new(receiver))
}

/// Sends bytes `start..end` of the zip to `out`, in order.
async fn produce(
    plan: &ZipPlan,
    cache: &CrcCache,
    mut crcs: HashMap<usize, u32>,
    start: u64,
    end: u64,
    out: &mpsc::Sender<std::io::Result<Bytes>>,
) -> std::io::Result<()> {
    // A closed channel means the Receiver went away: stop quietly.
    let send = |bytes: Bytes| async move { out.send(Ok(bytes)).await.map_err(|_| changed()) };
    for segment in plan.segments() {
        let segment_end = segment.offset + segment.len;
        if segment_end <= start || segment.offset >= end || segment.len == 0 {
            continue;
        }
        let from = start.saturating_sub(segment.offset);
        let to = end.min(segment_end) - segment.offset;
        match &segment.part {
            Part::Header(bytes) => {
                send(Bytes::copy_from_slice(&bytes[from as usize..to as usize])).await?;
            }
            Part::Descriptor { entry } => {
                let crc = *crcs.get(entry).ok_or_else(missing_crc)?;
                let bytes = plan.descriptor(*entry, crc);
                send(Bytes::from(bytes[from as usize..to as usize].to_vec())).await?;
            }
            Part::Tail => {
                let files = plan.entries().iter().enumerate();
                if files
                    .filter(|(_, e)| e.file.is_some())
                    .any(|(i, _)| !crcs.contains_key(&i))
                {
                    return Err(missing_crc());
                }
                let bytes = plan.tail(|entry| crcs[&entry]);
                send(Bytes::from(bytes[from as usize..to as usize].to_vec())).await?;
            }
            Part::Data { entry } => {
                let modified = plan.entries()[*entry].modified;
                let file_entry = plan.entries()[*entry].file.as_ref().expect("a file entry");
                let mut file = tokio::fs::File::open(&file_entry.path).await?;
                file.seek(SeekFrom::Start(from)).await?;
                // Only a file read from its first byte to its last yields its CRC-32.
                let mut hasher = (from == 0 && to == segment.len).then(crc32fast::Hasher::new);
                let mut position = from;
                while position < to {
                    let want = (to - position).min(CHUNK as u64) as usize;
                    let mut buffer = vec![0; want];
                    let read = file.read(&mut buffer).await?;
                    if read == 0 {
                        return Err(changed());
                    }
                    buffer.truncate(read);
                    position += read as u64;
                    if let Some(hasher) = &mut hasher {
                        hasher.update(&buffer);
                    }
                    if position == to && !unchanged(&file, file_entry.size, modified).await {
                        return Err(changed());
                    }
                    send(Bytes::from(buffer)).await?;
                }
                if let Some(hasher) = hasher {
                    let crc = hasher.finalize();
                    cache.insert(key(plan, *entry), crc);
                    crcs.insert(*entry, crc);
                }
            }
        }
    }
    Ok(())
}
