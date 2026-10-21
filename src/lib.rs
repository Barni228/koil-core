#![doc = include_str!("../README.md")]

use crate::apply::{Blocked, Undo, inside, same_file};
use crate::diff::Diff;
use crate::error::io_error;
use crate::pattern::{View, Walked};
use crate::sync::{FileKey, Seen};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf, is_separator};
use std::time::SystemTime;
use std::{fmt, io};
use typed_builder::TypedBuilder;

pub mod apply;
mod diff;
mod error;
mod location;
mod names;
mod pattern;
mod planner;
mod serde_path;
mod sort;
mod sync;
pub mod trash;

pub use error::{
    EntryError, EntryErrorKind, EntryWarning, EntryWarningKind, GlobError, KoilError, OpenError,
    OverLimit, UpdateError, UpdateOpenError, Warning,
};
pub use location::{Completion, Location};
pub use pattern::{Limits, Pattern};
pub use sort::{Metadata, Sort, SortBy, disk_size};
pub use sync::{Conflict, ConflictKind, Edit, Synced, Watched};

/// A stable handle of a path that koil has seen
/// It never starts pointing to a different file, even after navigating or applying, but it
/// follows its file when [`Koil::sync`] sees it renamed or moved on disk
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
    #[serde(with = "serde_path")]
    pub name: PathBuf,
    /// Whether this entry is a directory
    /// [`Koil::update`] only uses it for new entries, an existing entry keeps its type
    pub is_dir: bool,
}

/// How the listing is shown, a frontend usually lets user change these
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Show hidden entries (names starting with `.`)
    /// Hidden entries that were changed are always shown, so the change is not lost
    pub show_hidden: bool,

    /// Hide paths that git ignores (`.gitignore` files, `.git/info/exclude`, and the global
    /// excludes file), and the `.git` dir
    /// Like git, it only works inside a git repo, and reads `.gitignore` files up to its root
    /// Ignored entries that were changed are always shown, so the change is not lost
    pub respect_gitignore: bool,

    /// [`Koil::open`] reads patterns as regexes, instead of globs
    /// A pattern that is already open stays as it is until something else is opened
    pub regex: bool,

    /// The order of [`Koil::listing`]
    pub sort: Sort,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
/// Represent a filesystem operation
pub enum Action {
    /// `rm -rf <name>`
    DeleteDir(PathBuf),
    /// `rm -f <name>`
    DeleteFile(PathBuf),
    /// `mv <src> <dst>`
    Rename(PathBuf, PathBuf),
    /// `cp <src> <dst>`
    Copy(PathBuf, PathBuf),
    /// `touch <name>`
    CreateFile(PathBuf),
    /// `mkdir -p <name>`
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

/// What [`Koil::update_and_open`] did
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    /// Names that can be used, but are not recommended, see [`Koil::update`]
    pub warnings: Vec<EntryWarning>,
    /// Set if the open dir was read again with the new settings, and it is gone, or its pattern
    /// goes over [`Limits`] with them, so something else was opened (see [`Koil::set_settings`])
    pub warning: Option<Warning>,
    /// Whether the listing shows other entries now: another dir or pattern is open, or the
    /// settings show or hide other entries. A frontend should then show the listing as a new
    /// one, as the old one's text is of another view, which must not be read as this one's
    pub moved: bool,
}

/// A change that applying would make, see [`Koil::changes`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub action: Action,
    /// The indexes of the changes (in the list this one is in) that it can not be applied
    /// without, only directly: the one that moves or deletes what is at the path it creates,
    /// and the ones that create the new dir it creates something in
    /// The renames of a cycle (like swapping two names) need each other
    pub needs: Vec<usize>,
}

/// An apply (or [`Koil::create_now`]) that can be reverted, one of [`Koil::history`]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Applied {
    /// When it was applied, which also tells it apart from the others
    pub time: SystemTime,
    /// The dir that was open ([`Koil::current_dir`]), so a frontend can show its paths
    /// relative to it
    #[serde(with = "serde_path")]
    pub dir: PathBuf,
    /// The steps that revert it, in the order they run
    pub steps: Vec<Undo>,
}

/// An apply of [`Koil::history`], as [`Koil::undo_only`] can revert it, see
/// [`Koil::undoable`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undoable {
    pub applied: Applied,
    /// The indexes of the newer applies (in the list this one is in) that it can not be undone
    /// without, only directly: those that changed a path it changed, or one inside it or that
    /// it is in, so undoing them first puts back what it changed
    pub needs: Vec<usize>,
    /// Why it can not be undone now, with the applies it needs (all of them, not only
    /// directly): the first step that can not run, and why, `None` if it can
    pub blocked: Option<(Undo, Blocked)>,
}

/// What [`Koil::apply`] or [`Koil::undo`] did
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// How many changes were made on the filesystem
    pub changes: usize,
    /// Set if the open dir is gone now, and its closest parent was opened instead
    pub warning: Option<Warning>,
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
    #[serde(with = "serde_path::many")]
    /// Ignore every path in this set
    ignore: HashSet<PathBuf>,

    #[builder(default)]
    #[serde(default)]
    /// How much opening a pattern can read
    limits: Limits,

    // Private fields
    #[builder(default, setter(skip))]
    #[serde(with = "serde_path::many")]
    /// All IDs, pointing to their corresponding path
    ids: Vec<PathBuf>,

    #[builder(default, setter(skip))]
    /// The saved indexes that are shows in the current listing
    current_listing: HashSet<usize>,

    #[builder(default, setter(skip))]
    #[serde(with = "serde_path")]
    /// The currently open directory, or the base dir of [`Koil::pattern`]
    current_dir: PathBuf,

    #[builder(default, setter(skip))]
    #[serde(default)]
    /// The open pattern, relative to [`Koil::current_dir`], `None` if a plain dir is open
    pattern: Option<Pattern>,

    #[builder(default, setter(skip))]
    /// Partial diff, which stores all changes made in other listings
    diff: Diff,

    #[builder(default, setter(skip))]
    #[serde(default)]
    /// The applies that can be reverted, the last apply is last (see [`Koil::history`])
    undo: Vec<Applied>,

    #[builder(default, setter(skip))]
    #[serde(skip)]
    /// What was at the path of each ID when koil last read it, so [`Koil::sync`] can find it
    /// again after it is renamed or moved
    seen: HashMap<usize, Seen>,

    #[builder(default, setter(skip))]
    #[serde(skip)]
    /// [`Koil::current_dir`] and every dir it is in, as they were on disk when it was read, so
    /// [`Koil::sync`] can follow it when one of them is renamed or moved
    dirs_seen: Vec<(PathBuf, FileKey)>,
}

impl Default for Koil {
    fn default() -> Self {
        Koil::builder().build()
    }
}

// Public functions
impl Koil {
    /// Open `location`, which is either a dir, or a pattern like `src/**/*.rs`, read as a glob,
    /// or as a regex if [`Settings::regex`] is on
    /// If it is relative, it will be opened relative to [`Koil::current_dir`]
    /// A dir can also be a new dir that was written in a listing, but does not exist yet,
    /// then it is opened with an empty listing
    /// A pattern shows every file and dir whose path matches it, relative to its base dir:
    /// the dirs before the first part with a special character (`*?[{` for a glob, and
    /// `.,*+?()[]{}|^$\` for a regex). A dir's path has a `/` at its end, so `,*/` shows only
    /// dirs, and `,*` only files. A glob's `*` never matches `/`, and a regex must match the
    /// whole path, where `,` is any character except `/`
    /// Only `/` separates, also on Windows: a plain path with `\` (like one pasted from
    /// Windows) opens, and [`Koil::location`] then shows it with `/`, but in a pattern `\` is
    /// an escape, as everywhere else (so `C:\src\*.rs` is not the glob `*.rs` in `src`)
    /// A pattern fails with [`OverLimit`] if it matches or has to search more paths than
    /// [`Limits`] allow, dirs that nothing inside can match are never searched
    /// A path that is a dir is always opened as a dir, even if its name looks like a pattern
    /// It is read like a shell reads a path, without splitting it at spaces: what is quoted
    /// (`'my dir'`, `"my dir"`), or escaped with `\` (`my\ dir`, but not on Windows, where `\`
    /// is a separator) is never special in a pattern (`"[1]"*` is the glob `\[1\]*`), and
    /// [`Koil::location`] then shows it without quotes, and `~/` at its start is the home dir,
    /// see [`Koil::read_location`]
    /// The dir (or the base dir of the pattern) must be on disk or new in the diff, else it
    /// fails with [`OpenError::NotFound`], or [`OpenError::NotADirectory`] if it is a file
    /// This never adds changes, new dirs must be written in the listing
    pub fn open<P: AsRef<Path>>(&mut self, location: P) -> Result<(), OpenError> {
        self.atomically(|koil| koil.open_in_place(location.as_ref()))
    }

    /// [`Koil::open`] `location` with `settings`, as one step: if it fails, nothing changes,
    /// the settings neither. Unlike [`Koil::set_settings`] and then `open`, what is open now is
    /// not read again with them first
    /// This is how a frontend opens something when it has no listing to read (it shows
    /// something else), see [`Koil::update_and_open`] for when it has
    pub fn open_with<P: AsRef<Path>>(
        &mut self,
        location: P,
        settings: Settings,
    ) -> Result<(), OpenError> {
        self.atomically(|koil| {
            koil.settings = settings;
            koil.open_in_place(location.as_ref())
        })
    }

    /// The currently open directory as an absolute path, or the base dir of [`Koil::pattern`]
    /// Names of entries are relative to it
    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    /// The open pattern, relative to [`Koil::current_dir`], `None` if a plain dir is open
    pub fn pattern(&self) -> Option<&Pattern> {
        self.pattern.as_ref()
    }

    /// What is open, for frontends to show, as it can be given to [`Koil::open`] again:
    /// [`Koil::current_dir`], then a `/` and [`Koil::pattern`] if there is one
    /// It only has `/`, also on Windows (`C:/src/*.rs`), so a pattern can be written after it
    pub fn location(&self) -> PathBuf {
        let dir = with_slashes(&self.current_dir);
        let Some(pattern) = &self.pattern else {
            return dir;
        };
        let mut location = dir.into_os_string();
        // a root, like `/` or `C:/`, already ends with one
        if !location.to_string_lossy().ends_with(is_separator) {
            location.push("/");
        }
        location.push(pattern.as_str());
        location.into()
    }

    /// How the listing is shown
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Change how the listing is shown, and reopen [`Koil::current_dir`] with the new settings
    /// Returns a warning, if the open dir is gone and its closest parent was opened instead, or
    /// the open pattern goes over [`Limits`] now, and its base dir was opened instead
    pub fn set_settings(&mut self, settings: Settings) -> io::Result<Option<Warning>> {
        self.settings = settings;
        self.reopen()
    }

    /// The entries of the open dir or glob, with every change made so far, which user should
    /// modify
    /// Existing entries (dirs, then files), then new entries, each sorted as [`Settings::sort`]
    /// says (see [`Sort`])
    /// It never has `..`, which a frontend opens some other way
    pub fn listing(&self) -> Vec<Entry> {
        let view = self.view();
        let in_view = |index: usize, path: &Path| view.contains(path, self.ids[index].is_dir());
        let entry = |index: usize, path: &Path| Entry {
            id: Some(to_id(index)),
            name: self.name(path),
            is_dir: self.ids[index].is_dir(),
        };

        let mut entries = Vec::new();

        // IDs from current listing, and changed IDs from other folders
        let others = (self.diff.with_id.keys()).filter(|i| !self.current_listing.contains(i));
        for &index in self.current_listing.iter().chain(others) {
            match self.diff.with_id.get(&index) {
                Some((_before, afters)) => entries.extend(
                    afters
                        .iter()
                        .filter(|p| in_view(index, p))
                        .map(|p| entry(index, p)),
                ),
                None => entries.push(entry(index, &self.ids[index])),
            }
        }

        // dirs that are created because something new is inside them are shown too
        let parents = self.diff.missing_parents().into_iter().map(|p| (p, true));
        let created = self
            .diff
            .without_id
            .iter()
            .map(|(p, &is_dir)| (p.clone(), is_dir));
        let new = created
            .chain(parents)
            .filter(|(p, is_dir)| view.contains(p, *is_dir))
            .map(|(p, is_dir)| Entry {
                id: None,
                name: self.name(&p),
                is_dir,
            });
        entries.extend(new);

        // what is on disk is read once per entry, not once per comparison
        let mut sorted: Vec<(Entry, Option<Metadata>)> = (entries.into_iter())
            .map(|entry| {
                let meta = self.sort_metadata(&entry);
                (entry, meta)
            })
            .collect();
        sorted
            .sort_by(|(a, a_meta), (b, b_meta)| self.order(a, a_meta.as_ref(), b, b_meta.as_ref()));
        sorted.into_iter().map(|(entry, _)| entry).collect()
    }

    /// Update the internal diff with `entries`, the listing of the open dir as user edited it
    /// An entry of the open dir that is missing is deleted, a changed name is a rename (or a
    /// move, if it is a path in another dir), an ID written more than once is a copy, and an
    /// entry without an ID is created, along with its parents that do not exist yet
    /// Changes made in the listings of other dirs are kept
    /// If any entry is invalid, nothing is changed, and every problem is returned
    /// Returns warnings about names that can be used, but are not recommended
    pub fn update(&mut self, entries: &[Entry]) -> Result<Vec<EntryWarning>, UpdateError> {
        self.atomically(|koil| koil.update_entries(entries))
    }

    /// What [`Koil::update`] would return for `entries`, without changing anything
    /// A frontend can use it to show problems while user is still editing
    pub fn check(&self, entries: &[Entry]) -> Result<Vec<EntryWarning>, UpdateError> {
        self.clone().update_entries(entries)
    }

    /// [`Koil::update`] with `entries`, then use `settings`, then [`Koil::open`] `location`
    /// (if there is one) with them, as one step: if any of it fails, nothing changes
    /// This is how a frontend reads an edited listing, and then shows what user asked for. The
    /// entries must be read with the settings they were listed with, as `update` takes every
    /// listed entry that is missing as deleted: read with [`Settings::show_hidden`] just
    /// turned on, a listing that does not show the hidden entries yet would delete them all
    /// Without `location`, the open dir is read again if the settings changed
    pub fn update_and_open(
        &mut self,
        entries: &[Entry],
        settings: Settings,
        location: Option<&Path>,
    ) -> Result<Updated, UpdateOpenError> {
        // in a copy, so nothing changes if it fails
        let mut updated = self.clone();
        let warnings = updated.update_entries(entries)?;
        let mut warning = None;
        match location {
            // not read again first, as the location is read with them right away
            Some(location) => {
                updated.settings = settings;
                updated.open_in_place(location)?;
            }
            None if settings != updated.settings => {
                warning = updated.set_settings(settings).map_err(OpenError::from)?;
            }
            None => {}
        }
        let moved = updated.shows() != self.shows();
        *self = updated;
        Ok(Updated {
            warnings,
            warning,
            moved,
        })
    }

    /// The actions that [`Koil::apply`] would run to do what user did, in order
    pub fn compute_actions(&self) -> Vec<Action> {
        self.diff.clone().compute_actions()
    }

    /// Whether there are changes to apply (see [`Koil::compute_actions`]), from the changes
    /// alone, without reading the disk or planning them
    pub fn has_changes(&self) -> bool {
        !self.diff.is_empty()
    }

    /// What applying would change, one [`Change`] per action, in the order they run
    /// Unlike [`Koil::compute_actions`], a rename cycle (like swapping two names) is its
    /// renames, not the steps through a temp path that run it
    /// A frontend can show them to confirm, and apply only the ones user picks with
    /// [`Koil::apply_only`], picking what each needs along with it
    pub fn changes(&self) -> Vec<Change> {
        let actions: Vec<Action> = planner::order(&self.diff.clone().actions())
            .into_iter()
            .flatten()
            .collect();
        let needs = planner::needs(&actions);
        (actions.into_iter().zip(needs))
            .map(|(action, needs)| Change { action, needs })
            .collect()
    }

    /// Run every change made so far on the filesystem, deleted paths are moved to the trash
    /// Then the listing is refreshed, and the changes can be reverted with [`Koil::undo`]
    /// If an action fails, the rest are not run, and every change that was not applied is
    /// forgotten, the ones that were applied can still be undone
    pub fn apply(&mut self) -> Result<Report, KoilError> {
        let actions = self.compute_actions();
        self.run_actions(&actions)
    }

    /// [`Koil::apply`], but only the changes in `picked` (actions of [`Koil::changes`]), and
    /// every other change is forgotten, as the refresh shows what is on disk
    /// A change that needs one that is not picked is not applied either (see
    /// [`Change::needs`]): it would fail (a rename onto a path that is still taken), or do
    /// what was not picked (create the new dir it goes into)
    pub fn apply_only(&mut self, picked: &[Action]) -> Result<Report, KoilError> {
        let picked: HashSet<&Action> = picked.iter().collect();
        let (actions, needs): (Vec<Action>, Vec<Vec<usize>>) = (self.changes().into_iter())
            .map(|c| (c.action, c.needs))
            .unzip();
        let mut applied: Vec<bool> = actions.iter().map(|a| picked.contains(a)).collect();
        without_unmet(&needs, &mut applied);
        let actions: Vec<Action> = (actions.into_iter().zip(applied))
            .filter_map(|(action, applied)| applied.then_some(action))
            .collect();
        self.run_actions(&planner::plan_actions(&actions, on_disk))
    }

    /// What [`Koil::create_now`] would run to create the new file or dir at `path`: its
    /// create, after those of the new dirs it is in, in order
    /// Fails with [`KoilError::NotNew`] if the changes do not create `path`, and with
    /// [`KoilError::NeedsChanges`] if it needs changes that are not creates (see
    /// [`Change::needs`]), like the rename that moves away what is at `path` now
    pub fn create_steps(&self, path: &Path) -> Result<Vec<Action>, KoilError> {
        let (actions, needs): (Vec<Action>, Vec<Vec<usize>>) = (self.changes().into_iter())
            .map(|c| (c.action, c.needs))
            .unzip();
        let creates = |action: &Action| matches!(action, Action::CreateFile(p) | Action::CreateDir(p) if p == path);
        let first = (actions.iter())
            .position(creates)
            .ok_or_else(|| KoilError::NotNew(path.to_path_buf()))?;
        let needed = with_needs(&needs, first);
        if (needed.iter()).any(|&i| i != first && !matches!(actions[i], Action::CreateDir(_))) {
            return Err(KoilError::NeedsChanges(path.to_path_buf()));
        }
        // in the order they run
        Ok(needed.into_iter().map(|i| actions[i].clone()).collect())
    }

    /// Create the new file or dir at `path` on disk now, with the new dirs it is in (see
    /// [`Koil::create_steps`]), rather than when the changes are applied, so a frontend can
    /// open a new file to write in it
    /// Unlike [`Koil::apply_only`], every other change stays pending. The open dir is read
    /// again, so its listing shows what was created as on disk (with an ID), and it can be
    /// reverted with [`Koil::undo`], like an apply
    pub fn create_now(&mut self, path: &Path) -> Result<Report, KoilError> {
        let actions = self.create_steps(path)?;
        let result = self.run(&actions);
        // what was created is not new anymore, even if a later create failed (a missing
        // parent is not in the diff, it is only missing)
        for action in &actions {
            if on_disk(action.path()) {
                self.diff.without_id.remove(action.path());
            }
        }
        let reopened = self.reopen();
        result?;
        Ok(Report {
            changes: actions.len(),
            warning: reopened?,
        })
    }

    /// The steps that [`Koil::undo`] would run to revert the last apply, in order
    /// `None` if there is nothing to undo
    /// Fails if there are changes that are not applied, since undo would make them wrong
    pub fn undo_steps(&self) -> Result<Option<&[Undo]>, KoilError> {
        self.check_nothing_pending()?;
        Ok(self.undo.last().map(|applied| applied.steps.as_slice()))
    }

    /// Revert the last apply, deleted paths come back from the trash, and created paths are
    /// moved to the trash
    /// See [`Koil::undo_only`], which this is with only the last apply
    pub fn undo(&mut self) -> Result<Report, KoilError> {
        self.check_nothing_pending()?;
        let last = self.undo.last().cloned().ok_or(KoilError::NothingToUndo)?;
        self.undo_only(&[last])
    }

    /// Every apply (and [`Koil::create_now`]) that can still be reverted, the last one last
    /// A frontend can keep them across sessions with [`Koil::set_history`]
    pub fn history(&self) -> &[Applied] {
        &self.undo
    }

    /// Have `history` as the applies that can be reverted (see [`Koil::history`]), like the
    /// ones of an earlier session, the last one last
    pub fn set_history(&mut self, history: Vec<Applied>) {
        self.undo = history;
    }

    /// [`Koil::history`], each with the newer applies it can not be undone without, and why it
    /// can not be undone now, if it can not (see [`Undoable`]), for a frontend to pick from
    /// for [`Koil::undo_only`]
    /// Fails if there are changes that are not applied, since undo would make them wrong
    pub fn undoable(&self) -> Result<Vec<Undoable>, KoilError> {
        self.check_nothing_pending()?;
        let needs = self.undo_needs();
        let trash = trash::Contents::default();
        let undoable = (self.undo.iter().zip(&needs).enumerate())
            .map(|(i, (applied, direct))| {
                // with every apply it needs, newest first, as they would be undone
                let with = with_needs(&needs, i);
                let steps = with.iter().rev().flat_map(|&j| &self.undo[j].steps);
                Undoable {
                    applied: applied.clone(),
                    needs: direct.clone(),
                    blocked: apply::check(steps, &trash).err(),
                }
            })
            .collect();
        Ok(undoable)
    }

    /// Revert the applies of [`Koil::history`] in `picked`, newest first: deleted paths come
    /// back from the trash, and created paths are moved to the trash. Then the listing is
    /// refreshed
    /// An apply that needs one that is not picked (see [`Undoable::needs`]) is not undone
    /// either, nor are the ones not in the history anymore
    /// Nothing is undone if a step can not run, as far as can be told before running them
    /// ([`KoilError::UndoBlocked`]), and if a step fails anyway, the ones not run yet stay, so
    /// they can be undone later
    /// Fails if there are changes that are not applied, since undo would make them wrong
    pub fn undo_only(&mut self, picked: &[Applied]) -> Result<Report, KoilError> {
        self.check_nothing_pending()?;
        let picked: HashSet<&Applied> = picked.iter().collect();
        let mut undone: Vec<bool> = self.undo.iter().map(|a| picked.contains(a)).collect();
        without_unmet(&self.undo_needs(), &mut undone);
        // newest first, which also keeps the indexes of the rest as they are while each goes
        let order: Vec<usize> = (0..undone.len()).rev().filter(|&i| undone[i]).collect();
        if order.is_empty() {
            return Err(KoilError::NothingToUndo);
        }
        let steps = || order.iter().flat_map(|&i| &self.undo[i].steps);
        apply::check(steps(), &trash::Contents::default())
            .map_err(|(step, blocked)| KoilError::UndoBlocked { step, blocked })?;

        let total = steps().count();
        let mut done = 0;
        let mut result = Ok(());
        let mut removed = HashSet::new();
        'applies: for i in order {
            let steps = self.undo[i].steps.clone();
            for (k, step) in steps.iter().enumerate() {
                if let Undo::Trash(path) | Undo::Rename(path, _) = step {
                    removed.insert(path.clone());
                }
                if let Err(source) = step.run() {
                    // the steps that did not run stay, so they can be undone later
                    self.undo[i].steps = steps[k..].to_vec();
                    result = Err(KoilError::UndoFailed {
                        step: step.clone(),
                        done,
                        total,
                        source,
                    });
                    break 'applies;
                }
                done += 1;
            }
            self.undo.remove(i);
        }
        self.unsee(&removed);
        let refreshed = self.refresh();
        result?;
        Ok(Report {
            changes: done,
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

    /// `path` relative to [`Koil::current_dir`], only with `/` (see [`with_slashes`]), like
    /// the names in the listing, `None` if it is not inside it
    pub fn relative(&self, path: &Path) -> Option<PathBuf> {
        let rest = path.strip_prefix(&self.current_dir).ok()?;
        (!rest.as_os_str().is_empty()).then(|| with_slashes(rest))
    }

    /// Whether applying would change `entry`, as it is written in a listing of the open dir:
    /// it is new, or not at the path of its ID, so it is renamed, copied, or moved here from
    /// another dir
    /// A frontend can use it to mark the entries that are not on disk as they are written
    pub fn is_pending(&self, entry: &Entry) -> bool {
        match entry.id {
            None => true,
            Some(id) => self.path_of(id) != Some(self.current_dir.join(&entry.name).as_path()),
        }
    }

    /// `entry` as a frontend reads it back from a listing it showed as text, which can not
    /// show a name that is not Unicode (only on Linux, or a malformed name on Windows) but as
    /// `to_string_lossy` does, with `�`: each part of its name that is how the part of its ID's
    /// path in the same place from the end is shown, is that part, so a line user did not
    /// change is not read as a rename (`a�` is the `a\xff` it shows), nor one moved elsewhere
    /// (`dir/a�`)
    pub fn read_shown(&self, mut entry: Entry) -> Entry {
        let Some(path) = entry.id.and_then(|id| self.path_of(id)) else {
            return entry;
        };
        // almost every path
        if path.to_str().is_some() {
            return entry;
        }
        let mut parts: Vec<&OsStr> = entry.name.components().map(|c| c.as_os_str()).collect();
        let mut shown = false;
        for (part, real) in parts.iter_mut().rev().zip(path.components().rev()) {
            let real = real.as_os_str();
            if real.to_str().is_none() && part.to_str() == Some(&real.to_string_lossy()) {
                *part = real;
                shown = true;
            }
        }
        if shown {
            entry.name = parts.iter().collect();
        }
        entry
    }
}

// Private functions
impl Koil {
    /// Run `change` on a copy, which is kept only if it works, so nothing changes if it fails
    fn atomically<T, E>(&mut self, change: impl FnOnce(&mut Koil) -> Result<T, E>) -> Result<T, E> {
        let mut copy = self.clone();
        let result = change(&mut copy)?;
        *self = copy;
        Ok(result)
    }

    /// See [`Koil::open`], which runs it on a copy, as it is left half done if it fails
    fn open_in_place(&mut self, location: &Path) -> Result<(), OpenError> {
        let Location { dir, pattern, .. } = self.read_location(location, self.settings.regex);
        if let Some(pattern) = &pattern {
            pattern.matcher()?;
        }
        if !self.is_dir(&dir) {
            return Err(self.not_a_dir(&dir));
        }
        self.current_dir = dir;
        self.pattern = pattern;
        self.read_listing()
    }

    /// Run `actions` in order, then refresh, see [`Koil::apply`]
    fn run_actions(&mut self, actions: &[Action]) -> Result<Report, KoilError> {
        let result = self.run(actions);
        // even if some action failed, others changed the filesystem
        let refreshed = self.refresh();
        result?;
        Ok(Report {
            changes: actions.len(),
            warning: refreshed?,
        })
    }

    /// Run `actions` in order, and remember the steps that revert them for [`Koil::undo`]
    /// If one fails, the rest are not run, and the ones that ran can still be undone
    fn run(&mut self, actions: &[Action]) -> Result<(), KoilError> {
        let mut steps = Vec::new();
        let mut result = Ok(());
        let mut removed = HashSet::new();
        for (i, action) in actions.iter().enumerate() {
            match action.run() {
                Ok(step) => {
                    steps.extend(step);
                    removed.extend(action.removes().map(Path::to_path_buf));
                }
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
        self.unsee(&removed);
        // undo runs the steps backwards
        steps.reverse();
        self.push_undo(steps);
        result
    }

    /// Forget what was seen at the paths in `removed` and inside them, which were moved away or
    /// deleted: a file moved is at another path now, with another ID, and [`Koil::sync`] never
    /// follows a key that two IDs were seen with
    fn unsee(&mut self, removed: &HashSet<PathBuf>) {
        if removed.is_empty() {
            return;
        }
        let ids = &self.ids;
        (self.seen).retain(|&index, _| !ids[index].ancestors().any(|a| removed.contains(a)));
    }

    /// For each apply of [`Koil::history`], the newer ones it can not be undone without (see
    /// [`Undoable::needs`])
    fn undo_needs(&self) -> Vec<Vec<usize>> {
        let paths: Vec<Vec<&Path>> = (self.undo.iter())
            .map(|applied| applied.steps.iter().flat_map(Undo::paths).collect())
            .collect();
        // every path an apply changed, and the dirs they are in
        let around: Vec<HashSet<&Path>> = (paths.iter())
            .map(|paths| paths.iter().flat_map(|p| p.ancestors()).collect())
            .collect();
        let exact: Vec<HashSet<&Path>> = (paths.iter())
            .map(|paths| paths.iter().copied().collect())
            .collect();
        // a path of one is a path of the other, a dir one is in, or inside one
        let touches = |i: usize, j: usize| {
            (paths[i].iter())
                .any(|p| around[j].contains(p) || p.ancestors().any(|a| exact[j].contains(a)))
        };
        (0..self.undo.len())
            .map(|i| {
                (i + 1..self.undo.len())
                    .filter(|&j| touches(i, j))
                    .collect()
            })
            .collect()
    }

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
            self.undo.push(Applied {
                time: SystemTime::now(),
                dir: self.current_dir.clone(),
                steps,
            });
        }
    }

    /// Open [`Koil::location`] again, so it shows what is on disk now
    /// Does nothing if nothing was opened yet
    /// If the open dir is gone, or the open pattern goes over [`Limits`] now, something else is
    /// opened instead (see [`Koil::read_or_fall_back`])
    fn reopen(&mut self) -> io::Result<Option<Warning>> {
        if self.current_dir.as_os_str().is_empty() {
            return Ok(None);
        }
        // not parsed again, so a pattern stays a glob or a regex, even if the settings changed
        let (paths, warning) = self.read_or_fall_back()?;
        self.list(paths);
        Ok(warning)
    }

    /// Read [`Koil::current_listing`] for the open dir or pattern from the filesystem
    fn read_listing(&mut self) -> Result<(), OpenError> {
        let paths = self.read_view(&self.view())?;
        self.list(paths);
        Ok(())
    }

    /// [`Koil::read_view`] of the open view, but if the open dir is gone (like after an
    /// apply), its closest parent that is a dir is opened instead, and if the open pattern goes
    /// over [`Limits`], its base dir is, with a warning that says so
    /// [`Koil::open`] checks both first, so only reading what is open again can fall back
    fn read_or_fall_back(&mut self) -> io::Result<(Walked, Option<Warning>)> {
        let mut warning = None;
        if !self.is_dir(&self.current_dir) {
            let dir = (self.current_dir.ancestors())
                .find(|p| self.is_dir(p))
                .ok_or(io::Error::from(io::ErrorKind::NotFound))?
                .to_path_buf();
            warning = Some(Warning::DirNotFound {
                requested: std::mem::replace(&mut self.current_dir, dir.clone()),
                opened: dir,
            });
            // the pattern is relative to its base dir, so it can not be used in another dir
            self.pattern = None;
        }
        match self.read_view(&self.view()) {
            Err(OpenError::OverLimit(error)) => {
                // a plain dir has no limits
                self.pattern = None;
                let paths = self.read_view(&self.view()).map_err(io_error)?;
                let opened = self.current_dir.clone();
                Ok((paths, Some(Warning::OverLimit { error, opened })))
            }
            result => Ok((result.map_err(io_error)?, warning)),
        }
    }

    /// Every path on disk that `view` shows (see [`Koil::walk`]), with what is there, nothing
    /// if [`Koil::current_dir`] is a new dir that is not created yet
    fn read_view(&self, view: &View) -> Result<Walked, OpenError> {
        match self.current_dir.is_dir() {
            true => self.walk(view),
            false => Ok(Vec::new()),
        }
    }

    /// Make `paths` (read by [`Koil::read_view`]) [`Koil::current_listing`], giving new IDs to
    /// the ones koil has not seen, and remember what is at each, and what
    /// [`Koil::current_dir`] and the dirs it is in are (see [`Koil::dirs_seen`])
    /// A changed entry in the view stays too, even if it is hidden, so it is not taken for
    /// deleted in the next update
    fn list(&mut self, paths: Walked) {
        self.dirs_seen = (self.current_dir.ancestors())
            .filter_map(|dir| Some((dir.to_path_buf(), FileKey::at(dir)?)))
            .collect();
        self.current_listing.clear();
        // a pattern can show thousands of paths, so they are not searched for in `ids` one by one
        let found: Vec<(Option<usize>, PathBuf, Option<Seen>)> = {
            let known: HashMap<&Path, usize> = (self.ids.iter().enumerate())
                .map(|(index, path)| (path.as_path(), index))
                .collect();
            (paths.into_iter())
                .filter(|(path, _)| !self.ignore.contains(path))
                .map(|(path, seen)| (known.get(path.as_path()).copied(), path, seen))
                .collect()
        };
        for (index, path, seen) in found {
            let index = index.unwrap_or_else(|| {
                self.ids.push(path);
                self.ids.len() - 1
            });
            self.current_listing.insert(index);
            if let Some(seen) = seen {
                self.seen.insert(index, seen);
            }
        }

        let view = self.view();
        for (&index, (before, _afters)) in &self.diff.with_id {
            if view.contains(before, self.ids[index].is_dir()) {
                self.current_listing.insert(index);
            }
        }
    }

    /// Which paths the listing shows
    fn view(&self) -> View {
        View {
            base: self.current_dir.clone(),
            matcher: (self.pattern.as_ref())
                .map(|p| p.matcher().expect("the pattern was valid when opened")),
        }
    }

    /// The name of `path` in the listing, relative to [`Koil::current_dir`], only with `/`,
    /// also on Windows, as users write it
    fn name(&self, path: &Path) -> PathBuf {
        with_slashes(path.strip_prefix(&self.current_dir).unwrap())
    }

    /// What decides which entries the listing shows (not [`Settings::regex`], which only
    /// changes how the next location is read), see [`Updated::moved`]
    fn shows(&self) -> (&Path, Option<&Pattern>, bool, bool) {
        let settings = &self.settings;
        let pattern = self.pattern.as_ref();
        (
            &self.current_dir,
            pattern,
            settings.show_hidden,
            settings.respect_gitignore,
        )
    }

    /// Whether `path` is a dir on disk, or a new dir in [`Koil::diff`]
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir() || self.diff.creates_dir(path)
    }

    /// Why `path`, which is not a dir, can not be opened: its first part that is not a dir is
    /// a file (on disk, or new in the diff), or does not exist
    fn not_a_dir(&self, path: &Path) -> OpenError {
        let first = path.ancestors().take_while(|p| !self.is_dir(p)).last();
        let first = first.unwrap_or(path).to_path_buf();
        match first.exists() || self.diff.without_id.get(&first) == Some(&false) {
            true => OpenError::NotADirectory(first),
            false => OpenError::NotFound(first),
        }
    }

    /// Fail if there are changes that are not applied yet
    fn check_nothing_pending(&self) -> Result<(), KoilError> {
        match self.has_changes() {
            false => Ok(()),
            true => Err(KoilError::PendingChanges),
        }
    }

    /// The index of `id` in [`Koil::ids`], `None` if it is not there
    fn index_of(&self, id: Id) -> Option<usize> {
        usize::try_from(id.0).ok().filter(|&i| i < self.ids.len())
    }

    /// See [`Koil::update`], which runs it on a copy, as it is left half updated if it fails
    fn update_entries(&mut self, entries: &[Entry]) -> Result<Vec<EntryWarning>, UpdateError> {
        let mut errors = Vec::new();
        let mut error = |entry, kind| errors.push(EntryError { entry, kind });

        // each written path, and the entry that has it first
        let mut seen: HashMap<PathBuf, usize> = HashMap::new();
        // index of each ID, and every (entry, path) it is written as
        let mut with_id: BTreeMap<usize, Vec<(usize, PathBuf)>> = BTreeMap::new();
        // (entry, path, is_dir) of every new entry
        let mut without_id = Vec::new();

        for (i, entry) in entries.iter().enumerate() {
            let index = match entry.id.map(|id| (id, self.index_of(id))) {
                Some((_, Some(index))) => Some(index),
                Some((id, None)) => {
                    error(i, EntryErrorKind::UnknownId(id));
                    continue;
                }
                None => None,
            };
            // what the listing shows was on disk when it was read, and is followed by `sync`
            if let Some(index) = index
                && !self.current_listing.contains(&index)
                && !on_disk(&self.ids[index])
            {
                let path = &self.ids[index];
                let path = self.relative(path).unwrap_or_else(|| with_slashes(path));
                error(i, EntryErrorKind::NotOnDisk(path));
                continue;
            }
            let Some(path) = relative_path(&entry.name) else {
                error(i, EntryErrorKind::InvalidName(entry.name.clone()));
                continue;
            };
            if let Some(&first) = seen.get(&path) {
                let path = with_slashes(&path);
                error(i, EntryErrorKind::Duplicate { path, first });
                continue;
            }
            seen.insert(path.clone(), i);
            match index {
                Some(index) => with_id.entry(index).or_default().push((i, path)),
                None => without_id.push((i, path, entry.is_dir)),
            }
        }

        let view = self.view();
        let ids = &self.ids;

        // clear the diff for the current listing, since it is outdated
        self.diff.with_id.retain(|&index, (before, afters)| {
            let is_dir = ids[index].is_dir();
            afters.retain(|p| !view.contains(p, is_dir));
            // if before and after are from this listing, remove this from diff
            // or if before and after are exactly the same, then also remove it
            (!afters.is_empty() || !view.contains(before, is_dir))
                && afters.as_slice() != [before.as_path()]
        });
        self.diff
            .without_id
            .retain(|p, &mut is_dir| !view.contains(p, is_dir));

        // if something existed before, but does not exist now, tell the diff that it used to exist
        for &index in &self.current_listing {
            if !with_id.contains_key(&index) {
                self.diff.set_before(index, self.ids[index].clone());
            }
        }

        // every (entry, path, index of its ID) written in this listing, to check them later
        let mut written = Vec::new();

        // Creates
        for (i, path, is_dir) in without_id {
            let path = self.current_dir.join(path);
            self.diff.without_id.insert(path.clone(), is_dir);
            written.push((i, path, None));
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
                self.diff.set_before(index, self.ids[index].clone());

            // If this ID came from another folder, and no change was made to it there yet
            } else if !self.diff.with_id.contains_key(&index) {
                self.diff.set_before(index, self.ids[index].clone());

                // it still exists where it came from (if it did not, that folder's listing
                // would have changed it already)
                self.diff.push_after(index, self.ids[index].to_path_buf());
            }

            // Add what we see to `diff`
            for (i, path) in names {
                let path = self.current_dir.join(path);
                self.diff.push_after(index, path.clone());
                written.push((i, path, Some(index)));
            }
        }

        let mut warnings = Vec::new();
        for (i, path, index) in written {
            let mut error = |kind| errors.push(EntryError { entry: i, kind });
            // `dir/A` also means create `dir/`, if it does not exist yet, so it can not be a file
            if let Err(parent) = self.check_parents(&path) {
                error(EntryErrorKind::NotADirectory(parent));
            }
            // the name did not change
            if index.is_some_and(|index| self.ids[index] == path) {
                continue;
            }
            if self.taken(&path, index) {
                error(EntryErrorKind::AlreadyExists(self.name(&path)));
            }
            if index.is_some_and(|index| inside(&self.ids[index], &path)) {
                error(EntryErrorKind::IntoItself(self.name(&path)));
            }
            for problem in self.new_names(&path).flat_map(names::check) {
                match problem {
                    names::Problem::Error(kind) => error(kind),
                    names::Problem::Warning(kind) => {
                        warnings.push(EntryWarning { entry: i, kind });
                    }
                }
            }
        }

        errors.sort_by_key(|e| e.entry);
        warnings.sort_by_key(|w| w.entry);
        match errors.is_empty() {
            true => Ok(warnings),
            false => Err(UpdateError { errors, warnings }),
        }
    }

    /// Whether something is at `path` on disk, which is not deleted or moved away in
    /// [`Koil::diff`], so `path` can not be created there
    /// `index` is the ID of the entry that is written at `path`, if it has one
    fn taken(&self, path: &Path, index: Option<usize>) -> bool {
        if !on_disk(path) {
            return false;
        }
        let same = |a: &Path| a == path || same_file(a, path).unwrap_or(false);
        // `mkdir -p` does nothing to a dir that is already there
        let new_dir = self.diff.without_id.get(path) == Some(&true);
        // a rename that only changes the case, on a filesystem that ignores case
        let renamed = index.is_some_and(|index| same(&self.ids[index]));
        let moved_away = self
            .diff
            .with_id
            .values()
            .any(|(before, afters)| same(before) && !afters.iter().any(|a| a == path));
        !(renamed || moved_away || new_dir && path.is_dir())
    }

    /// Every part of `path` (inside [`Koil::current_dir`]) that is not on disk yet, so it is a
    /// new name
    fn new_names<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = &'a std::ffi::OsStr> {
        path.ancestors()
            .take_while(|p| p.starts_with(&self.current_dir) && *p != self.current_dir)
            .filter(|p| !on_disk(p))
            .filter_map(Path::file_name)
    }

    /// Check that no parent of `path` (inside [`Koil::current_dir`]) is a file, on disk or
    /// created in [`Koil::diff`]
    /// Fails with the parent (relative to [`Koil::current_dir`]) that is a file
    /// Parents that do not exist yet are created by [`Diff::compute_actions`]
    fn check_parents(&self, path: &Path) -> Result<(), PathBuf> {
        let parents = path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(&self.current_dir) && *p != self.current_dir);

        for parent in parents {
            if on_disk(parent) {
                if parent.is_dir() {
                    break;
                }
                return Err(self.name(parent));
            }
            if self.diff.without_id.get(parent) == Some(&false) {
                return Err(self.name(parent));
            }
        }
        Ok(())
    }
}

fn to_id(index: usize) -> Id {
    Id(index as u64)
}

/// Leave out of `picked` every item that needs one that is not picked, also through others,
/// where `needs` are the items each needs directly (see [`Change::needs`] and
/// [`Undoable::needs`])
fn without_unmet(needs: &[Vec<usize>], picked: &mut [bool]) {
    let mut needed_by = vec![Vec::new(); needs.len()];
    for (i, needs) in needs.iter().enumerate() {
        for &j in needs {
            needed_by[j].push(i);
        }
    }
    let mut left_out: Vec<usize> = (0..picked.len()).filter(|&i| !picked[i]).collect();
    while let Some(j) = left_out.pop() {
        for &i in &needed_by[j] {
            if picked[i] {
                picked[i] = false;
                left_out.push(i);
            }
        }
    }
}

/// `from` and every item it needs, also through others, where `needs` are the items each
/// needs directly
fn with_needs(needs: &[Vec<usize>], from: usize) -> BTreeSet<usize> {
    let mut with = BTreeSet::from([from]);
    let mut todo = needs[from].clone();
    while let Some(j) = todo.pop() {
        if with.insert(j) {
            todo.extend(&needs[j]);
        }
    }
    with
}

/// `name` as a path relative to the listing's dir, `None` if it is empty, absolute (or
/// starts with a drive, like `C:A` on Windows), or has a `.` or `..` part
/// Name can have `/` inside, like `dir/A` (and `\` on Windows, where it is a separator too)
fn relative_path(name: &Path) -> Option<PathBuf> {
    let raw = name.to_string_lossy();
    // `Path::components` ignores `.` in the middle of a path, so check the raw parts
    let dots = raw.split(is_separator).any(|p| p == "." || p == "..");
    let drive = matches!(name.components().next(), Some(Component::Prefix(_)));
    if raw.is_empty() || name.has_root() || drive || dots {
        return None;
    }
    Some(name.components().collect())
}

/// `path` with `/` instead of `\` on Windows, where both are separators, as users write it:
/// koil gives every path it shows that way (names, [`Koil::location`], and the paths in its
/// messages), and a frontend should show the others like that too
/// Not if it is verbatim (`\\?\`), where `/` is not a separator, or not Unicode
pub fn with_slashes(path: &Path) -> PathBuf {
    let verbatim =
        matches!(path.components().next(), Some(Component::Prefix(p)) if p.kind().is_verbatim());
    match path.to_str() {
        Some(s) if cfg!(windows) && !verbatim => s.replace('\\', "/").into(),
        _ => path.to_path_buf(),
    }
}

/// `path` as koil's messages show it, see [`with_slashes`]
pub(crate) fn shown(path: &Path) -> String {
    with_slashes(path).display().to_string()
}

/// Whether `name` is hidden, as it starts with a `.`
fn is_hidden(name: impl AsRef<OsStr>) -> bool {
    name.as_ref().as_encoded_bytes().starts_with(b".")
}

/// Whether something is at `path`, even a broken link, as a link is not followed
pub(crate) fn on_disk(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// `path` with its start `from` replaced by `to`, `None` if it is neither `from` nor inside it
pub(crate) fn rebase(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    // `join` with nothing would add a separator at the end
    Some(match rest.as_os_str().is_empty() {
        true => to.to_path_buf(),
        false => to.join(rest),
    })
}

#[cfg(test)]
mod tests;
