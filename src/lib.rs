use std::{
    collections::HashSet,
    fmt, fs, io,
    path::{Path, PathBuf},
};

pub mod parse;
pub mod planner;

type ID = String;

/// A single file or directory captured from a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Hexadecimal ID
    pub id: ID,
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
    DeleteDir(String),
    /// rm -f <name>
    DeleteFile(String),
    /// mv <src> <dst>
    Rename(String, String),
    /// cp <src> <dst>
    Copy(String, String),
    /// touch <name>
    CreateFile(String),
    /// mkdir -p <name>
    CreateDir(String),
}

impl Action {
    /// A shell command representing what this action would do
    pub fn command(&self) -> String {
        match self {
            Action::CreateFile(n) => format!("touch {}", n),
            Action::CreateDir(n) => format!("mkdir {}", n),
            Action::DeleteFile(n) => format!("rm {}", n),
            Action::DeleteDir(n) => format!("rm -rf {}", n),
            Action::Rename(s, d) => format!("mv {} {}", s, d),
            Action::Copy(s, d) => format!("cp {} {}", s, d),
        }
    }

    /// The name this action **frees** (removes from the filesystem), if any.
    pub fn clears(&self) -> Option<&str> {
        match self {
            Action::DeleteFile(n) | Action::DeleteDir(n) => Some(n),
            Action::Rename(s, _) => Some(s),
            Action::CreateFile(_) | Action::CreateDir(_) | Action::Copy(_, _) => None,
        }
    }

    /// The name this action **creates** (places on the filesystem), if any.
    pub fn creates(&self) -> Option<&str> {
        match self {
            Action::CreateFile(n) | Action::CreateDir(n) | Action::Copy(_, n) => Some(n),
            Action::Rename(_, d) => Some(d),
            Action::DeleteFile(_) | Action::DeleteDir(_) => None,
        }
    }

    /// The name this action requires to exist, if any
    pub fn depends_on(&self) -> Option<&str> {
        match self {
            Action::DeleteFile(n) | Action::DeleteDir(n) => Some(n),
            Action::Rename(s, _) | Action::Copy(s, _) => Some(s),
            Action::CreateFile(_) | Action::CreateDir(_) => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KoilError {
    #[error("`{0}` appears more than once")]
    DuplicatePath(String),

    #[error("Invalid ID: `{0}`, this ID is not recognized")]
    InvalidID(ID),
}

pub struct Koil {
    min_id_len: usize,
    ids: Vec<PathBuf>,
}

impl Default for Koil {
    fn default() -> Self {
        Self::new()
    }
}

impl Koil {
    pub fn new() -> Self {
        Koil {
            min_id_len: 6,
            ids: Vec::new(),
        }
    }

    pub fn open<P: AsRef<Path>>(&mut self, dir: P) -> io::Result<()> {
        for item in fs::read_dir(dir)? {
            let item = item?;
            self.ids.push(item.path());
        }
        Ok(())
    }

    pub fn entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();

        for i in 0..self.ids.len() {
            entries.push(self.get_entry(i).unwrap());
        }
        entries.sort();

        entries
    }

    pub fn write_listing<W: fmt::Write>(&self, out: &mut W) -> fmt::Result {
        for (i, entry) in self.entries().iter().enumerate() {
            if i > 0 {
                out.write_char('\n')?;
            }
            write!(out, "{entry}")?;
        }

        Ok(())
    }

    pub fn listing(&self) -> String {
        let mut s = String::new();
        self.write_listing(&mut s).unwrap();
        s
    }

    pub fn compute_actions(&self, content: String) -> Result<Vec<Action>, KoilError> {
        self.compute_actions_parsed(parse::parse_listing(content))
    }

    pub fn compute_actions_parsed(
        &self,
        parsed: parse::ParsedFile,
    ) -> Result<Vec<Action>, KoilError> {
        self.validate(&parsed)?;

        let mut actions = Vec::new();

        // Deletes
        for i in 0..self.ids.len() {
            let entry = self.get_entry(i).unwrap();
            if !parsed.with_id.contains_key(&entry.id) {
                if entry.is_dir {
                    actions.push(Action::DeleteDir(entry.name.clone()));
                } else {
                    actions.push(Action::DeleteFile(entry.name.clone()));
                }
            }
        }

        // Creates
        for name in parsed.without_id {
            if name.ends_with('/') {
                actions.push(Action::CreateDir(name.clone()));
            } else {
                actions.push(Action::CreateFile(name.clone()));
            }
        }

        // Renames / Copy
        for (id, mut entries) in parsed.with_id {
            let orig = self.get_entry_by_id(&id)?;
            if let Some(i) = entries.iter().position(|e| e.name == orig.name) {
                // remove original name from entries, to avoid copy(A, A)
                entries.swap_remove(i);
            // if the original name no longer exists, then there was a rename
            } else {
                // TODO: make the logic of detecting to who we renamed to smarter
                let renamed_to = entries.pop().unwrap();
                actions.push(Action::Rename(orig.name.to_string(), renamed_to.name));
            }

            // every name that is not original must be a copy
            for entry in entries {
                actions.push(Action::Copy(orig.name.clone(), entry.name));
            }
        }

        // sort the actions in correct order
        Ok(planner::plan_actions(&actions))
    }

    fn validate(&self, parsed: &parse::ParsedFile) -> Result<(), KoilError> {
        let mut seen = HashSet::new();
        for (id, entries) in &parsed.with_id {
            self.get_entry_by_id(id)?;
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

    pub fn get_entry_by_id(&self, id: &str) -> Result<Entry, KoilError> {
        let index = usize::from_str_radix(id, 16).unwrap();
        self.get_entry(index)
            .ok_or(KoilError::InvalidID(id.to_string()))
    }

    pub fn to_id(&self, index: usize) -> String {
        format!("{:0width$x}", index, width = self.min_id_len)
    }

    pub fn format_entry(&self, id: usize) -> String {
        let id_hex = format!("{:0width$x}", id, width = self.min_id_len);
        let path = &self.ids[id];
        let name = path.file_name().unwrap().to_str().unwrap();
        let entry_type = if path.is_dir() { '/' } else { '-' };
        format!("{id_hex}{entry_type} {name}")
    }
}

#[cfg(test)]
mod tests;
