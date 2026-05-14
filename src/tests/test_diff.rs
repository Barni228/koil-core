use super::*;
use diff::Diff;
use std::collections::HashMap;

fn diff<const A: usize, const B: usize>(
    with_id: [(usize, &str, &[&str]); A],
    without_id: [&str; B],
) -> Diff {
    let to_path = |s: &str| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test_dir")
            .join(s)
    };
    let with: HashMap<usize, (PathBuf, Vec<PathBuf>)> =
        HashMap::from_iter(with_id.into_iter().map(|(id, before, afters)| {
            (
                id,
                (
                    to_path(before),
                    afters.iter().map(|&p| to_path(p)).collect(),
                ),
            )
        }));

    let without: HashMap<PathBuf, bool> = HashMap::from_iter(without_id.into_iter().map(|s| {
        if let Some(stripped) = s.strip_suffix('/') {
            (to_path(stripped), true)
        } else {
            (to_path(s), false)
        }
    }));

    Diff {
        with_id: with,
        without_id: without,
    }
}
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

#[test]
fn test_generated_diff_no_change() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n",
    )
    .unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_generated_diff_add() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n\
        new",
    )
    .unwrap();
    assert_eq!(diff([], ["new"]), koil.diff);
}

#[test]
fn test_generated_diff_delete() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n",
    )
    .unwrap();
    assert_eq!(diff([(2, "qwerty", &[])], []), koil.diff);
}

#[test]
fn test_generated_diff_rename() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty1\n",
    )
    .unwrap();
    assert_eq!(diff([(2, "qwerty", &["qwerty1"])], []), koil.diff);
}

#[test]
fn test_generated_diff_copy() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n\
        :000002 qwerty2\n",
    )
    .unwrap();
    assert_eq!(diff([(2, "qwerty", &["qwerty", "qwerty2"])], []), koil.diff);
}

#[test]
fn test_generated_diff_undo() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        :000003 dir/\n\
        :000000 file3\n\
        :000002 poppy\n\
        :000002 another\n\
        new",
    )
    .unwrap();

    assert_eq!(
        diff(
            [
                (0, "file2", &["file3"]),
                (1, "file", &[]),
                (2, "qwerty", &["poppy", "another"])
            ],
            ["new"]
        ),
        koil.diff
    );
    // undo what I just did, and diff clears
    koil.update(
        "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n",
    )
    .unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_generated_diff_copy_cross_dir() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.open("dir").unwrap();
    koil.update(
        "\
        :000004 inside\n",
    )
    .unwrap();
    assert_eq!(Diff::default(), koil.diff);

    koil.update(
        "\
        :000004 inside\n\
        :000002 qwerty\n",
    )
    .unwrap();
    assert_eq!(
        diff([(2, "qwerty", &["qwerty", "dir/qwerty"])], []),
        koil.diff
    );

    koil.update(
        "\
        :000004 inside\n",
    )
    .unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

// Test to do weird stuff, like copy but delete original
