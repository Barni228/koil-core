use crate::serde_path::Repr;
use crate::{Action, on_disk, planner, rebase};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Each ID, with the path it had before, and the paths it has after
type WithId = HashMap<usize, (PathBuf, Vec<PathBuf>)>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    /// ID, which points to the name it had before, and names that now it has after
    #[serde(with = "with_id")]
    pub with_id: WithId,
    /// A new path without ID, true if it is a directory
    #[serde(with = "without_id")]
    pub without_id: HashMap<PathBuf, bool>,
}

/// [`Diff::with_id`], with its paths as [`crate::serde_path`] keeps them
mod with_id {
    use super::*;

    pub(super) fn serialize<S: Serializer>(
        with_id: &WithId,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let kept = with_id.iter().map(|(index, (before, afters))| {
            let afters: Vec<Repr> = afters.iter().map(|p| Repr::from(p.as_path())).collect();
            (index, (Repr::from(before.as_path()), afters))
        });
        serializer.collect_map(kept)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<WithId, D::Error> {
        let kept = HashMap::<usize, (Repr, Vec<Repr>)>::deserialize(deserializer)?;
        let paths = kept.into_iter().map(|(index, (before, afters))| {
            let afters = afters.into_iter().map(PathBuf::from).collect();
            (index, (before.into(), afters))
        });
        Ok(paths.collect())
    }
}

/// [`Diff::without_id`] as pairs, with its paths as [`crate::serde_path`] keeps them: one that
/// is not Unicode can not be a key in JSON
mod without_id {
    use super::*;

    pub(super) fn serialize<S: Serializer>(
        without_id: &HashMap<PathBuf, bool>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            without_id
                .iter()
                .map(|(p, is_dir)| (Repr::from(p.as_path()), is_dir)),
        )
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<HashMap<PathBuf, bool>, D::Error> {
        let kept = Vec::<(Repr, bool)>::deserialize(deserializer)?;
        Ok(kept
            .into_iter()
            .map(|(p, is_dir)| (p.into(), is_dir))
            .collect())
    }
}

impl Diff {
    /// Set the path this index had before, keeping its afters
    pub fn set_before(&mut self, index: usize, before_path: PathBuf) {
        self.with_id.entry(index).or_default().0 = before_path;
    }

    /// Add an after value for this index, if it is not there already
    pub fn push_after(&mut self, index: usize, after_path: PathBuf) {
        let afters = &mut self.with_id.entry(index).or_default().1;
        if !afters.contains(&after_path) {
            afters.push(after_path);
        }
    }

    /// Whether this diff changes nothing: every ID is only at the path it had (or not in it at
    /// all), and nothing is new
    pub fn is_empty(&self) -> bool {
        self.without_id.is_empty()
            && (self.with_id.values())
                .all(|(before, afters)| afters.as_slice() == [before.as_path()])
    }

    /// Return true if `path` is a dir that does not exist yet, but will after this diff
    /// Either it is created, or something new is placed inside it
    pub fn creates_dir(&self, path: &Path) -> bool {
        let afters = self.with_id.values().flat_map(|(_, afters)| afters);
        self.without_id.get(path) == Some(&true)
            || (self.without_id.keys().chain(afters)).any(|p| p != path && p.starts_with(path))
    }

    /// Dirs that are neither on disk nor created in this diff, but something new is placed
    /// inside them, so they must be created too
    /// A file that is moved away or deleted is not on disk then, so a dir is made where it was
    pub fn missing_parents(&self) -> BTreeSet<PathBuf> {
        // a set, as thousands of paths can be moved into a new dir
        let afters: HashSet<&Path> = (self.with_id.values())
            .flat_map(|(_, a)| a.iter().map(PathBuf::as_path))
            .collect();
        let gone = self.gone_files();
        let mut missing = BTreeSet::new();
        for path in self
            .without_id
            .keys()
            .map(PathBuf::as_path)
            .chain(afters.iter().copied())
        {
            for parent in path.ancestors().skip(1) {
                let exists = parent.as_os_str().is_empty()
                    || on_disk(parent) && !gone.contains(parent)
                    || self.without_id.contains_key(parent)
                    // a dir that was renamed or copied to this path
                    || afters.contains(parent);
                // if it was added already, so were its parents
                if exists || !missing.insert(parent.to_path_buf()) {
                    break;
                }
            }
        }
        missing
    }

    /// The files this diff moves away or deletes, which have nothing inside, so what is put
    /// inside one goes in a new dir made there (see [`planner::order`])
    fn gone_files(&self) -> HashSet<&Path> {
        (self.with_id.values())
            .filter(|(before, afters)| !afters.contains(before) && !before.is_dir())
            .map(|(before, _)| before.as_path())
            .collect()
    }

    /// Compute actions required to resolve this diff, in the order they can run
    pub fn compute_actions(self) -> Vec<Action> {
        planner::plan_actions(&self.actions(), on_disk, Path::is_dir)
    }

    /// The actions required to resolve this diff, in no particular order, and with rename
    /// cycles not broken yet (see [`planner::plan_actions`])
    pub fn actions(self) -> Vec<Action> {
        let mut actions: Vec<Action> = self
            .missing_parents()
            .into_iter()
            .map(Action::CreateDir)
            .collect();

        // Creates
        for (path, is_dir) in self.without_id {
            if is_dir {
                actions.push(Action::CreateDir(path))
            } else {
                actions.push(Action::CreateFile(path))
            }
        }

        for (_id, (before, mut after)) in self.with_id {
            // Deletes
            // if this id no longer has a path, then it was removed
            if after.is_empty() {
                if before.is_dir() {
                    actions.push(Action::DeleteDir(before.clone()));
                } else {
                    actions.push(Action::DeleteFile(before.clone()));
                }
                continue;
            }

            // Renames / Copy
            // if the old path still exists, remove it, to avoid copy(A, A)
            if let Some(i) = after.iter().position(|p| p == &before) {
                after.swap_remove(i);
            // if the original name no longer exists, then there was a rename
            } else {
                // the last one, which `sync` relies on (see `keep_on_disk`)
                let renamed_to = after.pop().unwrap();
                actions.push(Action::Rename(before.clone(), renamed_to));
            }

            // every name that is not original must be a copy
            for path in after {
                actions.push(Action::Copy(before.clone(), path));
            }
        }

        actions
    }
}

/// What a [`Diff`] does to the paths it changes, to check a path written in a listing against
/// the rest of it (see [`Diff::outcome`])
pub struct Outcome<'a> {
    /// The paths it makes or leaves a file at
    pub files: HashSet<&'a Path>,
    /// The files it moves away or deletes, see [`Diff::gone_files`]
    pub gone: HashSet<&'a Path>,
    /// The dirs it deletes
    deleted: HashSet<&'a Path>,
    /// Where each dir it renames goes, as [`Diff::actions`] picks it
    renamed: HashMap<&'a Path, &'a Path>,
}

impl Diff {
    /// See [`Outcome`]
    pub fn outcome(&self) -> Outcome<'_> {
        let mut files: HashSet<&Path> = (self.without_id.iter())
            .filter(|(_, is_dir)| !**is_dir)
            .map(|(path, _)| path.as_path())
            .collect();
        let mut deleted = HashSet::new();
        let mut renamed = HashMap::new();
        for (before, afters) in self.with_id.values() {
            if !before.is_dir() {
                files.extend(afters.iter().map(PathBuf::as_path));
                continue;
            }
            match afters.last() {
                None => {
                    deleted.insert(before.as_path());
                }
                Some(to) if !afters.contains(before) => {
                    renamed.insert(before.as_path(), to.as_path());
                }
                _ => {}
            }
        }
        Outcome {
            files,
            gone: self.gone_files(),
            deleted,
            renamed,
        }
    }
}

impl Outcome<'_> {
    /// Where the rename of `path` takes it, `None` if it is not renamed
    pub fn renamed_to(&self, path: &Path) -> Option<&Path> {
        self.renamed.get(path).copied()
    }

    /// Each path that what is written at `path` is at while the changes are applied, `path`
    /// first: a path inside a dir that is renamed is inside the old one, so it goes with it
    /// (see [`planner::plan_actions`]), the deepest dir first
    pub fn carried(&self, path: &Path) -> Vec<PathBuf> {
        let mut at = vec![path.to_path_buf()];
        // each rename carries it at most once, unless dirs are moved into each other
        for _ in 0..self.renamed.len() {
            let last = at.last().unwrap();
            let renamed = |dir: &Path| Some((dir.to_path_buf(), *self.renamed.get(dir)?));
            let Some((dir, to)) = last.ancestors().skip(1).find_map(renamed) else {
                break;
            };
            let next = rebase(last, &dir, to).unwrap();
            at.push(next);
        }
        at
    }

    /// The deleted dir that what is written at `path` goes into, also by being in a dir that
    /// is moved into it, `None` if it goes into none
    pub fn deleted_around(&self, path: &Path) -> Option<&Path> {
        if self.deleted.is_empty() {
            return None;
        }
        (self.carried(path).iter())
            .find_map(|p| p.ancestors().skip(1).find_map(|d| self.deleted.get(d)))
            .copied()
    }
}
