//! The order of the listing (see [`Sort`]), and what is on disk that it can be sorted by

use crate::{Entry, Id, Koil, Settings, is_hidden};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

/// The order of [`Koil::listing`] (see [`Koil::compare`]): the dirs with IDs, the files with
/// IDs, then the new entries, each sorted by [`Sort::by`], and then by name
/// Without [`Sort::dirs_first`], the dirs and files with IDs are sorted together
/// New entries are not on disk, so a [`SortBy`] that [`reads_metadata`](SortBy::reads_metadata)
/// sorts them only by name
/// `reverse` turns the order within each of those groups around, but not the groups
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub by: SortBy,
    /// The other way round: from Z to A, smallest first, oldest first
    pub reverse: bool,
    /// Dirs before files (the default), else sorted together, like `1/`, `2.txt`, `3/`
    /// By [`SortBy::Size`] or [`SortBy::Disk`], a dir's size is not known (only how many
    /// entries it has), so then they go after the files
    pub dirs_first: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            by: SortBy::default(),
            reverse: false,
            dirs_first: true,
        }
    }
}

/// What [`Sort`] sorts by, each in the order people usually want first
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    /// By name, from A to Z, as their bytes compare (so `B` before `a`, and `a10` before `a2`)
    #[default]
    Name,
    /// By name, from A to Z, as people sort names: ignoring case, with numbers by their value
    /// (`a2` before `a10`)
    Natural,
    /// By extension (after the last `.`, ignoring case), names without one first
    Extension,
    /// Biggest first: a file by its size, a dir by how many entries it has
    Size,
    /// Biggest first by the space it takes on disk (see [`disk_size`]): a file by its own, a dir
    /// by how many entries it has, as [`SortBy::Size`]
    /// On Windows it is much slower to read, as every file is opened
    Disk,
    /// Last modified first
    Modified,
    /// Last created first, on filesystems that keep it
    Created,
    /// Last accessed first, as the filesystem keeps it (many only now and then)
    Accessed,
}

impl SortBy {
    /// Whether it sorts by something [`Koil::metadata`] has to read from disk
    pub fn reads_metadata(self) -> bool {
        !matches!(self, SortBy::Name | SortBy::Natural | SortBy::Extension)
    }

    /// How `a` (with `a_meta`) and `b` (with `b_meta`) compare by this alone, in its own
    /// direction (biggest or newest first), what is not known last
    fn compare(
        self,
        a: &Entry,
        a_meta: Option<&Metadata>,
        b: &Entry,
        b_meta: Option<&Metadata>,
    ) -> std::cmp::Ordering {
        let key = |meta: Option<&Metadata>| {
            let meta = meta?;
            match self {
                SortBy::Size | SortBy::Disk if meta.is_dir => meta.entries.map(u128::from),
                SortBy::Size => Some(u128::from(meta.size)),
                SortBy::Disk => meta.disk_size.map(u128::from),
                SortBy::Modified => since_epoch(meta.modified),
                SortBy::Created => since_epoch(meta.created),
                SortBy::Accessed => since_epoch(meta.accessed),
                SortBy::Name | SortBy::Natural | SortBy::Extension => None,
            }
        };
        let extension = |e: &Entry| {
            let extension = e.name.extension().unwrap_or_default();
            extension.to_string_lossy().to_lowercase()
        };
        match self {
            SortBy::Name => a.name.cmp(&b.name),
            SortBy::Natural => natural_order(&a.name.to_string_lossy(), &b.name.to_string_lossy()),
            SortBy::Extension => extension(a).cmp(&extension(b)),
            // the bigger or newer first, and `None` last
            _ => key(b_meta).cmp(&key(a_meta)),
        }
    }
}

/// `time` as a number that sorts like it, what is before 1970 as 0
fn since_epoch(time: Option<SystemTime>) -> Option<u128> {
    let since = time?.duration_since(SystemTime::UNIX_EPOCH);
    Some(since.map_or(0, |d| d.as_nanos()))
}

/// `a` against `b` as people sort names: ignoring case, and with numbers by their value, so
/// `a2` comes before `a10` (and `a02` is the same as `a2`)
pub(crate) fn natural_order(a: &str, b: &str) -> std::cmp::Ordering {
    // the digits of the number `chars` start with, without its leading zeros
    fn number(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
        let mut digits = String::new();
        while let Some(d) = chars.next_if(char::is_ascii_digit) {
            digits.push(d);
        }
        digits.trim_start_matches('0').to_string()
    }
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        let order = match (a.peek(), b.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (n, m) = (number(&mut a), number(&mut b));
                // a longer number is bigger
                n.len().cmp(&m.len()).then_with(|| n.cmp(&m))
            }
            (Some(&x), Some(&y)) => {
                a.next();
                b.next();
                x.to_lowercase().cmp(y.to_lowercase())
            }
        };
        if order.is_ne() {
            return order;
        }
    }
}

/// What is on disk at an entry's path (of a link itself, for a link), which the listing can be
/// sorted by (see [`Koil::metadata`])
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Like [`Path::is_dir`], so a link to a dir is one, as the listing shows it
    pub is_dir: bool,
    /// The size in bytes
    pub size: u64,
    /// How many entries a dir has (without hidden ones, unless [`Settings::show_hidden`] is on),
    /// only counted when sorting by [`SortBy::Size`] or [`SortBy::Disk`]
    pub entries: Option<u64>,
    /// The space it takes on disk (see [`disk_size`]), only read when sorting by
    /// [`SortBy::Disk`]
    pub disk_size: Option<u64>,
    /// `None` where the filesystem does not keep it
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

impl Metadata {
    /// What `meta` (of `path`, the link itself for a link) says, how many entries a dir has
    /// if `settings` sort by size (or size on disk), and the space it takes on disk if they
    /// sort by that
    pub(crate) fn new(path: &Path, meta: &fs::Metadata, settings: &Settings) -> Metadata {
        let is_dir = meta.is_dir() || meta.file_type().is_symlink() && path.is_dir();
        let shown = |name: &OsStr| settings.show_hidden || !is_hidden(name);
        let by = settings.sort.by;
        let entries = (is_dir && matches!(by, SortBy::Size | SortBy::Disk))
            .then(|| fs::read_dir(path).ok())
            .flatten()
            .map(|entries| entries.flatten().filter(|e| shown(&e.file_name())).count() as u64);
        Metadata {
            is_dir,
            size: meta.len(),
            entries,
            disk_size: (by == SortBy::Disk)
                .then(|| disk_size(path, meta))
                .flatten(),
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            accessed: meta.accessed().ok(),
        }
    }

    /// What is at `path` on disk now
    fn read(path: &Path, settings: &Settings) -> Option<Metadata> {
        let meta = path.symlink_metadata().ok()?;
        Some(Metadata::new(path, &meta, settings))
    }
}

/// The space that what is at `path` (whose metadata, of the link itself for a link, is `meta`)
/// takes on disk: the blocks it has, so less than its size for a sparse or compressed file, and
/// a whole block for a small one (or nothing, where the filesystem keeps it with its name)
/// `None` if it can not be read
/// On Windows the file is opened for it (`meta` from a directory walk does not have it), which
/// is much slower than reading its size
#[cfg(unix)]
pub fn disk_size(_path: &Path, meta: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    // in blocks of 512 bytes, whatever the filesystem's own are
    Some(meta.blocks() * 512)
}

#[cfg(windows)]
pub fn disk_size(path: &Path, _meta: &fs::Metadata) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
    };
    let file = crate::apply::open_itself(path).ok()?;
    // SAFETY: all zeros is a valid `FILE_STANDARD_INFO` (numbers, and `false`)
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open while `file` is, and `info` is as big as the size given
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    match read {
        0 => None,
        _ => u64::try_from(info.AllocationSize).ok(),
    }
}

#[cfg(not(any(unix, windows)))]
pub fn disk_size(_path: &Path, _meta: &fs::Metadata) -> Option<u64> {
    None
}

impl Koil {
    /// Where [`Koil::listing`] lists `a` against `b` (see [`Sort`]), so a frontend can put an
    /// entry where the listing would have it, like one that [`Koil::sync`] adds
    pub fn compare(&self, a: &Entry, b: &Entry) -> std::cmp::Ordering {
        let (a_meta, b_meta) = (self.sort_metadata(a), self.sort_metadata(b));
        self.order(a, a_meta.as_ref(), b, b_meta.as_ref())
    }

    /// What is on disk at the path of `id` (of a link itself, for a link): as koil read it
    /// when it last read the open dir (see [`Koil::sync`]), if the listing has it, else as it
    /// is now, `None` if the ID is not known or nothing can be read there
    /// A frontend can show what the listing is sorted by with it, like a file's size
    pub fn metadata(&self, id: Id) -> Option<Metadata> {
        let index = self.index_of(id)?;
        match self.seen.get(&index) {
            Some(seen) if self.current_listing.contains(&index) => Some(seen.meta),
            _ => Metadata::read(&self.ids[index], &self.settings),
        }
    }

    /// What [`Koil::listing`] is sorted by on disk for `entry`, `None` if it is new, or the
    /// listing is sorted by its name
    pub(crate) fn sort_metadata(&self, entry: &Entry) -> Option<Metadata> {
        match self.settings.sort.by.reads_metadata() {
            true => self.metadata(entry.id?),
            false => None,
        }
    }

    /// See [`Koil::compare`], where `a_meta` and `b_meta` are what [`Koil::sort_metadata`]
    /// gives for `a` and `b`
    pub(crate) fn order(
        &self,
        a: &Entry,
        a_meta: Option<&Metadata>,
        b: &Entry,
        b_meta: Option<&Metadata>,
    ) -> std::cmp::Ordering {
        let Sort {
            by,
            reverse,
            dirs_first,
        } = self.settings.sort;
        let group = |e: &Entry| match e.id {
            Some(_) if e.is_dir && dirs_first => 0,
            Some(_) => 1,
            None => 2,
        };
        // a dir's size is how many entries it has, which does not compare to a file's, so
        // sorted with the files, the dirs go after them (new entries are sorted by name)
        let sized = matches!(by, SortBy::Size | SortBy::Disk) && !dirs_first;
        let kind = |e: &Entry| sized && e.is_dir && e.id.is_some();
        group(a).cmp(&group(b)).then_with(|| {
            let order = (kind(a).cmp(&kind(b)))
                .then_with(|| by.compare(a, a_meta, b, b_meta))
                .then_with(|| a.name.cmp(&b.name));
            match reverse {
                true => order.reverse(),
                false => order,
            }
        })
    }
}
