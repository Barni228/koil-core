use crate::trash::{self, Trashed};
use crate::{Action, shown};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

impl Action {
    /// Run this action on the filesystem, deleted paths are moved to the trash
    /// Never overwrites an existing path, the planner makes sure it was removed first
    /// Returns the step that reverts it, or `None` if it changed nothing
    pub fn run(&self) -> io::Result<Option<Undo>> {
        let undo = match self {
            Action::CreateFile(n) => {
                fs::File::create_new(n)?;
                Undo::Trash(n.clone())
            }
            Action::CreateDir(n) => {
                // `mkdir -p` does nothing if it is already there, so there is nothing to revert
                if n.is_dir() {
                    return Ok(None);
                }
                fs::create_dir_all(n)?;
                Undo::Trash(n.clone())
            }
            Action::DeleteFile(n) | Action::DeleteDir(n) => Undo::Restore(trash::trash(n)?),
            Action::Rename(s, d) => {
                // on case insensitive filesystems `a -> A` sees `A` as taken, but it is `a` itself
                if taken(d) && !same_file(s, d)? {
                    return Err(already_exists(d));
                }
                if inside(s, d) {
                    return Err(into_itself(s, d));
                }
                fs::rename(s, d)?;
                Undo::Rename(d.clone(), s.clone())
            }
            Action::Copy(s, d) => {
                // the copy would be copied again into itself, until the path is too long
                if inside(s, d) {
                    return Err(into_itself(s, d));
                }
                copy(s, d)?;
                Undo::Trash(d.clone())
            }
        };
        Ok(Some(undo))
    }
}

/// What the action does, with full paths, like `move a -> b`
impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::CreateFile(n) => write!(f, "create {}", shown(n)),
            Action::CreateDir(n) => write!(f, "create {}/", shown(n)),
            Action::DeleteFile(n) => write!(f, "delete {}", shown(n)),
            Action::DeleteDir(n) => write!(f, "delete {}/", shown(n)),
            Action::Rename(s, d) => write!(f, "move {} -> {}", shown(s), shown(d)),
            Action::Copy(s, d) => write!(f, "copy {} -> {}", shown(s), shown(d)),
        }
    }
}

/// A step that reverts an [`Action`] that was run
/// Nothing is deleted for good, what the action created is moved to the trash
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Undo {
    /// Move a created path to the trash
    Trash(PathBuf),
    /// Move a trashed path back from the trash
    Restore(Trashed),
    /// `mv <src> <dst>`
    Rename(PathBuf, PathBuf),
}

impl Undo {
    /// Run this step on the filesystem, never overwrites an existing path
    pub fn run(&self) -> io::Result<()> {
        match self {
            Undo::Trash(n) => {
                trash::trash(n)?;
            }
            Undo::Restore(t) => trash::restore(t)?,
            Undo::Rename(s, d) => Action::Rename(s.clone(), d.clone()).run().map(|_| ())?,
        }
        Ok(())
    }
}

/// What the step does, with full paths, like `move a -> b`
impl fmt::Display for Undo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Undo::Trash(n) => write!(f, "trash {}", shown(n)),
            Undo::Restore(t) => write!(f, "restore {}", shown(&t.original)),
            Undo::Rename(s, d) => write!(f, "move {} -> {}", shown(s), shown(d)),
        }
    }
}

/// Copy a file, dir (recursively), or symlink (the link itself) from `src` to `dst`
fn copy(src: &Path, dst: &Path) -> io::Result<()> {
    if taken(dst) {
        return Err(already_exists(dst));
    }
    let file_type = src.symlink_metadata()?.file_type();
    if file_type.is_symlink() {
        copy_symlink(src, dst)
    } else if file_type.is_dir() {
        fs::create_dir(dst)?;
        for item in fs::read_dir(src)? {
            let item = item?;
            copy(&item.path(), &dst.join(item.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(src)?, dst)
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst).map(|_| ())
}

/// Whether `a` and `b` are the same file, a link compared as itself, not what it points to
/// Not `same_file::is_same_file`, which follows links, and on Unix opens both files to read
/// them, which waits forever on a named pipe, and fails without read access
#[cfg(unix)]
pub(crate) fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let (a, b) = (a.symlink_metadata()?, b.symlink_metadata()?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

#[cfg(windows)]
pub(crate) fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_BACKUP_SEMANTICS, which opening a dir needs, and FILE_FLAG_OPEN_REPARSE_POINT,
    // which opens a link itself, not what it points to (like `symlink_metadata`)
    const FLAGS: u32 = 0x0200_0000 | 0x0020_0000;
    // opened here, as `same_file::is_same_file` follows links (and so would let `link` be
    // renamed onto the file it points to)
    let handle = |path: &Path| {
        // reading its information needs no access to the file
        let file = fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(FLAGS)
            .open(path)?;
        // compares the volume serial and file index
        same_file::Handle::from_file(file)
    };
    Ok(handle(a)? == handle(b)?)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn same_file(_a: &Path, _b: &Path) -> io::Result<bool> {
    Ok(false)
}

/// True if something (even a broken symlink) is at `path`
fn taken(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// True if `dst` is inside `src`, a dir (not a symlink to one), so copying or moving `src` to
/// `dst` would put it into itself
/// The parents of `dst` that exist are also compared as files, which catches a path that only
/// differs in case on a filesystem that ignores case
pub(crate) fn inside(src: &Path, dst: &Path) -> bool {
    src.symlink_metadata().is_ok_and(|m| m.is_dir())
        && (dst.ancestors().skip(1))
            .any(|parent| parent == src || same_file(parent, src).unwrap_or(false))
}

fn into_itself(src: &Path, dst: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "`{}` can not go into itself, at `{}`",
            shown(src),
            shown(dst)
        ),
    )
}

fn already_exists(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("`{}` already exists", shown(path)),
    )
}
