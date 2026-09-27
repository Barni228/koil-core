use crate::apply::Undo;
use crate::diff::Diff;
use serde::{Deserialize, Serialize};
use sqids::Sqids;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::{fmt, fs, io};
use typed_builder::TypedBuilder;

pub mod apply;
pub mod diff;
pub mod parse;
pub mod planner;
pub mod session;
pub mod trash;

/// A single file or directory captured from a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// ID encoded with [`Sqids`]
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

/// What [`Koil::apply`] or [`Koil::undo`] did
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// How many changes were made on the filesystem
    pub changes: usize,
    /// Set if the open dir is gone now, and its closest parent was opened instead
    pub warning: Option<Warning>,
}

#[derive(Debug, thiserror::Error)]
pub enum KoilError {
    #[error("IO Error")]
    IOError(#[from] io::Error),

    #[error("Failed to run `{action}`, {done} of {total} changes were applied")]
    ApplyFailed {
        action: Action,
        /// How many actions ran before this one, they can be undone
        done: usize,
        total: usize,
        #[source]
        source: io::Error,
    },

    #[error("Failed to run `{step}`, {done} of {total} changes were undone")]
    UndoFailed {
        step: Undo,
        /// How many steps ran before this one, the rest can be undone again
        done: usize,
        total: usize,
        #[source]
        source: io::Error,
    },

    #[error("Some changes are not applied yet, apply them or remove them from the listing first")]
    PendingChanges,

    #[error("Nothing to undo")]
    NothingToUndo,

    #[error("`{0}` appears more than once")]
    DuplicatePath(String),

    #[error("Invalid ID: `{0}`, this ID is not recognized")]
    InvalidID(String),

    #[error("`{0}` is not a directory")]
    NotADirectory(String),

    #[error("`{0}` is not a valid name, it can not be empty, absolute, or contain `.` or `..`")]
    InvalidName(String),
}

#[derive(Debug, Clone, TypedBuilder, Serialize, Deserialize)]
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
    #[allow(dead_code)] // Only used to build `sqids`
    min_id_len: u8,

    #[builder(via_mutators)]
    /// Ignore every path in this set
    ignore: HashSet<PathBuf>,

    // Private fields
    #[builder(default = new_sqids(min_id_len), setter(skip))]
    #[serde(skip)] // rebuilt from `min_id_len` in [`Koil::load_state`]
    /// Encodes indexes into random looking IDs, so they don't look sequential
    sqids: Sqids,

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

    #[builder(default, setter(skip))]
    #[serde(default)]
    /// Steps that revert each apply of this session, the last apply is last
    undo: Vec<Vec<Undo>>,
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
    /// If the listing is invalid, nothing is changed
    pub fn update(&mut self, modified_listing: &str) -> Result<Option<Warning>, KoilError> {
        let mut updated = self.clone();
        let warning = updated.update_parsed(parse::parse_listing(modified_listing))?;
        *self = updated;
        Ok(warning)
    }

    /// The actions that [`Koil::apply`] would run to do what user did, in order
    pub fn compute_actions(&self) -> Vec<Action> {
        self.diff.clone().compute_actions()
    }

    /// Run every change made so far on the filesystem, deleted paths are moved to the trash
    /// Then the listing is refreshed, and the changes can be reverted with [`Koil::undo`]
    /// If an action fails, the rest are not run, and every change that was not applied is
    /// forgotten, the ones that were applied can still be undone
    pub fn apply(&mut self) -> Result<Report, KoilError> {
        let actions = self.compute_actions();
        let mut steps = Vec::new();
        let mut result = Ok(());
        for (i, action) in actions.iter().enumerate() {
            match action.run() {
                Ok(step) => steps.extend(step),
                Err(source) => {
                    result = Err(KoilError::ApplyFailed {
                        action: action.clone(),
                        done: i,
                        total: actions.len(),
                        source,
                    });
                    break;
                }
            }
        }
        // undo runs the steps backwards
        steps.reverse();
        self.push_undo(steps);
        // even if some action failed, others changed the filesystem
        let refreshed = self.refresh();
        result?;
        Ok(Report {
            changes: actions.len(),
            warning: refreshed?,
        })
    }

    /// The steps that [`Koil::undo`] would run to revert the last apply, in order
    /// `None` if there is nothing to undo
    /// Fails if there are changes that are not applied, since undo would make them wrong
    pub fn undo_steps(&self) -> Result<Option<&[Undo]>, KoilError> {
        self.check_nothing_pending()?;
        Ok(self.undo.last().map(Vec::as_slice))
    }

    /// Revert the last apply of this session, deleted paths come back from the trash,
    /// and created paths are moved to the trash
    /// If a step fails, the steps that were not run yet stay, so they can be undone later
    pub fn undo(&mut self) -> Result<Report, KoilError> {
        self.check_nothing_pending()?;
        let steps = self.undo.pop().ok_or(KoilError::NothingToUndo)?;
        let mut result = Ok(());
        for (i, step) in steps.iter().enumerate() {
            if let Err(source) = step.run() {
                self.push_undo(steps[i..].to_vec());
                result = Err(KoilError::UndoFailed {
                    step: step.clone(),
                    done: i,
                    total: steps.len(),
                    source,
                });
                break;
            }
        }
        let refreshed = self.refresh();
        result?;
        Ok(Report {
            changes: steps.len(),
            warning: refreshed?,
        })
    }

    /// Forget every change, and reopen [`Koil::current_dir`] from the filesystem
    /// Use this after the actions were applied, so the listing shows what is on disk now
    /// IDs are kept, so an ID never starts pointing to a different path
    pub fn refresh(&mut self) -> io::Result<Option<Warning>> {
        self.diff = Diff::default();
        let dir = std::mem::take(&mut self.current_dir);
        self.open(dir)
    }

    /// Save the whole session (IDs, open dir, diff, and undo steps), so it can be continued later
    /// with [`Koil::load_state`], even by a different process
    pub fn save_state(&self) -> String {
        serde_json::to_string(self).expect("koil state is always valid JSON")
    }

    /// Continue a session saved with [`Koil::save_state`]
    pub fn load_state(state: &str) -> serde_json::Result<Koil> {
        let mut koil: Koil = serde_json::from_str(state)?;
        koil.sqids = new_sqids(&koil.min_id_len);
        Ok(koil)
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
        match self.sqids.decode(id)[..] {
            // Many strings decode to the same number, only accept the canonical one
            [index] if (index as usize) < self.ids.len() && self.to_id(index as usize) == id => {
                Ok(index as usize)
            }
            _ => Err(KoilError::InvalidID(id.to_string())),
        }
    }

    /// Convert an `index` to ID
    pub fn to_id(&self, index: usize) -> String {
        self.sqids
            .encode(&[index as u64])
            .expect("the blocklist can not block every ID")
    }
}

// Private functions
impl Koil {
    /// Remember `steps` that revert an apply, so [`Koil::undo`] can run them later
    /// `steps` must be in the order they should run, nothing is remembered if it is empty
    fn push_undo(&mut self, mut steps: Vec<Undo>) {
        // a path inside a trashed dir goes to the trash with it, not as a separate item
        let trashed: Vec<PathBuf> = steps
            .iter()
            .filter_map(|s| match s {
                Undo::Trash(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        steps.retain(|s| match s {
            Undo::Trash(p) => !trashed.iter().any(|dir| p != dir && p.starts_with(dir)),
            _ => true,
        });

        if !steps.is_empty() {
            self.undo.push(steps);
        }
    }

    /// Fail if there are changes that are not applied yet
    fn check_nothing_pending(&self) -> Result<(), KoilError> {
        if self.compute_actions().is_empty() {
            Ok(())
        } else {
            Err(KoilError::PendingChanges)
        }
    }

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

        // every path written in this listing, to create its missing parents later
        let mut written = Vec::new();

        // Creates
        for name in &modified_listing.without_id {
            written.push(self.add_create(name)?);
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
                let path = self.current_dir.join(parse_name(&entry.name)?.0);
                self.diff.push_after(index, path.clone());
                written.push(path);
            }
        }

        // `dir/A` also means create `dir/`, if it does not exist yet
        for path in written {
            self.add_missing_parents(&path)?;
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

    /// Add `name_to_create` to [`Koil::diff`], and return its path
    /// If `name_to_create` ends with `/`, a dir will be created
    fn add_create(&mut self, name_to_create: &str) -> Result<PathBuf, KoilError> {
        let (path, is_dir) = parse_name(name_to_create)?;
        let path = self.current_dir.join(path);
        self.diff.without_id.insert(path.clone(), is_dir);
        Ok(path)
    }

    /// Add every parent of `path` (inside [`Koil::current_dir`]) that does not exist yet
    /// to [`Koil::diff`] as a new dir
    fn add_missing_parents(&mut self, path: &Path) -> Result<(), KoilError> {
        let parents: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(&self.current_dir) && *p != self.current_dir)
            .map(Path::to_path_buf)
            .collect();

        for parent in parents {
            let not_a_dir = || {
                let name = parent.strip_prefix(&self.current_dir).unwrap();
                KoilError::NotADirectory(name.to_string_lossy().to_string())
            };
            if parent.symlink_metadata().is_ok() {
                if parent.is_dir() {
                    break;
                }
                return Err(not_a_dir());
            }
            // a dir that was renamed or copied to this path
            let is_after = |(_, afters): &(PathBuf, Vec<PathBuf>)| afters.contains(&parent);
            match self.diff.without_id.get(&parent) {
                Some(true) => {}
                Some(false) => return Err(not_a_dir()),
                None if self.diff.with_id.values().any(is_after) => {}
                None => {
                    self.diff.without_id.insert(parent, true);
                }
            }
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
                if !seen.insert(parse_name(&entry.name)?.0) {
                    return Err(KoilError::DuplicatePath(entry.name.clone()));
                }
            }
        }

        for name in &parsed.without_id {
            if !seen.insert(parse_name(name)?.0) {
                return Err(KoilError::DuplicatePath(name.clone()));
            }
        }

        Ok(())
    }
}

/// Convert a name from the listing to a path relative to the listing's dir,
/// and whether it is a dir (ends with `/`)
/// Name can have `/` inside, like `dir/A`
fn parse_name(name: &str) -> Result<(PathBuf, bool), KoilError> {
    let (stripped, is_dir) = match name.strip_suffix('/') {
        Some(stripped) => (stripped, true),
        None => (name, false),
    };
    let path = Path::new(stripped);
    // `Path::components` ignores `.` in the middle of a path, so check the raw parts
    if stripped.is_empty() || path.has_root() || stripped.split('/').any(|p| p == "." || p == "..")
    {
        return Err(KoilError::InvalidName(name.to_string()));
    }
    Ok((path.components().collect(), is_dir))
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

/// Only lowercase letters and digits, so IDs are easy to type
fn new_sqids(min_id_len: &u8) -> Sqids {
    // default alphabet is every lowercase and uppercase letter, and digits
    Sqids::builder()
        .alphabet("abcdefghijklmnopqrstuvwxyz0123456789".chars().collect())
        .min_length(*min_id_len)
        .build()
        .expect("the alphabet is valid")
}
