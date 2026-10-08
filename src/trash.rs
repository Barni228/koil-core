//! Moving paths to the system trash, and restoring them from it

use crate::shown;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub(crate) use platform::Contents;

/// Whether [`restore`] makes the dirs the item was in, if they are gone
/// The freedesktop trash does, but on macOS and Windows restoring there fails
pub(crate) const RESTORE_MAKES_PARENTS: bool = cfg!(all(unix, not(target_os = "macos")));

/// A path that was moved to the trash, and can be restored with [`restore`]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Trashed {
    /// Where it was before it was trashed
    pub original: PathBuf,
    /// Where to find it in the trash
    /// On macOS it is the path inside the trash, on other systems it is [`trash::TrashItem::id`]
    id: PathBuf,
}

/// Move `path` to the trash
pub fn trash(path: &Path) -> io::Result<Trashed> {
    Ok(Trashed {
        original: path.to_path_buf(),
        id: platform::trash(path)?,
    })
}

/// Move `trashed` back to where it was, never overwrites an existing path
pub fn restore(trashed: &Trashed) -> io::Result<()> {
    if trashed.original.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("`{}` already exists", shown(&trashed.original)),
        ));
    }
    platform::restore(trashed)
}

fn not_in_trash(trashed: &Trashed) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("`{}` is no longer in the trash", shown(&trashed.original)),
    )
}

// `trash` can only restore on Windows and Linux, and does not tell where it put the item on macOS,
// so on macOS it is trashed with `NSFileManager`, which returns the path inside the trash
#[cfg(target_os = "macos")]
mod platform {
    use super::{Trashed, not_in_trash};
    use objc2_foundation::{NSFileManager, NSURL};
    use std::path::{Path, PathBuf};
    use std::{fs, io};

    pub fn trash(path: &Path) -> io::Result<PathBuf> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid path");
        let url = NSURL::from_file_path(path).ok_or_else(invalid)?;
        let mut in_trash = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut in_trash))
            .map_err(|err| io::Error::other(err.localizedDescription().to_string()))?;
        in_trash
            .and_then(|url| url.to_file_path())
            .ok_or_else(|| io::Error::other("The trash did not say where it put the item"))
    }

    pub fn restore(trashed: &Trashed) -> io::Result<()> {
        if trashed.id.symlink_metadata().is_err() {
            return Err(not_in_trash(trashed));
        }
        fs::rename(&trashed.id, &trashed.original)
    }

    /// Tells whether items are still in the trash, so [`restore`] can bring them back
    /// Made with `default`, as on other systems, so not a unit struct
    #[derive(Default)]
    pub struct Contents {}

    impl Contents {
        pub fn has(&self, trashed: &Trashed) -> bool {
            trashed.id.symlink_metadata().is_ok()
        }
    }
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
mod platform {
    use super::{Trashed, not_in_trash};
    use std::cell::OnceCell;
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::{fs, io};
    use trash::TrashItem;
    use trash::os_limited::{list, restore_all};

    pub fn trash(path: &Path) -> io::Result<PathBuf> {
        // before it is trashed, while its parent can still be canonicalized the same way
        let parent = canonical_parent(path)?;
        trash::delete(path).map_err(io::Error::other)?;
        // the newest item that came from `path`, which must be the one just trashed
        let items = list().map_err(io::Error::other)?;
        items
            .into_iter()
            .filter(|item| {
                Some(item.name.as_os_str()) == path.file_name()
                    && fs::canonicalize(&item.original_parent).is_ok_and(|p| p == parent)
            })
            .max_by_key(|item| item.time_deleted)
            .map(|item| PathBuf::from(item.id))
            .ok_or_else(|| io::Error::other("Can not find the trashed item in the trash"))
    }

    pub fn restore(trashed: &Trashed) -> io::Result<()> {
        let items = list().map_err(io::Error::other)?;
        let item: TrashItem = items
            .into_iter()
            .find(|item| item.id == trashed.id.as_os_str())
            .ok_or_else(|| not_in_trash(trashed))?;
        restore_all([item]).map_err(io::Error::other)
    }

    /// Tells whether items are still in the trash, so [`restore`] can bring them back
    /// The trash is listed once, on the first item asked about
    #[derive(Default)]
    pub struct Contents {
        /// `None` if it can not be listed, then every item counts as there
        ids: OnceCell<Option<HashSet<OsString>>>,
    }

    impl Contents {
        pub fn has(&self, trashed: &Trashed) -> bool {
            let ids = self.ids.get_or_init(|| {
                let items = list().ok()?;
                Some(items.into_iter().map(|item| item.id).collect())
            });
            ids.as_ref()
                .is_none_or(|ids| ids.contains(trashed.id.as_os_str()))
        }
    }

    fn canonical_parent(path: &Path) -> io::Result<PathBuf> {
        match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => fs::canonicalize(p),
            _ => std::env::current_dir(),
        }
    }
}
