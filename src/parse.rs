use crate::Entry;
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct ParsedFile {
    /// id -> name
    pub with_id: HashMap<String, Vec<Entry>>,
    /// bare names with no id - to be created
    pub without_id: Vec<String>,
}

enum ParsedLine {
    Entry(Entry),
    WithoutId(String),
}

fn parse_line(raw: &str) -> Option<ParsedLine> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    if line.starts_with(':') && !line.starts_with("::") {
        let mut is_dir = false;
        let (mut id, mut name) = line.split_once(' ').unwrap_or_default();
        id = id.strip_prefix(':').unwrap();
        if let Some(stripped) = name.strip_suffix('/') {
            name = stripped;
            is_dir = true;
        }
        Some(ParsedLine::Entry(Entry {
            id: id.to_string(),
            name: name.to_string(),
            is_dir,
        }))
    } else {
        // TODO: actually respect ::
        Some(ParsedLine::WithoutId(line.to_string()))
    }
}

pub fn parse_listing(content: &str) -> ParsedFile {
    let mut with_id: HashMap<String, Vec<Entry>> = HashMap::new();
    let mut without_id = Vec::new();

    for line in content.lines() {
        match parse_line(line) {
            None => {}
            Some(ParsedLine::Entry(e)) => {
                with_id.entry(e.id.clone()).or_default().push(e);
            }
            Some(ParsedLine::WithoutId(name)) => {
                without_id.push(name);
            }
        }
    }

    ParsedFile {
        with_id,
        without_id,
    }
}
