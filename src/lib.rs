use crate::diff::Diff;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::{fmt, fs, io};
use typed_builder::TypedBuilder;

pub mod diff;
pub mod parse;
pub mod planner;

/// A single file or directory captured from a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Hexadecimal ID
    pub id: String,
    /// Display name, without trailing `/`.
    pub name: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entry_type = if self.is_dir { "/" } else { "" };
        write!(f, ":{} {}{}", self.id, self.name, entry_type)
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .is_dir
            .cmp(&self.is_dir)
            .then(self.name.cmp(&other.name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
/// Represent a filesystem operation
pub enum Action {
    /// rm -rf <name>
    DeleteDir(PathBuf),
    /// rm -f <name>
    DeleteFile(PathBuf),
    /// mv <src> <dst>
    Rename(PathBuf, PathBuf),
    /// cp <src> <dst>
    Copy(PathBuf, PathBuf),
    /// touch <name>
    CreateFile(PathBuf),
    /// mkdir -p <name>
    CreateDir(PathBuf),
}

impl Action {
    // /// A shell command representing what this action would do
    // pub fn command(&self) -> String {
    //     match self {
    //         Action::CreateFile(n) => format!("touch {}", n),
    //         Action::CreateDir(n) => format!("mkdir {}", n),
    //         Action::DeleteFile(n) => format!("rm {}", n),
    //         Action::DeleteDir(n) => format!("rm -rf {}", n),
    //         Action::Rename(s, d) => format!("mv {} {}", s, d),
    //         Action::Copy(s, d) => format!("cp {} {}", s, d),
    //     }
    // }

    /// The path this action **removes** (deletes from the filesystem), if any.
    pub fn removes(&self) -> Option<&Path> {
        match self {
            Action::DeleteFile(n) | Action::DeleteDir(n) => Some(n),
            Action::Rename(s, _) => Some(s),
            Action::CreateFile(_) | Action::CreateDir(_) | Action::Copy(_, _) => None,
        }
    }

    /// The path this action **creates** (places on the filesystem), if any.
    pub fn creates(&self) -> Option<&Path> {
        match self {
            Action::CreateFile(n) | Action::CreateDir(n) | Action::Copy(_, n) => Some(n),
            Action::Rename(_, d) => Some(d),
            Action::DeleteFile(_) | Action::DeleteDir(_) => None,
        }
    }

    /// The path this action requires to exist, if any
    pub fn depends_on(&self) -> Option<&Path> {
        match self {
            Action::DeleteFile(n) | Action::DeleteDir(n) => Some(n),
            Action::Rename(s, _) | Action::Copy(s, _) => Some(s),
            Action::CreateFile(n) | Action::CreateDir(n) => n.parent(),
            // Action::CreateFile(_) | Action::CreateDir(_) => None,
        }
    }

    /// Return the path that this action is operating on
    pub fn path(&self) -> &Path {
        match self {
            Action::DeleteFile(p)
            | Action::DeleteDir(p)
            | Action::Rename(p, _)
            | Action::Copy(p, _)
            | Action::CreateFile(p)
            | Action::CreateDir(p) => p,
        }
    }
}

/// Something that did not stop koil, but user should know about
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// The dir is neither on disk nor new in the diff, so its closest parent was opened instead
    DirNotFound {
        /// The dir that user wanted to open
        requested: PathBuf,
        /// The dir that was opened instead
        opened: PathBuf,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::DirNotFound { requested, opened } => write!(
                f,
                "`{}` is not a directory, and is not written in the listing, opened `{}` instead",
                requested.display(),
                opened.display()
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KoilError {
    #[error("IO Error")]
    IOError(#[from] io::Error),

    #[error("`{0}` appears more than once")]
    DuplicatePath(String),

    #[error("Invalid ID: `{0}`, this ID is not recognized")]
    InvalidID(String),

    #[error("`{0}` is not a directory, so it can not be opened")]
    NotADirectory(String),
}

#[derive(Debug, TypedBuilder)]
#[builder(mutators(
    /// Will ignore the given path (never show in the listing)
    /// This can be used to hide the koil listing file
    /// You can call this multiple times to ignore multiple files
    pub fn ignore<P: Into<PathBuf>>(self, path: P) {
        self.ignore.insert(path.into());
    }
))]
pub struct Koil {
    #[builder(default = true)]
    /// If true, the settings label will be shown in the listing
    show_settings: bool,

    #[builder(default = 6)]
    /// The minimum length to use for IDs, IDs can be longer than this but never shorter
    min_id_len: usize,

    #[builder(via_mutators)]
    /// Ignore every path in this set
    ignore: HashSet<PathBuf>,

    // Private fields
    #[builder(default, setter(skip))]
    /// All IDs, pointing to their corresponding path
    ids: Vec<PathBuf>,

    #[builder(default, setter(skip))]
    /// The saved indexes that are shows in the current listing
    current_listing: HashSet<usize>,

    #[builder(default, setter(skip))]
    /// The currently open directory
    current_dir: PathBuf,

    #[builder(default, setter(skip))]
    /// Partial diff, which stores all changes made in other listings
    diff: Diff,
}

impl Default for Koil {
    fn default() -> Self {
        Koil::builder().build()
    }
}

// Public functions
impl Koil {
    /// Open the dir given
    /// If it is a relative path, it will be opened relative to [`Koil::current_dir`]
    /// It can also be a new dir from [`Koil::diff`] that does not exist yet,
    /// then it is opened with an empty listing
    /// If the dir is neither on disk nor in the diff, the closest parent that is gets opened
    /// This never changes [`Koil::diff`], new dirs must be written in the listing
    /// Returns a warning, if the dir was not found and a parent was opened instead
    pub fn open<P: AsRef<Path>>(&mut self, dir: P) -> io::Result<Option<Warning>> {
        let path = resolve(&self.current_dir.join(dir))?;
        let dir = path
            .ancestors()
            .find(|p| p.is_dir() || self.diff.creates_dir(p))
            .ok_or(io::ErrorKind::NotFound)?
            .to_path_buf();
        let warning = (dir != path).then(|| Warning::DirNotFound {
            requested: path,
            opened: dir.clone(),
        });

        self.current_listing.clear();
        let on_disk = dir.is_dir();
        self.current_dir = dir;
        if !on_disk {
            return Ok(warning);
        }

        for item in fs::read_dir(&self.current_dir)? {
            let item = item?;
            if self.ignore.contains(&item.path()) {
                continue;
            }
            let id = self
                .ids
                .iter()
                .position(|p| p == &item.path())
                .unwrap_or_else(|| {
                    self.ids.push(item.path());
                    self.ids.len() - 1
                });
            self.current_listing.insert(id);
        }

        Ok(warning)
    }

    /// The currently open listing, which user should modify
    pub fn listing(&self) -> String {
        let mut lines = Vec::new();

        if self.show_settings {
            let glob = self.current_dir.to_string_lossy();
            let sep = "=".repeat(glob.len().max(42));
            lines.push(sep.clone());
            lines.push(glob.to_string());
            lines.push(sep);
        }

        let mut entries = Vec::new();
        // returns true if this path should be shown in the current listing
        let in_this_listing = |path: &Path| path.parent().is_some_and(|p| p == self.current_dir);

        // IDs from current listing
        for &index in &self.current_listing {
            if let Some((_before, afters)) = self.diff.with_id.get(&index) {
                for path in afters.iter().filter(|p| in_this_listing(p)) {
                    entries.push(Entry {
                        id: self.to_id(index),
                        name: path.file_name().unwrap().to_string_lossy().to_string(),
                        is_dir: self.ids[index].is_dir(),
                    });
                }
            } else {
                entries.push(self.get_entry(index).unwrap())
            }
        }

        // IDs from other folders
        for (&index, (_before, afters)) in &self.diff.with_id {
            // if it is in current_listing, then I already added it above
            if self.current_listing.contains(&index) {
                continue;
            }
            for path in afters.iter().filter(|p| in_this_listing(p)) {
                entries.push(Entry {
                    id: self.to_id(index),
                    name: path.file_name().unwrap().to_string_lossy().to_string(),
                    is_dir: self.ids[index].is_dir(),
                });
            }
        }

        // Add the sorted entries
        entries.sort();
        lines.extend(entries.iter().map(ToString::to_string));

        for (path, &is_dir) in self
            .diff
            .without_id
            .iter()
            .filter(|(p, _)| p.parent().is_some_and(|p| p == self.current_dir))
        {
            let mut name = path.file_name().unwrap().to_string_lossy().to_string();
            if is_dir {
                name.push('/');
            }
            lines.push(name);
        }

        lines.join("\n")
    }

    /// Update the internal diff based on modifications made in `modified_listing`
    /// Returns a warning, if something user asked for was ignored
    pub fn update(&mut self, modified_listing: &str) -> Result<Option<Warning>, KoilError> {
        self.update_parsed(parse::parse_listing(modified_listing))
    }

    /// This will consume the diff, and return the actions required to do what user did
    pub fn compute_actions(&mut self) -> Vec<Action> {
        // `take` will return an owned value, and replace the `&mut` to [`Default::default`]
        let diff = std::mem::take(&mut self.diff);
        diff.compute_actions()
    }

    pub fn get_entry(&self, index: usize) -> Option<Entry> {
        let path = &self.ids.get(index)?;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let id = self.to_id(index);

        Some(Entry {
            id,
            name,
            is_dir: path.is_dir(),
        })
    }

    /// Convert an ID string to index
    /// returned index is guaranteed to be in [`Koil::ids`]
    pub fn id_to_index(&self, id: &str) -> Result<usize, KoilError> {
        match usize::from_str_radix(id, 16) {
            Ok(index) if index < self.ids.len() => Ok(index),
            _ => Err(KoilError::InvalidID(id.to_string())),
        }
    }

    /// Convert an `index` to ID
    pub fn to_id(&self, index: usize) -> String {
        format!("{:0width$x}", index, width = self.min_id_len)
    }
}

// Private functions
impl Koil {
    /// See [`Koil::update`]
    fn update_parsed(
        &mut self,
        modified_listing: parse::ParsedFile,
    ) -> Result<Option<Warning>, KoilError> {
        self.validate(&modified_listing)?;
        let in_this_listing = |path: &Path| path.parent().is_some_and(|p| p == self.current_dir);

        // clear the diff for the current listing, since it is outdated
        self.diff.with_id.retain(|_index, (before, afters)| {
            afters.retain(|p| !in_this_listing(p));
            // if before and after are from this listing, remove this from diff
            // or if before and after are exactly the same, then also remove it
            (!afters.is_empty() || !in_this_listing(before))
                && afters.as_slice() != [before.as_path()]
        });
        self.diff.without_id.retain(|p, _| !in_this_listing(p));

        // if something existed before, but does not exist now, tell the diff that it used to exist
        for &index in &self.current_listing {
            if !modified_listing.with_id.contains_key(&self.to_id(index)) {
                self.diff.add_before_from(index, &self.ids);
            }
        }

        // Creates
        for name in &modified_listing.without_id {
            self.add_create(name)?;
        }

        // Renames / Copy
        for (id, entries) in modified_listing.with_id {
            let index = self.id_to_index(&id)?;
            // if this id only has 1 name, and that name is same as before, user did nothing
            if entries.len() == 1
                && self.current_listing.contains(&index)
                && entries[0].name == self.get_entry(index).unwrap().name
                && !self.diff.with_id.contains_key(&index)
            {
                continue;
            }
            // at this point, user did something...

            // If this ID came from current listing, just update before and after
            if self.current_listing.contains(&index) {
                self.diff.add_before_from(index, &self.ids);

            // If this ID did NOT come from current listing (came from another folder) AND
            // If we are the first ppl to know about this ID being involved in some actions
            } else if !self.diff.with_id.contains_key(&index) {
                // tell diff_before about this ID
                self.diff.add_before_from(index, &self.ids);

                // tell diff_after that this ID still exists in the place where it came from
                // (since if it didn't exist there, then that folder would already add it to diff_before)
                self.diff.push_after(index, self.ids[index].to_path_buf());
            }

            // Add what we see to `diff`
            for entry in entries {
                let path = self.current_dir.join(&entry.name);
                self.diff.push_after(index, path);
            }
        }

        let warning = match modified_listing.selected {
            Some(parse::Selected::Id(id)) => {
                let index = self.id_to_index(&id)?;
                self.open(self.ids[index].clone())?
            }
            // a new dir, that is not created yet
            Some(parse::Selected::New(name)) => match name.strip_suffix('/') {
                Some(dir) => self.open(dir)?,
                None => return Err(KoilError::NotADirectory(name)),
            },
            None => match modified_listing.settings {
                Some(settings) => self.open(settings.glob)?,
                None => None,
            },
        };

        Ok(warning)
    }

    /// Add `name_to_create` to [`Koil::diff`]
    /// If `name_to_create` ends with `/`, a dir will be created
    /// If this is a duplicate, [`KoilError`] is returned
    fn add_create(&mut self, name_to_create: &str) -> Result<(), KoilError> {
        let (path, is_dir) = if let Some(stripped) = name_to_create.strip_suffix('/') {
            (self.current_dir.join(stripped), true)
        } else {
            (self.current_dir.join(name_to_create), false)
        };

        if self.diff.without_id.insert(path, is_dir).is_some() {
            return Err(KoilError::DuplicatePath(name_to_create.to_string()));
        }
        Ok(())
    }

    // TODO: don't validate, make parsed file never have duplicates
    fn validate(&self, parsed: &parse::ParsedFile) -> Result<(), KoilError> {
        let mut seen = HashSet::new();
        for (id, entries) in &parsed.with_id {
            // make sure that the ID is valid
            self.id_to_index(id)?;
            // detect duplicates
            for entry in entries {
                if !seen.insert(&entry.name) {
                    return Err(KoilError::DuplicatePath(entry.name.clone()));
                }
            }
        }

        for name in &parsed.without_id {
            if !seen.insert(name) {
                return Err(KoilError::DuplicatePath(name.clone()));
            }
        }

        Ok(())
    }
}

/// Like [`Path::canonicalize`], but `path` does not need to exist
/// Existing part of the path is canonicalized, the rest is normalized without touching the filesystem
fn resolve(path: &Path) -> io::Result<PathBuf> {
    let mut resolved = PathBuf::new();
    for component in std::path::absolute(path)?.components() {
        match component {
            Component::CurDir => {}
            // `resolved` is canonical (no symlinks), so `..` is just its parent
            Component::ParentDir => {
                resolved.pop();
            }
            c => {
                resolved.push(c);
                if resolved.symlink_metadata().is_ok() {
                    resolved = resolved.canonicalize()?;
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests;
