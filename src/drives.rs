//! The drives on Windows, which are in no dir: koil lists them as the dir `/`, see
//! [`Koil::shows_drives`]

use crate::pattern::Walked;
use crate::sync::Seen;
use crate::{
    Edit, Entry, EntryError, EntryErrorKind, EntryWarning, Koil, Synced, UpdateError, to_id,
};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};

/// Where the drives are, on Windows: `/`, a root without a drive, so above every drive's root
/// (`C:/..` is it), and a drive is a name in it (`/C:` is `C:/`)
pub(crate) const DRIVES: &str = "/";

/// Whether `path` is the drives (see [`DRIVES`]), only on Windows
pub(crate) fn is_drives(path: &Path) -> bool {
    cfg!(windows) && path.components().eq([Component::RootDir])
}

/// Whether koil reads `path` in the drives (see [`Koil::shows_drives`]): it has a root, but no
/// drive (`/`, `/C:/x` or `/x`), only on Windows, which itself reads it on the drive of its
/// current dir
/// A frontend must not look for such a path on disk as it is: koil reads `/C:/x` as `C:/x`,
/// and `/x`, which is not on a drive, is nowhere
pub fn in_drives(path: &Path) -> bool {
    cfg!(windows) && path.components().next() == Some(Component::RootDir)
}

/// Whether `path` is the root of a drive, like `C:\`
pub(crate) fn is_drive_root(path: &Path) -> bool {
    let mut parts = path.components();
    drive_letter(parts.next()).is_some()
        && parts.next() == Some(Component::RootDir)
        && parts.next().is_none()
}

/// The letter of the drive that `prefix` is, if it is one (like `C:`, or `\\?\C:`)
fn drive_letter(prefix: Option<Component>) -> Option<u8> {
    match prefix? {
        Component::Prefix(p) => match p.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => Some(letter),
            _ => None,
        },
        _ => None,
    }
}

/// The root of the drive named `name`, a letter and a `:` (`c:` is `C:\`), only on Windows
pub(crate) fn drive_root(name: &OsStr) -> Option<PathBuf> {
    match name.as_encoded_bytes() {
        [letter, b':'] if cfg!(windows) && letter.is_ascii_alphabetic() => Some(PathBuf::from(
            format!("{}:\\", letter.to_ascii_uppercase() as char),
        )),
        _ => None,
    }
}

/// `path` as it is written in the drives (see [`DRIVES`]): its drive's name, then a `/` and
/// the rest of it (`C:/x`), and only the drive's name for its root (`C:`), as the listing of
/// the drives names it, `None` if it is not on a drive
pub(crate) fn drive_name(path: &Path) -> Option<PathBuf> {
    let mut parts = path.components();
    let letter = drive_letter(parts.next())?;
    if parts.next() != Some(Component::RootDir) {
        return None;
    }
    let mut name = OsString::from(format!("{}:", letter as char));
    for part in parts {
        name.push("/");
        name.push(part.as_os_str());
    }
    Some(name.into())
}

/// Whether `part` (of a location being written) can be the start of a drive's name: a letter,
/// or a letter and a `:`, only on Windows
pub(crate) fn starts_drive(part: &str) -> bool {
    match part.as_bytes() {
        [letter] | [letter, b':'] => cfg!(windows) && letter.is_ascii_alphabetic(),
        _ => false,
    }
}

/// The roots of the drives on this computer: on Windows, what `GetLogicalDrives` gives, so
/// also one with no disk in it, or a network drive that can not be reached; none elsewhere
pub(crate) fn drives() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        // SAFETY: it takes nothing, and only reads which drives there are
        let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        (0..26u8)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i) as char)))
            .collect()
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// The names of the drives (see [`drive_name`]), as [`Koil::complete`] gives dirs
pub(crate) fn drive_names() -> impl Iterator<Item = String> {
    (drives().into_iter()).filter_map(|root| Some(drive_name(&root)?.to_str()?.to_string()))
}

impl Koil {
    /// Whether the drives are open: only on Windows, where `..` of a drive's root (`C:/..`)
    /// opens them, as there is no dir they are in, and so does `/` ([`Koil::location`] then),
    /// which is a root without a drive, so a path that has one but no drive (`/x`) is in
    /// them, not on the drive of the open dir, as Windows has it, and `/C:/x` is `C:/x`
    /// The listing then has the root of each drive (named `C:`, its ID's path is `C:\`), as
    /// a dir, also one that can not be read (no disk in it): it only lists them, so a drive
    /// renamed, or anything else written there, is an error, and a drive left out of it stays
    /// (see [`Koil::update`]). A drive's ID can not be written in another listing either, as
    /// it can not be moved or copied ([`EntryErrorKind::Drive`])
    /// [`Koil::current_dir`] is `/` then, which is no dir on disk (see [`in_drives`]), and
    /// relative paths start with a drive's name (`C:/x`), a pattern can not be opened there
    /// ([`OpenError::PatternInDrives`](crate::OpenError::PatternInDrives)), and nothing is
    /// watched ([`Koil::watched`])
    pub fn shows_drives(&self) -> bool {
        is_drives(&self.current_dir)
    }

    /// The paths the listing of the drives shows, as [`Koil::walk`] gives them: the root of
    /// each drive, and what is there only if the listing is sorted by it, as reading a
    /// network drive that can not be reached can take long
    pub(crate) fn read_drives(&self) -> Walked {
        let reads = self.settings.sort.by.reads_metadata();
        (drives().into_iter())
            .map(|root| {
                let meta = reads.then(|| fs::metadata(&root).ok()).flatten();
                let seen = meta.map(|meta| Seen::new(&root, &meta, &self.settings));
                (root, seen)
            })
            .collect()
    }

    /// `path`, relative to the open dir, as an absolute path, which [`resolve`] reads: a
    /// drive's name alone (`C:`) is its root, rather than the dir Windows was last in on it,
    /// and in the drives, a relative path starts with a drive's name (`C:/x`, or `C:`), read
    /// in them (`/C:`). One with a root but no drive (`/x`) is in the drives (see
    /// [`Koil::shows_drives`]) wherever it is read
    ///
    /// [`resolve`]: crate::location::resolve
    pub(crate) fn joined(&self, path: &Path) -> PathBuf {
        if let Some(root) = drive_root(path.as_os_str()) {
            return root;
        }
        if in_drives(path) {
            return path.to_path_buf();
        }
        if self.shows_drives() && !path.has_root() {
            let mut joined = OsString::from(DRIVES);
            joined.push(path.as_os_str());
            return joined.into();
        }
        self.current_dir.join(path)
    }

    /// The path of the entry named `name` in the listing of the open dir: in the drives, a
    /// drive's name is its root
    pub(crate) fn path_at(&self, name: &Path) -> PathBuf {
        match self.shows_drives() {
            true => drive_root(name.as_os_str()).unwrap_or_else(|| self.joined(name)),
            false => self.current_dir.join(name),
        }
    }

    /// [`Koil::update`] of the listing of the drives, which only lists them, so nothing
    /// changes: a drive left out of it stays, and a drive renamed
    /// ([`EntryErrorKind::Drive`]), written more than once, an entry that is not a drive
    /// ([`EntryErrorKind::NotADrive`]), or a new one at a drive's name, is an error
    pub(crate) fn update_drives(
        &self,
        entries: &[Entry],
    ) -> Result<Vec<EntryWarning>, UpdateError> {
        let mut errors = Vec::new();
        // each drive written, and the entry that has it first
        let mut seen: HashMap<PathBuf, usize> = HashMap::new();
        for (i, entry) in entries.iter().enumerate() {
            let path = self.path_at(&entry.name);
            let listed = |path: &Path| (self.current_listing.iter()).any(|&i| self.ids[i] == path);
            let kind = match entry.id.map(|id| (id, self.index_of(id))) {
                Some((id, None)) => Some(EntryErrorKind::UnknownId(id)),
                Some((_, Some(index))) if self.ids[index] == path => None,
                Some((_, Some(index))) if is_drive_root(&self.ids[index]) => {
                    Some(EntryErrorKind::Drive(self.ids[index].clone()))
                }
                Some(_) => Some(EntryErrorKind::NotADrive(entry.name.clone())),
                None if listed(&path) => Some(EntryErrorKind::AlreadyExists(entry.name.clone())),
                None => Some(EntryErrorKind::NotADrive(entry.name.clone())),
            };
            let kind = kind.or_else(|| {
                let first = *seen.entry(path).or_insert(i);
                (first != i).then(|| EntryErrorKind::Duplicate {
                    path: entry.name.clone(),
                    first,
                })
            });
            if let Some(kind) = kind {
                errors.push(EntryError { entry: i, kind });
            }
        }
        match errors.is_empty() {
            true => Ok(Vec::new()),
            false => Err(UpdateError {
                errors,
                warnings: Vec::new(),
            }),
        }
    }

    /// [`Koil::sync`] of the listing of the drives: the drives that came, and the entries of
    /// those that went
    pub(crate) fn sync_drives(&mut self, entries: &[Entry]) -> Synced {
        let before = self.current_listing.clone();
        let paths = self.read_drives();
        self.list(paths);
        let index = |entry: &Entry| entry.id.and_then(|id| self.index_of(id));
        let mut edits: Vec<Edit> = Vec::new();
        for entry in entries {
            let gone = index(entry).is_some_and(|i| !self.current_listing.contains(&i));
            let remove = Edit::Remove(entry.clone());
            if gone && !edits.contains(&remove) {
                edits.push(remove);
            }
        }
        let mut came: Vec<usize> = (self.current_listing.iter().copied())
            .filter(|i| !before.contains(i) && !entries.iter().any(|e| index(e) == Some(*i)))
            .collect();
        came.sort_by(|&a, &b| self.ids[a].cmp(&self.ids[b]));
        for i in came {
            edits.push(Edit::Add(Entry {
                id: Some(to_id(i)),
                name: self.name(&self.ids[i]),
                is_dir: true,
            }));
        }
        Synced {
            edits,
            ..Synced::default()
        }
    }
}
