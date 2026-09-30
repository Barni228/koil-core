use crate::apply::Undo;
use crate::diff::Diff;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::{fmt, fs, io};
use typed_builder::TypedBuilder;

pub mod apply;
pub mod diff;
pub mod planner;
pub mod trash;

/// A stable handle of a path that koil has seen
/// It never starts pointing to a different path, even after navigating or applying
/// A frontend can show it however it likes, or hide it and keep it next to its entry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(pub u64);

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A single file or directory in a listing
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The path this entry was before, `None` if it is new and should be created
    pub id: Option<Id>,
    /// Path relative to the open dir
    /// [`Koil::listing`] only gives plain names, but [`Koil::update`] also takes nested paths
    /// like `dir/A`
    pub name: PathBuf,
    /// Whether this entry is a directory
    /// [`Koil::update`] only uses it for new entries, an existing entry keeps its type
    pub is_dir: bool,
}

impl Entry {
    /// The `..` entry, which only opens the parent dir
    /// [`Koil::listing`] starts with it when [`Settings::show_hidden`] is on, and
    /// [`Koil::update`] ignores it, so it can never be changed
    pub fn parent() -> Entry {
        Entry {
            id: None,
            name: "..".into(),
            is_dir: true,
        }
    }

    /// Whether this is the [`Entry::parent`] entry
    pub fn is_parent(&self) -> bool {
        self.id.is_none() && self.name == Path::new("..")
    }
}

/// How the listing is shown, a frontend usually lets user change these
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Show hidden entries (names starting with `.`), and [`Entry::parent`] to open the
    /// parent dir
    /// Hidden entries that were changed are always shown, so the change is not lost
    pub show_hidden: bool,
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

    #[error("Failed to {action}, {done} of {total} changes were applied")]
    ApplyFailed {
        action: Action,
        /// How many actions ran before this one, they can be undone
        done: usize,
        total: usize,
        #[source]
        source: io::Error,
    },

    #[error("Failed to {step}, {done} of {total} changes were undone")]
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
}

/// Why [`Koil::update`] rejected the entries, nothing was changed
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", join_lines(errors))]
pub struct UpdateError {
    /// Every problem that was found, in the order of the entries
    pub errors: Vec<EntryError>,
}

/// A problem with one of the entries given to [`Koil::update`]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Entry {entry}: {kind}")]
pub struct EntryError {
    /// Index of the entry in the entries given to [`Koil::update`]
    pub entry: usize,
    pub kind: EntryErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryErrorKind {
    #[error("ID `{0}` is not recognized")]
    UnknownId(Id),

    #[error(
        "`{}` is not a valid name, it can not be empty, absolute, or contain `.` or `..`",
        .0.display()
    )]
    InvalidName(PathBuf),

    #[error("`{}` appears more than once", path.display())]
    Duplicate {
        path: PathBuf,
        /// Index of the entry where this path appears first
        first: usize,
    },

    /// A parent of the entry is a file, so nothing can be inside it
    #[error("`{}` is not a directory", .0.display())]
    NotADirectory(PathBuf),
}

fn join_lines(errors: &[EntryError]) -> String {
    let lines: Vec<String> = errors.iter().map(ToString::to_string).collect();
    lines.join("\n")
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
    #[builder(default)]
    #[serde(default)]
    /// How the listing is shown
    settings: Settings,

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
            let path = item?.path();
            if self.ignore.contains(&path) {
                continue;
            }
            let index = self.ids.iter().position(|p| p == &path);
            // a changed entry stays, so it is not taken for deleted in the next update
            let changed = index.is_some_and(|i| self.diff.with_id.contains_key(&i));
            if is_hidden(&path) && !self.settings.show_hidden && !changed {
                continue;
            }
            let index = index.unwrap_or_else(|| {
                self.ids.push(path);
                self.ids.len() - 1
            });
            self.current_listing.insert(index);
        }

        Ok(warning)
    }

    /// The currently open directory, as an absolute path
    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    /// How the listing is shown
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Change how the listing is shown, and reopen [`Koil::current_dir`] with the new settings
    /// Returns a warning, if the open dir is gone and its closest parent was opened instead
    pub fn set_settings(&mut self, settings: Settings) -> io::Result<Option<Warning>> {
        self.settings = settings;
        self.reopen()
    }

    /// The entries of the open dir, with every change made so far, which user should modify
    /// Starts with [`Entry::parent`] if [`Settings::show_hidden`] is on, then existing entries
    /// (dirs, then files, each sorted by name), then new entries
    pub fn listing(&self) -> Vec<Entry> {
        // returns true if this path should be shown in the current listing
        let in_this_listing = |path: &Path| path.parent().is_some_and(|p| p == self.current_dir);
        let entry = |index: usize, path: &Path| Entry {
            id: Some(to_id(index)),
            name: path.file_name().unwrap().into(),
            is_dir: self.ids[index].is_dir(),
        };

        let mut entries = Vec::new();

        // IDs from current listing
        for &index in &self.current_listing {
            match self.diff.with_id.get(&index) {
                Some((_before, afters)) => entries.extend(
                    afters
                        .iter()
                        .filter(|p| in_this_listing(p))
                        .map(|p| entry(index, p)),
                ),
                None => entries.push(entry(index, &self.ids[index])),
            }
        }

        // IDs from other folders
        for (&index, (_before, afters)) in &self.diff.with_id {
            // if it is in current_listing, then I already added it above
            if self.current_listing.contains(&index) {
                continue;
            }
            entries.extend(
                afters
                    .iter()
                    .filter(|p| in_this_listing(p))
                    .map(|p| entry(index, p)),
            );
        }

        // dirs first, then by name
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

        let mut new: Vec<Entry> = self
            .diff
            .without_id
            .iter()
            .filter(|(p, _)| in_this_listing(p))
            .map(|(p, &is_dir)| Entry {
                id: None,
                name: p.file_name().unwrap().into(),
                is_dir,
            })
            .collect();
        new.sort_by(|a, b| a.name.cmp(&b.name));
        entries.extend(new);

        if self.settings.show_hidden && self.current_dir.parent().is_some() {
            entries.insert(0, Entry::parent());
        }
        entries
    }

    /// Update the internal diff with `entries`, the listing of the open dir as user edited it
    /// An entry of the open dir that is missing is deleted, a changed name is a rename (or a
    /// move, if it is a path in another dir), an ID written more than once is a copy, and an
    /// entry without an ID is created, along with its parents that do not exist yet
    /// [`Entry::parent`] is ignored, it can only be opened
    /// Changes made in the listings of other dirs are kept
    /// If any entry is invalid, nothing is changed, and every problem is returned
    pub fn update(&mut self, entries: &[Entry]) -> Result<(), UpdateError> {
        let mut updated = self.clone();
        let mut errors = updated.update_entries(entries);
        if !errors.is_empty() {
            errors.sort_by_key(|e| e.entry);
            return Err(UpdateError { errors });
        }
        *self = updated;
        Ok(())
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
        self.reopen()
    }

    /// Save the whole session (IDs, open dir, diff, and undo steps), so it can be continued later
    /// with [`Koil::load_state`], even by a different process
    pub fn save_state(&self) -> String {
        serde_json::to_string(self).expect("koil state is always valid JSON")
    }

    /// Continue a session saved with [`Koil::save_state`]
    pub fn load_state(state: &str) -> serde_json::Result<Koil> {
        serde_json::from_str(state)
    }

    /// The path that `id` pointed to when koil first saw it, `None` if the ID is not known
    /// It stays the same until the changes are applied, even if the entry was renamed
    pub fn path_of(&self, id: Id) -> Option<&Path> {
        self.ids.get(self.index_of(id)?).map(PathBuf::as_path)
    }

    /// The ID of `path`, `None` if koil has not seen it in any dir it opened
    pub fn id_of(&self, path: &Path) -> Option<Id> {
        self.ids.iter().position(|p| p == path).map(to_id)
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

    /// Open [`Koil::current_dir`] again, so it shows what is on disk now
    /// Does nothing if no dir was opened yet
    fn reopen(&mut self) -> io::Result<Option<Warning>> {
        if self.current_dir.as_os_str().is_empty() {
            return Ok(None);
        }
        let dir = std::mem::take(&mut self.current_dir);
        self.open(dir)
    }

    /// Fail if there are changes that are not applied yet
    fn check_nothing_pending(&self) -> Result<(), KoilError> {
        if self.compute_actions().is_empty() {
            Ok(())
        } else {
            Err(KoilError::PendingChanges)
        }
    }

    /// The index of `id` in [`Koil::ids`], `None` if it is not there
    fn index_of(&self, id: Id) -> Option<usize> {
        usize::try_from(id.0).ok().filter(|&i| i < self.ids.len())
    }

    /// See [`Koil::update`]
    /// Returns every problem found, if there are any, `self` is left half updated
    fn update_entries(&mut self, entries: &[Entry]) -> Vec<EntryError> {
        let mut errors = Vec::new();
        let mut error = |entry, kind| errors.push(EntryError { entry, kind });

        // each written path, and the entry that has it first
        let mut seen: HashMap<PathBuf, usize> = HashMap::new();
        // index of each ID, and every (entry, path) it is written as
        let mut with_id: BTreeMap<usize, Vec<(usize, PathBuf)>> = BTreeMap::new();
        // (entry, path, is_dir) of every new entry
        let mut without_id = Vec::new();

        for (i, entry) in entries.iter().enumerate() {
            if entry.is_parent() {
                continue;
            }
            let index = match entry.id.map(|id| (id, self.index_of(id))) {
                Some((_, Some(index))) => Some(index),
                Some((id, None)) => {
                    error(i, EntryErrorKind::UnknownId(id));
                    continue;
                }
                None => None,
            };
            let Some(path) = relative_path(&entry.name) else {
                error(i, EntryErrorKind::InvalidName(entry.name.clone()));
                continue;
            };
            if let Some(&first) = seen.get(&path) {
                error(i, EntryErrorKind::Duplicate { path, first });
                continue;
            }
            seen.insert(path.clone(), i);
            match index {
                Some(index) => with_id.entry(index).or_default().push((i, path)),
                None => without_id.push((i, path, entry.is_dir)),
            }
        }

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
            if !with_id.contains_key(&index) {
                self.diff.add_before_from(index, &self.ids);
            }
        }

        // every (entry, path) written in this listing, to create its missing parents later
        let mut written = Vec::new();

        // Creates
        for (i, path, is_dir) in without_id {
            let path = self.current_dir.join(path);
            self.diff.without_id.insert(path.clone(), is_dir);
            written.push((i, path));
        }

        // Renames / Copy
        for (index, names) in with_id {
            // if this id only has 1 name, and that name is same as before, user did nothing
            if let [(_, path)] = names.as_slice()
                && self.current_listing.contains(&index)
                && self.current_dir.join(path) == self.ids[index]
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
            for (i, path) in names {
                let path = self.current_dir.join(path);
                self.diff.push_after(index, path.clone());
                written.push((i, path));
            }
        }

        // `dir/A` also means create `dir/`, if it does not exist yet
        for (i, path) in written {
            if let Err(parent) = self.add_missing_parents(&path) {
                errors.push(EntryError {
                    entry: i,
                    kind: EntryErrorKind::NotADirectory(parent),
                });
            }
        }

        errors
    }

    /// Add every parent of `path` (inside [`Koil::current_dir`]) that does not exist yet
    /// to [`Koil::diff`] as a new dir
    /// Fails with the parent (relative to [`Koil::current_dir`]) that is a file
    fn add_missing_parents(&mut self, path: &Path) -> Result<(), PathBuf> {
        let parents: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(&self.current_dir) && *p != self.current_dir)
            .map(Path::to_path_buf)
            .collect();

        for parent in parents {
            let not_a_dir = || {
                parent
                    .strip_prefix(&self.current_dir)
                    .unwrap()
                    .to_path_buf()
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
}

/// Whether the name of `path` starts with `.`
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."))
}

fn to_id(index: usize) -> Id {
    Id(index as u64)
}

/// `name` as a path relative to the listing's dir, `None` if it is empty, absolute,
/// or has a `.` or `..` part
/// Name can have `/` inside, like `dir/A`
fn relative_path(name: &Path) -> Option<PathBuf> {
    let raw = name.to_string_lossy();
    // `Path::components` ignores `.` in the middle of a path, so check the raw parts
    if raw.is_empty() || name.has_root() || raw.split('/').any(|p| p == "." || p == "..") {
        return None;
    }
    Some(name.components().collect())
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
