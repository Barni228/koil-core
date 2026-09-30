use super::*;

fn add(f: &str) -> Action {
    Action::CreateFile(f.into())
}

fn add_dir(f: &str) -> Action {
    Action::CreateDir(f.into())
}

fn delete(f: &str) -> Action {
    Action::DeleteFile(f.into())
}

fn rename(from: &str, to: &str) -> Action {
    Action::Rename(from.into(), to.into())
}

fn copy(from: &str, to: &str) -> Action {
    Action::Copy(from.into(), to.into())
}

/// `name` without a trailing `/`, and whether it had one
fn split_dir(name: &str) -> (PathBuf, bool) {
    match name.strip_suffix('/') {
        Some(stripped) => (stripped.into(), true),
        None => (name.into(), false),
    }
}

/// An entry with `id`, written as `name`, a trailing `/` marks a dir
fn with_id(id: Id, name: &str) -> Entry {
    let (name, is_dir) = split_dir(name);
    Entry {
        id: Some(id),
        name,
        is_dir,
    }
}

/// A new entry, a trailing `/` marks a dir
fn without_id(name: &str) -> Entry {
    let (name, is_dir) = split_dir(name);
    Entry {
        id: None,
        name,
        is_dir,
    }
}

/// The ID of `path`, relative to the open dir
fn id(koil: &Koil, path: &str) -> Id {
    koil.id_of(&koil.current_dir().join(path)).unwrap()
}

/// The entry of `name` in the open dir, unchanged, a trailing `/` marks a dir
fn keep(koil: &Koil, name: &str) -> Entry {
    with_id(id(koil, name.trim_end_matches('/')), name)
}

mod test_apply;
mod test_diff;
mod test_koil;
mod test_names;
mod test_planner;
