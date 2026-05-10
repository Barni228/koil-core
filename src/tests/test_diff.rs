use super::*;
use diff::Diff;

// ── delete ───────────────────────────────────────────────────────────────────

#[test]
fn test_diff_delete() {
    let mut diff = Diff::default();
    diff.add_before(0, "poppy".into());
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
    diff.add_before(0, "before".into());
    diff.push_after(0, "src/before".into());
    assert_eq!(vec![rename("before", "src/before")], diff.compute_actions())
}

#[test]
fn test_diff_copy() {
    let mut diff = Diff::default();
    diff.add_before(0, "file".into());
    diff.push_after(0, "file".into());
    diff.push_after(0, "file2".into());
    assert_eq!(vec![copy("file", "file2")], diff.compute_actions())
}

#[test]
fn test_diff_rename_and_copy() {
    let mut diff = Diff::default();
    diff.add_before(0, "A".into());
    diff.push_after(0, "B".into());
    diff.push_after(0, "C".into());
    assert_eq!(
        vec![copy("A", "B"), rename("A", "C")],
        diff.compute_actions()
    )
}
