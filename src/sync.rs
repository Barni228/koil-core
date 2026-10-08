//! Following what changes on disk while koil has a dir open, see [`Koil::sync`]

use crate::apply::Undo;
use crate::{
    Entry, Id, Koil, Metadata, OpenError, Prune, Settings, View, Warning, io_error, to_id,
    with_slashes,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::{fs, io};

/// What tells a file on disk apart from the others, and stays the same when it is renamed or
/// moved within its filesystem: its device and inode on Unix, and when it was created, as a
/// new file can get the inode of one that was just deleted; and on Windows when it was created
/// and whether it is a dir, as its file index can only be read by opening it
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct FileKey(u64, u64, u64);

impl FileKey {
    /// The key of what is at `path` (of the link itself, for a link)
    pub(crate) fn at(path: &Path) -> Option<FileKey> {
        FileKey::of(&path.symlink_metadata().ok()?)
    }

    #[cfg(unix)]
    fn of(meta: &fs::Metadata) -> Option<FileKey> {
        use std::os::unix::fs::MetadataExt;
        // not every filesystem keeps when a file was created
        let created = (meta.created().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
        Some(FileKey(meta.dev(), meta.ino(), created))
    }

    #[cfg(windows)]
    fn of(meta: &fs::Metadata) -> Option<FileKey> {
        use std::os::windows::fs::MetadataExt;
        // some filesystems keep no creation time, and then nothing can be told apart
        let created = meta.creation_time();
        (created != 0).then_some(FileKey(created, u64::from(meta.is_dir()), 0))
    }

    #[cfg(not(any(unix, windows)))]
    fn of(_meta: &fs::Metadata) -> Option<FileKey> {
        None
    }
}

/// What koil saw at a path on disk
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seen {
    key: Option<FileKey>,
    /// What the listing can be sorted by, and whether it is a dir
    pub(crate) meta: Metadata,
}

impl Seen {
    /// What is at `path`, whose metadata (of the link itself, for a link) is `meta`, read for
    /// a listing shown with `settings`
    pub(crate) fn new(path: &Path, meta: &fs::Metadata, settings: &Settings) -> Seen {
        Seen {
            key: FileKey::of(meta),
            meta: Metadata::new(path, meta, settings),
        }
    }
}

/// What [`Koil::sync`] found changed on disk
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Synced {
    /// How to change the entries given to `sync`, so the listing shows what is on disk now,
    /// with what user changed in it kept
    pub edits: Vec<Edit>,
    /// What changed on disk against what user changed, which a frontend should ask user
    /// about: each is left as its [`ConflictKind`] says, until [`Koil::resolve`] takes the
    /// other way
    pub conflicts: Vec<Conflict>,
    /// Whether the listing shows other entries now, so it must be shown as a new one, and
    /// `edits` are of no use: the open dir is gone, or its pattern goes over
    /// [`Limits`](crate::Limits) now (see `warning`)
    pub moved: bool,
    /// Why something else than what was open is open now
    pub warning: Option<Warning>,
}

/// A change to the entries of a listing, so it shows what changed on disk (see
/// [`Koil::sync`])
/// The entries it is about are found by their ID and name (and whether they are dirs, for new
/// ones), as they were given to `sync`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edit {
    /// Add this entry: it is new on disk, or came back, or was moved into the open dir
    Add(Entry),
    /// Remove the entries like this one: they are gone from disk, or moved out of the open dir
    Remove(Entry),
    /// The entries like `from` become `to`: renamed on disk, or new ones that are on disk now
    Change { from: Entry, to: Entry },
}

/// What changed on disk against what user changed in a listing, see [`Koil::sync`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub kind: ConflictKind,
    /// The ID it is about
    pub id: Id,
    /// Its path before it changed on disk
    pub from: PathBuf,
    /// Its path on disk now, `None` if it is gone
    pub to: Option<PathBuf>,
    pub is_dir: bool,
    /// Its entries in the listing, as they were given to `sync`, but the ones that are still at
    /// `from` (for [`ConflictKind::Taken`], the entries user wrote at its path)
    pub listed: Vec<Entry>,
    /// The paths user put it at in the listings of other dirs
    pub elsewhere: Vec<PathBuf>,
}

/// How something changed on disk against what user changed, see [`Conflict`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictKind {
    /// User deleted it, but it was renamed or moved on disk
    /// It is kept for now, so it is not deleted under a name that user never saw; resolving
    /// deletes it
    Deleted,
    /// User renamed or moved it, but it was renamed or moved somewhere else on disk
    /// User's paths are kept for now; resolving keeps it where it is on disk, and makes the
    /// rest of user's paths copies of it
    Renamed,
    /// User renamed, moved or copied it, but it is gone from disk (deleted, or moved where koil
    /// does not see it go), so that can not be done
    /// What user wrote is dropped for now; resolving creates it as new, empty files or dirs
    Gone,
    /// It is new on disk (or was renamed or moved there), at a path user wrote for another
    /// entry, so the listing has that path twice, which `update` rejects
    /// Both are kept for now; resolving deletes it, so user's entry can be there
    Taken,
}

/// What a frontend should watch on disk, to call [`Koil::sync`] when something there
/// changes, see [`Koil::watched`]
#[derive(Clone, Default)]
pub struct Watched {
    /// Each dir to watch, and whether to watch what is inside its dirs too
    pub dirs: Vec<(PathBuf, bool)>,
    /// The open dir
    open: PathBuf,
    view: Option<View>,
    prune: Option<Prune>,
    /// The dirs of paths changed in the listings of other dirs
    others: HashSet<PathBuf>,
}

impl Watched {
    /// Whether something created, removed or renamed at `path` on disk can change what
    /// [`Koil::sync`] finds, so a frontend does not sync for what can not
    pub fn affects(&self, path: &Path) -> bool {
        let Some(view) = &self.view else {
            return false;
        };
        // the open dir, or a dir it is in
        if self.open.starts_with(path) {
            return true;
        }
        let parent = path.parent();
        if parent.is_some_and(|p| self.others.contains(p)) {
            return true;
        }
        match &view.matcher {
            None => parent == Some(self.open.as_path()),
            // a dir that has matches inside can be renamed, it is matched with a `/`
            Some(_) => {
                path.starts_with(&self.open)
                    && (view.contains(path, false)
                        || view.contains(path, true)
                        || self.prune.as_ref().is_none_or(|p| !p.skips_inside(path)))
            }
        }
    }
}

/// What user wants done with an ID, from the listing and the changes made in others
struct Intent {
    /// Its path before anything moved on disk
    path: PathBuf,
    /// Whether it was a dir when koil last read it
    is_dir: bool,
    /// Its entries in the listing
    listed: Vec<Entry>,
    /// Whether it stays where it is (maybe with copies elsewhere)
    stays: bool,
    /// Every other path user put it at
    targets: Vec<PathBuf>,
}

impl Koil {
    /// Read the open dir (or pattern) from disk again, to follow what changed there since koil
    /// read it, and in the dirs of the changes made in other listings (see
    /// [`Koil::watched`]), and return how `entries` should change to show it: the listing as
    /// user has it now, maybe edited, but not given to [`Koil::update`] yet
    /// A path that is gone, whose file is now somewhere else in the view or in the dir it was
    /// in (the same device and inode on Unix, or creation time on Windows), was renamed or
    /// moved there, and its ID follows it, with what is inside it and every change made to it
    /// A new path gets a new ID, and a path that is gone keeps its ID, which `update` rejects
    /// then ([`EntryErrorKind::NotOnDisk`](crate::EntryErrorKind::NotOnDisk))
    /// The open dir is followed too, if it (or a dir it is in) was renamed or moved within the
    /// dir it was in; if it is gone, its closest parent is opened ([`Synced::moved`])
    /// What changed against what user changed is returned in [`Synced::conflicts`], and stays
    /// as its [`ConflictKind`] says until [`Koil::resolve`]
    /// If reading fails, nothing changes
    pub fn sync(&mut self, entries: &[Entry]) -> io::Result<Synced> {
        if self.current_dir.as_os_str().is_empty() {
            return Ok(Synced::default());
        }
        // in a copy, so nothing changes if it fails
        let mut synced = self.clone();
        let result = synced.sync_with(entries)?;
        *self = synced;
        Ok(result)
    }

    /// Take the other way in `conflict`, which [`Koil::sync`] returned (see
    /// [`ConflictKind`]), and return how the listing should change for it
    pub fn resolve(&mut self, conflict: &Conflict) -> Vec<Edit> {
        let Some(index) = self.index_of(conflict.id) else {
            return Vec::new();
        };
        let in_listing = self.current_listing.contains(&index);
        let entry = |path: &Path| {
            Some(Entry {
                id: Some(conflict.id),
                name: self.relative(path)?,
                is_dir: conflict.is_dir,
            })
        };
        match (conflict.kind, &conflict.to) {
            (ConflictKind::Deleted, Some(to)) if in_listing => {
                entry(to).map(Edit::Remove).into_iter().collect()
            }
            (ConflictKind::Deleted, Some(to)) => {
                self.diff.with_id.insert(index, (to.clone(), Vec::new()));
                Vec::new()
            }
            (ConflictKind::Renamed, Some(to)) => self.keep_on_disk(index, to, conflict),
            (ConflictKind::Gone, _) => {
                for path in &conflict.elsewhere {
                    self.diff.without_id.insert(path.clone(), conflict.is_dir);
                }
                let new = |e: &Entry| Entry {
                    id: None,
                    name: e.name.clone(),
                    is_dir: conflict.is_dir,
                };
                conflict.listed.iter().map(|e| Edit::Add(new(e))).collect()
            }
            (ConflictKind::Taken, Some(to)) => entry(to).map(Edit::Remove).into_iter().collect(),
            _ => Vec::new(),
        }
    }

    /// What a frontend should watch, to call [`Koil::sync`] when something there changes on
    /// disk: the open dir (and what is inside its dirs, for a pattern), the dir it is in (so
    /// its rename is seen), and the dirs of the changes made in other listings
    pub fn watched(&self) -> Watched {
        if self.current_dir.as_os_str().is_empty() {
            return Watched::default();
        }
        let view = self.view();
        let others: HashSet<PathBuf> = (self.diff.with_id.iter())
            .filter(|&(&index, (before, _))| !view.contains(before, self.ids[index].is_dir()))
            .filter_map(|(_, (before, _))| Some(before.parent()?.to_path_buf()))
            .collect();
        let mut dirs: BTreeMap<PathBuf, bool> = others.iter().map(|d| (d.clone(), false)).collect();
        if let Some(parent) = self.current_dir.parent() {
            dirs.insert(parent.to_path_buf(), false);
        }
        dirs.insert(self.current_dir.clone(), self.pattern.is_some());
        Watched {
            dirs: dirs.into_iter().collect(),
            open: self.current_dir.clone(),
            prune: Prune::new(&view),
            view: Some(view),
            others,
        }
    }
}

// Private functions
impl Koil {
    /// See [`Koil::sync`], which runs it on a copy, as it is left half done if it fails
    fn sync_with(&mut self, entries: &[Entry]) -> io::Result<Synced> {
        let mut result = Synced::default();
        self.follow_dir();
        let (old_view, old_listing) = (self.view(), self.current_listing.clone());
        // gone: its closest parent, as after an apply
        if !self.is_dir(&self.current_dir) {
            let dir = (self.current_dir.ancestors())
                .find(|p| self.is_dir(p))
                .ok_or(io::Error::from(io::ErrorKind::NotFound))?
                .to_path_buf();
            result.warning = Some(Warning::DirNotFound {
                requested: std::mem::replace(&mut self.current_dir, dir.clone()),
                opened: dir,
            });
            self.pattern = None;
            result.moved = true;
        }
        let paths = loop {
            match self.read_view(&self.view()) {
                Ok(paths) => break paths,
                // as `reopen` does
                Err(OpenError::OverLimit(error)) => {
                    result.warning = Some(Warning::OverLimit {
                        error,
                        opened: self.current_dir.clone(),
                    });
                    self.pattern = None;
                    result.moved = true;
                }
                Err(error) => return Err(io_error(error)),
            }
        };
        let moved = result.moved;
        let dir = self.current_dir.clone();

        // what user wrote in the listing, unless it is of what is not open anymore
        let mut written: BTreeMap<usize, Vec<Entry>> = BTreeMap::new();
        let mut created: HashMap<PathBuf, Entry> = HashMap::new();
        for entry in entries.iter().filter(|e| !moved && !e.is_parent()) {
            match entry.id {
                Some(id) => {
                    if let Some(index) = self.index_of(id) {
                        written.entry(index).or_default().push(entry.clone());
                    }
                }
                None => {
                    (created.entry(dir.join(&entry.name))).or_insert_with(|| entry.clone());
                }
            }
        }
        let listed_before = |index: usize| !moved && old_listing.contains(&index);
        let tracked: BTreeSet<usize> = (old_listing.iter().filter(|_| !moved))
            .chain(written.keys())
            .chain(self.diff.with_id.keys())
            .copied()
            .collect();
        let intents: BTreeMap<usize, Intent> = (tracked.iter())
            .map(|&index| {
                let intent = self.intent(index, &written, &old_view, listed_before(index), moved);
                (index, intent)
            })
            .collect();

        // the IDs whose own file moved, not just a dir they are in
        let mut direct = HashSet::new();
        for (index, to) in self.find_moved(&tracked, &paths, listed_before) {
            let from = self.ids[index].clone();
            if from != to {
                self.repoint(&from, &to, Some(index));
                direct.insert(index);
            }
        }
        // on disk, as the walk just read them
        let walked: HashSet<PathBuf> = paths.iter().map(|(p, _)| p.clone()).collect();
        self.list(paths);
        self.see_dirs();
        // a changed entry stays, even if it is out of the view now (like one that git ignores
        // now), so it is not taken for deleted in the next update
        let view = self.view();
        for (&index, intent) in &intents {
            let path = &self.ids[index];
            let changed = !intent.stays || !intent.targets.is_empty();
            if listed_before(index)
                && changed
                && *path == intent.path
                && view.contains(path, intent.is_dir)
                && path.symlink_metadata().is_ok()
            {
                self.current_listing.insert(index);
            }
        }

        let entry_at = |index: usize, path: &Path| Entry {
            id: Some(to_id(index)),
            name: with_slashes(path.strip_prefix(&dir).unwrap_or(path)),
            is_dir: path.is_dir(),
        };
        let mut edits = Vec::new();
        let mut conflicts = Vec::new();
        for (&index, intent) in &intents {
            let path = self.ids[index].clone();
            let exists = walked.contains(&path) || path.symlink_metadata().is_ok();
            let now = exists.then_some(path);
            let in_listing = self.current_listing.contains(&index);
            // the paths user put it at elsewhere, which follow what moved on disk
            let elsewhere: Vec<PathBuf> = (self.diff.with_id.get(&index).into_iter())
                .flat_map(|(_, afters)| afters)
                .filter(|p| moved || !old_view.contains(p, intent.is_dir))
                .filter(|p| now.as_ref() != Some(*p))
                .cloned()
                .collect();
            let wants = |path: &PathBuf| {
                elsewhere.contains(path) || intent.listed.iter().any(|e| dir.join(&e.name) == *path)
            };
            // its entries that still have the path it had, and the ones user changed
            let (same, changed): (Vec<&Entry>, Vec<&Entry>) =
                (intent.listed.iter()).partition(|e| dir.join(&e.name) == intent.path);
            let remove = |entries: &[&Entry]| -> Vec<Edit> {
                entries.iter().map(|&e| Edit::Remove(e.clone())).collect()
            };
            let conflict = |kind, to: Option<PathBuf>| Conflict {
                kind,
                id: to_id(index),
                from: intent.path.clone(),
                is_dir: to.as_ref().map_or(intent.is_dir, |to| to.is_dir()),
                to,
                listed: changed.iter().map(|&e| e.clone()).collect(),
                elsewhere: elsewhere.clone(),
            };
            match now {
                None => {
                    edits.extend(remove(&same));
                    if !intent.targets.is_empty() {
                        edits.extend(remove(&changed));
                        conflicts.push(conflict(ConflictKind::Gone, None));
                    }
                    self.diff.with_id.remove(&index);
                }
                Some(now) if intent.stays => {
                    if in_listing {
                        let to = entry_at(index, &now);
                        let renamed = now != intent.path || to.is_dir != intent.is_dir;
                        for &e in same.iter().filter(|_| renamed) {
                            let from = e.clone();
                            edits.push(Edit::Change {
                                from,
                                to: to.clone(),
                            });
                        }
                        // else the next update would delete it
                        if same.is_empty() && !changed.iter().any(|e| dir.join(&e.name) == now) {
                            edits.push(Edit::Add(to));
                        }
                    } else {
                        edits.extend(remove(&same));
                        // it stays where it is now, and what user wrote are copies of it
                        if let Some((_, afters)) = self.diff.with_id.get_mut(&index)
                            && !afters.contains(&now)
                        {
                            afters.push(now);
                        }
                    }
                }
                // only a dir it is in moved, and what user wrote moved with it, or user put it
                // where it is now
                Some(now) if !direct.contains(&index) || wants(&now) => {}
                Some(now) if intent.targets.is_empty() => {
                    if in_listing {
                        edits.push(Edit::Add(entry_at(index, &now)));
                    }
                    self.diff.with_id.remove(&index);
                    conflicts.push(conflict(ConflictKind::Deleted, Some(now)));
                }
                Some(now) => {
                    // the next update renames it from where it is now, not copies it
                    if !in_listing {
                        self.diff.with_id.entry(index).or_default().0 = now.clone();
                    }
                    conflicts.push(conflict(ConflictKind::Renamed, Some(now)));
                }
            }
        }

        // what is new in the listing
        if !moved {
            let mut appeared: Vec<usize> = (self.current_listing.iter().copied())
                .filter(|index| !old_listing.contains(index) && !intents.contains_key(index))
                .collect();
            appeared.sort_by(|&a, &b| self.ids[a].cmp(&self.ids[b]));
            for index in appeared {
                let path = &self.ids[index];
                let entry = entry_at(index, path);
                match created.get(path) {
                    // the new entry user wrote is on disk now
                    Some(new) if new.is_dir == entry.is_dir => edits.push(Edit::Change {
                        from: new.clone(),
                        to: entry,
                    }),
                    _ => edits.push(Edit::Add(entry)),
                }
            }
        }

        // an edit that puts something at a path user wrote for another entry
        let mut wrote: HashMap<PathBuf, Vec<&Entry>> = HashMap::new();
        for entry in written.values().flatten().chain(created.values()) {
            wrote.entry(dir.join(&entry.name)).or_default().push(entry);
        }
        for edit in &edits {
            let to = match edit {
                Edit::Add(to) => to,
                Edit::Change { from, to } if from.id.is_some() => to,
                _ => continue,
            };
            let path = dir.join(&to.name);
            let others: Vec<Entry> = (wrote.get(&path).into_iter().flatten())
                .filter(|e| e.id != to.id)
                .map(|&e| e.clone())
                .collect();
            if let (Some(id), false) = (to.id, others.is_empty()) {
                conflicts.push(Conflict {
                    kind: ConflictKind::Taken,
                    id,
                    from: path.clone(),
                    to: Some(path),
                    is_dir: to.is_dir,
                    listed: others,
                    elsewhere: Vec::new(),
                });
            }
        }

        result.edits = edits;
        result.conflicts = conflicts;
        Ok(result)
    }

    /// What user wants done with `index` (see [`Intent`]), from what they `written` in the
    /// listing of `view` (if it was `listed` there before, and it is still open, so not
    /// `moved`), and the diff
    fn intent(
        &self,
        index: usize,
        written: &BTreeMap<usize, Vec<Entry>>,
        view: &View,
        listed: bool,
        moved: bool,
    ) -> Intent {
        let path = self.ids[index].clone();
        let is_dir = self
            .seen
            .get(&index)
            .map_or_else(|| path.is_dir(), |s| s.meta.is_dir);
        let entries = written.get(&index).cloned().unwrap_or_default();
        // the diff's paths in the listing are of when it was last read, and `entries` are
        // what it has now
        let others: Vec<PathBuf> = match self.diff.with_id.get(&index) {
            Some((_, afters)) => (afters.iter())
                .filter(|p| moved || !view.contains(p, is_dir))
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        let mut targets: Vec<PathBuf> = (entries.iter())
            .map(|e| self.current_dir.join(&e.name))
            .chain(others)
            .collect();
        // written in this listing from another dir, so it stays there (see `update`)
        if !listed && !self.diff.with_id.contains_key(&index) {
            targets.push(path.clone());
        }
        let stays = targets.contains(&path);
        targets.retain(|t| *t != path);
        Intent {
            path,
            is_dir,
            listed: entries,
            stays,
            targets,
        }
    }

    /// Where each of the `tracked` IDs that is not where it was went on disk, by its key (see
    /// [`FileKey`]): to a new path in `paths` (the open view, as it is now), or else near the
    /// dir it was in (see [`near`]). One that was `listed` before is not where it was if it is
    /// not in `paths`
    /// A key that two files share (see [`shared`]) is never followed, as it can be either
    fn find_moved(
        &self,
        tracked: &BTreeSet<usize>,
        paths: &[(PathBuf, Option<Seen>)],
        listed: impl Fn(usize) -> bool,
    ) -> Vec<(usize, PathBuf)> {
        let in_view: HashSet<&Path> = paths.iter().map(|(p, _)| p.as_path()).collect();
        // a path koil knows is always the same entry, even if its file was replaced
        let known: HashSet<&Path> = self.ids.iter().map(PathBuf::as_path).collect();
        let missing: Vec<usize> = (tracked.iter().copied())
            .filter(|&index| {
                let path = self.ids[index].as_path();
                !in_view.contains(path) && (listed(index) || path.symlink_metadata().is_err())
            })
            .collect();
        // what koil saw when it last read, and the new paths in the view
        let ambiguous = shared(self.seen.values().filter_map(|s| s.key));
        let key_of = |index: &usize| (self.seen.get(index)?.key).filter(|k| !ambiguous.contains(k));
        let by_key: HashMap<FileKey, usize> = (missing.iter())
            .filter_map(|index| Some((key_of(index)?, *index)))
            .collect();
        let new: Vec<(&PathBuf, Option<FileKey>)> = (paths.iter())
            .filter(|(path, _)| !known.contains(path.as_path()) && !self.ignore.contains(path))
            .map(|(path, seen)| (path, seen.and_then(|s| s.key)))
            .collect();
        let new_ambiguous = shared(new.iter().filter_map(|&(_, key)| key));

        let mut moves = Vec::new();
        let mut found = HashSet::new();
        let mut claimed = HashSet::new();
        for (path, key) in new {
            let key = key.filter(|key| !new_ambiguous.contains(key));
            let index = key.and_then(|key| by_key.get(&key));
            if let Some(&index) = index
                && found.insert(index)
            {
                claimed.insert(path.clone());
                moves.push((index, path.clone()));
            }
        }
        // a hidden name, or one that the pattern does not match, is not in the view
        let mut dirs: HashMap<PathBuf, HashMap<FileKey, PathBuf>> = HashMap::new();
        let mut nears: HashMap<&Path, Vec<PathBuf>> = HashMap::new();
        for &index in missing.iter().filter(|index| !found.contains(index)) {
            let path = &self.ids[index];
            // still there, but out of the view now
            if path.symlink_metadata().is_ok() {
                continue;
            }
            let (Some(key), Some(parent)) = (key_of(&index), path.parent()) else {
                continue;
            };
            for dir in nears.entry(parent).or_insert_with(|| near(parent)) {
                let keys = (dirs.entry(dir.clone())).or_insert_with_key(|dir| keys_in(dir, &known));
                if let Some(to) = keys.get(&key) {
                    if claimed.insert(to.clone()) {
                        moves.push((index, to.clone()));
                    }
                    break;
                }
            }
        }
        moves
    }

    /// If [`Koil::current_dir`] (or a dir it is in) is gone from disk, but was renamed or
    /// moved within the dir it was in, follow it there (see [`Koil::repoint`])
    fn follow_dir(&mut self) {
        for _ in 0..self.dirs_seen.len() {
            // from the root down, as the first that is gone takes the rest with it
            let first_gone = (self.dirs_seen.iter().rev())
                .find(|(dir, _)| dir.symlink_metadata().is_err())
                .cloned();
            let Some((gone, key)) = first_gone else {
                return;
            };
            let to = gone
                .parent()
                .and_then(|p| keys_in(p, &HashSet::new()).remove(&key));
            let Some(to) = to else {
                return;
            };
            self.repoint(&gone, &to, None);
        }
    }

    /// `from` was renamed or moved to `to` on disk: every path koil keeps that is `from` or
    /// inside it follows it there (the IDs, the open dir, the changes, and the steps of undo),
    /// but an ID whose new path another ID has already
    /// `from` is the path of the ID `index`, if it is one: if that was a file, no other ID can
    /// be inside it, so they are not all looked at (a thousand files renamed at once took a
    /// second)
    fn repoint(&mut self, from: &Path, to: &Path, index: Option<usize>) {
        let moved = |path: &Path| -> Option<PathBuf> {
            let rest = path.strip_prefix(from).ok()?;
            Some(match rest.as_os_str().is_empty() {
                true => to.to_path_buf(),
                false => to.join(rest),
            })
        };
        let follow = |path: &mut PathBuf| {
            if let Some(new) = moved(path) {
                *path = new;
            }
        };
        match index.filter(|i| self.seen.get(i).is_some_and(|s| !s.meta.is_dir)) {
            Some(index) => self.ids[index] = to.to_path_buf(),
            None => {
                let taken: HashSet<PathBuf> = (self.ids.iter())
                    .filter(|p| p.starts_with(to))
                    .cloned()
                    .collect();
                for path in &mut self.ids {
                    if let Some(new) = moved(path)
                        && !taken.contains(&new)
                    {
                        *path = new;
                    }
                }
            }
        }
        follow(&mut self.current_dir);
        for (dir, _) in &mut self.dirs_seen {
            follow(dir);
        }
        for (before, afters) in self.diff.with_id.values_mut() {
            follow(before);
            afters.iter_mut().for_each(follow);
        }
        let created = std::mem::take(&mut self.diff.without_id);
        for (mut path, is_dir) in created {
            follow(&mut path);
            self.diff.without_id.insert(path, is_dir);
        }
        for applied in &mut self.undo {
            follow(&mut applied.dir);
        }
        for step in self.undo.iter_mut().flat_map(|applied| &mut applied.steps) {
            match step {
                Undo::Trash(path) => follow(path),
                Undo::Restore(trashed) => follow(&mut trashed.original),
                Undo::Rename(a, b) => {
                    follow(a);
                    follow(b);
                }
            }
        }
    }

    /// Resolve a [`ConflictKind::Renamed`]: it stays where it is on disk (`to`), and the rest
    /// of the paths user put it at are copies of it
    fn keep_on_disk(&mut self, index: usize, to: &Path, conflict: &Conflict) -> Vec<Edit> {
        let in_listing = self.current_listing.contains(&index);
        let entry = self.relative(to).map(|name| Entry {
            id: Some(conflict.id),
            name,
            is_dir: conflict.is_dir,
        });
        match (conflict.listed.first(), entry) {
            // user's first entry in the listing becomes it
            (Some(first), Some(entry)) if in_listing => vec![Edit::Change {
                from: first.clone(),
                to: entry,
            }],
            (Some(first), _) => {
                let (_, afters) = (self.diff.with_id.entry(index))
                    .or_insert_with(|| (to.to_path_buf(), Vec::new()));
                afters.push(to.to_path_buf());
                vec![Edit::Remove(first.clone())]
            }
            // the path it is renamed to is the last (see `Diff::compute_actions`)
            (None, entry) => {
                if let Some((before, afters)) = self.diff.with_id.get_mut(&index) {
                    afters.pop();
                    if !afters.iter().any(|a| a == to) {
                        afters.push(to.to_path_buf());
                    }
                    if afters.as_slice() == [before.as_path()] {
                        self.diff.with_id.remove(&index);
                    }
                }
                entry
                    .filter(|_| in_listing)
                    .map(Edit::Add)
                    .into_iter()
                    .collect()
            }
        }
    }
}

/// How many dirs inside a dir [`near`] looks in, so a gone path in a dir with thousands of them
/// is not looked for in each
const NEAR_DIRS: usize = 256;

/// Where to look for what is gone from `dir`, as a file manager moves it: in `dir` (renamed), in
/// a dir inside it (moved into it, if it has at most [`NEAR_DIRS`]), or in the dir it is in
/// (moved up)
fn near(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    if let Ok(items) = fs::read_dir(dir) {
        let inside = (items.flatten())
            .filter(|item| item.file_type().is_ok_and(|t| t.is_dir()))
            .map(|item| item.path())
            .take(NEAR_DIRS + 1);
        let inside: Vec<PathBuf> = inside.collect();
        if inside.len() <= NEAR_DIRS {
            dirs.extend(inside);
        }
    }
    dirs.extend(dir.parent().map(Path::to_path_buf));
    dirs
}

/// The key of each path in `dir` (not inside its dirs) that is not in `known`, but the keys
/// that several share (see [`shared`])
fn keys_in(dir: &Path, known: &HashSet<&Path>) -> HashMap<FileKey, PathBuf> {
    let Ok(items) = fs::read_dir(dir) else {
        return HashMap::new();
    };
    let keys: Vec<(FileKey, PathBuf)> = (items.flatten())
        .map(|item| item.path())
        .filter(|path| !known.contains(path.as_path()))
        .filter_map(|path| Some((FileKey::at(&path)?, path)))
        .collect();
    let ambiguous = shared(keys.iter().map(|&(key, _)| key));
    (keys.into_iter())
        .filter(|(key, _)| !ambiguous.contains(key))
        .collect()
}

/// The keys that more than one of `keys` has, which tell no file apart: hard links share one,
/// and on Windows, where a key is when its file was created, so do files made in the same tick
/// of the clock (by a checkout or an unzip, say)
fn shared(keys: impl IntoIterator<Item = FileKey>) -> HashSet<FileKey> {
    let mut once = HashSet::new();
    keys.into_iter().filter(|&key| !once.insert(key)).collect()
}
