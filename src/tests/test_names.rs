use super::*;
use names::Problem;
use std::ffi::OsStr;

fn check(name: &str) -> Vec<Problem> {
    names::check(OsStr::new(name))
}

fn warning(kind: EntryWarningKind) -> Problem {
    Problem::Warning(kind)
}

fn error(kind: EntryErrorKind) -> Problem {
    Problem::Error(kind)
}

#[test]
fn test_fine_names() {
    let names = [
        "file",
        "file.txt",
        ".hidden",
        "My File (1).txt",
        "Andy's notes",
        "[draft] a,b+c=d@e%f",
        "café",
        "日本語",
        "console",
        "x#~",
    ];
    for name in names {
        assert_eq!(Vec::<Problem>::new(), check(name), "{name}");
    }
}

#[test]
fn test_control_characters() {
    for char in ['\t', '\u{1b}', '\0', '\u{7f}', '\u{85}'] {
        let name = format!("a{char}b");
        assert_eq!(
            vec![error(EntryErrorKind::ControlCharacter {
                name: name.clone(),
                char
            })],
            check(&name)
        );
    }
}

#[test]
fn test_name_too_long() {
    assert_eq!(Vec::<Problem>::new(), check(&"a".repeat(255)));
    let name = "a".repeat(256);
    assert_eq!(
        vec![error(EntryErrorKind::NameTooLong {
            name: name.clone(),
            len: 256
        })],
        check(&name)
    );
    // bytes, not characters
    let name = "é".repeat(128);
    assert_eq!(
        vec![error(EntryErrorKind::NameTooLong {
            name: name.clone(),
            len: 256
        })],
        check(&name)
    );
}

#[test]
fn test_windows_characters() {
    assert_eq!(
        vec![warning(EntryWarningKind::WindowsCharacter {
            name: r"a:b<c:d\e".into(),
            chars: r":<:\".into()
        })],
        check(r"a:b<c:d\e")
    );
    // repeated ones are only written once
    assert_eq!(
        vec![warning(EntryWarningKind::WindowsCharacter {
            name: "a::b".into(),
            chars: ":".into()
        })],
        check("a::b")
    );
}

#[test]
fn test_shell_characters() {
    let shell = |name: &str, chars: &str| {
        vec![warning(EntryWarningKind::ShellCharacter {
            name: name.into(),
            chars: chars.into(),
        })]
    };
    assert_eq!(shell("a$b&c", "$&"), check("a$b&c"));
    assert_eq!(shell("`x`;!^", "`;!^"), check("`x`;!^"));
    // only at the start
    assert_eq!(shell("~x", "~"), check("~x"));
    assert_eq!(shell("#x$", "#$"), check("#x$"));
}

#[test]
fn test_windows_reserved_names() {
    for name in ["CON", "con.txt", "Nul.tar.gz", "LPT9", "com1"] {
        assert_eq!(
            vec![warning(EntryWarningKind::WindowsReservedName {
                name: name.into()
            })],
            check(name),
            "{name}"
        );
    }
}

#[test]
fn test_emoji() {
    for name in ["📁 docs", "⭐", "☀️", "👨‍👩‍👧", "🇺🇦"] {
        assert_eq!(
            vec![warning(EntryWarningKind::Emoji { name: name.into() })],
            check(name),
            "{name}"
        );
    }
}

#[test]
fn test_unusual_characters() {
    for char in [
        '\u{200B}', '\u{200D}', '\u{A0}', '\u{202E}', '\u{FEFF}', '\u{3000}',
    ] {
        let name = format!("a{char}b");
        assert_eq!(
            vec![warning(EntryWarningKind::UnusualCharacter {
                name: name.clone(),
                char
            })],
            check(&name),
            "{char:?}"
        );
    }
}

#[test]
fn test_edges() {
    let space = |name: &str| warning(EntryWarningKind::SpaceAtEdge { name: name.into() });
    assert_eq!(vec![space(" a")], check(" a"));
    assert_eq!(vec![space("a ")], check("a "));
    assert_eq!(
        vec![warning(EntryWarningKind::TrailingDot { name: "a.".into() })],
        check("a.")
    );
    assert_eq!(
        vec![warning(EntryWarningKind::LeadingDash {
            name: "-rf".into()
        })],
        check("-rf")
    );
}

#[test]
fn test_many_problems() {
    assert_eq!(
        vec![
            error(EntryErrorKind::ControlCharacter {
                name: "-a:\tb.".into(),
                char: '\t'
            }),
            warning(EntryWarningKind::WindowsCharacter {
                name: "-a:\tb.".into(),
                chars: ":".into()
            }),
            warning(EntryWarningKind::TrailingDot {
                name: "-a:\tb.".into()
            }),
            warning(EntryWarningKind::LeadingDash {
                name: "-a:\tb.".into()
            }),
        ],
        check("-a:\tb.")
    );
}

#[cfg(unix)]
#[test]
fn test_not_utf8() {
    use std::os::unix::ffi::OsStrExt;
    assert_eq!(
        vec![warning(EntryWarningKind::NotUtf8 {
            name: "a\u{FFFD}b".into()
        })],
        names::check(OsStr::from_bytes(b"a\xffb"))
    );
}
