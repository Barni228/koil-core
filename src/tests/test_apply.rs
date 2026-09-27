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
    let mut koil = Koil::builder().show_settings(false).build();
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

/// The ID of `name` inside the open dir of `koil`
fn id(koil: &Koil, name: &str) -> String {
    let path = koil.current_dir.join(name);
    let index = koil.ids.iter().position(|p| p == &path).unwrap();
    koil.to_id(index)
}

/// Apply `listing` to a fresh [`test_temp_dir`], check it gives `after`, then undo it
/// and check that everything is as it was
fn check_undo(listing: impl Fn(&Koil) -> String, after: &[(&str, Option<&str>)]) {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    koil.update(&listing(&koil)).unwrap();
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
    check_undo(
        |k| {
            format!(
                "\
                :{} b\n",
                id(k, "b")
            )
        },
        &[("b", Some("b"))],
    );
}

#[test]
fn test_undo_rename() {
    check_undo(
        |k| {
            format!(
                "\
                :{} renamed\n\
                :{} b\n\
                :{} dir2/\n",
                id(k, "a"),
                id(k, "b"),
                id(k, "dir")
            )
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
            format!(
                "\
                :{} b\n\
                :{} a\n\
                :{} dir/\n",
                id(k, "a"),
                id(k, "b"),
                id(k, "dir")
            )
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
            format!(
                "\
                :{} a\n\
                :{} a2\n\
                :{} b\n\
                :{} dir/\n\
                :{} dir2/\n\
                new\n\
                new_dir/nested/file\n",
                id(k, "a"),
                id(k, "a"),
                id(k, "b"),
                id(k, "dir"),
                id(k, "dir")
            )
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
    koil.update(&format!(
        "\
        :{} b\n\
        >:{} dir/\n",
        id(&koil, "b"),
        id(&koil, "dir")
    ))
    .unwrap();
    koil.update(&format!(
        "\
        :{} sub/\n\
        :{} x\n\
        :{a} a\n",
        id(&koil, "sub"),
        id(&koil, "x")
    ))
    .unwrap();
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
    koil.update(&format!(
        "\
        :{} b\n\
        :{} dir/\n",
        id(&koil, "b"),
        id(&koil, "dir")
    ))
    .unwrap();
    koil.apply().unwrap();
    let deleted = snapshot(temp.path());
    // rename `b` to `a`, so the second apply depends on the first one
    koil.update(&format!(
        "\
        :{} a\n\
        :{} dir/\n",
        id(&koil, "b"),
        id(&koil, "dir")
    ))
    .unwrap();
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
    koil.update(&format!(
        "\
        :{} c\n\
        :{} dir/\n",
        id(&koil, "b"),
        id(&koil, "dir")
    ))
    .unwrap();
    koil.apply().unwrap();
    fs::write(temp.path().join("a"), "new a").unwrap();

    // `c` is renamed back to `b`, then restoring `a` fails
    let result = koil.undo();
    assert!(
        matches!(
            result,
            Err(KoilError::UndoFailed { done: 1, total: 2, ref source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists
        ),
        "{result:?}"
    );
    assert_eq!(
        Some("new a".to_string()),
        fs::read_to_string(temp.path().join("a")).ok()
    );
    assert!(temp.path().join("b").exists());

    // once the new `a` is gone, the step that failed can still be undone
    fs::remove_file(temp.path().join("a")).unwrap();
    koil.undo().unwrap();
    assert_eq!(before, snapshot(temp.path()));
    assert_eq!(None, koil.undo_steps().unwrap());
}

#[test]
fn test_undo_failed_apply() {
    let temp = test_temp_dir();
    let before = snapshot(temp.path());
    let mut koil = temp_koil(&temp);

    // delete `a` and create `new`, but `new` appears before it is applied
    koil.update(&format!(
        "\
        :{} b\n\
        :{} dir/\n\
        new\n",
        id(&koil, "b"),
        id(&koil, "dir")
    ))
    .unwrap();
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
