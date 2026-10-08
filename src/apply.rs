use crate::trash::{self, Contents, Trashed};
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

    /// The paths this step changes on disk: what it trashes, restores, or moves, and where to
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Undo::Trash(p) => vec![p],
            Undo::Restore(t) => vec![&t.original],
            Undo::Rename(s, d) => vec![s, d],
        }
    }
}

/// Why an [`Undo`] step can not run now (see [`check`])
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Blocked {
    /// Nothing is at the path it trashes or moves, or the dir it puts something in is gone
    Missing(PathBuf),
    /// Something is already at the path it puts something at
    Taken(PathBuf),
    /// What it restores is no longer in the trash (it was emptied)
    NotInTrash(PathBuf),
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocked::Missing(p) => write!(f, "`{}` is gone", shown(p)),
            Blocked::Taken(p) => write!(f, "`{}` already exists", shown(p)),
            Blocked::NotInTrash(p) => write!(f, "`{}` is no longer in the trash", shown(p)),
        }
    }
}

/// Checks that `steps` can run in order now, as far as can be told without running them:
/// what each one trashes or moves is there, where it puts something is free and in a dir
/// that is there, and what it restores is still in the trash (asked of `trash`)
/// Each step is checked on the filesystem as the steps before it would leave it
/// Returns the first step that can not run, and why
pub(crate) fn check<'a>(
    steps: impl IntoIterator<Item = &'a Undo>,
    trash: &Contents,
) -> Result<(), (Undo, Blocked)> {
    let mut disk = Simulated::default();
    for step in steps {
        disk.run(step, trash)
            .map_err(|blocked| (step.clone(), blocked))?;
    }
    Ok(())
}

/// The filesystem as some [`Undo`] steps would leave it, without running them
#[derive(Default)]
struct Simulated {
    /// What the steps did, in order
    done: Vec<Done>,
}

enum Done {
    /// Nothing is at the path, nor inside it
    Gone(PathBuf),
    /// What was at the first path (and inside it) is at the second
    Moved(PathBuf, PathBuf),
    /// Something is at the path, but what is inside it is not known
    Restored(PathBuf),
}

impl Simulated {
    /// Whether something would be at `path`, `None` if that can not be told (inside a dir that
    /// comes back from the trash)
    fn exists(&self, path: &Path) -> Option<bool> {
        let mut path = path.to_path_buf();
        // the newest step that says anything about it decides, a move sends it back to where
        // it was before
        for done in self.done.iter().rev() {
            match done {
                Done::Gone(p) if path.starts_with(p) => return Some(false),
                Done::Moved(from, to) => {
                    if let Ok(rest) = path.strip_prefix(to) {
                        path = match rest.as_os_str().is_empty() {
                            true => from.clone(),
                            false => from.join(rest),
                        };
                    } else if path.starts_with(from) {
                        return Some(false);
                    }
                }
                Done::Restored(p) if path == *p => return Some(true),
                Done::Restored(p) if path.starts_with(p) => return None,
                _ => {}
            }
        }
        Some(path.symlink_metadata().is_ok())
    }

    /// Something is (or may be) at `path`
    fn there(&self, path: &Path) -> Result<(), Blocked> {
        match self.exists(path) {
            Some(false) => Err(Blocked::Missing(path.to_path_buf())),
            _ => Ok(()),
        }
    }

    /// The dir `path` goes in is (or may be) there
    fn in_dir(&self, path: &Path) -> Result<(), Blocked> {
        match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => self.there(parent),
            _ => Ok(()),
        }
    }

    /// Checks that `step` can run, as [`Undo::run`] would, then does it
    fn run(&mut self, step: &Undo, trash: &Contents) -> Result<(), Blocked> {
        match step {
            Undo::Trash(p) => {
                self.there(p)?;
                self.done.push(Done::Gone(p.clone()));
            }
            Undo::Restore(t) => {
                if !trash.has(t) {
                    return Err(Blocked::NotInTrash(t.original.clone()));
                }
                if self.exists(&t.original) == Some(true) {
                    return Err(Blocked::Taken(t.original.clone()));
                }
                if !trash::RESTORE_MAKES_PARENTS {
                    self.in_dir(&t.original)?;
                }
                self.done.push(Done::Restored(t.original.clone()));
            }
            Undo::Rename(s, d) => {
                self.there(s)?;
                // as in `Action::run`, on case insensitive filesystems `A -> a` sees `a` as taken,
                // but it is `A` itself
                if self.exists(d) == Some(true) && !same_file(s, d).unwrap_or(false) {
                    return Err(Blocked::Taken(d.clone()));
                }
                self.in_dir(d)?;
                self.done.push(Done::Moved(s.clone(), d.clone()));
            }
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
