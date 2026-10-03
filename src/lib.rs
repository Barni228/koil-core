#![doc = include_str!("../README.md")]

use crate::apply::{Undo, inside, same_file};
use crate::diff::Diff;
use globset::{GlobBuilder, GlobMatcher};
use ignore::{WalkBuilder, WalkState};
use regex::Regex;
use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf, is_separator};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{fmt, fs, io};
use typed_builder::TypedBuilder;

pub mod apply;
pub mod diff;
mod names;
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

    /// Hide paths that git ignores (`.gitignore` files, `.git/info/exclude`, and the global
    /// excludes file), and the `.git` dir
    /// Like git, it only works inside a git repo, and reads `.gitignore` files up to its root
    /// Ignored entries that were changed are always shown, so the change is not lost
    pub respect_gitignore: bool,

    /// [`Koil::open`] reads patterns as regexes, instead of globs
    /// A pattern that is already open stays as it is until something else is opened
    pub regex: bool,
}

/// How much opening a pattern can read, so one that matches too much (like `.*` in a home dir)
/// stops early, instead of reading every path, and the listing stays short enough to edit
/// A plain dir has no limits
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// How many paths a pattern can match
    pub matches: usize,
    /// How many paths can be read to find them (dirs that nothing inside can match are skipped)
    pub searched: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            matches: 10_000,
            searched: 100_000,
        }
    }
}

impl Limits {
    /// Fail if `pattern` went over a limit, with `matches` paths found in `searched` ones
    fn check(&self, pattern: &Pattern, matches: usize, searched: usize) -> Result<(), OverLimit> {
        let pattern = || pattern.as_str().to_string();
        if matches > self.matches {
            return Err(OverLimit::Matches {
                pattern: pattern(),
                limit: self.matches,
            });
        }
        if searched > self.searched {
            return Err(OverLimit::Searched {
                pattern: pattern(),
                limit: self.searched,
            });
        }
        Ok(())
    }
}

/// A pattern of the paths to show, relative to [`Koil::current_dir`]
/// It shows every file and dir that matches it, a dir is matched with a `/` after its path
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pattern {
    /// Like `**/*.rs`, where `*` never matches `/`, and `\*` is a `*` (also on Windows, where
    /// a pattern's parts are split only at `/`)
    Glob(String),
    /// Like `src/.*\.rs`, which must match the whole path
    /// `,` means any character except `/` (like `.`, but within one dir), and `\,` is a `,`
    Regex(String),
}

impl Pattern {
    /// The pattern, as it was written
    pub fn as_str(&self) -> &str {
        match self {
            Pattern::Glob(p) | Pattern::Regex(p) => p,
        }
    }

    /// Whether `part` of a path has a special character of this kind of pattern
    fn is_pattern(syntax_regex: bool, part: &str) -> bool {
        match syntax_regex {
            false => part.contains(['*', '?', '[', '{']),
            true => part.contains([
                '.', ',', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '^', '$', '\\',
            ]),
        }
    }

    fn matcher(&self) -> Result<Matcher, OpenError> {
        match self {
            Pattern::Glob(glob) => {
                // globset reads `**/` as `**`, so a trailing `/` is matched apart
                let (rest, slash) = match glob.strip_suffix('/') {
                    Some(rest) => (rest, true),
                    None => (glob.as_str(), false),
                };
                GlobBuilder::new(rest)
                    .literal_separator(true)
                    // globset's default is off on Windows, where `\` is a separator, but
                    // patterns use only `/`
                    .backslash_escape(true)
                    .build()
                    .map(|g| Matcher::Glob(g.compile_matcher(), slash))
                    .map_err(|source| OpenError::InvalidGlob {
                        glob: glob.clone(),
                        source,
                    })
            }
            Pattern::Regex(regex) => {
                let invalid = |source| OpenError::InvalidRegex {
                    regex: regex.clone(),
                    source,
                };
                // checked as it was written first, so errors point at what user wrote
                Regex::new(regex).map_err(invalid)?;
                // the whole path must match
                let whole = format!("^(?:{})$", expand_commas(regex));
                let whole = Regex::new(&whole).map_err(invalid)?;
                Ok(Matcher::Regex(whole))
            }
        }
    }
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
    /// The open dir is neither on disk nor new in the diff anymore (when it is opened again,
    /// like after an apply), so its closest parent was opened instead
    DirNotFound {
        /// The dir that user wanted to open
        requested: PathBuf,
        /// The dir that was opened instead
        opened: PathBuf,
    },

    /// The open pattern went over [`Limits`] when it was opened again (like after showing
    /// hidden paths), so its base dir was opened instead
    OverLimit {
        error: OverLimit,
        /// The dir that was opened instead
        opened: PathBuf,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::DirNotFound { requested, opened } => write!(
                f,
                "`{}` is not a directory, opened `{}` instead",
                requested.display(),
                opened.display()
            ),
            Warning::OverLimit { error, opened } => {
                write!(f, "{error}, opened `{}` instead", opened.display())
            }
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

/// Why [`Koil::open`] failed, nothing was changed
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("Can not read the dir")]
    Io(#[from] io::Error),

    /// The first part of the path that is neither on disk nor new in the diff
    #[error("`{}` does not exist", .0.display())]
    NotFound(PathBuf),

    /// The first part of the path that is a file, on disk or new in the diff
    #[error("`{}` is a file, not a directory", .0.display())]
    NotADirectory(PathBuf),

    #[error("`{glob}` is not a valid glob pattern")]
    InvalidGlob {
        glob: String,
        #[source]
        source: globset::Error,
    },

    #[error("`{regex}` is not a valid regex")]
    InvalidRegex {
        regex: String,
        #[source]
        source: regex::Error,
    },

    #[error(transparent)]
    OverLimit(#[from] OverLimit),
}

/// A pattern matched or searched more paths than [`Limits`] allow
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverLimit {
    #[error("`{pattern}` matches more than {limit} paths")]
    Matches { pattern: String, limit: usize },

    #[error("`{pattern}` has to search more than {limit} paths")]
    Searched { pattern: String, limit: usize },
}

/// Why [`Koil::update`] rejected the entries, nothing was changed
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", join_lines(errors))]
pub struct UpdateError {
    /// Every problem that was found, in the order of the entries
    pub errors: Vec<EntryError>,
    /// Every warning that was found, in the order of the entries, like [`Koil::update`] returns
    /// when it works
    pub warnings: Vec<EntryWarning>,
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

    /// Something that is not deleted or moved away is already at this path, maybe hidden or
    /// ignored, or with a name that only differs in case on a filesystem that ignores case
    #[error("`{}` already exists", .0.display())]
    AlreadyExists(PathBuf),

    /// The entry is a dir copied or moved to a path inside it: the move would fail, and the
    /// copy would copy itself again, until the path is too long
    #[error("`{}` would copy or move a dir into itself", .0.display())]
    IntoItself(PathBuf),

    /// A part of the name is longer than most filesystems allow
    #[error("`{name}` is {len} bytes long, but a name can be at most 255")]
    NameTooLong { name: String, len: usize },

    /// A part of the name has a control character, which breaks terminals and scripts
    #[error("`{}` has the control character {char:?}", name.escape_debug())]
    ControlCharacter { name: String, char: char },

    /// On Windows, a name it can not use (elsewhere only a warning): a character it does not
    /// allow, a reserved name, or a `.` at the end, which it would remove
    #[error("{0}")]
    WindowsName(EntryWarningKind),
}

/// Something about one of the entries given to [`Koil::update`] that is not recommended, but
/// does not stop the update
/// Only names that do not exist yet are checked
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Entry {entry}: {kind}")]
pub struct EntryWarning {
    /// Index of the entry in the entries given to [`Koil::update`]
    pub entry: usize,
    pub kind: EntryWarningKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryWarningKind {
    #[error("`{name}` has `{chars}`, which can not be used in names on Windows")]
    WindowsCharacter { name: String, chars: String },

    #[error("`{name}` has `{chars}`, which must be quoted in a shell")]
    ShellCharacter { name: String, chars: String },

    /// Like `CON` or `nul.txt`
    #[error("`{name}` is a reserved name on Windows")]
    WindowsReservedName { name: String },

    #[error("`{name}` has emoji, which some programs and terminals do not show well")]
    Emoji { name: String },

    /// An invisible character, or one that looks like a different one, like a no-break space
    #[error(
        "`{}` has {char:?}, which is invisible, or looks like a different character",
        name.escape_debug()
    )]
    UnusualCharacter { name: String, char: char },

    #[error("`{name}` starts or ends with a space, which is easy to miss")]
    SpaceAtEdge { name: String },

    #[error("`{name}` ends with `.`, which Windows removes")]
    TrailingDot { name: String },

    #[error("`{name}` starts with `-`, so commands can read it as an option")]
    LeadingDash { name: String },

    #[error("`{name}` is not valid UTF-8")]
    NotUtf8 { name: String },
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

    #[builder(default)]
    #[serde(default)]
    /// How much opening a pattern can read
    limits: Limits,

    // Private fields
    #[builder(default, setter(skip))]
    /// All IDs, pointing to their corresponding path
    ids: Vec<PathBuf>,

    #[builder(default, setter(skip))]
    /// The saved indexes that are shows in the current listing
    current_listing: HashSet<usize>,

    #[builder(default, setter(skip))]
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
    /// The dir (or the base dir of the pattern) must be on disk or new in the diff, else it
    /// fails with [`OpenError::NotFound`], or [`OpenError::NotADirectory`] if it is a file
    /// This never adds changes, new dirs must be written in the listing
    pub fn open<P: AsRef<Path>>(&mut self, location: P) -> Result<(), OpenError> {
        let location = location.as_ref();
        let path = resolve(&self.current_dir.join(location))?;
        let split = match self.is_dir(&path) {
            // so a dir with a name like `a[1]` is not taken for a pattern
            true => None,
            false => self.split_pattern(location),
        };
        let (base, pattern) = match split {
            Some((base, pattern)) => {
                pattern.matcher()?;
                (base, Some(pattern))
            }
            None => (path, None),
        };
        if !self.is_dir(&base) {
            return Err(self.not_a_dir(&base));
        }
        // in a copy, so nothing changes if it fails
        let mut opened = self.clone();
        opened.open_at(base, pattern)?;
        *self = opened;
        Ok(())
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
    /// Starts with [`Entry::parent`] if [`Settings::show_hidden`] is on, then existing entries
    /// (dirs, then files, each sorted by name), then new entries
    pub fn listing(&self) -> Vec<Entry> {
        let view = self.view();
        let in_view = |index: usize, path: &Path| view.contains(path, self.ids[index].is_dir());
        let entry = |index: usize, path: &Path| Entry {
            id: Some(to_id(index)),
            name: self.name(path),
            is_dir: self.ids[index].is_dir(),
        };

        let mut entries = Vec::new();

        // IDs from current listing
        for &index in &self.current_listing {
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

        // IDs from other folders
        for (&index, (_before, afters)) in &self.diff.with_id {
            // if it is in current_listing, then I already added it above
            if self.current_listing.contains(&index) {
                continue;
            }
            entries.extend(
                afters
                    .iter()
                    .filter(|p| in_view(index, p))
                    .map(|p| entry(index, p)),
            );
        }

        // dirs first, then by name
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

        // dirs that are created because something new is inside them are shown too
        let parents = self.diff.missing_parents().into_iter().map(|p| (p, true));
        let created = self
            .diff
            .without_id
            .iter()
            .map(|(p, &is_dir)| (p.clone(), is_dir));
        let mut new: Vec<Entry> = created
            .chain(parents)
            .filter(|(p, is_dir)| view.contains(p, *is_dir))
            .map(|(p, is_dir)| Entry {
                id: None,
                name: self.name(&p),
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
    /// Returns warnings about names that can be used, but are not recommended
    pub fn update(&mut self, entries: &[Entry]) -> Result<Vec<EntryWarning>, UpdateError> {
        let mut updated = self.clone();
        let (mut errors, mut warnings) = updated.update_entries(entries);
        errors.sort_by_key(|e| e.entry);
        warnings.sort_by_key(|w| w.entry);
        if !errors.is_empty() {
            return Err(UpdateError { errors, warnings });
        }
        *self = updated;
        Ok(warnings)
    }

    /// What [`Koil::update`] would return for `entries`, without changing anything
    /// A frontend can use it to show problems while user is still editing
    pub fn check(&self, entries: &[Entry]) -> Result<Vec<EntryWarning>, UpdateError> {
        self.clone().update(entries)
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

    /// Open [`Koil::location`] again, so it shows what is on disk now
    /// Does nothing if nothing was opened yet
    /// If the open pattern goes over [`Limits`] now, its base dir is opened instead
    fn reopen(&mut self) -> io::Result<Option<Warning>> {
        if self.current_dir.as_os_str().is_empty() {
            return Ok(None);
        }
        // not parsed again, so a pattern stays a glob or a regex, even if the settings changed
        match self.open_at(self.current_dir.clone(), self.pattern.clone()) {
            Err(OpenError::OverLimit(error)) => {
                self.pattern = None;
                self.read_listing().map_err(io_error)?;
                Ok(Some(Warning::OverLimit {
                    error,
                    opened: self.current_dir.clone(),
                }))
            }
            result => result.map_err(io_error),
        }
    }

    /// Open `pattern` (a valid one) inside `base`, or just `base` if there is no pattern
    /// See [`Koil::open`], which checks that `base` is a dir first, so only when reopening can
    /// it be gone: then its closest parent that is a dir is opened instead, with a warning
    fn open_at(
        &mut self,
        base: PathBuf,
        pattern: Option<Pattern>,
    ) -> Result<Option<Warning>, OpenError> {
        let dir = base
            .ancestors()
            .find(|p| self.is_dir(p))
            .ok_or(io::Error::from(io::ErrorKind::NotFound))?
            .to_path_buf();
        let found = dir == base;
        let warning = (!found).then(|| Warning::DirNotFound {
            requested: base,
            opened: dir.clone(),
        });

        self.current_dir = dir;
        // the pattern is relative to its base dir, so it can not be used in another dir
        self.pattern = pattern.filter(|_| found);
        self.read_listing()?;
        Ok(warning)
    }

    /// Read [`Koil::current_listing`] for the open dir or pattern from the filesystem
    fn read_listing(&mut self) -> Result<(), OpenError> {
        self.current_listing.clear();
        let view = self.view();

        let paths = match self.current_dir.is_dir() {
            true => self.walk(&view)?,
            // a new dir that is not created yet
            false => Vec::new(),
        };
        // a pattern can show thousands of paths, so they are not searched for in `ids` one by one
        let known: HashMap<&Path, usize> = (self.ids.iter().enumerate())
            .map(|(index, path)| (path.as_path(), index))
            .collect();
        let mut new = Vec::new();
        for path in paths {
            if self.ignore.contains(&path) {
                continue;
            }
            match known.get(path.as_path()) {
                Some(&index) => {
                    self.current_listing.insert(index);
                }
                None => new.push(path),
            }
        }
        for path in new {
            self.ids.push(path);
            self.current_listing.insert(self.ids.len() - 1);
        }

        // a changed entry stays, even if it is hidden, so it is not taken for deleted in
        // the next update
        for (&index, (before, _afters)) in &self.diff.with_id {
            if view.contains(before, self.ids[index].is_dir()) {
                self.current_listing.insert(index);
            }
        }

        Ok(())
    }

    /// Every path on disk that `view` shows, without hidden entries if they are not shown,
    /// and without ignored paths if [`Settings::respect_gitignore`] is on
    /// Fails if [`Koil::current_dir`] can not be read, but dirs inside it that can not be read
    /// are skipped, or if a pattern goes over [`Limits`]
    fn walk(&self, view: &View) -> Result<Vec<PathBuf>, OpenError> {
        fs::read_dir(&self.current_dir)?;
        let max_depth = match &self.pattern {
            None => Some(1),
            // without `**`, a glob can only match paths with as many parts as it has
            Some(Pattern::Glob(glob)) if !glob.contains("**") => {
                Some(Path::new(glob).components().count())
            }
            Some(_) => None,
        };
        let gitignore = self.settings.respect_gitignore;
        let walk = WalkBuilder::new(&self.current_dir)
            .max_depth(max_depth)
            .hidden(!self.settings.show_hidden)
            // only what git ignores, not the `.ignore` files of ripgrep
            .ignore(false)
            .git_ignore(gitignore)
            .git_global(gitignore)
            .git_exclude(gitignore)
            .parents(gitignore)
            // git never shows its own dir
            .filter_entry(move |item| !(gitignore && item.file_name() == ".git"))
            .build_parallel();

        let prune = Prune::new(view);
        let (pattern, limits) = (self.pattern.as_ref(), self.limits);
        let paths = Mutex::new(Vec::new());
        let (searched, matched) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let over_limit = Mutex::new(None);
        walk.run(|| {
            Box::new(|item| {
                // what can not be read is skipped
                let Ok(item) = item else {
                    return WalkState::Continue;
                };
                // the walk starts with the dir itself
                if item.depth() == 0 {
                    return WalkState::Continue;
                }
                let is_dir = item.file_type().is_some_and(|t| t.is_dir());
                let skip_inside = is_dir && prune.as_ref().is_some_and(|p| p.skips_inside(&item));
                let searched = searched.fetch_add(1, Ordering::Relaxed) + 1;
                let matches = match view.contains(item.path(), is_dir) {
                    true => {
                        paths.lock().unwrap().push(item.into_path());
                        matched.fetch_add(1, Ordering::Relaxed) + 1
                    }
                    false => matched.load(Ordering::Relaxed),
                };
                if let Some(pattern) = pattern
                    && let Err(error) = limits.check(pattern, matches, searched)
                {
                    *over_limit.lock().unwrap() = Some(error);
                    return WalkState::Quit;
                }
                match skip_inside {
                    true => WalkState::Skip,
                    false => WalkState::Continue,
                }
            })
        });

        if let Some(error) = over_limit.into_inner().unwrap() {
            return Err(error.into());
        }
        let mut paths = paths.into_inner().unwrap();
        // the walk runs on many threads, in no order, and new IDs are given in this one
        paths.sort();
        Ok(paths)
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

    /// Split `location` (as given to [`Koil::open`]) into its base dir, and the pattern relative
    /// to it, which is a glob, or a regex if [`Settings::regex`] is on
    /// `None` if it is not a pattern, because no part of it after the longest part that is a
    /// dir has a special character
    /// Only a `/` (or the root, like `/` or `C:\`) ends a part, so on Windows a `\` after the
    /// base dir stays in the pattern, where it is an escape
    fn split_pattern(&self, location: &Path) -> Option<(PathBuf, Pattern)> {
        let bytes = location.as_os_str().as_encoded_bytes();
        // the longest part before a `/` that is a dir, else the root (nothing, if relative),
        // and what comes after it
        let (dir, rest) = location
            .ancestors()
            .filter(|a| a.parent().is_none() || bytes.get(a.as_os_str().len()) == Some(&b'/'))
            .find_map(|a| {
                let dir = resolve(&self.current_dir.join(a)).ok()?;
                let rest = &bytes[a.as_os_str().len()..];
                self.is_dir(&dir).then_some((dir, rest))
            })?;
        let rest = String::from_utf8_lossy(rest);
        let parts: Vec<&str> = rest
            .split('/')
            .filter(|p| !matches!(*p, "" | "."))
            .collect();
        let regex = self.settings.regex;
        let first = parts.iter().position(|p| Pattern::is_pattern(regex, p))?;
        // not `join`, which with no parts adds a separator at the end, and then on Windows
        // `location` would put the pattern after a `\`, where it is an escape
        let mut base = dir;
        base.extend(&parts[..first]);
        let mut pattern = parts[first..].join("/");
        // which makes it match only dirs
        if rest.ends_with('/') {
            pattern.push('/');
        }
        let pattern = match regex {
            true => Pattern::Regex(pattern),
            false => Pattern::Glob(pattern),
        };
        Some((base, pattern))
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
    /// Returns every error and warning found, if there are errors, `self` is left half updated
    fn update_entries(&mut self, entries: &[Entry]) -> (Vec<EntryError>, Vec<EntryWarning>) {
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
                self.diff.add_before_from(index, &self.ids);
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

        (errors, warnings)
    }

    /// Whether something is at `path` on disk, which is not deleted or moved away in
    /// [`Koil::diff`], so `path` can not be created there
    /// `index` is the ID of the entry that is written at `path`, if it has one
    fn taken(&self, path: &Path, index: Option<usize>) -> bool {
        if path.symlink_metadata().is_err() {
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
            .filter(|p| p.symlink_metadata().is_err())
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
            if parent.symlink_metadata().is_ok() {
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

/// Which paths a listing shows
struct View {
    /// The open dir, or the base dir of the pattern
    base: PathBuf,
    matcher: Option<Matcher>,
}

impl View {
    /// Whether `path` is shown in the listing
    fn contains(&self, path: &Path, is_dir: bool) -> bool {
        match &self.matcher {
            None => path.parent() == Some(&self.base),
            Some(matcher) => path
                .strip_prefix(&self.base)
                .is_ok_and(|p| matcher.is_match(p, is_dir)),
        }
    }
}

/// A compiled [`Pattern`]
enum Matcher {
    /// The glob without its trailing `/`, and whether it had one
    Glob(GlobMatcher, bool),
    Regex(Regex),
}

impl Matcher {
    /// Whether `path`, relative to the base dir, matches (see [`match_text`])
    fn is_match(&self, path: &Path, is_dir: bool) -> bool {
        let text = match_text(path, is_dir);
        match self {
            Matcher::Glob(glob, true) => text.strip_suffix('/').is_some_and(|t| glob.is_match(t)),
            Matcher::Glob(glob, false) => glob.is_match(&text),
            Matcher::Regex(regex) => regex.is_match(&text),
        }
    }

    /// The regex that [`Matcher::is_match`] uses, as a DFA, `None` if it would be too big
    fn dfa(&self) -> Option<dense::DFA<Vec<u32>>> {
        // a glob is matched as bytes, where `.` also matches a new line, like globset does
        let (regex, glob) = match self {
            Matcher::Glob(glob, _) => (glob.glob().regex(), true),
            Matcher::Regex(regex) => (regex.as_str(), false),
        };
        let size = Some(DFA_SIZE_LIMIT);
        let config = dense::Config::new()
            .start_kind(StartKind::Anchored)
            .determinize_size_limit(size)
            .dfa_size_limit(size);
        dense::Builder::new()
            .configure(config)
            .syntax(syntax::Config::new().utf8(!glob).dot_matches_new_line(glob))
            .build(regex)
            .ok()
    }
}

/// How many bytes a pattern's DFA can take, a bigger one skips no dirs
const DFA_SIZE_LIMIT: usize = 1 << 20;

/// The text a pattern is matched against: `path` (relative to the base dir) with a `/` between
/// its parts, and after it if it is a dir
fn match_text(path: &Path, is_dir: bool) -> String {
    let parts: Vec<_> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    let mut text = parts.join("/");
    if is_dir {
        text.push('/');
    }
    text
}

/// Which dirs a walk of a pattern does not need to enter, since nothing inside them can match
struct Prune {
    /// The base dir of the pattern
    base: PathBuf,
    /// [`Matcher::dfa`], which tells when no text that starts with a dir's can match
    dfa: dense::DFA<Vec<u32>>,
}

impl Prune {
    /// `None` without a pattern, or if its DFA would be too big, then every dir is entered
    fn new(view: &View) -> Option<Prune> {
        Some(Prune {
            base: view.base.clone(),
            dfa: view.matcher.as_ref()?.dfa()?,
        })
    }

    /// Whether nothing inside the dir `item` can match
    fn skips_inside(&self, item: &ignore::DirEntry) -> bool {
        let Ok(path) = item.path().strip_prefix(&self.base) else {
            return false;
        };
        // the text of every path inside is the dir's (its `/` included), and then more
        let text = match_text(path, true);
        let input = Input::new(&text).anchored(Anchored::Yes);
        let Ok(mut state) = self.dfa.start_state_forward(&input) else {
            return false;
        };
        for &byte in text.as_bytes() {
            state = self.dfa.next_state(state, byte);
            if self.dfa.is_quit_state(state) {
                return false;
            }
            if self.dfa.is_dead_state(state) {
                return true;
            }
        }
        (0..=u8::MAX).all(|byte| self.dfa.is_dead_state(self.dfa.next_state(state, byte)))
    }
}

/// `error` as an [`io::Error`], for what can not fail any other way
fn io_error(error: OpenError) -> io::Error {
    match error {
        OpenError::Io(error) => error,
        error => io::Error::other(error),
    }
}

fn to_id(index: usize) -> Id {
    Id(index as u64)
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

/// `regex` with every `,` replaced by `[^/]` (any character except `/`), except an escaped `\,`,
/// and a `,` inside a `[...]` class or a `{...}` repetition, which keeps its usual meaning
fn expand_commas(regex: &str) -> String {
    let mut expanded = String::with_capacity(regex.len());
    let mut chars = regex.chars().peekable();
    // how many classes are open, they can be nested like `[a[bc]]`
    let mut classes = 0;
    let mut in_repetition = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                expanded.push(c);
                expanded.extend(chars.next());
                continue;
            }
            '[' => {
                expanded.push(c);
                classes += 1;
                // `]` right after `[` or `[^` is a literal `]`, not the end of the class
                expanded.extend(chars.next_if_eq(&'^'));
                expanded.extend(chars.next_if_eq(&']'));
                continue;
            }
            ']' if classes > 0 => classes -= 1,
            '{' if classes == 0 => in_repetition = true,
            '}' if classes == 0 => in_repetition = false,
            ',' if classes == 0 && !in_repetition => {
                expanded.push_str("[^/]");
                continue;
            }
            _ => {}
        }
        expanded.push(c);
    }
    expanded
}

/// `path` with `/` instead of `\` on Windows, where both are separators, and users write `/`
/// Not if it is verbatim (`\\?\`), where `/` is not a separator, or not Unicode
fn with_slashes(path: &Path) -> PathBuf {
    let verbatim =
        matches!(path.components().next(), Some(Component::Prefix(p)) if p.kind().is_verbatim());
    match path.to_str() {
        Some(s) if cfg!(windows) && !verbatim => s.replace('\\', "/").into(),
        _ => path.to_path_buf(),
    }
}

/// Like [`Path::canonicalize`], but `path` does not need to exist
/// Existing part of the path is canonicalized, the rest is normalized without touching the filesystem
/// On Windows, it has no `\\?\` before it (which `canonicalize` adds, and frontends would show),
/// unless the path can not work without it
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
                    resolved = dunce::canonicalize(&resolved)?;
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests;
