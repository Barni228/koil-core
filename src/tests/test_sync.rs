use super::*;
use std::fs;
use tempfile::TempDir;

/// A temp dir with the files `a` and `b`, and the dir `d` with the file `d/inside`, and its
/// path as koil reads it (canonical, like `/private/var` on macOS)
fn setup() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    fs::write(root.join("a"), "a").unwrap();
    fs::write(root.join("b"), "b").unwrap();
    fs::create_dir(root.join("d")).unwrap();
    fs::write(root.join("d/inside"), "inside").unwrap();
    (tmp, root)
}

/// Koil with `dir` open
fn open(dir: &Path) -> Koil {
    let mut koil = Koil::default();
    koil.open(dir).unwrap();
    koil
}

/// The unchanged listing of `setup`'s dir
fn listing(koil: &Koil) -> Vec<Entry> {
    vec![keep(koil, "d/"), keep(koil, "a"), keep(koil, "b")]
}

/// `listing` with `a` written as `entry` instead (or left out, for `None`)
fn listing_with_a(koil: &Koil, entry: Option<Entry>) -> Vec<Entry> {
    let mut entries = vec![keep(koil, "d/"), keep(koil, "b")];
    entries.extend(entry);
    entries
}

/// The actions koil would apply, with paths relative to `root`
fn actions(koil: &Koil, root: &Path) -> Vec<Action> {
    let relative = |p: &Path| p.strip_prefix(root).unwrap().to_path_buf();
    let actions = koil
        .compute_actions()
        .into_iter()
        .map(|action| match action {
            Action::DeleteDir(p) => Action::DeleteDir(relative(&p)),
            Action::DeleteFile(p) => Action::DeleteFile(relative(&p)),
            Action::Rename(a, b) => Action::Rename(relative(&a), relative(&b)),
            Action::Copy(a, b) => Action::Copy(relative(&a), relative(&b)),
            Action::CreateFile(p) => Action::CreateFile(relative(&p)),
            Action::CreateDir(p) => Action::CreateDir(relative(&p)),
        });
    actions.collect()
}

#[test]
fn test_sync_nothing_changed() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    assert_eq!(Synced::default(), koil.sync(&listing(&koil)).unwrap());
}

#[test]
fn test_sync_added() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    fs::write(root.join("c"), "").unwrap();
    fs::create_dir(root.join("e")).unwrap();
    let synced = koil.sync(&listing(&koil)).unwrap();
    let edits = vec![Edit::Add(keep(&koil, "c")), Edit::Add(keep(&koil, "e/"))];
    assert_eq!(edits, synced.edits);
    assert!(synced.conflicts.is_empty());
}

#[test]
fn test_sync_deleted() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = keep(&koil, "a");
    fs::remove_file(root.join("a")).unwrap();
    let synced = koil.sync(&listing(&koil)).unwrap();
    assert_eq!(vec![Edit::Remove(a)], synced.edits);
    assert!(synced.conflicts.is_empty());
    // what is left is unchanged
    koil.update(&listing_with_a(&koil, None)).unwrap();
    assert!(koil.compute_actions().is_empty());
}

#[test]
fn test_sync_renamed() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = keep(&koil, "a");
    let d = keep(&koil, "d/");
    fs::rename(root.join("a"), root.join("z")).unwrap();
    fs::rename(root.join("d"), root.join("e")).unwrap();
    let synced = koil.sync(&listing(&koil)).unwrap();
    let edits = vec![
        Edit::Change {
            from: a.clone(),
            to: with_id(a.id.unwrap(), "z"),
        },
        Edit::Change {
            from: d.clone(),
            to: with_id(d.id.unwrap(), "e/"),
        },
    ];
    assert_eq!(edits, synced.edits);
    assert!(synced.conflicts.is_empty());
    // the IDs follow their files
    assert_eq!(Some(root.join("z").as_path()), koil.path_of(a.id.unwrap()));
    assert_eq!(Some(root.join("e").as_path()), koil.path_of(d.id.unwrap()));
}

#[test]
fn test_sync_copied() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    fs::copy(root.join("a"), root.join("a2")).unwrap();
    let synced = koil.sync(&listing(&koil)).unwrap();
    assert_eq!(vec![Edit::Add(keep(&koil, "a2"))], synced.edits);
    assert_ne!(a, id(&koil, "a2"));
}

#[test]
fn test_sync_renamed_but_deleted() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    let entries = listing_with_a(&koil, None);
    fs::rename(root.join("a"), root.join("z")).unwrap();
    let synced = koil.sync(&entries).unwrap();
    // kept until user says otherwise
    assert_eq!(vec![Edit::Add(with_id(a, "z"))], synced.edits);
    let conflict = Conflict {
        kind: ConflictKind::Deleted,
        id: a,
        from: root.join("a"),
        to: Some(root.join("z")),
        is_dir: false,
        listed: vec![],
        elsewhere: vec![],
    };
    assert_eq!(vec![conflict.clone()], synced.conflicts);
    assert_eq!(vec![Edit::Remove(with_id(a, "z"))], koil.resolve(&conflict));
    koil.update(&entries).unwrap();
    assert_eq!(vec![Action::DeleteFile("z".into())], actions(&koil, &root));
}

#[test]
fn test_sync_renamed_both() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    let entries = listing_with_a(&koil, Some(with_id(a, "c")));
    fs::rename(root.join("a"), root.join("z")).unwrap();
    let synced = koil.sync(&entries).unwrap();
    assert!(synced.edits.is_empty());
    let conflict = Conflict {
        kind: ConflictKind::Renamed,
        id: a,
        from: root.join("a"),
        to: Some(root.join("z")),
        is_dir: false,
        listed: vec![with_id(a, "c")],
        elsewhere: vec![],
    };
    assert_eq!(vec![conflict.clone()], synced.conflicts);
    // user's name is kept
    koil.check(&entries).unwrap();
    let mut kept = koil.clone();
    kept.update(&entries).unwrap();
    assert_eq!(vec![rename("z", "c")], actions(&kept, &root));
    // or the one on disk
    let edits = vec![Edit::Change {
        from: with_id(a, "c"),
        to: with_id(a, "z"),
    }];
    assert_eq!(edits, koil.resolve(&conflict));
}

#[test]
fn test_sync_renamed_the_same() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let entries = listing_with_a(&koil, Some(with_id(id(&koil, "a"), "z")));
    fs::rename(root.join("a"), root.join("z")).unwrap();
    assert_eq!(Synced::default(), koil.sync(&entries).unwrap());
    koil.update(&entries).unwrap();
    assert!(koil.compute_actions().is_empty());
}

#[test]
fn test_sync_gone_but_renamed() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    let entries = listing_with_a(&koil, Some(with_id(a, "c")));
    fs::remove_file(root.join("a")).unwrap();
    let synced = koil.sync(&entries).unwrap();
    // dropped until user says otherwise
    assert_eq!(vec![Edit::Remove(with_id(a, "c"))], synced.edits);
    let conflict = Conflict {
        kind: ConflictKind::Gone,
        id: a,
        from: root.join("a"),
        to: None,
        is_dir: false,
        listed: vec![with_id(a, "c")],
        elsewhere: vec![],
    };
    assert_eq!(vec![conflict.clone()], synced.conflicts);
    assert_eq!(vec![Edit::Add(without_id("c"))], koil.resolve(&conflict));
    // its ID is of nothing now
    let error = koil.check(&entries).unwrap_err();
    let kind = EntryErrorKind::NotOnDisk("a".into());
    assert_eq!(vec![EntryError { entry: 2, kind }], error.errors);
}

#[test]
fn test_sync_gone_but_copied() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    let mut entries = listing(&koil);
    entries.push(with_id(a, "a2"));
    fs::remove_file(root.join("a")).unwrap();
    let synced = koil.sync(&entries).unwrap();
    let edits = vec![
        Edit::Remove(with_id(a, "a")),
        Edit::Remove(with_id(a, "a2")),
    ];
    assert_eq!(edits, synced.edits);
    assert_eq!(1, synced.conflicts.len());
    assert_eq!(ConflictKind::Gone, synced.conflicts[0].kind);
    assert_eq!(vec![with_id(a, "a2")], synced.conflicts[0].listed);
}

#[test]
fn test_sync_created_what_user_wrote() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let mut entries = listing(&koil);
    entries.push(without_id("c"));
    entries.push(without_id("e/"));
    fs::write(root.join("c"), "").unwrap();
    // a file, where user wrote a dir
    fs::write(root.join("e"), "").unwrap();
    let synced = koil.sync(&entries).unwrap();
    let edits = vec![
        Edit::Change {
            from: without_id("c"),
            to: keep(&koil, "c"),
        },
        Edit::Add(keep(&koil, "e")),
    ];
    assert_eq!(edits, synced.edits);
    let conflict = Conflict {
        kind: ConflictKind::Taken,
        id: id(&koil, "e"),
        from: root.join("e"),
        to: Some(root.join("e")),
        is_dir: false,
        listed: vec![without_id("e/")],
        elsewhere: vec![],
    };
    assert_eq!(vec![conflict], synced.conflicts);
}

#[test]
fn test_sync_taken() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    let entries = listing_with_a(&koil, Some(with_id(a, "c")));
    fs::write(root.join("c"), "").unwrap();
    let synced = koil.sync(&entries).unwrap();
    let c = id(&koil, "c");
    assert_eq!(vec![Edit::Add(with_id(c, "c"))], synced.edits);
    assert_eq!(1, synced.conflicts.len());
    let conflict = &synced.conflicts[0];
    assert_eq!((ConflictKind::Taken, c), (conflict.kind, conflict.id));
    assert_eq!(vec![with_id(a, "c")], conflict.listed);
    // replaced: the new one is deleted, and user's renamed there
    assert_eq!(vec![Edit::Remove(with_id(c, "c"))], koil.resolve(conflict));
    koil.update(&entries).unwrap();
    let actions = actions(&koil, &root);
    assert_eq!(vec![delete("c"), rename("a", "c")], actions);
}

#[test]
fn test_sync_kind_changed() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let b = keep(&koil, "b");
    fs::remove_file(root.join("b")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    let synced = koil.sync(&listing(&koil)).unwrap();
    let edits = vec![Edit::Change {
        from: b.clone(),
        to: with_id(b.id.unwrap(), "b/"),
    }];
    assert_eq!(edits, synced.edits);
}

#[test]
fn test_sync_moved_out() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = keep(&koil, "a");
    fs::rename(root.join("a"), root.join("d/a")).unwrap();
    let synced = koil.sync(&listing(&koil)).unwrap();
    assert_eq!(vec![Edit::Remove(a.clone())], synced.edits);
    assert_eq!(
        Some(root.join("d/a").as_path()),
        koil.path_of(a.id.unwrap())
    );
}

#[test]
fn test_sync_moved_out_but_renamed() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    let entries = listing_with_a(&koil, Some(with_id(a, "c")));
    fs::rename(root.join("a"), root.join("d/a")).unwrap();
    let synced = koil.sync(&entries).unwrap();
    assert!(synced.edits.is_empty());
    assert_eq!(1, synced.conflicts.len());
    assert_eq!(ConflictKind::Renamed, synced.conflicts[0].kind);
    // moved back here, not copied
    koil.update(&entries).unwrap();
    assert_eq!(vec![rename("d/a", "c")], actions(&koil, &root));
}

#[test]
fn test_sync_moved_in() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    // a hidden name is not listed, but the file is found in the dir it was in
    let a = keep(&koil, "a");
    fs::rename(root.join("a"), root.join(".a")).unwrap();
    let synced = koil.sync(&listing(&koil)).unwrap();
    assert_eq!(vec![Edit::Remove(a.clone())], synced.edits);
    assert_eq!(Some(root.join(".a").as_path()), koil.path_of(a.id.unwrap()));
    // and back, where nothing has it anymore
    fs::rename(root.join(".a"), root.join("a")).unwrap();
    let synced = koil.sync(&listing_with_a(&koil, None)).unwrap();
    assert_eq!(vec![Edit::Add(keep(&koil, "a"))], synced.edits);
}

#[test]
fn test_sync_open_dir_renamed() {
    let (_tmp, root) = setup();
    let mut koil = open(&root.join("d"));
    let inside = id(&koil, "inside");
    let entries = vec![with_id(inside, "renamed")];
    fs::rename(root.join("d"), root.join("e")).unwrap();
    let synced = koil.sync(&entries).unwrap();
    assert_eq!(Synced::default(), synced);
    assert_eq!(root.join("e"), koil.current_dir());
    assert_eq!(Some(root.join("e/inside").as_path()), koil.path_of(inside));
    koil.update(&entries).unwrap();
    assert_eq!(vec![rename("e/inside", "e/renamed")], actions(&koil, &root));
}

#[test]
fn test_sync_open_dir_deleted() {
    let (_tmp, root) = setup();
    let mut koil = open(&root.join("d"));
    fs::remove_dir_all(root.join("d")).unwrap();
    let synced = koil.sync(&[keep(&koil, "inside")]).unwrap();
    assert!(synced.moved);
    let warning = Warning::DirNotFound {
        requested: root.join("d"),
        opened: root.clone(),
    };
    assert_eq!(Some(warning), synced.warning);
    assert_eq!(root, koil.current_dir());
    assert_eq!(vec![keep(&koil, "a"), keep(&koil, "b")], koil.listing());
}

#[test]
fn test_sync_elsewhere_renamed_both() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    let entries = listing_with_a(&koil, Some(with_id(a, "c")));
    koil.update_and_open(&entries, Settings::default(), Some(Path::new("d")))
        .unwrap();
    fs::rename(root.join("a"), root.join("z")).unwrap();
    let synced = koil.sync(&[keep(&koil, "inside")]).unwrap();
    assert!(synced.edits.is_empty());
    let conflict = Conflict {
        kind: ConflictKind::Renamed,
        id: a,
        from: root.join("a"),
        to: Some(root.join("z")),
        is_dir: false,
        listed: vec![],
        elsewhere: vec![root.join("c")],
    };
    assert_eq!(vec![conflict.clone()], synced.conflicts);
    assert_eq!(vec![rename("z", "c")], actions(&koil, &root));
    // it stays where it is on disk
    assert!(koil.resolve(&conflict).is_empty());
    assert!(koil.compute_actions().is_empty());
}

#[test]
fn test_sync_elsewhere_renamed_but_deleted() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    let entries = listing_with_a(&koil, None);
    koil.update_and_open(&entries, Settings::default(), Some(Path::new("d")))
        .unwrap();
    fs::rename(root.join("a"), root.join("z")).unwrap();
    let synced = koil.sync(&[keep(&koil, "inside")]).unwrap();
    assert_eq!(1, synced.conflicts.len());
    assert_eq!(
        (ConflictKind::Deleted, a),
        (synced.conflicts[0].kind, synced.conflicts[0].id)
    );
    // kept until user says otherwise
    assert!(koil.compute_actions().is_empty());
    assert!(koil.resolve(&synced.conflicts[0]).is_empty());
    assert_eq!(vec![delete("z")], actions(&koil, &root));
}

#[test]
fn test_sync_dir_with_changes_renamed() {
    let (_tmp, root) = setup();
    let mut koil = open(&root.join("d"));
    let inside = id(&koil, "inside");
    let entries = [with_id(inside, "renamed")];
    koil.update_and_open(&entries, Settings::default(), Some(Path::new("..")))
        .unwrap();
    let d = keep(&koil, "d/");
    fs::rename(root.join("d"), root.join("e")).unwrap();
    let synced = koil.sync(&listing(&koil)).unwrap();
    let edits = vec![Edit::Change {
        from: d.clone(),
        to: with_id(d.id.unwrap(), "e/"),
    }];
    assert_eq!(edits, synced.edits);
    // the change inside it follows it
    assert_eq!(vec![rename("e/inside", "e/renamed")], actions(&koil, &root));
}

#[test]
fn test_sync_new_dir() {
    let (_tmp, root) = setup();
    let mut koil = open(&root.join("d"));
    // a dir that is not created yet, inside one that is gone
    let entries = [keep(&koil, "inside"), without_id("new/")];
    koil.update_and_open(&entries, Settings::default(), Some(Path::new("new")))
        .unwrap();
    let before = koil.save_state();
    let synced = koil.sync(&[]).unwrap();
    assert_eq!(Synced::default(), synced);
    assert_eq!(before, koil.save_state());
}

#[test]
fn test_not_on_disk() {
    let (_tmp, root) = setup();
    let mut koil = open(&root);
    let a = id(&koil, "a");
    koil.open("d").unwrap();
    fs::remove_file(root.join("a")).unwrap();
    // moved here from the dir it was in, but it is gone
    let entries = [keep(&koil, "inside"), with_id(a, "a")];
    let error = koil.update(&entries).unwrap_err();
    let kind = EntryErrorKind::NotOnDisk(with_slashes(&root.join("a")));
    assert_eq!(vec![EntryError { entry: 1, kind }], error.errors);
}

#[test]
fn test_watched() {
    let (_tmp, root) = setup();
    let koil = open(&root);
    let watched = koil.watched();
    let parent = root.parent().unwrap().to_path_buf();
    assert_eq!(
        vec![(parent.clone(), false), (root.clone(), false)],
        watched.dirs
    );
    assert!(watched.affects(&root.join("new")));
    assert!(watched.affects(&root));
    assert!(!watched.affects(&root.join("d/new")));
    assert!(!watched.affects(&parent.join("sibling")));

    let mut koil = open(&root);
    koil.open("**/*.rs").unwrap();
    let watched = koil.watched();
    assert_eq!(vec![(parent, false), (root.clone(), true)], watched.dirs);
    assert!(watched.affects(&root.join("d/main.rs")));
    // a dir can have matches inside
    assert!(watched.affects(&root.join("d/e")));

    koil.open(&root).unwrap();
    koil.open("*.rs").unwrap();
    let watched = koil.watched();
    assert!(watched.affects(&root.join("main.rs")));
    assert!(!watched.affects(&root.join("notes.txt")));
    assert!(!watched.affects(&root.join("d/main.rs")));
}
