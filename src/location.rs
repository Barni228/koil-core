//! Reading a location as it is written, like a shell reads a path, see
//! [`Koil::read_location`] and [`Koil::complete`]

use crate::pattern::walker;
use crate::{Koil, Pattern, Settings, is_hidden, on_disk};
use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};

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
    /// The dirs in [`Completion::dir`] whose names start with the part, or if none does, have
    /// its characters in order, ignoring case unless the part has an uppercase letter (smart
    /// case), sorted, each with a `/` after it
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

    let first = parts.iter().position(|(part, _)| is_pattern(regex, part));
    let first = first.unwrap_or(parts.len());
    // not `join`, which with no parts adds a separator at the end, and then on Windows
    // `location` would put the pattern after a `\`, where it is an escape
    let mut base = dir;
    base.extend(parts[..first].iter().map(|(part, _)| read_text(part)));
    let Some(&(_, pattern_start)) = parts.get(first) else {
        return Location::dir(resolve(&base).unwrap_or(base));
    };
    let parts: Vec<String> = (parts[first..].iter())
        .map(|(part, _)| part_text(regex, part))
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

/// Whether `part` of a location (see [`read_quoted`]) has a special character of a regex if
/// `regex`, else of a glob, that is not quoted
fn is_pattern(regex: bool, part: &[ReadChar]) -> bool {
    let special: &[char] = match regex {
        false => &['*', '?', '[', '{'],
        true => REGEX_SPECIAL,
    };
    part.iter().any(|r| !r.quoted && special.contains(&r.c))
}

/// `part` of a location (see [`read_quoted`]) as a regex if `regex`, else as a glob, with the
/// quoted characters that have a meaning in it escaped
fn part_text(regex: bool, part: &[ReadChar]) -> String {
    let escaped: &[char] = match regex {
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

/// Whether `name` has the characters of `part` in the same order, with anything between them
/// (`my_tools` has `myt`), see [`Koil::complete`]
fn has_in_order(name: &str, part: &str) -> bool {
    let mut rest = name.chars();
    part.chars().all(|c| rest.any(|n| n == c))
}

/// Like [`Path::canonicalize`], but `path` does not need to exist
/// Existing part of the path is canonicalized, the rest is normalized without touching the filesystem
/// On Windows, it has no `\\?\` before it (which `canonicalize` adds, and frontends would show),
/// unless the path can not work without it
pub(crate) fn resolve(path: &Path) -> io::Result<PathBuf> {
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
                if on_disk(&resolved) {
                    resolved = dunce::canonicalize(&resolved)?;
                }
            }
        }
    }
    Ok(resolved)
}

impl Koil {
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
    /// If none of them starts with the part, they are the ones that have its characters in
    /// order, with anything between them, like zsh's fuzzy `r:|?=**` matcher: `myt` gives
    /// `my_tools`, and `own` gives `Downloads`
    /// Case is ignored unless the part has an uppercase letter, like vim's `smartcase`
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
        // smart case, like vim's `smartcase`: case is ignored unless the part has an uppercase
        // letter
        let ignore_case = !part.chars().any(char::is_uppercase);
        let as_matched = |name: &str| match ignore_case {
            true => name.to_lowercase(),
            false => name.to_string(),
        };
        let wanted = as_matched(&part);
        let matched: Vec<(String, &String)> = dirs.iter().map(|n| (as_matched(n), n)).collect();
        let names_where = |matches: fn(&str, &str) -> bool| -> Vec<String> {
            (matched.iter())
                .filter(|(m, _)| matches(m, &wanted))
                .map(|(_, n)| format!("{n}/"))
                .collect()
        };
        // the names that start with the part, or if none does, like zsh's fuzzy `r:|?=**`
        // matcher, those that have its characters in order (`myt` gives `my_tools`)
        let mut names = names_where(|name, part| name.starts_with(part));
        if names.is_empty() {
            names = names_where(has_in_order);
        }
        Completion {
            dir,
            start,
            part,
            names,
        }
    }

    /// The names of the dirs in `dir` (see [`Koil::complete`]): on disk (also links to dirs),
    /// without hidden ones unless `show_hidden`, and without ignored ones if `gitignore`, and
    /// the ones that are new in the diff
    fn dirs_in(&self, dir: &Path, show_hidden: bool, gitignore: bool) -> BTreeSet<String> {
        let walk = walker(dir, show_hidden, gitignore)
            .max_depth(Some(1))
            .build();
        let dirs = walk.flatten().filter(|item| item.depth() == 1);
        let dirs = dirs.filter(|item| item.path().is_dir());
        let mut names: BTreeSet<String> = dirs
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
            let new = !on_disk(&path) && self.diff.creates_dir(&path);
            if new && (show_hidden || !is_hidden(name)) {
                names.insert(name.to_string());
            }
        }
        names
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
}
