//! These tests change a real temp dir, and move deleted paths to the system trash

use super::*;
use std::collections::BTreeMap;
use tempfile::TempDir;

/// A temp dir with:
/// - `a` (contains "a")
/// - `b` (contains "b")
/// - `dir/x` (contains "x")
/// - `dir/sub/y` (contains "y")
fn test_temp_dir() -> TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("dir/sub")).unwrap();
    fs::write(root.join("a"), "a").unwrap();
    fs::write(root.join("b"), "b").unwrap();
    fs::write(root.join("dir/x"), "x").unwrap();
    fs::write(root.join("dir/sub/y"), "y").unwrap();
    temp
}

/// Koil with `temp` opened
fn temp_koil(temp: &TempDir) -> Koil {
    let mut koil = Koil::default();
    koil.open(temp.path()).unwrap();
    koil
}

/// Every path inside `root`, relative to it, with the contents of files (`None` for dirs)
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<String>> {
    fn walk(root: &Path, dir: &Path, result: &mut BTreeMap<PathBuf, Option<String>>) {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                result.insert(rel, None);
                walk(root, &path, result);
            } else {
                result.insert(rel, Some(fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

/// Apply `listing` to a fresh [`test_temp_dir`], check it gives `after`, then undo it
/// and check that everything is as it was
fn check_undo(listing: impl Fn(&Koil) -> Vec<Entry>, after: &[(&str, Option<&str>)]) {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    let entries = listing(&koil);
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    let expected: BTreeMap<PathBuf, Option<String>> = after
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.map(str::to_string)))
        .collect();
    assert_eq!(expected, snapshot(temp.path()));

    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
    assert_eq!(None, koil.undo_steps().unwrap());
}

#[test]
fn test_undo_delete() {
    check_undo(|k| vec![keep(k, "b")], &[("b", Some("b"))]);
}

#[test]
fn test_undo_rename() {
    check_undo(
        |k| {
            vec![
                with_id(id(k, "a"), "renamed"),
                keep(k, "b"),
                with_id(id(k, "dir"), "dir2/"),
            ]
        },
        &[
            ("b", Some("b")),
            ("dir2", None),
            ("dir2/sub", None),
            ("dir2/sub/y", Some("y")),
            ("dir2/x", Some("x")),
            ("renamed", Some("a")),
        ],
    );
}

#[test]
fn test_undo_swap() {
    check_undo(
        |k| {
            vec![
                with_id(id(k, "a"), "b"),
                with_id(id(k, "b"), "a"),
                keep(k, "dir/"),
            ]
        },
        &[
            ("a", Some("b")),
            ("b", Some("a")),
            ("dir", None),
            ("dir/sub", None),
            ("dir/sub/y", Some("y")),
            ("dir/x", Some("x")),
        ],
    );
}

#[test]
fn test_undo_copy_and_create() {
    check_undo(
        |k| {
            vec![
                keep(k, "a"),
                with_id(id(k, "a"), "a2"),
                keep(k, "b"),
                keep(k, "dir/"),
                with_id(id(k, "dir"), "dir2/"),
                without_id("new"),
                without_id("new_dir/nested/file"),
            ]
        },
        &[
            ("a", Some("a")),
            ("a2", Some("a")),
            ("b", Some("b")),
            ("dir", None),
            ("dir/sub", None),
            ("dir/sub/y", Some("y")),
            ("dir/x", Some("x")),
            ("dir2", None),
            ("dir2/sub", None),
            ("dir2/sub/y", Some("y")),
            ("dir2/x", Some("x")),
            ("new", Some("")),
            ("new_dir", None),
            ("new_dir/nested", None),
            ("new_dir/nested/file", Some("")),
        ],
    );
}

#[test]
fn test_undo_move_into_other_dir() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // remove `a` here, and write it in `dir`
    let a = id(&koil, "a");
    let entries = [keep(&koil, "b"), keep(&koil, "dir/")];
    koil.update(&entries).unwrap();
    koil.open("dir").unwrap();
    let entries = [keep(&koil, "sub/"), keep(&koil, "x"), with_id(a, "a")];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    assert!(temp.path().join("dir/a").exists());
    assert!(!temp.path().join("a").exists());

    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
}

#[test]
fn test_undo_last_apply_first() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // delete `a`
    let entries = [keep(&koil, "b"), keep(&koil, "dir/")];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    let deleted = snapshot(temp.path());
    // rename `b` to `a`, so the second apply depends on the first one
    let entries = [with_id(id(&koil, "b"), "a"), keep(&koil, "dir/")];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    assert_eq!(
        Some("b".to_string()),
        fs::read_to_string(temp.path().join("a")).ok()
    );

    koil.undo().unwrap();
    assert_eq!(deleted, snapshot(temp.path()));
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
    assert_eq!(None, koil.undo_steps().unwrap());
}

#[test]
fn test_undo_never_overwrites() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // rename `b` to `c`, delete `a`, then make a new `a` outside of koil
    let entries = [with_id(id(&koil, "b"), "c"), keep(&koil, "dir/")];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    fs::write(temp.path().join("a"), "new a").unwrap();
    let applied = snapshot(temp.path());

    // restoring `a` would fail, so nothing is undone
    let result = koil.undo();
    assert!(
        matches!(
            result,
            Err(KoilError::UndoBlocked { blocked: Blocked::Taken(ref p), .. })
                if p.ends_with("a")
        ),
        "{result:?}"
    );
    assert_eq!(applied, snapshot(temp.path()));

    // once the new `a` is gone, it can be undone
    fs::remove_file(temp.path().join("a")).unwrap();
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
    assert_eq!(None, koil.undo_steps().unwrap());
}

/// The applies of `koil`'s history that can be undone now, oldest first, by whether they are
/// blocked, and their needs
fn undoable(koil: &Koil) -> Vec<(bool, Vec<usize>)> {
    (koil.undoable().unwrap().into_iter())
        .map(|u| (u.blocked.is_none(), u.needs))
        .collect()
}

#[test]
fn test_undo_older_apply() {
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);

    // rename `a`, then `b`, which have nothing to do with each other
    let entries = [
        with_id(id(&koil, "a"), "a2"),
        keep(&koil, "b"),
        keep(&koil, "dir/"),
    ];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    let entries = [
        keep(&koil, "a2"),
        with_id(id(&koil, "b"), "b2"),
        keep(&koil, "dir/"),
    ];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    let history = koil.history().to_vec();
    assert_eq!(2, history.len());
    assert!(history[0].time < history[1].time);
    assert_eq!(koil.current_dir(), history[0].dir);
    assert_eq!(vec![(true, vec![]), (true, vec![])], undoable(&koil));

    // the first one is undone, and the second one stays
    assert_eq!(1, koil.undo_only(&history[..1]).unwrap().changes);
    assert!(temp.path().join("a").exists());
    assert!(temp.path().join("b2").exists());
    assert_eq!(&history[1..], koil.history());
}

#[test]
fn test_undo_with_newer_apply() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // delete `a`, then rename `b` to `a`, then rename `dir/x`
    let entries = [keep(&koil, "b"), keep(&koil, "dir/")];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    let entries = [with_id(id(&koil, "b"), "a"), keep(&koil, "dir/")];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    koil.open("dir").unwrap();
    let entries = [keep(&koil, "sub/"), with_id(id(&koil, "x"), "x2")];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    // the first one can not be undone without the second one, which put something at `a`
    assert_eq!(
        vec![(true, vec![1]), (true, vec![]), (true, vec![])],
        undoable(&koil)
    );

    // so it is not undone without it
    let history = koil.history().to_vec();
    let result = koil.undo_only(&history[..1]);
    assert!(
        matches!(result, Err(KoilError::NothingToUndo)),
        "{result:?}"
    );
    assert_eq!(history, koil.history());

    // and with it, both are, the newest first
    assert_eq!(2, koil.undo_only(&history[..2]).unwrap().changes);
    assert_eq!(&history[2..], koil.history());
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
}

#[test]
fn test_undo_blocked_by_changes_on_disk() {
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);

    // rename `a` to `c`, which is then deleted outside of koil
    let entries = [
        with_id(id(&koil, "a"), "c"),
        keep(&koil, "b"),
        keep(&koil, "dir/"),
    ];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    fs::remove_file(temp.path().join("c")).unwrap();
    let undoable = koil.undoable().unwrap();
    let c = koil.current_dir().join("c");
    assert_eq!(
        Some(Blocked::Missing(c.clone())),
        undoable[0].blocked.clone().map(|(_, blocked)| blocked)
    );
    let history = koil.history().to_vec();
    let result = koil.undo_only(&history);
    assert!(
        matches!(result, Err(KoilError::UndoBlocked { blocked: Blocked::Missing(ref p), .. }) if *p == c),
        "{result:?}"
    );
    assert_eq!(history, koil.history());
}

/// Once the trash is emptied, the path of what an apply deleted is free there, and what is
/// trashed at it later must never be restored in its place
/// Only on macOS, where the trash's paths can be emptied one by one
#[cfg(target_os = "macos")]
#[test]
fn test_undo_after_the_trash_was_emptied() {
    let since = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    let name = format!("koil-test-{}", since.unwrap().as_nanos());
    let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fs::write(first.path().join(&name), "first").unwrap();
    fs::write(second.path().join(&name), "second").unwrap();
    let mut koil = Koil::default();
    koil.open(first.path()).unwrap();
    koil.update(&[]).unwrap();
    koil.apply().unwrap();

    // the trash emptied (of this test's file only), then the other one trashed at its path
    let in_trash = std::env::home_dir().unwrap().join(".Trash").join(&name);
    assert_eq!("first", fs::read_to_string(&in_trash).unwrap());
    fs::remove_file(&in_trash).unwrap();
    let other = crate::trash::trash(&second.path().join(&name)).unwrap();
    assert_eq!("second", fs::read_to_string(&in_trash).unwrap());

    let undoable = koil.undoable().unwrap();
    let undone = koil.undo();
    // out of the trash before anything fails
    crate::trash::restore(&other).unwrap();
    assert!(
        matches!(undoable[0].blocked, Some((_, Blocked::NotInTrash(_)))),
        "{undoable:?}"
    );
    assert!(undone.is_err());
    assert!(!first.path().join(&name).exists());
}

#[test]
fn test_history_in_another_session() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // delete `a` and make `dir/new`
    let entries = [keep(&koil, "b"), keep(&koil, "dir/"), without_id("dir/new")];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();

    // a new koil, somewhere else, undoes it
    let history = koil.history().to_vec();
    let saved = serde_json::to_string(&history).unwrap();
    let mut other = Koil::default();
    other.open(temp.path().join("dir/sub")).unwrap();
    other.set_history(serde_json::from_str(&saved).unwrap());
    assert_eq!(history, other.history());
    other.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
    assert!(other.history().is_empty());
}

#[test]
fn test_never_into_itself() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let dir = temp.path().join("dir");
    // a copy would copy itself again, until the path is too long
    let actions = [
        Action::Copy(dir.clone(), dir.join("copy")),
        Action::Rename(dir.clone(), dir.join("sub/moved")),
    ];
    for action in actions {
        let error = action.run().unwrap_err();
        assert_eq!(io::ErrorKind::InvalidInput, error.kind(), "{action}");
    }
    assert_eq!(before, snapshot(temp.path()));

    // a symlink to a dir is copied as a link, so it can go in that dir
    #[cfg(unix)]
    {
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        Action::Copy(link, dir.join("link")).run().unwrap();
        assert!(dir.join("link").symlink_metadata().unwrap().is_symlink());
    }
}

#[test]
fn test_undo_failed_apply() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // delete `a` and create `new`, but `new` appears before it is applied
    let entries = [keep(&koil, "b"), keep(&koil, "dir/"), without_id("new")];
    koil.update(&entries).unwrap();
    fs::write(temp.path().join("new"), "new").unwrap();
    assert!(matches!(
        koil.apply(),
        Err(KoilError::ApplyFailed {
            done: 1,
            total: 2,
            ..
        })
    ));
    assert!(!temp.path().join("a").exists());

    // the delete that did run can be undone, and the new file is left alone
    fs::remove_file(temp.path().join("new")).unwrap();
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
}

/// Apply only the changes of `listing` (on a fresh [`test_temp_dir`]) that `pick` picks, check
/// it gives `after`, and that the rest are forgotten, then undo it
fn check_apply_only(
    listing: impl Fn(&Koil) -> Vec<Entry>,
    pick: impl Fn(&Action) -> bool,
    after: &[(&str, Option<&str>)],
) {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    koil.update(&listing(&koil)).unwrap();
    let picked: Vec<Action> = (koil.changes().into_iter())
        .map(|c| c.action)
        .filter(|a| pick(a))
        .collect();
    koil.apply_only(&picked).unwrap();
    let expected: BTreeMap<PathBuf, Option<String>> = after
        .iter()
        .map(|(p, c)| (PathBuf::from(p), c.map(str::to_string)))
        .collect();
    assert_eq!(expected, snapshot(temp.path()));
    assert_eq!(Vec::<Action>::new(), koil.compute_actions());

    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
}

/// Everything in [`test_temp_dir`] that is in `dir`
const DIR: [(&str, Option<&str>); 4] = [
    ("dir", None),
    ("dir/sub", None),
    ("dir/sub/y", Some("y")),
    ("dir/x", Some("x")),
];

#[test]
fn test_apply_only_some() {
    // the rename, but not the delete
    check_apply_only(
        |k| vec![with_id(id(k, "a"), "renamed"), keep(k, "dir/")],
        |a| matches!(a, Action::Rename(..)),
        &[[("b", Some("b")), ("renamed", Some("a"))].as_slice(), &DIR].concat(),
    );
}

#[test]
fn test_apply_only_with_what_it_needs() {
    // `a` goes to `a2` and is copied to `b`, and `b` to `a`; without `b -> a`, `b` is still
    // taken, so the copy onto it is left out too
    check_apply_only(
        |k| {
            vec![
                with_id(id(k, "a"), "b"),
                with_id(id(k, "b"), "a"),
                with_id(id(k, "a"), "a2"),
                keep(k, "dir/"),
            ]
        },
        |a| !matches!(a, Action::Rename(s, _) if s.ends_with("b")),
        &[[("a2", Some("a")), ("b", Some("b"))].as_slice(), &DIR].concat(),
    );
}

#[test]
fn test_apply_only_new_dir() {
    // the new dir, but not what goes in it
    check_apply_only(
        |k| {
            vec![
                keep(k, "a"),
                keep(k, "b"),
                keep(k, "dir/"),
                without_id("new/nested/file"),
                without_id("new/file"),
            ]
        },
        |a| {
            matches!(a, Action::CreateDir(p) if p.ends_with("new"))
                || matches!(a, Action::CreateFile(p) if p.ends_with("nested/file"))
        },
        &[
            [("a", Some("a")), ("b", Some("b")), ("new", None)].as_slice(),
            &DIR,
        ]
        .concat(),
    );
}

#[test]
fn test_create_now() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);
    let dir = koil.current_dir().to_path_buf();
    let a = id(&koil, "a");

    // `new/file` is created with its new dir, and the other changes stay
    let entries = [
        with_id(a, "renamed"),
        keep(&koil, "b"),
        keep(&koil, "dir/"),
        without_id("new/file"),
        without_id("other"),
    ];
    koil.update(&entries).unwrap();
    let new = dir.join("new/file");
    let created = vec![
        Action::CreateDir(dir.join("new")),
        Action::CreateFile(new.clone()),
    ];
    assert_eq!(created, koil.create_steps(&new).unwrap());
    assert_eq!(2, koil.create_now(&new).unwrap().changes);
    let expected: BTreeMap<PathBuf, Option<String>> = [
        [("a", Some("a")), ("b", Some("b"))].as_slice(),
        &DIR,
        &[("new", None), ("new/file", Some(""))],
    ]
    .concat()
    .iter()
    .map(|(p, c)| (PathBuf::from(p), c.map(str::to_string)))
    .collect();
    assert_eq!(expected, snapshot(temp.path()));
    let mut pending = koil.compute_actions();
    pending.sort();
    let mut expected = vec![
        Action::Rename(dir.join("a"), dir.join("renamed")),
        Action::CreateFile(dir.join("other")),
    ];
    expected.sort();
    assert_eq!(expected, pending);
    // the new dir is on disk now, with an ID
    assert!(koil.listing().contains(&keep(&koil, "new/")));

    // undone like an apply, once nothing else is pending
    assert!(matches!(koil.undo(), Err(KoilError::PendingChanges)));
    let entries = [
        keep(&koil, "a"),
        keep(&koil, "b"),
        keep(&koil, "dir/"),
        keep(&koil, "new/"),
    ];
    koil.update(&entries).unwrap();
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
}

#[test]
fn test_create_now_needs_other_changes() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);
    let dir = koil.current_dir().to_path_buf();

    // a new `a` where `a` is moved away from, and a new file in a renamed dir
    let entries = [
        with_id(id(&koil, "a"), "a2"),
        without_id("a"),
        keep(&koil, "b"),
        with_id(id(&koil, "dir"), "dir2/"),
        without_id("dir2/new"),
    ];
    koil.update(&entries).unwrap();
    let actions = koil.compute_actions();
    for name in ["a", "dir2/new"] {
        let result = koil.create_now(&dir.join(name));
        assert!(matches!(result, Err(KoilError::NeedsChanges(_))));
    }
    let result = koil.create_now(&dir.join("b"));
    assert!(matches!(result, Err(KoilError::NotNew(_))));
    // nothing changed
    assert_eq!(before, snapshot(temp.path()));
    assert_eq!(actions, koil.compute_actions());
}

#[test]
fn test_dir_replaced_by_one_inside() {
    for renamed in [false, true] {
        let temp = test_temp_dir();
        let before = snapshot(temp.path());
        let mut koil = temp_koil(&temp);

        // `dir/sub` moves out of `dir`, takes its name, and `dir` is deleted or renamed
        koil.open("dir").unwrap();
        let sub = id(&koil, "sub");
        let entries = [keep(&koil, "x")];
        koil.update(&entries).unwrap();
        koil.open(temp.path()).unwrap();
        let mut entries = vec![keep(&koil, "a"), keep(&koil, "b"), with_id(sub, "dir/")];
        if renamed {
            entries.push(with_id(id(&koil, "dir"), "old/"));
        }
        koil.update(&entries).unwrap();
        koil.apply().unwrap();
        let mut after = vec![
            ("a", Some("a")),
            ("b", Some("b")),
            ("dir", None),
            ("dir/y", Some("y")),
        ];
        if renamed {
            after.extend([("old", None), ("old/x", Some("x"))]);
        }
        let after: BTreeMap<PathBuf, Option<String>> = (after.into_iter())
            .map(|(p, c)| (PathBuf::from(p), c.map(str::to_string)))
            .collect();
        assert_eq!(after, snapshot(temp.path()));
        koil.undo().unwrap();
        assert_eq!(before, snapshot(temp.path()));
    }
}

#[test]
fn test_undo_in_renamed_dir() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // `dir/x` moves out of `dir`, which is renamed: undo puts `x` back in a dir that one of
    // its own steps moves
    koil.open("dir").unwrap();
    let x = id(&koil, "x");
    let entries = [keep(&koil, "sub/")];
    koil.update(&entries).unwrap();
    koil.open(temp.path()).unwrap();
    let entries = [
        keep(&koil, "a"),
        keep(&koil, "b"),
        with_id(id(&koil, "dir"), "dir2/"),
        with_id(x, "x"),
    ];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    assert!(temp.path().join("dir2/sub").exists());
    assert!(temp.path().join("x").exists());
    assert_eq!(vec![(true, vec![])], undoable(&koil));
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
}
