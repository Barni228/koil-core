use crate::Action;
use std::fs;
use std::io;
use std::path::Path;

impl Action {
    /// A shell command that does the same thing as [`Action::run`]
    /// Paths inside `base` are written relative to it
    pub fn command(&self, base: &Path) -> String {
        let q = |path: &Path| match path.strip_prefix(base) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
            // so a name like `-rf` is not read as a flag
            Ok(rel) if rel.to_string_lossy().starts_with('-') => quote(&Path::new(".").join(rel)),
            Ok(rel) => quote(rel),
            Err(_) => quote(path),
        };
        match self {
            Action::CreateFile(n) => format!("touch {}", q(n)),
            Action::CreateDir(n) => format!("mkdir -p {}", q(n)),
            Action::DeleteFile(n) => format!("rm {}", q(n)),
            Action::DeleteDir(n) => format!("rm -r {}", q(n)),
            Action::Rename(s, d) => format!("mv {} {}", q(s), q(d)),
            Action::Copy(s, d) if s.is_dir() => format!("cp -R {} {}", q(s), q(d)),
            Action::Copy(s, d) => format!("cp {} {}", q(s), q(d)),
        }
    }

    /// Run this action on the filesystem
    /// Never overwrites an existing path, the planner makes sure it was removed first
    pub fn run(&self) -> io::Result<()> {
        match self {
            Action::CreateFile(n) => {
                fs::File::create_new(n)?;
            }
            Action::CreateDir(n) => fs::create_dir_all(n)?,
            Action::DeleteFile(n) => fs::remove_file(n)?,
            Action::DeleteDir(n) => fs::remove_dir_all(n)?,
            Action::Rename(s, d) => {
                // on case insensitive filesystems `a -> A` sees `A` as taken, but it is `a` itself
                if taken(d) && !same_file(s, d)? {
                    return Err(already_exists(d));
                }
                fs::rename(s, d)?
            }
            Action::Copy(s, d) => copy(s, d)?,
        }
        Ok(())
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
