//! The patterns a listing can show (see [`Pattern`]), and walking the open dir to find what
//! they match

use crate::sync::Seen;
use crate::{GlobError, Koil, OpenError, OverLimit, is_hidden};
use globset::{GlobBuilder, GlobMatcher};
use ignore::{WalkBuilder, WalkState};
use regex::Regex;
use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

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

    pub(crate) fn matcher(&self) -> Result<Matcher, OpenError> {
        match self {
            Pattern::Glob(glob) => {
                let invalid = |source| OpenError::InvalidGlob {
                    glob: glob.clone(),
                    source,
                };
                if let Some(name) = posix_class(glob) {
                    return Err(invalid(GlobError::PosixClass(name)));
                }
                let (rest, slash) = split_slash(glob);
                glob_builder(rest)
                    .build()
                    .map(|g| Matcher::Glob(g.compile_matcher(), slash))
                    .map_err(|source| invalid(source.into()))
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

/// Paths read from disk, with what is at each (`None` if it can not be read), see
/// [`Koil::walk`]
pub(crate) type Walked = Vec<(PathBuf, Option<Seen>)>;

/// Which paths a listing shows
#[derive(Clone)]
pub(crate) struct View {
    /// The open dir, or the base dir of the pattern
    pub(crate) base: PathBuf,
    pub(crate) matcher: Option<Matcher>,
}

impl View {
    /// Whether `path` is shown in the listing
    pub(crate) fn contains(&self, path: &Path, is_dir: bool) -> bool {
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
pub(crate) enum Matcher {
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

/// `glob` without its trailing `/`, and whether it had one, which makes it match only dirs:
/// globset reads `**/` as `**`, so a trailing `/` is matched apart
fn split_slash(glob: &str) -> (&str, bool) {
    match glob.strip_suffix('/') {
        Some(rest) => (rest, true),
        None => (glob, false),
    }
}

/// A builder of `glob` as koil reads globs
fn glob_builder(glob: &str) -> GlobBuilder<'_> {
    let mut builder = GlobBuilder::new(glob);
    builder
        .literal_separator(true)
        // globset's default is off on Windows, where `\` is a separator, but patterns use
        // only `/`
        .backslash_escape(true)
        // `a{,.txt}` is `a` or `a.txt`, as a shell expands it
        .empty_alternates(true);
    builder
}

/// A piece of a glob, as globset reads it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobPiece<'a> {
    /// A `[...]` class, with its brackets (a `\` in it is a plain character)
    Class(&'a str),
    /// `\` and the character it escapes
    Escaped(&'a str),
    /// `{`
    Open,
    /// `,`, which is a plain character outside `{...}`
    Comma,
    /// `}`
    Close,
    /// `/`
    Slash,
    /// Any other character
    Char(&'a str),
}

impl GlobPiece<'_> {
    fn text(&self) -> &str {
        match self {
            GlobPiece::Class(text) | GlobPiece::Escaped(text) | GlobPiece::Char(text) => text,
            GlobPiece::Open => "{",
            GlobPiece::Comma => ",",
            GlobPiece::Close => "}",
            GlobPiece::Slash => "/",
        }
    }
}

/// The pieces of `glob`, `None` if a class is never closed (which globset rejects)
fn glob_pieces(glob: &str) -> Option<Vec<GlobPiece<'_>>> {
    let mut pieces = Vec::new();
    let mut chars = glob.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => {
                // `!` or `^` negates it, and a `]` first is in it
                chars.next_if(|&(_, c)| c == '!' || c == '^');
                chars.next_if(|&(_, c)| c == ']');
                chars.find(|&(_, c)| c == ']')?;
            }
            _ => {}
        }
        let end = chars.peek().map_or(glob.len(), |&(i, _)| i);
        let text = &glob[start..end];
        pieces.push(match c {
            '\\' => GlobPiece::Escaped(text),
            '[' => GlobPiece::Class(text),
            '{' => GlobPiece::Open,
            ',' => GlobPiece::Comma,
            '}' => GlobPiece::Close,
            '/' => GlobPiece::Slash,
            _ => GlobPiece::Char(text),
        });
    }
    Some(pieces)
}

/// The name of the first POSIX class in `glob`, like `digit` in `[[:digit:]]`, see
/// [`GlobError::PosixClass`]
fn posix_class(glob: &str) -> Option<String> {
    const NAMES: [&str; 12] = [
        "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
        "upper", "xdigit",
    ];
    glob_pieces(glob)?.into_iter().find_map(|piece| {
        let GlobPiece::Class(class) = piece else {
            return None;
        };
        // globset ends the class at the `]` of its `:]`
        let inside = class.strip_suffix(":]")?;
        let name = &inside[inside.rfind("[:")? + 2..];
        NAMES.contains(&name).then(|| name.to_string())
    })
}

/// The most globs [`expand_braces`] makes, a glob with more does not show hidden entries
/// (see [`Dots`])
const MAX_EXPANDED: usize = 64;

/// The globs that the `{...}` in `pieces` stand for, as a shell expands them, each without
/// braces, `None` if they are more than [`MAX_EXPANDED`], or a brace is not closed
/// It reads from `at`, up to the `,` or `}` that ends it, if it is inside braces (not `top`)
fn expand_braces<'a>(
    pieces: &[GlobPiece<'a>],
    at: &mut usize,
    top: bool,
) -> Option<Vec<Vec<GlobPiece<'a>>>> {
    let mut globs = vec![Vec::new()];
    while let Some(&piece) = pieces.get(*at) {
        let options = match piece {
            GlobPiece::Comma | GlobPiece::Close if !top => return Some(globs),
            GlobPiece::Open => {
                *at += 1;
                let mut options = Vec::new();
                loop {
                    options.extend(expand_braces(pieces, at, false)?);
                    let end = *pieces.get(*at)?;
                    *at += 1;
                    if end == GlobPiece::Close {
                        break options;
                    }
                }
            }
            piece => {
                *at += 1;
                vec![vec![piece]]
            }
        };
        if globs.len() * options.len() > MAX_EXPANDED {
            return None;
        }
        globs = (globs.iter())
            .flat_map(|glob| {
                options
                    .iter()
                    .map(move |option| [glob.as_slice(), option].concat())
            })
            .collect();
    }
    Some(globs)
}

/// Which hidden entries (names starting with `.`) a glob shows while they are not shown (see
/// [`Settings::show_hidden`]): as in a shell, a hidden name is matched only by a part of the
/// glob that starts with a `.` itself, never by a wildcard (`.*`, `.config/*`, `**/.env`),
/// and `**` does not go through hidden dirs
/// A walk of the glob then has the hidden entries too (see [`Koil::walk`]), and this says
/// which to take
struct Dots {
    /// The parts of each glob its `{...}` stand for (see [`expand_braces`])
    globs: Vec<Vec<DotPart>>,
    /// Whether the glob ends with a `/`, so it matches only dirs (see [`split_slash`])
    slash: bool,
}

/// A part of a glob between `/`
enum DotPart {
    /// `**`, any number of dirs that are not hidden
    Dirs,
    /// One name, and whether the part starts with a `.`, so it can match a hidden one
    Name(GlobMatcher, bool),
}

impl Dots {
    /// `None` if no part of `glob` starts with a `.`, so it shows no hidden entries, or if it
    /// is not read here: its `{...}` stand for over [`MAX_EXPANDED`] globs, or a class has a
    /// `/` in it
    fn new(glob: &str) -> Option<Dots> {
        let (rest, slash) = split_slash(glob);
        let pieces = glob_pieces(rest)?;
        let globs: Vec<Vec<DotPart>> = (expand_braces(&pieces, &mut 0, true)?.iter())
            .map(|pieces| Dots::parts(pieces))
            .collect::<Option<_>>()?;
        let has_dot = (globs.iter().flatten()).any(|part| matches!(part, DotPart::Name(_, true)));
        has_dot.then_some(Dots { globs, slash })
    }

    /// The parts of a glob without braces, `None` if a class has a `/` in it
    fn parts(pieces: &[GlobPiece]) -> Option<Vec<DotPart>> {
        let mut parts = vec![String::new()];
        for piece in pieces {
            match piece {
                // an escaped `/` still only matches a `/`
                GlobPiece::Slash | GlobPiece::Escaped("\\/") => parts.push(String::new()),
                GlobPiece::Class(class) if class.contains('/') => return None,
                piece => parts.last_mut().unwrap().push_str(piece.text()),
            }
        }
        (parts.iter())
            .map(|part| match part.as_str() {
                "**" => Some(DotPart::Dirs),
                part => {
                    let glob = glob_builder(part).build().ok()?.compile_matcher();
                    Some(DotPart::Name(
                        glob,
                        part.starts_with('.') || part.starts_with("\\."),
                    ))
                }
            })
            .collect()
    }

    /// Whether a walk that has hidden entries (see [`walker`]) takes `item`, in the base dir
    /// `base`: shows it, and goes inside it if it is a dir (as a walk without them does for an
    /// entry that is not hidden, nor in a hidden dir)
    fn walks(&self, item: &ignore::DirEntry, base: &Path, is_dir: bool) -> (bool, bool) {
        // a name can not start with a `.` that this hides
        if !is_hidden(item.file_name()) && hidden_attribute(item) {
            return (false, false);
        }
        let path = item.path().strip_prefix(base).unwrap_or(item.path());
        if !path.components().any(is_hidden) {
            return (true, true);
        }
        let mut names: Vec<_> = (path.components())
            .map(|c| c.as_os_str().to_string_lossy())
            .collect();
        let enters = is_dir
            && (self.globs.iter())
                .any(|parts| Dots::states(parts, &names)[..parts.len()].contains(&true));
        // as for [`match_text`], a dir has a `/` after it, which a `*` matches with nothing
        if is_dir && !self.slash {
            names.push("".into());
        }
        let shows = (is_dir || !self.slash)
            && (self.globs.iter()).any(|parts| Dots::states(parts, &names)[parts.len()]);
        (shows, enters)
    }

    /// Which of `parts` can come next after `names` (`parts.len()` if they all matched)
    fn states(parts: &[DotPart], names: &[impl AsRef<str>]) -> Vec<bool> {
        // a `**` can match no dirs, so the part after it can come next too
        let past_dirs = |states: &mut Vec<bool>| {
            for (i, part) in parts.iter().enumerate() {
                if states[i] && matches!(part, DotPart::Dirs) {
                    states[i + 1] = true;
                }
            }
        };
        let mut states = vec![false; parts.len() + 1];
        states[0] = true;
        past_dirs(&mut states);
        for name in names {
            let name = name.as_ref();
            let hidden = is_hidden(name);
            let mut next = vec![false; parts.len() + 1];
            for (i, part) in parts.iter().enumerate().filter(|&(i, _)| states[i]) {
                match part {
                    DotPart::Dirs if !hidden => next[i] = true,
                    DotPart::Name(glob, dot) if (*dot || !hidden) && glob.is_match(name) => {
                        next[i + 1] = true;
                    }
                    _ => {}
                }
            }
            past_dirs(&mut next);
            states = next;
        }
        states
    }
}

/// Whether Windows hides `item` by its attributes, as [`walker`] does without hidden entries
#[cfg(windows)]
fn hidden_attribute(item: &ignore::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    /// `FILE_ATTRIBUTE_HIDDEN`
    const HIDDEN: u32 = 0x2;
    (item.metadata()).is_ok_and(|m| m.file_attributes() & HIDDEN != 0)
}

/// Whether Windows hides `item` by its attributes, never elsewhere
#[cfg(not(windows))]
fn hidden_attribute(_item: &ignore::DirEntry) -> bool {
    false
}

/// Which dirs a walk of a pattern does not need to enter, since nothing inside them can match
#[derive(Clone)]
pub(crate) struct Prune {
    /// The base dir of the pattern
    base: PathBuf,
    /// [`Matcher::dfa`], which tells when no text that starts with a dir's can match
    dfa: dense::DFA<Vec<u32>>,
}

impl Prune {
    /// `None` without a pattern, or if its DFA would be too big, then every dir is entered
    pub(crate) fn new(view: &View) -> Option<Prune> {
        Some(Prune {
            base: view.base.clone(),
            dfa: view.matcher.as_ref()?.dfa()?,
        })
    }

    /// Whether nothing inside the dir `path` can match
    pub(crate) fn skips_inside(&self, path: &Path) -> bool {
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

/// `regex` with every `,` replaced by `[^/]` (any character except `/`), except an escaped `\,`,
/// and a `,` inside a `[...]` class or a `{...}` repetition, which keeps its usual meaning
pub(crate) fn expand_commas(regex: &str) -> String {
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

/// A walk of `dir`, without hidden entries unless `show_hidden`, and without what git ignores
/// (and the `.git` dir) if `gitignore`, see [`Koil::walk`]
pub(crate) fn walker(dir: &Path, show_hidden: bool, gitignore: bool) -> WalkBuilder {
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

impl Koil {
    /// Every path on disk that `view` shows, without hidden entries if they are not shown,
    /// and without ignored paths if [`Settings::respect_gitignore`] is on, and what is at each
    /// (`None` if it can not be read)
    /// Fails if [`Koil::current_dir`] can not be read, but dirs inside it that can not be read
    /// are skipped, or if a pattern goes over [`Limits`]
    pub(crate) fn walk(&self, view: &View) -> Result<Walked, OpenError> {
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
        // a glob with a part that starts with a `.` shows the hidden entries that part matches
        let dots = match &self.pattern {
            Some(Pattern::Glob(glob)) if !hidden => Dots::new(glob),
            _ => None,
        };
        let walk = walker(&self.current_dir, hidden || dots.is_some(), gitignore)
            .max_depth(max_depth)
            .build_parallel();

        let prune = Prune::new(view);
        let base = self.current_dir.as_path();
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
                // with `dots`, the walk has the hidden entries too, and they say which to take
                let (dots_show, dots_enter) = match &dots {
                    Some(dots) => dots.walks(&item, base, is_dir),
                    None => (true, true),
                };
                if !dots_show && !dots_enter {
                    return WalkState::Skip;
                }
                let skip_inside = is_dir
                    && (!dots_enter || prune.as_ref().is_some_and(|p| p.skips_inside(item.path())));
                let searched = searched.fetch_add(1, Ordering::Relaxed) + 1;
                let matches = match dots_show && view.contains(item.path(), is_dir) {
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
}
