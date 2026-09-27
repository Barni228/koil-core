use crate::Action;
use crate::trash::{self, Trashed};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

impl Action {
    /// A shell command that does the same thing as [`Action::run`]
    /// Paths inside `base` are written relative to it
    pub fn command(&self, base: &Path) -> String {
        let q = |path: &Path| shell_path(path, base);
        match self {
            Action::CreateFile(n) => format!("touch {}", q(n)),
            Action::CreateDir(n) => format!("mkdir -p {}", q(n)),
            Action::DeleteFile(n) | Action::DeleteDir(n) => format!("trash {}", q(n)),
            Action::Rename(s, d) => format!("mv {} {}", q(s), q(d)),
            Action::Copy(s, d) if s.is_dir() => format!("cp -R {} {}", q(s), q(d)),
            Action::Copy(s, d) => format!("cp {} {}", q(s), q(d)),
        }
    }

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
                fs::rename(s, d)?;
                Undo::Rename(d.clone(), s.clone())
            }
            Action::Copy(s, d) => {
                copy(s, d)?;
                Undo::Trash(d.clone())
            }
        };
        Ok(Some(undo))
    }
}

/// The shell command, with full paths
impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // every path starts with the empty path, so nothing is made relative
        f.write_str(&self.command(Path::new("")))
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
    /// mv <src> <dst>
    Rename(PathBuf, PathBuf),
}

impl Undo {
    /// A shell command that does the same thing as [`Undo::run`]
    /// Restoring from the trash has no standard command, so it is shown as `restore <path>`
    /// Paths inside `base` are written relative to it
    pub fn command(&self, base: &Path) -> String {
        let q = |path: &Path| shell_path(path, base);
        match self {
            Undo::Trash(n) => format!("trash {}", q(n)),
            Undo::Restore(t) => format!("restore {}", q(&t.original)),
            Undo::Rename(s, d) => format!("mv {} {}", q(s), q(d)),
        }
    }

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

/// The shell command, with full paths
impl fmt::Display for Undo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // every path starts with the empty path, so nothing is made relative
        f.write_str(&self.command(Path::new("")))
    }
}

/// `path` for a shell command, relative to `base` if it is inside it
fn shell_path(path: &Path, base: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        // so a name like `-rf` is not read as a flag
        Ok(rel) if rel.to_string_lossy().starts_with('-') => quote(&Path::new(".").join(rel)),
        Ok(rel) => quote(rel),
        Err(_) => quote(path),
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

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let (a, b) = (a.symlink_metadata()?, b.symlink_metadata()?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

#[cfg(not(unix))]
fn same_file(_a: &Path, _b: &Path) -> io::Result<bool> {
    Ok(false)
}

/// True if something (even a broken symlink) is at `path`
fn taken(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn already_exists(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("`{}` already exists", path.display()),
    )
}

/// Quote `path` for a POSIX shell, only if it needs it
fn quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./,:@%+=".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
