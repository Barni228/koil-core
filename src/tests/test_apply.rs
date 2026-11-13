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
fn test_undo_move_into_renamed_dir() {
    // `a` and a copy of `b` go into the old `dir`, and with it to `dir2`, as a new file would
    check_undo(
        |koil| {
            vec![
                keep(koil, "b"),
                with_id(id(koil, "dir"), "dir2/"),
                with_id(id(koil, "a"), "dir/a"),
                with_id(id(koil, "b"), "dir/b"),
            ]
        },
        &[
            ("b", Some("b")),
            ("dir2", None),
            ("dir2/a", Some("a")),
            ("dir2/b", Some("b")),
            ("dir2/sub", None),
            ("dir2/sub/y", Some("y")),
            ("dir2/x", Some("x")),
        ],
    );
}

#[test]
fn test_undo_new_dir_where_a_file_was() {
    // `a` is moved away, and a dir is made where it was for `a/new`
    check_undo(
        |koil| {
            vec![
                with_id(id(koil, "a"), "a2"),
                keep(koil, "b"),
                keep(koil, "dir/"),
                without_id("a/new"),
            ]
        },
        &[
            ("a", None),
            ("a/new", Some("")),
            ("a2", Some("a")),
            ("b", Some("b")),
            ("dir", None),
            ("dir/sub", None),
            ("dir/sub/y", Some("y")),
            ("dir/x", Some("x")),
        ],
    );
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

/// Apply only the changes `pick` picks, of what `koil` (on `temp`) would apply, check it gives
/// `after`, and that what is left to apply is `left`, then take that back, and undo the apply
fn check_picked(
    temp: &TempDir,
    mut koil: Koil,
    pick: impl Fn(&Action) -> bool,
    after: &[(&str, Option<&str>)],
    left: &[Action],
) {
    let before = snapshot(temp.path());
    let picked: Vec<Action> = (koil.changes().into_iter())
        .map(|c| c.action)
        .filter(|a| pick(a))
        .collect();
    koil.apply_only(&picked).unwrap();
    assert_eq!(expected(after), snapshot(temp.path()));
    assert_eq!(sorted(left), actions_left(&koil));

    let all: Vec<Action> = koil.changes().into_iter().map(|c| c.action).collect();
    assert_eq!(left.len(), koil.discard_only(&all).unwrap().changes);
    assert!(!koil.has_changes());
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
}

/// [`check_picked`] of the changes of `listing`, on a fresh [`test_temp_dir`]
fn check_apply_only(
    listing: impl Fn(&Koil) -> Vec<Entry>,
    pick: impl Fn(&Action) -> bool,
    after: &[(&str, Option<&str>)],
    left: &[Action],
) {
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);
    koil.update(&listing(&koil)).unwrap();
    check_picked(&temp, koil, pick, after, left);
}

/// `paths` as [`snapshot`] gives them
fn expected(paths: &[(&str, Option<&str>)]) -> BTreeMap<PathBuf, Option<String>> {
    (paths.iter())
        .map(|(p, c)| (PathBuf::from(p), c.map(str::to_string)))
        .collect()
}

fn sorted(actions: &[Action]) -> Vec<Action> {
    let mut actions = actions.to_vec();
    actions.sort();
    actions
}

/// What applying would do, with paths relative to the open dir, as [`rename`] and the like
/// write them, sorted
fn actions_left(koil: &Koil) -> Vec<Action> {
    let relative = |p: &PathBuf| p.strip_prefix(koil.current_dir()).unwrap().to_path_buf();
    let left: Vec<Action> = (koil.compute_actions().iter())
        .map(|action| match action {
            Action::CreateFile(p) => Action::CreateFile(relative(p)),
            Action::CreateDir(p) => Action::CreateDir(relative(p)),
            Action::DeleteFile(p) => Action::DeleteFile(relative(p)),
            Action::DeleteDir(p) => Action::DeleteDir(relative(p)),
            Action::Rename(s, d) => Action::Rename(relative(s), relative(d)),
            Action::Copy(s, d) => Action::Copy(relative(s), relative(d)),
        })
        .collect();
    sorted(&left)
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
    // the rename, but not the delete, which stays
    check_apply_only(
        |k| vec![with_id(id(k, "a"), "renamed"), keep(k, "dir/")],
        |a| matches!(a, Action::Rename(..)),
        &[[("b", Some("b")), ("renamed", Some("a"))].as_slice(), &DIR].concat(),
        &[delete("b")],
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
        // `a` is at `a2` now, which the copy is of
        &[copy("a2", "b"), rename("b", "a")],
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
        &[
            add("new/file"),
            add_dir("new/nested"),
            add("new/nested/file"),
        ],
    );
}

#[test]
fn test_apply_only_follows_a_renamed_dir() {
    // what user changed inside `dir` goes with it, and so does what was written in it from
    // outside, but `a` goes where `dir` was, which the rename freed
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);
    koil.open(temp.path().join("dir")).unwrap();
    let inside = [with_id(id(&koil, "x"), "x2"), keep(&koil, "sub/")];
    koil.update(&inside).unwrap();
    koil.open(temp.path()).unwrap();
    let outside = [
        with_id(id(&koil, "dir"), "renamed/"),
        with_id(id(&koil, "a"), "dir"),
        keep(&koil, "b"),
        without_id("dir/new"),
    ];
    koil.update(&outside).unwrap();
    check_picked(
        &temp,
        koil,
        |a| matches!(a, Action::Rename(s, _) if s.ends_with("dir")),
        &[
            ("a", Some("a")),
            ("b", Some("b")),
            ("renamed", None),
            ("renamed/sub", None),
            ("renamed/sub/y", Some("y")),
            ("renamed/x", Some("x")),
        ],
        &[
            add("renamed/new"),
            rename("a", "dir"),
            rename("renamed/x", "renamed/x2"),
        ],
    );
}

#[test]
fn test_apply_only_swap() {
    // a swap goes through a temp path, which nothing is left at
    check_apply_only(
        |k| {
            vec![
                with_id(id(k, "a"), "b"),
                with_id(id(k, "b"), "a"),
                with_id(id(k, "dir/"), "dir2/"),
            ]
        },
        |a| !matches!(a, Action::Rename(s, _) if s.ends_with("dir")),
        &[[("a", Some("b")), ("b", Some("a"))].as_slice(), &DIR].concat(),
        &[rename("dir", "dir2")],
    );
}

#[test]
fn test_apply_only_deleted_dir() {
    // what user changed inside a dir that is deleted goes to the trash with it, and what was
    // moved into it stays where it is, but what is outside stays pending
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);
    let b = id(&koil, "b");
    koil.open(temp.path().join("dir")).unwrap();
    let inside = [
        with_id(id(&koil, "x"), "x2"),
        keep(&koil, "sub/"),
        without_id("new"),
        with_id(b, "b"),
    ];
    koil.update(&inside).unwrap();
    koil.open(temp.path()).unwrap();
    koil.update(&[with_id(id(&koil, "a"), "a2")]).unwrap();
    check_picked(
        &temp,
        koil,
        |a| matches!(a, Action::DeleteDir(_)),
        &[("a", Some("a")), ("b", Some("b"))],
        &[rename("a", "a2")],
    );
}

#[test]
fn test_apply_only_keeps_what_failed() {
    // the create fails, as `new` appeared, but it stays, with what did not run after it
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);
    let entries = [
        keep(&koil, "b"),
        keep(&koil, "dir/"),
        without_id("new"),
        without_id("new2"),
    ];
    koil.update(&entries).unwrap();
    fs::write(temp.path().join("new"), "new").unwrap();
    let all: Vec<Action> = koil.changes().into_iter().map(|c| c.action).collect();
    assert!(matches!(
        koil.apply_only(&all),
        Err(KoilError::ApplyFailed { done: 1, .. })
    ));
    assert!(!temp.path().join("a").exists());
    assert_eq!(sorted(&[add("new"), add("new2")]), actions_left(&koil));
}

#[test]
fn test_discard_only() {
    // the rename of `a` is taken back, and the copy onto `a`, which needs it, goes with it;
    // the other copy and the new file stay
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);
    let entries = [
        with_id(id(&koil, "a"), "c"),
        keep(&koil, "b"),
        with_id(id(&koil, "b"), "a"),
        with_id(id(&koil, "b"), "b2"),
        keep(&koil, "dir/"),
        without_id("new"),
    ];
    koil.update(&entries).unwrap();
    let before = snapshot(temp.path());
    let renamed = (koil.changes().into_iter())
        .map(|c| c.action)
        .find(|a| matches!(a, Action::Rename(..)))
        .unwrap();
    assert_eq!(2, koil.discard_only(&[renamed]).unwrap().changes);
    assert_eq!(before, snapshot(temp.path()));
    assert_eq!(sorted(&[add("new"), copy("b", "b2")]), actions_left(&koil));
    // the listing shows them as they are again
    let mut names: Vec<PathBuf> = koil.listing().into_iter().map(|e| e.name).collect();
    names.sort();
    let expected: Vec<PathBuf> = ["a", "b", "b2", "dir", "new"].map(PathBuf::from).into();
    assert_eq!(expected, names);

    // a rename with a copy is the copy alone without it (the last path is the rename's)
    let mut koil = temp_koil(&temp);
    let entries = [
        with_id(id(&koil, "a"), "a2"),
        with_id(id(&koil, "a"), "a3"),
        keep(&koil, "b"),
        keep(&koil, "dir/"),
    ];
    koil.update(&entries).unwrap();
    let renamed = (koil.changes().into_iter())
        .map(|c| c.action)
        .find(|a| matches!(a, Action::Rename(..)))
        .unwrap();
    koil.discard_only(&[renamed]).unwrap();
    assert_eq!(vec![copy("a", "a2")], actions_left(&koil));
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
    assert_eq!(created, koil.create_steps(&[&new]).unwrap());
    assert_eq!(2, koil.create_now(&[&new]).unwrap().changes);
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
        let result = koil.create_now(&[dir.join(name)]);
        assert!(matches!(result, Err(KoilError::NeedsChanges(_))));
    }
    let result = koil.create_now(&[dir.join("b")]);
    assert!(matches!(result, Err(KoilError::NotNew(_))));
    // nothing changed
    assert_eq!(before, snapshot(temp.path()));
    assert_eq!(actions, koil.compute_actions());
}

/// What [`snapshot`] gives for `paths`
fn tree(paths: &[(&str, Option<&str>)]) -> BTreeMap<PathBuf, Option<String>> {
    (paths.iter())
        .map(|(p, c)| (PathBuf::from(p), c.map(str::to_string)))
        .collect()
}

#[test]
fn test_copy_now() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);
    let dir = koil.current_dir().to_path_buf();
    let a = id(&koil, "a");

    // `a` is copied to `copy` and `new/a`, and renamed to `renamed` (the last of its lines)
    let entries = [
        with_id(a, "copy"),
        with_id(a, "new/a"),
        with_id(a, "renamed"),
        keep(&koil, "b"),
        keep(&koil, "dir/"),
    ];
    koil.update(&entries).unwrap();
    let copy = dir.join("copy");
    assert_eq!(Some(dir.join("a")), koil.copied_from(&copy));
    assert_eq!(None, koil.copied_from(&dir.join("renamed")));
    assert_eq!(None, koil.copied_from(&dir.join("b")));
    assert_eq!(
        vec![copy.clone(), dir.join("new/a")],
        koil.copies_of(&dir.join("a"))
    );
    assert!(koil.copies_of(&dir.join("b")).is_empty());

    // a copy is made on its own, with the new dir it goes in
    let steps = vec![
        Action::CreateDir(dir.join("new")),
        Action::Copy(dir.join("a"), dir.join("new/a")),
    ];
    assert_eq!(steps, koil.create_steps(&[dir.join("new/a")]).unwrap());
    assert_eq!(1, koil.create_now(&[&copy]).unwrap().changes);
    assert_eq!(None, koil.copied_from(&copy));
    assert_eq!(vec![dir.join("new/a")], koil.copies_of(&dir.join("a")));
    let mut pending = koil.compute_actions();
    pending.sort();
    let mut expected = vec![
        Action::CreateDir(dir.join("new")),
        Action::Copy(dir.join("a"), dir.join("new/a")),
        Action::Rename(dir.join("a"), dir.join("renamed")),
    ];
    expected.sort();
    assert_eq!(expected, pending);

    // it is a file of its own, with an ID of its own: what changes in `a` it does not get
    let listing = koil.listing();
    assert!(listing.contains(&keep(&koil, "copy")));
    assert_ne!(Some(a), koil.id_of(&copy));
    fs::write(dir.join("a"), "changed").unwrap();
    koil.apply().unwrap();
    let after = [
        [
            ("b", Some("b")),
            ("copy", Some("a")),
            ("new", None),
            ("new/a", Some("changed")),
            ("renamed", Some("changed")),
        ]
        .as_slice(),
        &DIR,
    ]
    .concat();
    assert_eq!(tree(&after), snapshot(temp.path()));

    // undone like two applies
    fs::write(dir.join("renamed"), "a").unwrap();
    fs::write(dir.join("new/a"), "a").unwrap();
    koil.undo().unwrap();
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
}

#[test]
fn test_copy_dir_now() {
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);
    let dir = koil.current_dir().to_path_buf();

    // `dir/x` is renamed, and a new file goes in `dir`, which is copied
    koil.open("dir").unwrap();
    let entries = [
        with_id(id(&koil, "x"), "x2"),
        keep(&koil, "sub/"),
        without_id("new"),
    ];
    koil.update(&entries).unwrap();
    koil.open(temp.path()).unwrap();
    let entries = [
        keep(&koil, "a"),
        keep(&koil, "b"),
        keep(&koil, "dir/"),
        with_id(id(&koil, "dir"), "dir2/"),
    ];
    koil.update(&entries).unwrap();
    // a file in `dir`, or in a dir in it, goes into the copy as it is now
    let copies = vec![dir.join("dir2")];
    assert_eq!(copies, koil.copies_of(&dir.join("dir/x")));
    assert_eq!(copies, koil.copies_of(&dir.join("dir/sub/y")));
    assert_eq!(copies, koil.copies_of(&dir.join("dir")));
    // creating `new` now makes the copy first, which applying would make without it
    let steps = vec![
        Action::Copy(dir.join("dir"), dir.join("dir2")),
        Action::CreateFile(dir.join("dir/new")),
    ];
    assert_eq!(steps, koil.create_steps(&[dir.join("dir/new")]).unwrap());

    // the copy is of `dir` as it is on disk, without the changes inside it
    koil.apply().unwrap();
    let after = [
        [("a", Some("a")), ("b", Some("b"))].as_slice(),
        &[("dir", None), ("dir/new", Some("")), ("dir/x2", Some("x"))],
        &[("dir/sub", None), ("dir/sub/y", Some("y"))],
        &[
            ("dir2", None),
            ("dir2/sub", None),
            ("dir2/sub/y", Some("y")),
        ],
        &[("dir2/x", Some("x"))],
    ]
    .concat();
    assert_eq!(tree(&after), snapshot(temp.path()));
}

#[test]
fn test_copy_to_a_path_moved_into_it() {
    let temp = test_temp_dir();
    let mut koil = temp_koil(&temp);

    // `b` goes into `dir`, which is copied to `b`
    let entries = [
        keep(&koil, "a"),
        with_id(id(&koil, "b"), "dir/b"),
        keep(&koil, "dir/"),
        with_id(id(&koil, "dir"), "b/"),
    ];
    koil.update(&entries).unwrap();
    koil.apply().unwrap();
    let after = [
        [("a", Some("a")), ("b", None), ("b/x", Some("x"))].as_slice(),
        &[("b/sub", None), ("b/sub/y", Some("y"))],
        &DIR,
        &[("dir/b", Some("b"))],
    ]
    .concat();
    assert_eq!(tree(&after), snapshot(temp.path()));
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

/// When what is at `path` (the link itself, for a link) was modified, in seconds
fn modified(path: &Path) -> i64 {
    filetime::FileTime::from_last_modification_time(&path.symlink_metadata().unwrap())
        .unix_seconds()
}

/// Make everything in a [`test_temp_dir`] at `root` modified at times of its own, long ago,
/// the dirs after what is in them, as a file made in a dir changes when it was modified
fn set_old_times(root: &Path) -> Vec<(&'static str, i64)> {
    let times = [
        ("a", 1_000_000_000),
        ("dir/sub/y", 1_100_000_000),
        ("dir/sub", 1_200_000_000),
        ("dir/x", 1_300_000_000),
        ("dir", 1_400_000_000),
    ];
    for (path, time) in times {
        let time = filetime::FileTime::from_unix_time(time, 0);
        filetime::set_file_times(root.join(path), time, time).unwrap();
    }
    times.to_vec()
}

#[test]
fn test_move_across() {
    // what a move to another filesystem does, here within one: a copy, then the source removed
    let temp = test_temp_dir();
    let root = temp.path();
    let times = set_old_times(root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // a link is moved as a link, and a dir keeps its permissions
        std::os::unix::fs::symlink("x", root.join("dir/link")).unwrap();
        fs::set_permissions(root.join("dir/sub"), fs::Permissions::from_mode(0o700)).unwrap();
        // the link changed when `dir` was modified
        set_old_times(root);
    }
    let before = snapshot(&root.join("dir"));

    crate::apply::move_across(&root.join("dir"), &root.join("moved")).unwrap();
    assert!(!root.join("dir").exists());
    assert_eq!(before, snapshot(&root.join("moved")));
    for (path, time) in times {
        if let Some(rest) = path.strip_prefix("dir") {
            let moved = format!("moved{rest}");
            assert_eq!(time, modified(&root.join(&moved)), "{moved}");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let link = root.join("moved/link");
        assert_eq!(Path::new("x"), fs::read_link(link).unwrap());
        let mode = root.join("moved/sub").metadata().unwrap().permissions();
        assert_eq!(0o700, mode.mode() & 0o777);
    }

    // a file, with the time it had
    crate::apply::move_across(&root.join("a"), &root.join("moved/a")).unwrap();
    assert!(!root.join("a").exists());
    assert_eq!("a", fs::read_to_string(root.join("moved/a")).unwrap());
    assert_eq!(1_000_000_000, modified(&root.join("moved/a")));
}

#[test]
fn test_move_across_never_overwrites() {
    let temp = test_temp_dir();
    let root = temp.path();
    let before = snapshot(root);
    for (src, dst) in [("a", "b"), ("a", "dir"), ("dir", "b"), ("dir/sub", "dir/x")] {
        let error = crate::apply::move_across(&root.join(src), &root.join(dst)).unwrap_err();
        assert_eq!(io::ErrorKind::AlreadyExists, error.kind(), "{src} -> {dst}");
    }
    assert_eq!(before, snapshot(root));
}

/// Failures that take permissions to make, which only Unix has (and root ignores)
#[cfg(unix)]
#[test]
fn test_move_across_fails_whole() {
    use std::os::unix::fs::PermissionsExt;
    let temp = test_temp_dir();
    let root = temp.path();
    let before = snapshot(root);
    let set_mode = |path: &str, mode| {
        fs::set_permissions(root.join(path), fs::Permissions::from_mode(mode)).unwrap()
    };

    // `dir/x` is copied, then `dir/sub/y` can not be read: what was copied is removed
    set_mode("dir/sub/y", 0o000);
    let error = crate::apply::move_across(&root.join("dir"), &root.join("moved")).unwrap_err();
    assert_eq!(io::ErrorKind::PermissionDenied, error.kind());
    set_mode("dir/sub/y", 0o644);
    assert_eq!(before, snapshot(root));

    // `dir/sub/y` is copied, but nothing can be removed from `dir/sub`: the copy is removed
    set_mode("dir/sub", 0o555);
    let error = crate::apply::move_across(&root.join("dir/sub/y"), &root.join("y")).unwrap_err();
    assert_eq!(io::ErrorKind::PermissionDenied, error.kind());
    set_mode("dir/sub", 0o755);
    assert_eq!(before, snapshot(root));

    // `dir/x` is removed, then `dir/sub/y` can not be: the copy stays, as it is all of it now
    set_mode("dir/sub", 0o555);
    let after = snapshot(&root.join("dir"));
    let error = crate::apply::move_across(&root.join("dir"), &root.join("moved")).unwrap_err();
    assert!(error.to_string().contains("was copied to"), "{error}");
    set_mode("dir/sub", 0o755);
    set_mode("moved/sub", 0o755);
    assert_eq!(after, snapshot(&root.join("moved")));
    assert!(root.join("dir/sub/y").exists());
}

/// A temp dir on another filesystem than [`test_temp_dir`]'s, which nothing there can be
/// renamed to: one in `KOIL_TEST_OTHER_FILESYSTEM`, in `/dev/shm` (memory, on Linux), or next
/// to the build (on CI's Windows, another drive than the temp dir's)
/// `None` if they are all on the same one, or not there
fn other_filesystem() -> Option<TempDir> {
    let temp = tempfile::tempdir().unwrap();
    let probe = temp.path().join("probe");
    let dirs = [
        std::env::var_os("KOIL_TEST_OTHER_FILESYSTEM").map(PathBuf::from),
        Some(PathBuf::from("/dev/shm")),
        Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("target")),
    ];
    (dirs.into_iter().flatten())
        .filter_map(|dir| tempfile::tempdir_in(dir).ok())
        .find(|other| {
            fs::write(&probe, "").unwrap();
            fs::rename(&probe, other.path().join("probe"))
                .is_err_and(|error| error.kind() == io::ErrorKind::CrossesDevices)
        })
}

#[test]
fn test_undo_move_to_another_filesystem() {
    let Some(other) = other_filesystem() else {
        return;
    };
    let temp = test_temp_dir();
    let times = set_old_times(temp.path());
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // remove `a` and `dir` here, and write them in the other dir
    let (a, dir) = (id(&koil, "a"), id(&koil, "dir"));
    koil.update(&[keep(&koil, "b")]).unwrap();
    koil.open(other.path()).unwrap();
    koil.update(&[with_id(a, "a"), with_id(dir, "moved/")])
        .unwrap();
    koil.apply().unwrap();
    assert_eq!(tree(&[("b", Some("b"))]), snapshot(temp.path()));
    let moved = [
        ("a", Some("a")),
        ("moved", None),
        ("moved/sub", None),
        ("moved/sub/y", Some("y")),
        ("moved/x", Some("x")),
    ];
    assert_eq!(tree(&moved), snapshot(other.path()));
    assert_eq!(1_000_000_000, modified(&other.path().join("a")));
    assert_eq!(1_100_000_000, modified(&other.path().join("moved/sub/y")));
    assert_eq!(1_400_000_000, modified(&other.path().join("moved")));

    // and back, as they were
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
    assert_eq!(tree(&[]), snapshot(other.path()));
    for (path, time) in times {
        assert_eq!(time, modified(&temp.path().join(path)), "{path}");
    }
}
