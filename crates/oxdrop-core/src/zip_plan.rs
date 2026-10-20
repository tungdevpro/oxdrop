//! The layout of a stored (uncompressed) zip, worked out before any byte is sent.
//!
//! Every entry is stored as is, so each header, file and offset has a size known from
//! the snapshot alone. That gives an exact `Content-Length` up front and lets any byte
//! range be produced on its own, which is what lets browsers show progress and resume.
//! CRC-32s aren't known until a file is read, so files use data descriptors (general
//! purpose bit 3) and the central directory, written last, repeats them.
//!
//! Format: PKWARE APPNOTE 6.3.10, with Zip64 records whenever a size or offset doesn't
//! fit in 32 bits.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{EntryKind, FolderSnapshot, Item, ItemKind, android_safe_name};

/// One entry of the zip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    /// The name inside the zip, `/`-separated; directories end with `/`.
    pub name: String,
    pub modified: SystemTime,
    /// For a file, where its bytes come from and how many there are.
    pub file: Option<ZipFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipFile {
    pub path: PathBuf,
    pub size: u64,
}

/// What occupies one stretch of the zip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// A local file header, complete as is.
    Header(Vec<u8>),
    /// The bytes of entry `entry`'s file.
    Data { entry: usize },
    /// Entry `entry`'s data descriptor, which needs its CRC-32.
    Descriptor { entry: usize },
    /// The central directory and end records, which need every file's CRC-32.
    Tail,
}

/// A stretch of the zip: `len` bytes starting at `offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub offset: u64,
    pub len: u64,
    pub part: Part,
}

/// A zip laid out in full: its entries, the segments they occupy in order, and its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipPlan {
    entries: Vec<ZipEntry>,
    /// Where each entry's local header starts.
    header_offsets: Vec<u64>,
    segments: Vec<Segment>,
    central_directory_offset: u64,
    len: u64,
}

const LOCAL_HEADER: u32 = 0x0403_4b50;
const DATA_DESCRIPTOR: u32 = 0x0807_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const ZIP64_END: u32 = 0x0606_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const END: u32 = 0x0605_4b50;

/// Bit 3: sizes and CRC follow the data. Bit 11: the name is UTF-8.
const FLAGS_FILE: u16 = 1 << 3 | 1 << 11;
const FLAGS_DIRECTORY: u16 = 1 << 11;
/// Version 2.0 (directories, data descriptors), 4.5 (Zip64).
const VERSION_DEFAULT: u16 = 20;
const VERSION_ZIP64: u16 = 45;
/// Made by Unix (so external attributes carry permissions), spec version 4.5.
const MADE_BY: u16 = 3 << 8 | VERSION_ZIP64;
/// Marks a 32-bit (or 16-bit) field whose real value is in a Zip64 record.
const OVERFLOW_32: u32 = u32::MAX;
const OVERFLOW_16: u16 = u16::MAX;

/// The extended timestamp extra field (0x5455) with just the modification time.
const TIMESTAMP_EXTRA_LEN: u64 = 9;

impl ZipPlan {
    /// The zip of a folder Item named `name` whose files are under `root`: every entry
    /// sits under a top-level `name/` directory, with names made safe for Android.
    pub fn for_folder(
        name: &str,
        root: &Path,
        modified: SystemTime,
        snapshot: &FolderSnapshot,
    ) -> Self {
        Self::new(folder_entries(name, root, modified, snapshot))
    }

    /// The zip of a whole Transfer: each Item at the root under its Item Name, a file
    /// Item as a file and a folder Item as a directory, with names made safe for Android.
    pub fn for_items(items: &[Item]) -> Self {
        let mut entries = Vec::new();
        for item in items {
            match &item.kind {
                ItemKind::File => entries.push(ZipEntry {
                    name: android_safe_name(&item.name),
                    modified: item.modified,
                    file: Some(ZipFile {
                        path: item.path.clone(),
                        size: item.size,
                    }),
                }),
                ItemKind::Folder(snapshot) => entries.extend(folder_entries(
                    &item.name,
                    &item.path,
                    item.modified,
                    snapshot,
                )),
            }
        }
        Self::new(entries)
    }

    pub fn new(entries: Vec<ZipEntry>) -> Self {
        let mut segments = Vec::new();
        let mut header_offsets = Vec::with_capacity(entries.len());
        let mut offset = 0u64;
        let push = |segments: &mut Vec<Segment>, offset: &mut u64, len: u64, part: Part| {
            segments.push(Segment {
                offset: *offset,
                len,
                part,
            });
            *offset += len;
        };
        for (index, entry) in entries.iter().enumerate() {
            header_offsets.push(offset);
            let header = local_header(entry);
            push(
                &mut segments,
                &mut offset,
                header.len() as u64,
                Part::Header(header),
            );
            if let Some(file) = &entry.file {
                push(
                    &mut segments,
                    &mut offset,
                    file.size,
                    Part::Data { entry: index },
                );
                let descriptor_len = if is_zip64_size(file.size) { 24 } else { 16 };
                push(
                    &mut segments,
                    &mut offset,
                    descriptor_len,
                    Part::Descriptor { entry: index },
                );
            }
        }
        let central_directory_offset = offset;
        let mut plan = Self {
            entries,
            header_offsets,
            segments,
            central_directory_offset,
            len: 0,
        };
        let tail_len = plan.tail(|_| 0).len() as u64;
        plan.segments.push(Segment {
            offset: central_directory_offset,
            len: tail_len,
            part: Part::Tail,
        });
        plan.len = central_directory_offset + tail_len;
        plan
    }

    /// The zip's exact length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Every stretch of the zip, in order and without gaps.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Entry `entry`'s data descriptor, given its file's CRC-32.
    pub fn descriptor(&self, entry: usize, crc: u32) -> Vec<u8> {
        let size = self.entries[entry]
            .file
            .as_ref()
            .map_or(0, |file| file.size);
        let mut out = Vec::with_capacity(24);
        put32(&mut out, DATA_DESCRIPTOR);
        put32(&mut out, crc);
        if is_zip64_size(size) {
            put64(&mut out, size);
            put64(&mut out, size);
        } else {
            put32(&mut out, size as u32);
            put32(&mut out, size as u32);
        }
        out
    }

    /// The central directory and end records, given each file entry's CRC-32
    /// (`crc` is only asked about entries that are files).
    pub fn tail(&self, crc: impl Fn(usize) -> u32) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let crc = if entry.file.is_some() { crc(index) } else { 0 };
            central_header(&mut out, entry, crc, self.header_offsets[index]);
        }
        let directory_len = out.len() as u64;
        let directory_offset = self.central_directory_offset;
        let count = self.entries.len() as u64;
        if count >= u64::from(OVERFLOW_16)
            || directory_offset >= u64::from(OVERFLOW_32)
            || directory_len >= u64::from(OVERFLOW_32)
        {
            let zip64_end_offset = directory_offset + directory_len;
            put32(&mut out, ZIP64_END);
            put64(&mut out, 44); // size of the rest of this record
            put16(&mut out, MADE_BY);
            put16(&mut out, VERSION_ZIP64);
            put32(&mut out, 0); // this disk
            put32(&mut out, 0); // disk with the central directory
            put64(&mut out, count);
            put64(&mut out, count);
            put64(&mut out, directory_len);
            put64(&mut out, directory_offset);
            put32(&mut out, ZIP64_LOCATOR);
            put32(&mut out, 0); // disk with the Zip64 end record
            put64(&mut out, zip64_end_offset);
            put32(&mut out, 1); // total disks
        }
        put32(&mut out, END);
        put16(&mut out, 0); // this disk
        put16(&mut out, 0); // disk with the central directory
        let count16 = count.min(u64::from(OVERFLOW_16)) as u16;
        put16(&mut out, count16);
        put16(&mut out, count16);
        put32(&mut out, directory_len.min(u64::from(OVERFLOW_32)) as u32);
        put32(
            &mut out,
            directory_offset.min(u64::from(OVERFLOW_32)) as u32,
        );
        put16(&mut out, 0); // comment length
        out
    }
}

/// The entries of a folder named `name` whose files are under `root`, all under a
/// top-level `name/` directory.
fn folder_entries(
    name: &str,
    root: &Path,
    modified: SystemTime,
    snapshot: &FolderSnapshot,
) -> Vec<ZipEntry> {
    let top = android_safe_name(name);
    let mut entries = vec![ZipEntry {
        name: format!("{top}/"),
        modified,
        file: None,
    }];
    for entry in &snapshot.entries {
        let inner: Vec<String> = entry
            .relative
            .components()
            .map(|part| android_safe_name(&part.as_os_str().to_string_lossy()))
            .collect();
        let path = format!("{top}/{}", inner.join("/"));
        entries.push(match entry.kind {
            EntryKind::Directory => ZipEntry {
                name: format!("{path}/"),
                modified: entry.modified,
                file: None,
            },
            EntryKind::File { size } => ZipEntry {
                name: path,
                modified: entry.modified,
                file: Some(ZipFile {
                    path: root.join(&entry.relative),
                    size,
                }),
            },
        });
    }
    entries
}

fn is_zip64_size(size: u64) -> bool {
    size >= u64::from(OVERFLOW_32)
}

fn local_header(entry: &ZipEntry) -> Vec<u8> {
    let size = entry.file.as_ref().map(|file| file.size);
    let zip64 = size.is_some_and(is_zip64_size);
    let (date, time) = dos_date_time(entry.modified);
    let extra_len = TIMESTAMP_EXTRA_LEN + if zip64 { 20 } else { 0 };
    let mut out = Vec::with_capacity(30 + entry.name.len() + extra_len as usize);
    put32(&mut out, LOCAL_HEADER);
    put16(
        &mut out,
        if zip64 {
            VERSION_ZIP64
        } else {
            VERSION_DEFAULT
        },
    );
    put16(
        &mut out,
        if size.is_some() {
            FLAGS_FILE
        } else {
            FLAGS_DIRECTORY
        },
    );
    put16(&mut out, 0); // stored
    put16(&mut out, time);
    put16(&mut out, date);
    put32(&mut out, 0); // CRC-32: in the data descriptor
    // Sizes are in the data descriptor too; Zip64 marks them as living in its extra field.
    let sizes = if zip64 { OVERFLOW_32 } else { 0 };
    put32(&mut out, sizes);
    put32(&mut out, sizes);
    put16(&mut out, entry.name.len() as u16);
    put16(&mut out, extra_len as u16);
    out.extend_from_slice(entry.name.as_bytes());
    if zip64 {
        put16(&mut out, 0x0001);
        put16(&mut out, 16);
        put64(&mut out, 0);
        put64(&mut out, 0);
    }
    timestamp_extra(&mut out, entry.modified);
    out
}

fn central_header(out: &mut Vec<u8>, entry: &ZipEntry, crc: u32, header_offset: u64) {
    let size = entry.file.as_ref().map(|file| file.size);
    let size_overflows = size.is_some_and(is_zip64_size);
    let offset_overflows = header_offset >= u64::from(OVERFLOW_32);
    let zip64_fields = if size_overflows { 2 } else { 0 } + usize::from(offset_overflows);
    let zip64_extra_len = if zip64_fields > 0 {
        4 + 8 * zip64_fields
    } else {
        0
    };
    let (date, time) = dos_date_time(entry.modified);
    put32(out, CENTRAL_HEADER);
    put16(out, MADE_BY);
    put16(
        out,
        if zip64_fields > 0 {
            VERSION_ZIP64
        } else {
            VERSION_DEFAULT
        },
    );
    put16(
        out,
        if size.is_some() {
            FLAGS_FILE
        } else {
            FLAGS_DIRECTORY
        },
    );
    put16(out, 0); // stored
    put16(out, time);
    put16(out, date);
    put32(out, crc);
    let size32 = if size_overflows {
        OVERFLOW_32
    } else {
        size.unwrap_or(0) as u32
    };
    put32(out, size32);
    put32(out, size32);
    put16(out, entry.name.len() as u16);
    put16(out, (zip64_extra_len as u64 + TIMESTAMP_EXTRA_LEN) as u16);
    put16(out, 0); // comment length
    put16(out, 0); // disk number
    put16(out, 0); // internal attributes
    let external = match size {
        None => 0o040_755 << 16 | 0x10, // Unix directory, and the MS-DOS directory bit
        Some(_) => 0o100_644 << 16,
    };
    put32(out, external);
    put32(
        out,
        if offset_overflows {
            OVERFLOW_32
        } else {
            header_offset as u32
        },
    );
    out.extend_from_slice(entry.name.as_bytes());
    if zip64_fields > 0 {
        put16(out, 0x0001);
        put16(out, (8 * zip64_fields) as u16);
        if let Some(size) = size.filter(|_| size_overflows) {
            put64(out, size);
            put64(out, size);
        }
        if offset_overflows {
            put64(out, header_offset);
        }
    }
    timestamp_extra(out, entry.modified);
}

/// Extended timestamp extra field (0x5455): the modification time in UTC seconds.
fn timestamp_extra(out: &mut Vec<u8>, modified: SystemTime) {
    let seconds = match modified.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => since.as_secs().min(u64::from(u32::MAX)) as u32,
        Err(_) => 0,
    };
    put16(out, 0x5455);
    put16(out, 5);
    out.push(1); // modification time present
    put32(out, seconds);
}

/// MS-DOS date and time fields, in UTC and clamped to what they can hold (1980–2107).
fn dos_date_time(modified: SystemTime) -> (u16, u16) {
    let seconds = match modified.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => since.as_secs(),
        Err(_) => 0,
    };
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    if year < 1980 {
        return (1 << 5 | 1, 0); // 1980-01-01 00:00:00
    }
    if year > 2107 {
        return (127 << 9 | 12 << 5 | 31, 23 << 11 | 59 << 5 | 29);
    }
    let of_day = seconds % 86_400;
    let (hour, minute, second) = (of_day / 3600, of_day / 60 % 60, of_day % 60);
    let date = ((year - 1980) as u16) << 9 | (month as u16) << 5 | day as u16;
    let time = (hour as u16) << 11 | (minute as u16) << 5 | (second / 2) as u16;
    (date, time)
}

/// The (year, month, day) of a day counted from 1970-01-01, proleptic Gregorian.
/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}
