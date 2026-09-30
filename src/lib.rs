use crate::apply::{Undo, same_file};
use crate::diff::Diff;
use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
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

/// A pattern of the paths to show, relative to [`Koil::current_dir`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pattern {
    /// Like `**/*.rs`, where `*` never matches `/`
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
            Pattern::Glob(glob) => GlobBuilder::new(glob)
                .literal_separator(true)
                .build()
                .map(|g| Matcher::Glob(g.compile_matcher()))
                .map_err(|source| OpenError::InvalidGlob {
                    glob: glob.clone(),
                    source,
                }),
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

/// Why [`Koil::open`] failed, nothing was changed
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("Can not read the dir")]
    Io(#[from] io::Error),

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

    /// A part of the name is longer than most filesystems allow
    #[error("`{name}` is {len} bytes long, but a name can be at most 255")]
    NameTooLong { name: String, len: usize },

    /// A part of the name has a control character, which breaks terminals and scripts
    #[error("`{}` has the control character {char:?}", name.escape_debug())]
    ControlCharacter { name: String, char: char },
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
    /// A dir can also be a new dir from [`Koil::diff`] that does not exist yet,
    /// then it is opened with an empty listing
    /// A pattern shows every file (not dir) whose path matches it, relative to its base dir:
    /// the dirs before the first part with a special character (`*?[{` for a glob, and
    /// `.,*+?()[]{}|^$\` for a regex). A glob's `*` never matches `/`, and a regex must match the
    /// whole path, where `,` is any character except `/`
    /// A path that is a dir is always opened as a dir, even if its name looks like a pattern
    /// If the dir (or the base dir of the pattern) is neither on disk nor in the diff, the
    /// closest parent that is gets opened as a dir
    /// This never changes [`Koil::diff`], new dirs must be written in the listing
    /// Returns a warning, if the dir was not found and a parent was opened instead
    pub fn open<P: AsRef<Path>>(&mut self, location: P) -> Result<Option<Warning>, OpenError> {
        let path = resolve(&self.current_dir.join(location))?;
        let (base, pattern) = match self.split_pattern(&path) {
            Some((base, pattern)) => {
                pattern.matcher()?;
                (base, Some(pattern))
            }
            None => (path, None),
        };
        Ok(self.open_at(base, pattern)?)
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

    /// What is open, as it can be given to [`Koil::open`] again:
    /// [`Koil::current_dir`], joined with [`Koil::pattern`] if there is one
    pub fn location(&self) -> PathBuf {
        match &self.pattern {
            Some(pattern) => self.current_dir.join(pattern.as_str()),
            None => self.current_dir.clone(),
        }
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
    fn reopen(&mut self) -> io::Result<Option<Warning>> {
        if self.current_dir.as_os_str().is_empty() {
            return Ok(None);
        }
        // not parsed again, so a pattern stays a glob or a regex, even if the settings changed
        self.open_at(self.current_dir.clone(), self.pattern.clone())
    }

    /// Open `pattern` (a valid one) inside `base`, or just `base` if there is no pattern
    /// See [`Koil::open`]
    fn open_at(&mut self, base: PathBuf, pattern: Option<Pattern>) -> io::Result<Option<Warning>> {
        let dir = base
            .ancestors()
            .find(|p| self.is_dir(p))
            .ok_or(io::ErrorKind::NotFound)?
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
    fn read_listing(&mut self) -> io::Result<()> {
        self.current_listing.clear();
        let view = self.view();

        let paths = match self.current_dir.is_dir() {
            true => self.walk(&view)?,
            // a new dir that is not created yet
            false => Vec::new(),
        };
        for path in paths {
            if self.ignore.contains(&path) {
                continue;
            }
            let index = self.ids.iter().position(|p| p == &path);
            let index = index.unwrap_or_else(|| {
                self.ids.push(path);
                self.ids.len() - 1
            });
            self.current_listing.insert(index);
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
    /// are skipped
    fn walk(&self, view: &View) -> io::Result<Vec<PathBuf>> {
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
            .build();

        let mut paths = Vec::new();
        for item in walk.flatten() {
            let is_dir = item.file_type().is_some_and(|t| t.is_dir());
            // the walk starts with the dir itself
            if item.depth() > 0 && view.contains(item.path(), is_dir) {
                paths.push(item.into_path());
            }
        }
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

    /// The name of `path` in the listing, relative to [`Koil::current_dir`]
    fn name(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.current_dir).unwrap().to_path_buf()
    }

    /// Whether `path` is a dir on disk, or a new dir in [`Koil::diff`]
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir() || self.diff.creates_dir(path)
    }

    /// Split `path` into its base dir, and the pattern relative to it, which is a glob, or a
    /// regex if [`Settings::regex`] is on
    /// `None` if it is not a pattern, because no part of it after the longest part that is a
    /// dir has a special character
    fn split_pattern(&self, path: &Path) -> Option<(PathBuf, Pattern)> {
        // so a dir with a name like `a[1]` is not taken for a pattern
        let dir = path.ancestors().find(|p| self.is_dir(p))?;
        let parts: Vec<String> = path
            .strip_prefix(dir)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        let regex = self.settings.regex;
        let first = parts.iter().position(|p| Pattern::is_pattern(regex, p))?;
        let base = dir.join(parts[..first].iter().collect::<PathBuf>());
        let pattern = parts[first..].join("/");
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
            // a pattern only shows files
            Some(matcher) => {
                !is_dir
                    && path
                        .strip_prefix(&self.base)
                        .is_ok_and(|p| matcher.is_match(p))
            }
        }
    }
}

/// A compiled [`Pattern`]
enum Matcher {
    Glob(GlobMatcher),
    Regex(Regex),
}

impl Matcher {
    /// Whether `path`, relative to the base dir, matches
    fn is_match(&self, path: &Path) -> bool {
        match self {
            Matcher::Glob(glob) => glob.is_match(path),
            Matcher::Regex(regex) => regex.is_match(&path.to_string_lossy()),
        }
    }
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
