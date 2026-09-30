//! Checks for names that can not be used, or that are not recommended

use crate::{EntryErrorKind, EntryWarningKind};
use std::ffi::OsStr;

/// Something wrong with a name
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Problem {
    /// The name can not be used
    Error(EntryErrorKind),
    /// The name can be used, but it is not recommended
    Warning(EntryWarningKind),
}

/// Most filesystems do not allow longer names
const MAX_NAME_BYTES: usize = 255;

/// Characters that can not be in names on Windows
const WINDOWS: &[char] = &['<', '>', ':', '"', '\\', '|', '?', '*'];

/// Characters that a shell reads as special, unless they are quoted or escaped
/// Ones that are in [`WINDOWS`] already are not here
const SHELL: &[char] = &['`', '$', '&', ';', '!', '^'];

/// Names that are reserved on Windows, even with an extension like `CON.txt`
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Every problem of `name`, a single part of a path
pub(crate) fn check(name: &OsStr) -> Vec<Problem> {
    use EntryErrorKind as E;
    use EntryWarningKind as W;

    let mut problems = Vec::new();
    let len = name.as_encoded_bytes().len();
    let Some(name) = name.to_str() else {
        let name = name.to_string_lossy().to_string();
        return vec![Problem::Warning(W::NotUtf8 { name })];
    };
    let mut error = |kind| problems.push(Problem::Error(kind));
    let owned = || name.to_string();

    if len > MAX_NAME_BYTES {
        error(E::NameTooLong { name: owned(), len });
    }
    if let Some(char) = name.chars().find(|c| c.is_control()) {
        error(E::ControlCharacter {
            name: owned(),
            char,
        });
    }

    let mut warning = |kind| problems.push(Problem::Warning(kind));
    let found = |set: &[char]| -> String {
        let mut found: Vec<char> = name.chars().filter(|c| set.contains(c)).collect();
        found.dedup();
        found.into_iter().collect()
    };

    let chars = found(WINDOWS);
    if !chars.is_empty() {
        warning(W::WindowsCharacter {
            name: owned(),
            chars,
        });
    }
    let mut chars = found(SHELL);
    // only special at the start, like `~/` or `# a comment`
    if let Some(first @ ('~' | '#')) = name.chars().next() {
        chars.insert(0, first);
    }
    if !chars.is_empty() {
        warning(W::ShellCharacter {
            name: owned(),
            chars,
        });
    }

    let stem = name.split('.').next().unwrap_or_default();
    if WINDOWS_RESERVED.contains(&stem.to_uppercase().as_str()) {
        warning(W::WindowsReservedName { name: owned() });
    }

    let has_emoji = name.chars().any(is_emoji);
    if has_emoji {
        warning(W::Emoji { name: owned() });
    }
    // a zero width joiner is fine in emoji like 👨‍👩‍👧
    let unusual = |&c: &char| is_unusual(c) && !(has_emoji && c == '\u{200D}');
    if let Some(char) = name.chars().find(unusual) {
        warning(W::UnusualCharacter {
            name: owned(),
            char,
        });
    }

    if name.starts_with(' ') || name.ends_with(' ') {
        warning(W::SpaceAtEdge { name: owned() });
    }
    if name.ends_with('.') {
        warning(W::TrailingDot { name: owned() });
    }
    if name.starts_with('-') {
        warning(W::LeadingDash { name: owned() });
    }

    problems
}

/// Whether `c` is (a part of) an emoji
fn is_emoji(c: char) -> bool {
    matches!(
        c as u32,
        // emoticons, symbols, pictographs, transport, flags, and more
        0x1F000..=0x1FAFF
            // misc symbols and dingbats, like ☀ and ✂
            | 0x2600..=0x27BF
            // like ⌚ and ⏰
            | 0x231A..=0x231B
            | 0x23E9..=0x23FA
            // like ⭐
            | 0x2B50..=0x2B55
            // makes the character before it look like an emoji
            | 0xFE0F
    )
}

/// Whether `c` is invisible, or looks like a different character (like a space that is not ` `)
fn is_unusual(c: char) -> bool {
    matches!(
        c,
        // no-break and soft hyphen
        '\u{00A0}' | '\u{00AD}'
            // spaces of different widths, and zero width characters
            | '\u{2000}'..='\u{200F}'
            // line and paragraph separators, and text direction
            | '\u{2028}'..='\u{202F}'
            | '\u{205F}'..='\u{206F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}
