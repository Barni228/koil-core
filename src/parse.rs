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
    /// A currently selected ID, if any
    pub selected: Option<String>,
}

enum ParsedLine {
    Entry(Entry, bool),
    WithoutId(String),
}

fn parse_line(raw: &str) -> Option<ParsedLine> {
    let mut line = raw.trim();
    let selected = line.starts_with("::");

    if selected {
        line = &line[1..];
    }

    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    if line.starts_with(':') {
        let mut is_dir = false;
        let (mut id, mut name) = line.split_once(' ').unwrap_or_default();
        id = id.strip_prefix(':').unwrap();
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
        // TODO: actually respect ::
        Some(ParsedLine::WithoutId(line.to_string()))
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
                if selected && parsed.selected.is_some() {
                    panic!("More than 1 thing is selected");
                } else if selected {
                    parsed.selected = Some(e.id.clone());
                }
                parsed.with_id.entry(e.id.clone()).or_default().push(e);
            }
            Some(ParsedLine::WithoutId(name)) => {
                parsed.without_id.push(name);
            }
        }
    }

    parsed
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
