use crate::Entry;
use std::collections::HashMap;

///////////////////////////////////////////////
// TODO: improve everything about this file  //
///////////////////////////////////////////////

#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub glob: String,
}

#[derive(Debug, Default, Clone)]
pub struct ParsedFile {
    /// The settings for the current listing
    pub settings: Option<Settings>,
    /// id -> name
    pub with_id: HashMap<String, Vec<Entry>>,
    /// bare names with no id - to be created
    pub without_id: Vec<String>,
    /// A currently selected line, if any
    pub selected: Option<Selected>,
}

/// A line that was selected with `>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selected {
    /// An existing entry, by its ID
    Id(String),
    /// A new entry that does not exist yet, by its name
    New(String),
}

enum ParsedLine {
    Entry(Entry, bool),
    WithoutId(String, bool),
}

fn parse_line(raw: &str) -> Option<ParsedLine> {
    // `>` selects this line, so it is opened after the update
    let (line, selected) = match raw.trim().strip_prefix('>') {
        Some(rest) => (rest.trim_start(), true),
        None => (raw.trim(), false),
    };

    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    if let Some(id_line) = line.strip_prefix(':') {
        let mut is_dir = false;
        let (id, mut name) = id_line.split_once(' ').unwrap_or_default();
        if let Some(stripped) = name.strip_suffix('/') {
            name = stripped;
            is_dir = true;
        }
        Some(ParsedLine::Entry(
            Entry {
                id: id.to_string(),
                name: name.to_string(),
                is_dir,
            },
            selected,
        ))
    } else {
        Some(ParsedLine::WithoutId(line.to_string(), selected))
    }
}

pub fn parse_listing(content: &str) -> ParsedFile {
    let mut parsed = ParsedFile::default();

    let mut lines = content.lines();
    if let Some(settings) = parse_settings(&mut lines) {
        parsed.settings = Some(settings)
    } else {
        lines = content.lines();
    }

    for line in lines {
        match parse_line(line) {
            None => {}
            Some(ParsedLine::Entry(e, selected)) => {
                if selected {
                    select(&mut parsed, Selected::Id(e.id.clone()));
                }
                parsed.with_id.entry(e.id.clone()).or_default().push(e);
            }
            Some(ParsedLine::WithoutId(name, selected)) => {
                if selected {
                    select(&mut parsed, Selected::New(name.clone()));
                }
                parsed.without_id.push(name);
            }
        }
    }

    parsed
}

fn select(parsed: &mut ParsedFile, selected: Selected) {
    if parsed.selected.is_some() {
        panic!("More than 1 thing is selected");
    }
    parsed.selected = Some(selected);
}

fn parse_settings<'a>(lines: &mut impl Iterator<Item = &'a str>) -> Option<Settings> {
    if !lines.next()?.starts_with("===") {
        return None;
    }

    // let glob = lines.next()?.trim().strip_prefix("glob: ")?.to_string();
    let glob = lines.next()?.trim().to_string();
    if !lines.next()?.starts_with("===") {
        return None;
    }

    Some(Settings { glob })
}
