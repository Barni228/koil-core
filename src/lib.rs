#![doc = include_str!("../README.md")]

use crate::apply::{Undo, inside, same_file};
use crate::diff::Diff;
use crate::sync::{FileKey, Seen};
use globset::{GlobBuilder, GlobMatcher};
use ignore::{WalkBuilder, WalkState};
use regex::Regex;
use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf, is_separator};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;
use std::{fmt, fs, io};
use typed_builder::TypedBuilder;

pub mod apply;
pub mod diff;
mod names;
pub mod planner;
mod sync;
pub mod trash;

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

    /// The order of [`Koil::listing`]
    pub sort: Sort,
}

/// The order of [`Koil::listing`] (see [`Koil::compare`]): [`Entry::parent`], the dirs with IDs,
/// the files with IDs, then the new entries, each sorted by [`Sort::by`], and then by name
/// New entries are not on disk, so a [`SortBy`] that [`reads_metadata`](SortBy::reads_metadata)
/// sorts them only by name
/// `reverse` turns the order within each of those groups around, but not the groups
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub by: SortBy,
    /// The other way round: from Z to A, smallest first, oldest first
    pub reverse: bool,
}

/// What [`Sort`] sorts by, each in the order people usually want first
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    /// By name, from A to Z, as their bytes compare (so `B` before `a`, and `a10` before `a2`)
    #[default]
    Name,
    /// By name, from A to Z, as people sort names: ignoring case, with numbers by their value
    /// (`a2` before `a10`)
    Natural,
    /// By extension (after the last `.`, ignoring case), names without one first
    Extension,
    /// Biggest first: a file by its size, a dir by how many entries it has
    Size,
    /// Biggest first by the space it takes on disk (see [`disk_size`]): a file by its own, a dir
    /// by how many entries it has, as [`SortBy::Size`]
    /// On Windows it is much slower to read, as every file is opened
    Disk,
    /// Last modified first
    Modified,
    /// Last created first, on filesystems that keep it
    Created,
    /// Last accessed first, as the filesystem keeps it (many only now and then)
    Accessed,
}

impl SortBy {
    /// Whether it sorts by something [`Koil::metadata`] has to read from disk
    pub fn reads_metadata(self) -> bool {
        !matches!(self, SortBy::Name | SortBy::Natural | SortBy::Extension)
    }

    /// How `a` (with `a_meta`) and `b` (with `b_meta`) compare by this alone, in its own
    /// direction (biggest or newest first), what is not known last
    fn compare(
        self,
        a: &Entry,
        a_meta: Option<&Metadata>,
        b: &Entry,
        b_meta: Option<&Metadata>,
    ) -> std::cmp::Ordering {
        let key = |meta: Option<&Metadata>| {
            let meta = meta?;
            match self {
                SortBy::Size | SortBy::Disk if meta.is_dir => meta.entries.map(u128::from),
                SortBy::Size => Some(u128::from(meta.size)),
                SortBy::Disk => meta.disk_size.map(u128::from),
                SortBy::Modified => since_epoch(meta.modified),
                SortBy::Created => since_epoch(meta.created),
                SortBy::Accessed => since_epoch(meta.accessed),
                SortBy::Name | SortBy::Natural | SortBy::Extension => None,
            }
        };
        let extension = |e: &Entry| {
            let extension = e.name.extension().unwrap_or_default();
            extension.to_string_lossy().to_lowercase()
        };
        match self {
            SortBy::Name => a.name.cmp(&b.name),
            SortBy::Natural => natural_order(&a.name.to_string_lossy(), &b.name.to_string_lossy()),
            SortBy::Extension => extension(a).cmp(&extension(b)),
            // the bigger or newer first, and `None` last
            _ => key(b_meta).cmp(&key(a_meta)),
        }
    }
}

/// `time` as a number that sorts like it, what is before 1970 as 0
fn since_epoch(time: Option<SystemTime>) -> Option<u128> {
    let since = time?.duration_since(SystemTime::UNIX_EPOCH);
    Some(since.map_or(0, |d| d.as_nanos()))
}

/// `a` against `b` as people sort names: ignoring case, and with numbers by their value, so
/// `a2` comes before `a10` (and `a02` is the same as `a2`)
fn natural_order(a: &str, b: &str) -> std::cmp::Ordering {
    // the digits of the number `chars` start with, without its leading zeros
    fn number(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
        let mut digits = String::new();
        while let Some(d) = chars.next_if(char::is_ascii_digit) {
            digits.push(d);
        }
        digits.trim_start_matches('0').to_string()
    }
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        let order = match (a.peek(), b.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (n, m) = (number(&mut a), number(&mut b));
                // a longer number is bigger
                n.len().cmp(&m.len()).then_with(|| n.cmp(&m))
            }
            (Some(&x), Some(&y)) => {
                a.next();
                b.next();
                x.to_lowercase().cmp(y.to_lowercase())
            }
        };
        if order.is_ne() {
            return order;
        }
    }
}

/// What is on disk at an entry's path (of a link itself, for a link), which the listing can be
/// sorted by (see [`Koil::metadata`])
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Like [`Path::is_dir`], so a link to a dir is one, as the listing shows it
    pub is_dir: bool,
    /// The size in bytes
    pub size: u64,
    /// How many entries a dir has (without hidden ones, unless [`Settings::show_hidden`] is on),
    /// only counted when sorting by [`SortBy::Size`] or [`SortBy::Disk`]
    pub entries: Option<u64>,
    /// The space it takes on disk (see [`disk_size`]), only read when sorting by
    /// [`SortBy::Disk`]
    pub disk_size: Option<u64>,
    /// `None` where the filesystem does not keep it
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

impl Metadata {
    /// What `meta` (of `path`, the link itself for a link) says, how many entries a dir has
    /// if `settings` sort by size (or size on disk), and the space it takes on disk if they
    /// sort by that
    fn new(path: &Path, meta: &fs::Metadata, settings: &Settings) -> Metadata {
        let is_dir = meta.is_dir() || meta.file_type().is_symlink() && path.is_dir();
        let shown = |name: &std::ffi::OsStr| {
            settings.show_hidden || !name.as_encoded_bytes().starts_with(b".")
        };
        let by = settings.sort.by;
        let entries = (is_dir && matches!(by, SortBy::Size | SortBy::Disk))
            .then(|| fs::read_dir(path).ok())
            .flatten()
            .map(|entries| entries.flatten().filter(|e| shown(&e.file_name())).count() as u64);
        Metadata {
            is_dir,
            size: meta.len(),
            entries,
            disk_size: (by == SortBy::Disk)
                .then(|| disk_size(path, meta))
                .flatten(),
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            accessed: meta.accessed().ok(),
        }
    }

    /// What is at `path` on disk now
    fn read(path: &Path, settings: &Settings) -> Option<Metadata> {
        let meta = path.symlink_metadata().ok()?;
        Some(Metadata::new(path, &meta, settings))
    }
}

/// The space that what is at `path` (whose metadata, of the link itself for a link, is `meta`)
/// takes on disk: the blocks it has, so less than its size for a sparse or compressed file, and
/// a whole block for a small one (or nothing, where the filesystem keeps it with its name)
/// `None` if it can not be read
/// On Windows the file is opened for it (`meta` from a directory walk does not have it), which
/// is much slower than reading its size
#[cfg(unix)]
pub fn disk_size(_path: &Path, meta: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    // in blocks of 512 bytes, whatever the filesystem's own are
    Some(meta.blocks() * 512)
}

#[cfg(windows)]
pub fn disk_size(path: &Path, _meta: &fs::Metadata) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
    };
    // as in `same_file`: a dir can be opened, and a link is opened itself
    const FLAGS: u32 = 0x0200_0000 | 0x0020_0000;
    // reading it needs no access to the file
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FLAGS)
        .open(path)
        .ok()?;
    // SAFETY: all zeros is a valid `FILE_STANDARD_INFO` (numbers, and `false`)
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open while `file` is, and `info` is as big as the size given
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    match read {
        0 => None,
        _ => u64::try_from(info.AllocationSize).ok(),
    }
}

#[cfg(not(any(unix, windows)))]
pub fn disk_size(_path: &Path, _meta: &fs::Metadata) -> Option<u64> {
    None
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

    /// Whether `part` of a location (see [`read_quoted`]) has a special character of this kind
    /// of pattern, that is not quoted
    fn is_pattern(syntax_regex: bool, part: &[ReadChar]) -> bool {
        let special: &[char] = match syntax_regex {
            false => &['*', '?', '[', '{'],
            true => REGEX_SPECIAL,
        };
        part.iter().any(|r| !r.quoted && special.contains(&r.c))
    }

    /// `part` of a location (see [`read_quoted`]) as this kind of pattern, with the quoted
    /// characters that have a meaning in it escaped
    fn part_text(syntax_regex: bool, part: &[ReadChar]) -> String {
        let escaped: &[char] = match syntax_regex {
            false => &['*', '?', '[', ']', '{', '}', ',', '\\'],
            true => REGEX_SPECIAL,
        };
        let mut text = String::new();
        for r in part {
            if r.quoted && escaped.contains(&r.c) {
                text.push('\\');
            }
            text.push(r.c);
        }
        text
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

/// What a location given to [`Koil::open`] means, see [`Koil::read_location`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The dir to open, or the base dir of [`Location::pattern`], absolute
    /// It does not have to exist, opening it then fails
    pub dir: PathBuf,
    /// The pattern of the paths to show, relative to [`Location::dir`], `None` for a dir
    pub pattern: Option<Pattern>,
    /// Where the pattern starts in the location as written (a byte index), `None` for a dir
    pub pattern_start: Option<usize>,
}

impl Location {
    /// The location of the dir `dir`, without a pattern
    fn dir(dir: PathBuf) -> Location {
        Location {
            dir,
            pattern: None,
            pattern_start: None,
        }
    }
}

/// What a location being written can go on as, see [`Koil::complete`]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completion {
    /// The dir of the part being written (the location's last part, after its last `/`)
    pub dir: PathBuf,
    /// Where that part starts in the location as written (a byte index): each of
    /// [`Completion::names`] can be written from there instead of it
    pub start: usize,
    /// The part, as it is read (see [`Koil::read_location`]), without its quotes
    pub part: String,
    /// The dirs in [`Completion::dir`] whose names start with the part (or, if none do, the
    /// ones whose names do when case is ignored), sorted, each with a `/` after it
    pub names: Vec<String>,
}

/// The characters that make a part of a location a regex, and that are escaped in it when
/// they are quoted
const REGEX_SPECIAL: &[char] = &[
    '.', ',', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '^', '$', '\\',
];

/// A character of a location, as a shell reads it (see [`read_quoted`])
#[derive(Debug, Clone, Copy)]
struct ReadChar {
    c: char,
    /// Whether it was in quotes, or escaped with a `\`, so it is never special in a pattern
    quoted: bool,
    /// Where it is in the location as written, a byte index
    at: usize,
}

/// `location`, from its byte `from`, read like a shell reads one word, but not split at
/// spaces: what is in `'...'` or `"..."` is quoted, and so is a character escaped with a `\`
/// (which is taken off): any outside quotes, and `"`, `\`, `$` or `` ` `` in `"..."`
/// A quote that is never closed is a plain character, so a name like `it's` can be written
/// as it is
/// `\` only escapes what is not an ASCII letter or digit (no shell needs those escaped), so a
/// regex's `\d` stays one, and on Windows, where it is a separator, it never escapes
fn read_quoted(location: &str, from: usize) -> Vec<ReadChar> {
    let escapes = !cfg!(windows);
    let mut read = Vec::new();
    let mut quote = None;
    let mut at = from;
    while let Some(c) = location[at..].chars().next() {
        let after = at + c.len_utf8();
        let escaped = location[after..].chars().next().filter(|&n| {
            escapes
                && c == '\\'
                && match quote {
                    None => !n.is_ascii_alphanumeric(),
                    Some('"') => matches!(n, '"' | '\\' | '$' | '`'),
                    Some(_) => false,
                }
        });
        if let Some(n) = escaped {
            read.push(ReadChar {
                c: n,
                quoted: true,
                at: after,
            });
            at = after + n.len_utf8();
            continue;
        }
        match quote {
            None if matches!(c, '\'' | '"') && closes(&location[after..], c, escapes) => {
                quote = Some(c)
            }
            Some(q) if c == q => quote = None,
            _ => read.push(ReadChar {
                c,
                quoted: quote.is_some(),
                at,
            }),
        }
        at = after;
    }
    read
}

/// Whether `quote` is closed in `after` (what comes after it), see [`read_quoted`]
fn closes(after: &str, quote: char, escapes: bool) -> bool {
    let mut chars = after.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c == quote => return true,
            // `\"` does not close it, and `\\` is skipped whole, so a `"` after it does
            '\\' if quote == '"' && escapes => {
                chars.next();
            }
            _ => {}
        }
    }
    false
}

/// The location of `rest` (see [`read_quoted`]) in `dir`, where `rest` starts at the byte
/// `start` of the location as written: the parts of `rest` until the first that is a
/// pattern (a glob, or a regex if `regex`) are in the dir, and that one starts the pattern
fn location_in(dir: PathBuf, rest: &[ReadChar], start: usize, regex: bool) -> Location {
    // its parts between `/`, and where each starts as written
    let mut parts = Vec::new();
    let (mut from, mut at) = (0, start);
    for (i, r) in rest.iter().enumerate() {
        if r.c == '/' {
            parts.push((&rest[from..i], at));
            (from, at) = (i + 1, r.at + 1);
        }
    }
    parts.push((&rest[from..], at));
    parts.retain(|(part, _)| !matches!(read_text(part).as_str(), "" | "."));

    let first = parts
        .iter()
        .position(|(part, _)| Pattern::is_pattern(regex, part));
    let first = first.unwrap_or(parts.len());
    // not `join`, which with no parts adds a separator at the end, and then on Windows
    // `location` would put the pattern after a `\`, where it is an escape
    let mut base = dir;
    base.extend(parts[..first].iter().map(|(part, _)| read_text(part)));
    let Some(&(_, pattern_start)) = parts.get(first) else {
        return Location::dir(resolve(&base).unwrap_or(base));
    };
    let parts: Vec<String> = (parts[first..].iter())
        .map(|(part, _)| Pattern::part_text(regex, part))
        .collect();
    let mut pattern = parts.join("/");
    // which makes it match only dirs
    if rest.last().is_some_and(|r| r.c == '/') {
        pattern.push('/');
    }
    Location {
        dir: base,
        pattern: Some(match regex {
            true => Pattern::Regex(pattern),
            false => Pattern::Glob(pattern),
        }),
        pattern_start: Some(pattern_start),
    }
}

/// A location as it was written, see [`Koil::read_location`]
struct Written<'a> {
    text: &'a str,
    /// The `~` at its start that is the home dir (see [`expand_tilde`]): where it is, and the
    /// home dir
    home: Option<(usize, String)>,
}

impl Written<'_> {
    /// Its text until the byte `end`, with its `~` as the home dir
    fn until(&self, end: usize) -> String {
        match &self.home {
            Some((at, home)) if *at < end => {
                format!("{}{home}{}", &self.text[..*at], &self.text[at + 1..end])
            }
            _ => self.text[..end].to_string(),
        }
    }
}

/// Makes the `~` that starts `read` (see [`read_quoted`]) of `location` the home dir, if it
/// is followed by a `/` or nothing, and returns where it is written and the home dir
/// Also right after the quote that starts `location` (`"~/my dir"`), but not after a `\`
/// The home dir is quoted, so it is never special in a pattern
fn expand_tilde(location: &str, read: &mut Vec<ReadChar>) -> Option<(usize, String)> {
    let first = read.first()?;
    let at_start = first.at == 0 || (first.at == 1 && location.starts_with(['\'', '"']));
    if first.c != '~' || !at_start || read.get(1).is_some_and(|r| r.c != '/') {
        return None;
    }
    let home = std::env::home_dir()?.to_str()?.to_string();
    if home.is_empty() {
        return None;
    }
    let at = first.at;
    let home_chars = home.chars().map(|c| ReadChar {
        c,
        quoted: true,
        at,
    });
    read.splice(..1, home_chars);
    Some((at, home))
}

/// The text of `chars` (see [`read_quoted`])
fn read_text(chars: &[ReadChar]) -> String {
    chars.iter().map(|r| r.c).collect()
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
                shown(requested),
                shown(opened)
            ),
            Warning::OverLimit { error, opened } => {
                write!(f, "{error}, opened `{}` instead", shown(opened))
            }
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

    /// [`Koil::create_now`] was given a path that the changes do not create
    #[error("`{}` is not new", shown(.0))]
    NotNew(PathBuf),

    /// [`Koil::create_now`] can not create the path without applying other changes, like the
    /// one that moves away what is at the path now
    #[error("`{}` can not be created before the other changes are applied", shown(.0))]
    NeedsChanges(PathBuf),
}

/// Why [`Koil::open`] failed, nothing was changed
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

/// Why [`Koil::update_and_open`] failed, nothing was changed
#[derive(Debug, thiserror::Error)]
pub enum UpdateOpenError {
    /// The entries can not be read
    #[error(transparent)]
    Update(#[from] UpdateError),
    /// The location can not be opened, or the open dir read again with the new settings
    #[error(transparent)]
    Open(#[from] OpenError),
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
        let Location { dir, pattern, .. } = self.read_location(location, self.settings.regex);
        if let Some(pattern) = &pattern {
            pattern.matcher()?;
        }
        if !self.is_dir(&dir) {
            return Err(self.not_a_dir(&dir));
        }
        // in a copy, so nothing changes if it fails
        let mut opened = self.clone();
        opened.open_at(dir, pattern)?;
        *self = opened;
        Ok(())
    }

    /// What [`Koil::open`] would open for `location`, with a pattern read as a regex if
    /// `regex`, else as a glob (`open` uses [`Settings::regex`]), without opening it, or
    /// checking that the dir exists, or that the pattern is valid
    /// A `~` at its start, before a `/` or nothing, is the home dir, also right after the
    /// quote that starts it (`"~/my dir"`, which a shell would not expand, but nobody means a
    /// dir named `~`, which is written `\~` or `./~`)
    /// A location that is a dir as it is written is that dir (also a name with quotes or `\`
    /// in it, like `it's`), else it is read like a shell reads one word, but not split at
    /// spaces: what is in `'...'` or `"..."` is quoted, and so is a character after a `\`
    /// (taken off), which only escapes what is not an ASCII letter or digit (so a regex's `\d`
    /// stays one), and on Windows never does. A quote that is not closed is a plain character
    /// The base dir is the longest part before a `/` that is a dir, as written or as read (else
    /// the root, or [`Koil::current_dir`] for a relative location), then every part after it
    /// until the first with a special character that is not quoted (`*?[{` for a glob, and
    /// `.,*+?()[]{}|^$\` for a regex), which starts the pattern, where the quoted characters
    /// that are special in it are escaped
    pub fn read_location<P: AsRef<Path>>(&self, location: P, regex: bool) -> Location {
        let location = location.as_ref();
        let text = location.to_string_lossy();
        let mut read = read_quoted(&text, 0);
        let written = Written {
            home: expand_tilde(&text, &mut read),
            text: &text,
        };
        // so a name with quotes opens as it is shown, and one like `a[1]` is not a pattern
        let as_written = match written.home {
            None => location.to_path_buf(),
            Some(_) => written.until(text.len()).into(),
        };
        if let Some(dir) = self.dir_at(&as_written) {
            return Location::dir(dir);
        }
        let path = PathBuf::from(read_text(&read));
        if path != as_written
            && let Some(dir) = self.dir_at(&path)
        {
            return Location::dir(dir);
        }
        (self.split_location(&written, &read, regex))
            .unwrap_or_else(|| Location::dir(self.absolute(&path)))
    }

    /// What `location`, written up to where it is being written, can go on as, for a frontend
    /// to complete it like a shell: the dirs its last part (after its last `/`) can be the
    /// start of, in the dir before that part, read like [`Koil::read_location`] reads it (with
    /// [`Settings::regex`] of `settings`), or in [`Koil::current_dir`] if it has no `/`
    /// They are the dirs on disk, without hidden ones unless [`Settings::show_hidden`] is on
    /// or the part starts with a `.`, and without ignored ones if
    /// [`Settings::respect_gitignore`] is on, the dirs that are new in the diff (which
    /// [`Koil::open`] can open), and `..` for a part that is `.` or `..`
    /// A name can be written as it is, even one like `a[1]`, as a location that is a dir as
    /// written is never read as a pattern
    /// The `~` alone completes to `~/`. Nothing completes if the part's dir is not a dir, or is
    /// a pattern, or the part starts in quotes (or after an escaped `/`)
    pub fn complete(&self, location: &str, settings: &Settings) -> Completion {
        let mut read = read_quoted(location, 0);
        let home = expand_tilde(location, &mut read);
        // the home dir is read in place of the `~`, so it has no `/` of its own
        let tilde = home.as_ref().map(|(at, _)| *at);
        let slash = read.iter().rposition(|r| r.c == '/' && Some(r.at) != tilde);
        if let (None, Some((_, home))) = (slash, &home) {
            let home = Path::new(home);
            return Completion {
                dir: home.parent().unwrap_or(home).to_path_buf(),
                start: 0,
                part: "~".to_string(),
                names: vec!["~/".to_string()],
            };
        }
        let (dir, start) = match slash {
            None => (self.current_dir.clone(), 0),
            Some(i) if read[i].quoted => return Completion::default(),
            Some(i) => {
                let start = read[i].at + 1;
                match self.read_location(&location[..start], settings.regex) {
                    Location {
                        dir, pattern: None, ..
                    } if self.is_dir(&dir) => (dir, start),
                    _ => return Completion::default(),
                }
            }
        };
        let part = read_text(&read[slash.map_or(0, |i| i + 1)..]);

        let show_hidden = settings.show_hidden || part.starts_with('.');
        let mut dirs = self.dirs_in(&dir, show_hidden, settings.respect_gitignore);
        if !part.is_empty() && "..".starts_with(&part) {
            dirs.insert("..".to_string());
        }
        let starting = |ignore_case: bool| -> Vec<String> {
            let lower = part.to_lowercase();
            let starts = |name: &&String| match ignore_case {
                true => name.to_lowercase().starts_with(&lower),
                false => name.starts_with(&part),
            };
            dirs.iter()
                .filter(starts)
                .map(|n| format!("{n}/"))
                .collect()
        };
        let mut names = starting(false);
        if names.is_empty() {
            names = starting(true);
        }
        Completion {
            dir,
            start,
            part,
            names,
        }
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
    /// (dirs, then files), then new entries, each sorted as [`Settings::sort`] says (see
    /// [`Sort`])
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
        let mut entries: Vec<Entry> = sorted.into_iter().map(|(entry, _)| entry).collect();

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
        let mut updated = self.clone();
        let warnings = updated.update(entries)?;
        let mut warning = None;
        match location {
            // not read again first, as the location is read with them right away
            Some(location) => {
                updated.settings = settings;
                updated.open(location)?;
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
        let changes = self.changes();
        let mut applied: Vec<bool> = changes.iter().map(|c| picked.contains(&c.action)).collect();
        let mut needed_by = vec![Vec::new(); changes.len()];
        for (i, change) in changes.iter().enumerate() {
            for &j in &change.needs {
                needed_by[j].push(i);
            }
        }
        let mut left_out: Vec<usize> = (0..changes.len()).filter(|&i| !applied[i]).collect();
        while let Some(j) = left_out.pop() {
            for &i in &needed_by[j] {
                if applied[i] {
                    applied[i] = false;
                    left_out.push(i);
                }
            }
        }
        let actions: Vec<Action> = (changes.into_iter().zip(applied))
            .filter_map(|(change, applied)| applied.then_some(change.action))
            .collect();
        // symlink_metadata().is_ok() checks if path OR SYMLINK exists there
        self.run_actions(&planner::plan_actions(&actions, |p| {
            p.symlink_metadata().is_ok()
        }))
    }

    /// What [`Koil::create_now`] would run to create the new file or dir at `path`: its
    /// create, after those of the new dirs it is in, in order
    /// Fails with [`KoilError::NotNew`] if the changes do not create `path`, and with
    /// [`KoilError::NeedsChanges`] if it needs changes that are not creates (see
    /// [`Change::needs`]), like the rename that moves away what is at `path` now
    pub fn create_steps(&self, path: &Path) -> Result<Vec<Action>, KoilError> {
        let changes = self.changes();
        let creates = |action: &Action| matches!(action, Action::CreateFile(p) | Action::CreateDir(p) if p == path);
        let first = (changes.iter())
            .position(|c| creates(&c.action))
            .ok_or_else(|| KoilError::NotNew(path.to_path_buf()))?;
        let mut needed = vec![false; changes.len()];
        needed[first] = true;
        let mut todo = vec![first];
        while let Some(i) = todo.pop() {
            for &j in &changes[i].needs {
                if !matches!(changes[j].action, Action::CreateDir(_)) {
                    return Err(KoilError::NeedsChanges(path.to_path_buf()));
                }
                if !needed[j] {
                    needed[j] = true;
                    todo.push(j);
                }
            }
        }
        Ok((changes.into_iter().zip(needed))
            .filter_map(|(change, needed)| needed.then_some(change.action))
            .collect())
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
            if action.path().symlink_metadata().is_ok() {
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

    /// Where [`Koil::listing`] lists `a` against `b` (see [`Sort`]), so a frontend can put an
    /// entry where the listing would have it, like one that [`Koil::sync`] adds
    pub fn compare(&self, a: &Entry, b: &Entry) -> std::cmp::Ordering {
        let (a_meta, b_meta) = (self.sort_metadata(a), self.sort_metadata(b));
        self.order(a, a_meta.as_ref(), b, b_meta.as_ref())
    }

    /// What is on disk at the path of `id` (of a link itself, for a link): as koil read it
    /// when it last read the open dir (see [`Koil::sync`]), if the listing has it, else as it
    /// is now, `None` if the ID is not known or nothing can be read there
    /// A frontend can show what the listing is sorted by with it, like a file's size
    pub fn metadata(&self, id: Id) -> Option<Metadata> {
        let index = self.index_of(id)?;
        match self.seen.get(&index) {
            Some(seen) if self.current_listing.contains(&index) => Some(seen.meta),
            _ => Metadata::read(&self.ids[index], &self.settings),
        }
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
    /// it is new (but not [`Entry::parent`]), or not at the path of its ID, so it is renamed,
    /// copied, or moved here from another dir
    /// A frontend can use it to mark the entries that are not on disk as they are written
    pub fn is_pending(&self, entry: &Entry) -> bool {
        match entry.id {
            None => !entry.is_parent(),
            Some(id) => self.path_of(id) != Some(self.current_dir.join(&entry.name).as_path()),
        }
    }
}

// Private functions
impl Koil {
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
        result
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
        let paths = self.read_view(&self.view())?;
        self.list(paths);
        self.see_dirs();
        Ok(())
    }

    /// Every path on disk that `view` shows (see [`Koil::walk`]), with what is there, nothing
    /// if [`Koil::current_dir`] is a new dir that is not created yet
    fn read_view(&self, view: &View) -> Result<Vec<(PathBuf, Option<Seen>)>, OpenError> {
        match self.current_dir.is_dir() {
            true => self.walk(view),
            false => Ok(Vec::new()),
        }
    }

    /// Make `paths` (read by [`Koil::read_view`]) [`Koil::current_listing`], giving new IDs to
    /// the ones koil has not seen, and remember what is at each
    /// A changed entry in the view stays too, even if it is hidden, so it is not taken for
    /// deleted in the next update
    fn list(&mut self, paths: Vec<(PathBuf, Option<Seen>)>) {
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

    /// Remember what [`Koil::current_dir`] and the dirs it is in are on disk (see
    /// [`Koil::dirs_seen`])
    fn see_dirs(&mut self) {
        self.dirs_seen = (self.current_dir.ancestors())
            .filter_map(|dir| Some((dir.to_path_buf(), FileKey::at(dir)?)))
            .collect();
    }

    /// Every path on disk that `view` shows, without hidden entries if they are not shown,
    /// and without ignored paths if [`Settings::respect_gitignore`] is on, and what is at each
    /// (`None` if it can not be read)
    /// Fails if [`Koil::current_dir`] can not be read, but dirs inside it that can not be read
    /// are skipped, or if a pattern goes over [`Limits`]
    fn walk(&self, view: &View) -> Result<Vec<(PathBuf, Option<Seen>)>, OpenError> {
        fs::read_dir(&self.current_dir)?;
        let max_depth = match &self.pattern {
            None => Some(1),
            // without `**`, a glob can only match paths with as many parts as it has
            Some(Pattern::Glob(glob)) if !glob.contains("**") => {
                Some(Path::new(glob).components().count())
            }
            Some(_) => None,
        };
        let (hidden, gitignore) = (self.settings.show_hidden, self.settings.respect_gitignore);
        let walk = walker(&self.current_dir, hidden, gitignore)
            .max_depth(max_depth)
            .build_parallel();

        let prune = Prune::new(view);
        let (pattern, limits, settings) = (self.pattern.as_ref(), self.limits, &self.settings);
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
                let skip_inside =
                    is_dir && prune.as_ref().is_some_and(|p| p.skips_inside(item.path()));
                let searched = searched.fetch_add(1, Ordering::Relaxed) + 1;
                let matches = match view.contains(item.path(), is_dir) {
                    true => {
                        // here, on the walk's threads, as it reads the disk again (and a dir's
                        // entries, when sorting by size)
                        let seen =
                            (item.metadata().ok()).map(|m| Seen::new(item.path(), &m, settings));
                        paths.lock().unwrap().push((item.into_path(), seen));
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
        paths.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(paths)
    }

    /// The names of the dirs in `dir` (see [`Koil::complete`]): on disk (also links to dirs),
    /// without hidden ones unless `show_hidden`, and without ignored ones if `gitignore`, and
    /// the ones that are new in the diff
    fn dirs_in(&self, dir: &Path, show_hidden: bool, gitignore: bool) -> BTreeSet<String> {
        let walk = walker(dir, show_hidden, gitignore)
            .max_depth(Some(1))
            .build();
        let on_disk = walk.flatten().filter(|item| item.depth() == 1);
        let on_disk = on_disk.filter(|item| item.path().is_dir());
        let mut names: BTreeSet<String> = on_disk
            .filter_map(|item| item.file_name().to_str().map(String::from))
            .collect();
        let afters = self.diff.with_id.values().flat_map(|(_, afters)| afters);
        for path in self.diff.without_id.keys().chain(afters) {
            let first = path
                .strip_prefix(dir)
                .ok()
                .and_then(|p| p.components().next());
            let Some(Component::Normal(name)) = first else {
                continue;
            };
            let Some(name) = name.to_str() else {
                continue;
            };
            let path = dir.join(name);
            let new = path.symlink_metadata().is_err() && self.diff.creates_dir(&path);
            if new && (show_hidden || !name.starts_with('.')) {
                names.insert(name.to_string());
            }
        }
        names
    }

    /// What [`Koil::listing`] is sorted by on disk for `entry`, `None` if it is new, or the
    /// listing is sorted by its name
    fn sort_metadata(&self, entry: &Entry) -> Option<Metadata> {
        match self.settings.sort.by.reads_metadata() {
            true => self.metadata(entry.id?),
            false => None,
        }
    }

    /// See [`Koil::compare`], where `a_meta` and `b_meta` are what [`Koil::sort_metadata`]
    /// gives for `a` and `b`
    fn order(
        &self,
        a: &Entry,
        a_meta: Option<&Metadata>,
        b: &Entry,
        b_meta: Option<&Metadata>,
    ) -> std::cmp::Ordering {
        let group = |e: &Entry| match e.id {
            _ if e.is_parent() => 0,
            Some(_) if e.is_dir => 1,
            Some(_) => 2,
            None => 3,
        };
        let Sort { by, reverse } = self.settings.sort;
        group(a).cmp(&group(b)).then_with(|| {
            let order = (by.compare(a, a_meta, b, b_meta)).then_with(|| a.name.cmp(&b.name));
            match reverse {
                true => order.reverse(),
                false => order,
            }
        })
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

    /// `path` (relative to [`Koil::current_dir`]) as an absolute path, if it is a dir
    fn dir_at(&self, path: &Path) -> Option<PathBuf> {
        let path = resolve(&self.current_dir.join(path)).ok()?;
        self.is_dir(&path).then_some(path)
    }

    /// `path` (relative to [`Koil::current_dir`]) as an absolute path, resolved as far as it
    /// can be
    fn absolute(&self, path: &Path) -> PathBuf {
        let path = self.current_dir.join(path);
        resolve(&path).unwrap_or(path)
    }

    /// [`Koil::read_location`] of a location that is not a dir, `written` as given, and as
    /// [`read_quoted`] reads it (`read`, with its `~` expanded), `None` if no part of it (not
    /// even its root) is a dir
    /// Only a `/` (or the root, like `/` or `C:\`) ends a part, so on Windows a `\` after the
    /// base dir stays in the pattern, where it is an escape
    fn split_location(
        &self,
        written: &Written,
        read: &[ReadChar],
        regex: bool,
    ) -> Option<Location> {
        let text = read_text(read);
        // like `/` or `C:\`, nothing if it is relative
        let root = Path::new(&text).ancestors().last();
        let root = &text[..root.map_or(0, |r| r.as_os_str().len())];
        let root_chars = root.chars().count();
        // the longest part before a `/` that is a dir, as written (then what comes after it is
        // read again from there), or as read
        // Not as written before a quoted `/`, where it would end in the middle of the quotes
        // (`"a/..` is the dir it is in, even if `"a` does not exist)
        for i in (root_chars..read.len()).rev().filter(|&i| read[i].c == '/') {
            let at = read[i].at;
            if !read[i].quoted
                && let Some(dir) = self.dir_at(Path::new(&written.until(at)))
            {
                let rest = read_quoted(written.text, at + 1);
                return Some(location_in(dir, &rest, at + 1, regex));
            }
            let as_read = read_text(&read[..i]);
            if as_read != written.until(at)
                && let Some(dir) = self.dir_at(Path::new(&as_read))
            {
                return Some(location_in(dir, &read[i + 1..], at + 1, regex));
            }
        }
        let dir = self.dir_at(Path::new(root))?;
        let start = match root_chars {
            0 => 0,
            _ => read[root_chars - 1].at + read[root_chars - 1].c.len_utf8(),
        };
        Some(location_in(dir, &read[root_chars..], start, regex))
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
            // what the listing shows was on disk when it was read, and is followed by `sync`
            if let Some(index) = index
                && !self.current_listing.contains(&index)
                && self.ids[index].symlink_metadata().is_err()
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
#[derive(Clone)]
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
#[derive(Clone)]
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
#[derive(Clone)]
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

    /// Whether nothing inside the dir `path` can match
    fn skips_inside(&self, path: &Path) -> bool {
        let Ok(path) = path.strip_prefix(&self.base) else {
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

/// A walk of `dir`, without hidden entries unless `show_hidden`, and without what git ignores
/// (and the `.git` dir) if `gitignore`, see [`Koil::walk`]
fn walker(dir: &Path, show_hidden: bool, gitignore: bool) -> WalkBuilder {
    let mut walk = WalkBuilder::new(dir);
    walk.hidden(!show_hidden)
        // only what git ignores, not the `.ignore` files of ripgrep
        .ignore(false)
        .git_ignore(gitignore)
        .git_global(gitignore)
        .git_exclude(gitignore)
        .parents(gitignore)
        // git never shows its own dir
        .filter_entry(move |item| !(gitignore && item.file_name() == ".git"));
    walk
}

#[cfg(test)]
mod tests;
