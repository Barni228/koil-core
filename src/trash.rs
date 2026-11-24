//! Moving paths to the system trash, and restoring them from it

use crate::{on_disk, shown};
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
    #[serde(with = "crate::serde_path")]
    pub original: PathBuf,
    /// Where to find it in the trash
    /// On macOS and the web it is the path inside the trash, on other systems it is
    /// [`trash::TrashItem::id`]
    #[serde(with = "crate::serde_path")]
    id: PathBuf,
    /// What tells it apart from what is trashed later at `id`, which is free again once the
    /// trash is emptied (see `platform::Stamp`), so that is never restored in its place
    /// `None` if it can not be told (a trash that keeps no times), then whatever is at `id` is
    /// taken for it
    stamp: Option<platform::Stamp>,
}

/// Move `path` to the trash
pub fn trash(path: &Path) -> io::Result<Trashed> {
    let (id, stamp) = platform::trash(path)?;
    Ok(Trashed {
        original: path.to_path_buf(),
        id,
        stamp,
    })
}

/// Move `trashed` back to where it was, never overwrites an existing path
pub fn restore(trashed: &Trashed) -> io::Result<()> {
    if on_disk(&trashed.original) {
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
    use crate::on_disk;
    use crate::sync::FileKey;
    use objc2_foundation::{NSFileManager, NSURL};
    use std::path::{Path, PathBuf};
    use std::{fs, io};

    /// The key of the item in the trash (see [`FileKey`]), which moving it there keeps: a
    /// file trashed later with the same name, once the trash is emptied, has another
    pub(crate) type Stamp = FileKey;

    pub fn trash(path: &Path) -> io::Result<(PathBuf, Option<Stamp>)> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid path");
        let url = NSURL::from_file_path(path).ok_or_else(invalid)?;
        let mut in_trash = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut in_trash))
            .map_err(|err| io::Error::other(err.localizedDescription().to_string()))?;
        let in_trash = in_trash
            .and_then(|url| url.to_file_path())
            .ok_or_else(|| io::Error::other("The trash did not say where it put the item"))?;
        let stamp = FileKey::at(&in_trash);
        Ok((in_trash, stamp))
    }

    pub fn restore(trashed: &Trashed) -> io::Result<()> {
        if !in_trash(trashed) {
            return Err(not_in_trash(trashed));
        }
        fs::rename(&trashed.id, &trashed.original)
    }

    /// Whether what was trashed is at its path in the trash still, and not something trashed
    /// there later
    fn in_trash(trashed: &Trashed) -> bool {
        match trashed.stamp {
            Some(stamp) => FileKey::at(&trashed.id) == Some(stamp),
            None => on_disk(&trashed.id),
        }
    }

    /// Tells whether items are still in the trash, so [`restore`] can bring them back
    /// Made with `default`, as on other systems, so not a unit struct
    #[derive(Default)]
    pub struct Contents {}

    impl Contents {
        pub fn has(&self, trashed: &Trashed) -> bool {
            in_trash(trashed)
        }
    }
}

// The web (Emscripten) has a home dir, where `trash` keeps the freedesktop trash, but `trash`
// can not find the mount points there, and fails to trash, list or restore anything without
// them, so there koil keeps that trash itself (see `home`)
#[cfg(target_os = "emscripten")]
mod platform {
    use super::{Trashed, home, not_in_trash};
    use std::io;
    use std::path::{Path, PathBuf};

    pub(crate) use home::Stamp;

    pub fn trash(path: &Path) -> io::Result<(PathBuf, Option<Stamp>)> {
        let (id, stamp) = home::trash(&home::dir()?, path)?;
        Ok((id, Some(stamp)))
    }

    pub fn restore(trashed: &Trashed) -> io::Result<()> {
        if !in_trash(trashed) {
            return Err(not_in_trash(trashed));
        }
        home::put_back(&trashed.id, &trashed.original)
    }

    fn in_trash(trashed: &Trashed) -> bool {
        home::has(&trashed.id, trashed.stamp.as_deref())
    }

    /// Tells whether items are still in the trash, so [`restore`] can bring them back
    /// Made with `default`, as on other systems, so not a unit struct
    #[derive(Default)]
    pub struct Contents {}

    impl Contents {
        pub fn has(&self, trashed: &Trashed) -> bool {
            in_trash(trashed)
        }
    }
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android"),
        not(target_os = "emscripten")
    )
))]
mod platform {
    use super::{Trashed, not_in_trash};
    use crate::apply::retry;
    use crate::on_disk;
    use std::cell::OnceCell;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::{fs, io};
    use trash::TrashItem;
    use trash::os_limited::{list, restore_all};

    /// When the item was trashed ([`TrashItem::time_deleted`]): one trashed later at its `id`
    /// (the freedesktop trash names its info file after the item) was trashed at another time
    pub(crate) type Stamp = i64;

    pub fn trash(path: &Path) -> io::Result<(PathBuf, Option<Stamp>)> {
        // before it is trashed, while its parent can still be canonicalized the same way
        let parent = canonical_parent(path)?;
        // while it is still there, as what was aborted moved nothing
        retry(|| trash::delete(path), |err| refused(err) && on_disk(path))
            .map_err(io::Error::other)?;
        // the newest item that came from `path`, which must be the one just trashed
        let items = list().map_err(io::Error::other)?;
        items
            .into_iter()
            .filter(|item| {
                Some(item.name.as_os_str()) == path.file_name()
                    && fs::canonicalize(&item.original_parent).is_ok_and(|p| p == parent)
            })
            .max_by_key(|item| item.time_deleted)
            .map(|item| (PathBuf::from(&item.id), stamp(&item)))
            .ok_or_else(|| io::Error::other("Can not find the trashed item in the trash"))
    }

    pub fn restore(trashed: &Trashed) -> io::Result<()> {
        let items = list().map_err(io::Error::other)?;
        let item: TrashItem = items
            .into_iter()
            .find(|item| item.id == trashed.id.as_os_str() && same(stamp(item), trashed))
            .ok_or_else(|| not_in_trash(trashed))?;
        // what is at its path then is never overwritten: `restore_all` fails on it
        retry(|| restore_all([item.clone()]), refused).map_err(io::Error::other)
    }

    /// Whether Windows' shell refused to trash or restore something for now, as another
    /// program has it, or something in it, open (see [`retry`]): the copy engine's errors for
    /// that, and Windows' own, or a trash only said to be aborted, which is all it says then
    #[cfg(windows)]
    fn refused(err: &trash::Error) -> bool {
        const IN_USE: [u32; 7] = [
            0x8007_0005, // E_ACCESSDENIED
            0x8007_0020, // ERROR_SHARING_VIOLATION
            0x8007_0021, // ERROR_LOCK_VIOLATION
            0x8027_0021, // COPYENGINE_E_ACCESS_DENIED_SRC
            0x8027_0022, // COPYENGINE_E_ACCESS_DENIED_DEST
            0x8027_0027, // COPYENGINE_E_SHARING_VIOLATION_SRC
            0x8027_0028, // COPYENGINE_E_SHARING_VIOLATION_DEST
        ];
        match err {
            trash::Error::Unknown { .. } => true,
            trash::Error::Os { code, .. } => IN_USE.contains(&(*code as u32)),
            _ => false,
        }
    }

    /// Nothing refuses that way on the freedesktop trash (see [`retry`])
    #[cfg(not(windows))]
    fn refused(_: &trash::Error) -> bool {
        false
    }

    /// The stamp of `item`, `None` if the trash does not say when it was trashed
    fn stamp(item: &TrashItem) -> Option<Stamp> {
        (item.time_deleted >= 0).then_some(item.time_deleted)
    }

    /// Whether an item in the trash at the `id` of `trashed`, with `stamp`, is what was trashed
    fn same(stamp: Option<Stamp>, trashed: &Trashed) -> bool {
        trashed.stamp.is_none() || stamp == trashed.stamp
    }

    /// Tells whether items are still in the trash, so [`restore`] can bring them back
    /// The trash is listed once, on the first item asked about
    #[derive(Default)]
    pub struct Contents {
        /// The items' stamps by their IDs, `None` if it can not be listed, then every item
        /// counts as there
        ids: OnceCell<Option<HashMap<OsString, Option<Stamp>>>>,
    }

    impl Contents {
        pub fn has(&self, trashed: &Trashed) -> bool {
            let ids = self.ids.get_or_init(|| {
                let items = list().ok()?;
                Some(
                    items
                        .iter()
                        .map(|item| (item.id.clone(), stamp(item)))
                        .collect(),
                )
            });
            ids.as_ref().is_none_or(|ids| {
                (ids.get(trashed.id.as_os_str())).is_some_and(|&stamp| same(stamp, trashed))
            })
        }
    }

    fn canonical_parent(path: &Path) -> io::Result<PathBuf> {
        match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => fs::canonicalize(p),
            _ => std::env::current_dir(),
        }
    }
}

/// The freedesktop home trash, as koil keeps it on the web (see `platform` there):
/// `$XDG_DATA_HOME/Trash`, by default `~/.local/share/Trash`, with the items in its `files`,
/// and an info file for each in its `info` (`<its name>.trashinfo`), which says where it came
/// from and when it was trashed. Making the info file takes the name, so an item gets the first
/// one free: its own, then `name.2`, `name.3`…, as `trash` gives on Linux
/// Built everywhere, so it keeps compiling and is tested, but only the web uses it, so it takes
/// an item's path in the trash and its stamp, not a [`Trashed`], whose stamp is another elsewhere
#[cfg_attr(not(target_os = "emscripten"), allow(dead_code))]
pub(crate) mod home {
    use crate::apply::{inside, into_itself, rename};
    use crate::{on_disk, shown};
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::{env, fs, io};

    /// When the item was trashed, as its info file's `DeletionDate` says (local time, to the
    /// second; the web has no time zones, so UTC there): an item trashed at the same path
    /// after the trash was emptied has another
    pub(crate) type Stamp = String;

    /// The home trash: `$XDG_DATA_HOME/Trash`, else `$HOME/.local/share/Trash`
    pub fn dir() -> io::Result<PathBuf> {
        let var = |name| (env::var_os(name).map(PathBuf::from)).filter(|path| path.is_absolute());
        let data =
            var("XDG_DATA_HOME").or_else(|| var("HOME").map(|home| home.join(".local/share")));
        (data.map(|data| data.join("Trash"))).ok_or_else(|| {
            io::Error::other("Neither XDG_DATA_HOME nor HOME says where the trash is")
        })
    }

    /// Move `path` into the trash at `dir`, which is made if it is not there
    /// Returns where it is in the trash, and its stamp
    pub fn trash(dir: &Path, path: &Path) -> io::Result<(PathBuf, Stamp)> {
        let path = std::path::absolute(path)?;
        let Some(name) = path.file_name() else {
            let why = format!("`{}` has no name to trash it by", shown(&path));
            return Err(io::Error::new(io::ErrorKind::InvalidInput, why));
        };
        let files = dir.join("files");
        // a dir the trash is in: the web's home dir is a file system of its own, and a dir moved
        // to it from outside is copied, which would copy it into itself
        if inside(&path, &files.join(name)) {
            return Err(into_itself(&path, &files));
        }
        fs::create_dir_all(&files)?;
        fs::create_dir_all(dir.join("info"))?;
        let date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let info = format!(
            "[Trash Info]\nPath={}\nDeletionDate={date}\n",
            uri_path(&path)
        );
        let mut count = 0;
        loop {
            count += 1;
            let mut in_trash = name.to_owned();
            if count > 1 {
                in_trash.push(format!(".{count}"));
            }
            let id = files.join(&in_trash);
            let info_file = info_file(&id);
            let mut file = match fs::File::create_new(&info_file) {
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                file => file?,
            };
            // before it moves, so nothing is in the trash without saying where it came from;
            // `rename` copies it, if it is on another file system, and never overwrites
            match (file.write_all(info.as_bytes())).and_then(|()| rename(&path, &id)) {
                Ok(()) => return Ok((id, date)),
                Err(err) => {
                    let _ = fs::remove_file(&info_file);
                    // something is in `files` at that name, without an info file
                    if err.kind() != io::ErrorKind::AlreadyExists {
                        return Err(err);
                    }
                }
            }
        }
    }

    /// Move the item at `id` in the trash back to `original`, making the dirs it was in if
    /// they are gone, as the freedesktop trash does (see [`super::RESTORE_MAKES_PARENTS`]); never
    /// overwrites what is there
    pub fn put_back(id: &Path, original: &Path) -> io::Result<()> {
        if let Some(parent) = original.parent() {
            fs::create_dir_all(parent)?;
        }
        rename(id, original)?;
        // it is back, and an info file left behind would only keep its name taken
        let _ = fs::remove_file(info_file(id));
        Ok(())
    }

    /// Whether the item at `id` is still in the trash, and not one trashed at its path later:
    /// its info file has `stamp`, if one is given
    pub fn has(id: &Path, stamp: Option<&str>) -> bool {
        on_disk(id) && stamp.is_none_or(|stamp| deletion_date(id).as_deref() == Some(stamp))
    }

    /// The `DeletionDate` of the item at `id` in the trash, from its info file
    fn deletion_date(id: &Path) -> Option<String> {
        let info = fs::read_to_string(info_file(id)).ok()?;
        let date = info
            .lines()
            .find_map(|line| line.strip_prefix("DeletionDate="))?;
        Some(date.trim().to_string())
    }

    /// The info file of the item at `id`, in the trash's `files`
    fn info_file(id: &Path) -> PathBuf {
        let trash = id.parent().and_then(Path::parent).unwrap_or(Path::new(""));
        let mut name = id.file_name().unwrap_or_default().to_owned();
        name.push(".trashinfo");
        trash.join("info").join(name)
    }

    /// `path` as an info file's `Path` has it: escaped as in a URI, every byte but an ASCII
    /// letter or digit and `/-._~` as `%` and its hex
    fn uri_path(path: &Path) -> String {
        let mut uri = String::new();
        for &byte in path.as_os_str().as_encoded_bytes() {
            match byte {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' => {
                    uri.push(char::from(byte))
                }
                _ => {
                    let _ = write!(uri, "%{byte:02X}");
                }
            }
        }
        uri
    }
}
