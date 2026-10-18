//! The errors and warnings that koil returns

use crate::apply::{Blocked, Undo};
use crate::{Action, Id, shown};
use std::path::PathBuf;
use std::{fmt, io};

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

    /// The open pattern went over [`Limits`](crate::Limits) when it was opened again (like
    /// after showing hidden paths), so its base dir was opened instead
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
                shown(requested),
                shown(opened)
            ),
            Warning::OverLimit { error, opened } => {
                write!(f, "{error}, opened `{}` instead", shown(opened))
            }
        }
    }
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

    /// [`Koil::undo`](crate::Koil::undo) or [`Koil::undo_only`](crate::Koil::undo_only) can
    /// not run a step, so nothing was undone
    #[error("Can not {step}, {blocked}")]
    UndoBlocked { step: Undo, blocked: Blocked },

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

    /// [`Koil::create_now`](crate::Koil::create_now) was given a path that the changes do not
    /// create
    #[error("`{}` is not new", shown(.0))]
    NotNew(PathBuf),

    /// [`Koil::create_now`](crate::Koil::create_now) can not create the path without applying
    /// other changes, like the one that moves away what is at the path now
    #[error("`{}` can not be created before the other changes are applied", shown(.0))]
    NeedsChanges(PathBuf),
}

/// Why [`Koil::open`](crate::Koil::open) failed, nothing was changed
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("Can not read the dir")]
    Io(#[from] io::Error),

    /// The first part of the path that is neither on disk nor new in the diff
    #[error("`{}` does not exist", shown(.0))]
    NotFound(PathBuf),

    /// The first part of the path that is a file, on disk or new in the diff
    #[error("`{}` is a file, not a directory", shown(.0))]
    NotADirectory(PathBuf),

    #[error("`{glob}` is not a valid glob pattern")]
    InvalidGlob {
        glob: String,
        #[source]
        source: GlobError,
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

/// Why a glob is not valid, see [`OpenError::InvalidGlob`]
#[derive(Debug, thiserror::Error)]
pub enum GlobError {
    #[error(transparent)]
    Syntax(#[from] globset::Error),

    /// The name of a POSIX class, like `digit` in `[[:digit:]]`, which globset does not
    /// support: it reads it as a class of `[:digt` and a `]`
    #[error("{}", posix_message(.0))]
    PosixClass(String),
}

/// What [`GlobError::PosixClass`] says for the class `name`
fn posix_message(name: &str) -> String {
    let range = match name {
        "digit" => "0-9",
        "alpha" => "a-zA-Z",
        "alnum" => "a-zA-Z0-9",
        "upper" => "A-Z",
        "lower" => "a-z",
        "xdigit" => "0-9a-fA-F",
        _ => {
            return format!(
                "POSIX classes are not supported, write the characters of `[[:{name}:]]` instead"
            );
        }
    };
    format!("POSIX classes are not supported, write `[{range}]` instead of `[[:{name}:]]`")
}

/// Why [`Koil::update_and_open`](crate::Koil::update_and_open) failed, nothing was changed
#[derive(Debug, thiserror::Error)]
pub enum UpdateOpenError {
    /// The entries can not be read
    #[error(transparent)]
    Update(#[from] UpdateError),
    /// The location can not be opened, or the open dir read again with the new settings
    #[error(transparent)]
    Open(#[from] OpenError),
}

/// A pattern matched or searched more paths than [`Limits`](crate::Limits) allow
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverLimit {
    #[error("`{pattern}` matches more than {limit} paths")]
    Matches { pattern: String, limit: usize },

    #[error("`{pattern}` has to search more than {limit} paths")]
    Searched { pattern: String, limit: usize },
}

/// Why [`Koil::update`](crate::Koil::update) rejected the entries, nothing was changed
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", join_lines(errors))]
pub struct UpdateError {
    /// Every problem that was found, in the order of the entries
    pub errors: Vec<EntryError>,
    /// Every warning that was found, in the order of the entries, like
    /// [`Koil::update`](crate::Koil::update) returns when it works
    pub warnings: Vec<EntryWarning>,
}

/// A problem with one of the entries given to [`Koil::update`](crate::Koil::update)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Entry {entry}: {kind}")]
pub struct EntryError {
    /// Index of the entry in the entries given to [`Koil::update`](crate::Koil::update)
    pub entry: usize,
    pub kind: EntryErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryErrorKind {
    #[error("ID `{0}` is not recognized")]
    UnknownId(Id),

    #[error(
        "`{}` is not a valid name, it can not be empty, absolute, or contain `.` or `..`",
        shown(.0)
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

    /// The entry's ID is of a path that is no longer on disk (it was deleted, or moved where
    /// koil did not see it go), so nothing can be done with it
    #[error("`{}` is no longer on disk", shown(.0))]
    NotOnDisk(PathBuf),

    /// On Windows, a name it can not use (elsewhere only a warning): a character it does not
    /// allow, a reserved name, or a `.` at the end, which it would remove
    #[error("{0}")]
    WindowsName(EntryWarningKind),
}

/// Something about one of the entries given to [`Koil::update`](crate::Koil::update) that is
/// not recommended, but does not stop the update
/// Only names that do not exist yet are checked
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Entry {entry}: {kind}")]
pub struct EntryWarning {
    /// Index of the entry in the entries given to [`Koil::update`](crate::Koil::update)
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

/// `error` as an [`io::Error`], for what can not fail any other way
pub(crate) fn io_error(error: OpenError) -> io::Error {
    match error {
        OpenError::Io(error) => error,
        error => io::Error::other(error),
    }
}
