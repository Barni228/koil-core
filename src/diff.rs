use crate::Action;
use crate::planner;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    /// ID, which points to the name it had before, and names that now it has after
    pub with_id: HashMap<usize, (PathBuf, Vec<PathBuf>)>,
    /// A new path without ID, true if it is a directory
    pub without_id: HashMap<PathBuf, bool>,
}

impl Diff {
    /// Add a before value for this index
    pub fn add_before(&mut self, index: usize, before_path: PathBuf) {
        self.with_id.entry(index).or_default().0 = before_path;
    }

    /// Add a before value for this index
    /// Basically `add_before(index, paths[index])`
    pub fn add_before_from(&mut self, index: usize, paths: &[PathBuf]) {
        self.add_before(index, paths[index].clone());
    }

    /// Add an after value for this index, if it is not there already
    pub fn push_after(&mut self, index: usize, after_path: PathBuf) {
        let afters = &mut self.with_id.entry(index).or_default().1;
        if !afters.contains(&after_path) {
            afters.push(after_path);
        }
    }

    /// Return true if `path` is a dir that does not exist yet, but will after this diff
    /// Either it is created, or something new is placed inside it
    pub fn creates_dir(&self, path: &Path) -> bool {
        let afters = self.with_id.values().flat_map(|(_, afters)| afters);
        self.without_id.get(path) == Some(&true)
            || (self.without_id.keys().chain(afters)).any(|p| p != path && p.starts_with(path))
    }

    /// Compute actions required to resolve this diff
    pub fn compute_actions(self) -> Vec<Action> {
        let mut actions = Vec::new();

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
                // TODO: make the logic of detecting to who we renamed smarter
                let renamed_to = after.pop().unwrap();
                actions.push(Action::Rename(before.clone(), renamed_to));
            }

            // every name that is not original must be a copy
            for path in after {
                actions.push(Action::Copy(before.clone(), path));
            }
        }

        // sort the actions in correct order
        // symlink_metadata().is_ok() checks if path OR SYMLINK exists there
        planner::plan_actions(&actions, |p| p.symlink_metadata().is_ok())
    }
}
