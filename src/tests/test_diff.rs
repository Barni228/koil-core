use super::*;
use diff::Diff;

#[test]
fn test_diff_after_undo() {
    // what an undo that failed halfway ran: a rename back carries what is inside the dir,
    // and a trashed dir takes what was changed inside it
    let mut diff = Diff::default();
    diff.set_before(0, "b/x".into());
    diff.push_after(0, "b/y".into());
    diff.set_before(1, "new/z".into());
    diff.push_after(1, "w".into());
    diff.without_id.insert("b/n".into(), false);
    diff.without_id.insert("c".into(), true);
    diff.after_undo(&[
        Undo::Rename("b".into(), "a".into()),
        Undo::Trash("new".into()),
    ]);
    let with_id = HashMap::from([(0, ("a/x".into(), vec!["a/y".into()]))]);
    assert_eq!(with_id, diff.with_id);
    let without_id = HashMap::from([("a/n".into(), false), ("c".into(), true)]);
    assert_eq!(without_id, diff.without_id);
}

#[test]
fn test_diff_delete() {
    let mut diff = Diff::default();
    diff.set_before(0, "poppy".into());
    assert_eq!(vec![delete("poppy")], diff.compute_actions())
}

#[test]
fn test_diff_add() {
    let mut diff = Diff::default();
    diff.without_id.insert("new".into(), false);
    assert_eq!(vec![add("new")], diff.compute_actions())
}

#[test]
fn test_diff_move() {
    let mut diff = Diff::default();
    diff.set_before(0, "before".into());
    diff.push_after(0, "src/before".into());
    assert_eq!(vec![rename("before", "src/before")], diff.compute_actions())
}

#[test]
fn test_diff_copy() {
    let mut diff = Diff::default();
    diff.set_before(0, "file".into());
    diff.push_after(0, "file".into());
    diff.push_after(0, "file2".into());
    assert_eq!(vec![copy("file", "file2")], diff.compute_actions())
}

#[test]
fn test_diff_rename_and_copy() {
    let mut diff = Diff::default();
    diff.set_before(0, "A".into());
    diff.push_after(0, "B".into());
    diff.push_after(0, "C".into());
    assert_eq!(
        vec![copy("A", "B"), rename("A", "C")],
        diff.compute_actions()
    )
}

#[test]
fn test_diff_push_after_twice() {
    let mut diff = Diff::default();
    diff.set_before(0, "A".into());
    diff.push_after(0, "B".into());
    diff.push_after(0, "B".into());
    assert_eq!(vec![rename("A", "B")], diff.compute_actions())
}

#[test]
fn test_diff_creates_dir() {
    let mut diff = Diff::default();
    diff.without_id.insert("new".into(), true);
    diff.without_id.insert("file".into(), false);
    diff.without_id.insert("a/b/c".into(), false);
    diff.set_before(0, "A".into());
    diff.push_after(0, "moved/A".into());

    // created as a dir
    assert!(diff.creates_dir(Path::new("new")));
    // created as a file
    assert!(!diff.creates_dir(Path::new("file")));
    // something new is placed inside it
    assert!(diff.creates_dir(Path::new("a")));
    assert!(diff.creates_dir(Path::new("a/b")));
    assert!(diff.creates_dir(Path::new("moved")));
    // not in the diff at all
    assert!(!diff.creates_dir(Path::new("other")));
}
