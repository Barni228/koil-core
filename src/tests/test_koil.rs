use super::*;
use diff::Diff;
use std::collections::HashMap;

/// The unchanged `test_dir` listing
const TEST_DIR_LISTING: &str = "\
    :d0n6oe dir/\n\
    :52updl file\n\
    :tdffoi file2\n\
    :75ra32 qwerty\n";

/// Path to `s` inside `test_dir`
fn test_path(s: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test_dir")
        .join(s)
}

/// Koil with `test_dir` opened
fn test_koil() -> Koil {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil
}

/// Open `test_dir`, and update it with the unchanged listing + `extra`
fn update_test_dir(extra: &str) -> Result<Koil, KoilError> {
    let mut koil = test_koil();
    koil.update(&format!("{TEST_DIR_LISTING}{extra}"))?;
    Ok(koil)
}

fn diff<const A: usize, const B: usize>(
    with_id: [(usize, &str, &[&str]); A],
    without_id: [&str; B],
) -> Diff {
    let with: HashMap<usize, (PathBuf, Vec<PathBuf>)> =
        HashMap::from_iter(with_id.into_iter().map(|(id, before, afters)| {
            (
                id,
                (
                    test_path(before),
                    afters.iter().map(|&p| test_path(p)).collect(),
                ),
            )
        }));

    let without: HashMap<PathBuf, bool> = HashMap::from_iter(without_id.into_iter().map(|s| {
        if let Some(stripped) = s.strip_suffix('/') {
            (test_path(stripped), true)
        } else {
            (test_path(s), false)
        }
    }));

    Diff {
        with_id: with,
        without_id: without,
    }
}

#[test]
fn test_generated_diff_no_change() {
    let koil = update_test_dir("").unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_generated_diff_add() {
    let koil = update_test_dir("new").unwrap();
    assert_eq!(diff([], ["new"]), koil.diff);
}

#[test]
fn test_generated_diff_delete() {
    let mut koil = test_koil();
    koil.update(
        "\
        :d0n6oe dir/\n\
        :52updl file\n\
        :tdffoi file2\n",
    )
    .unwrap();
    assert_eq!(diff([(2, "qwerty", &[])], []), koil.diff);
}

#[test]
fn test_generated_diff_rename() {
    let mut koil = test_koil();
    koil.update(
        "\
        :d0n6oe dir/\n\
        :52updl file\n\
        :tdffoi file2\n\
        :75ra32 qwerty1\n",
    )
    .unwrap();
    assert_eq!(diff([(2, "qwerty", &["qwerty1"])], []), koil.diff);
}

#[test]
fn test_generated_diff_copy() {
    let mut koil = test_koil();
    koil.update(
        "\
        :d0n6oe dir/\n\
        :52updl file\n\
        :tdffoi file2\n\
        :75ra32 qwerty\n\
        :75ra32 qwerty2\n",
    )
    .unwrap();
    assert_eq!(diff([(2, "qwerty", &["qwerty", "qwerty2"])], []), koil.diff);
}

#[test]
fn test_generated_diff_undo() {
    let mut koil = test_koil();
    koil.update(
        "\
        :d0n6oe dir/\n\
        :tdffoi file3\n\
        :75ra32 poppy\n\
        :75ra32 another\n\
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
    koil.update(TEST_DIR_LISTING).unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_generated_diff_copy_cross_dir() {
    // open test_dir, to load all the IDs
    let mut koil = test_koil();
    // open the dir, to also load all of its IDs
    koil.open("dir").unwrap();
    // f0djx0 is ID that I loaded from `dir`
    koil.update(
        "\
        :f0djx0 inside\n",
    )
    .unwrap();
    assert_eq!(Diff::default(), koil.diff);

    koil.update(
        "\
        :f0djx0 inside\n\
        :75ra32 qwerty\n",
    )
    .unwrap();
    assert_eq!(
        diff([(2, "qwerty", &["qwerty", "dir/qwerty"])], []),
        koil.diff
    );

    koil.update(
        "\
        :f0djx0 inside\n",
    )
    .unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_copy_then_delete_original() {
    let mut koil = test_koil();
    koil.update(
        "\
        >:d0n6oe dir/\n\
        :52updl file\n\
        :tdffoi file2\n\
        :75ra32 qwerty\n",
    )
    .unwrap();
    assert_eq!(Diff::default(), koil.diff);

    koil.update(
        "\
        :f0djx0 inside\n\
        :75ra32 qwerty\n",
    )
    .unwrap();

    assert_eq!(
        diff([(2, "qwerty", &["qwerty", "dir/qwerty"])], []),
        koil.diff
    );

    koil.open("..").unwrap();
    // delete qwerty
    koil.update(
        "\
        >:d0n6oe dir/\n\
        :52updl file\n\
        :tdffoi file2\n",
    )
    .unwrap();

    assert_eq!(diff([(2, "qwerty", &["dir/qwerty"])], []), koil.diff);
}

#[test]
fn test_enter_new_dir() {
    let mut koil = update_test_dir(">newdir/\n").unwrap();
    assert_eq!(test_path("newdir"), koil.current_dir);
    assert_eq!("", koil.listing());
    assert_eq!(diff([], ["newdir/"]), koil.diff);

    // copy qwerty into the new dir
    koil.update(":75ra32 qwerty\n").unwrap();
    assert_eq!(":75ra32 qwerty", koil.listing());
    assert_eq!(
        diff([(2, "qwerty", &["qwerty", "newdir/qwerty"])], ["newdir/"]),
        koil.diff
    );

    // go back, and delete the original, so it becomes a move
    koil.open("..").unwrap();
    assert_eq!(format!("{TEST_DIR_LISTING}newdir/"), koil.listing());
    koil.update(
        "\
        :d0n6oe dir/\n\
        :52updl file\n\
        :tdffoi file2\n\
        newdir/\n",
    )
    .unwrap();
    assert_eq!(
        diff([(2, "qwerty", &["newdir/qwerty"])], ["newdir/"]),
        koil.diff
    );
}

#[test]
fn test_settings_made_up_dir() {
    let mut koil = test_koil();
    // `made/up` does not exist, and is not in the listing, so go as far as possible
    let warning = koil
        .update(&format!(
            "\
            ===\n\
            {}\n\
            ===\n\
            {TEST_DIR_LISTING}",
            test_path("dir/made/up").display()
        ))
        .unwrap();
    assert_eq!(
        Some(Warning::DirNotFound {
            requested: test_path("dir/made/up"),
            opened: test_path("dir"),
        }),
        warning
    );
    assert_eq!(test_path("dir"), koil.current_dir);
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_settings_enter_new_dir() {
    let listing = format!("{TEST_DIR_LISTING}newdir/\n");
    let with_settings = |dir: &str| {
        format!(
            "\
            ===\n\
            {}\n\
            ===\n\
            {listing}",
            test_path(dir).display()
        )
    };

    // a new dir written in the same listing can be entered
    let mut koil = test_koil();

    assert_eq!(None, koil.update(&with_settings("newdir")).unwrap());
    assert_eq!(test_path("newdir"), koil.current_dir);
    assert_eq!(diff([], ["newdir/"]), koil.diff);

    // a new dir written in an earlier listing can be entered too,
    // but a made up dir inside it is not created
    let mut koil = test_koil();
    koil.update(&listing).unwrap();
    assert_eq!(
        Some(Warning::DirNotFound {
            requested: test_path("newdir/made/up"),
            opened: test_path("newdir"),
        }),
        koil.update(&with_settings("newdir/made/up")).unwrap()
    );
    assert_eq!(test_path("newdir"), koil.current_dir);
    assert_eq!(diff([], ["newdir/"]), koil.diff);
}

#[test]
fn test_enter_new_dir_same_as_create_then_enter() {
    // enter the new dir right away
    let one_step = update_test_dir(">newdir/\n").unwrap();

    // create the new dir first, then enter it
    let mut two_steps = update_test_dir("newdir/\n").unwrap();
    assert_eq!(format!("{TEST_DIR_LISTING}newdir/"), two_steps.listing());
    assert_eq!(diff([], ["newdir/"]), two_steps.diff);
    two_steps
        .update(&format!("{TEST_DIR_LISTING}>newdir/\n"))
        .unwrap();

    assert_eq!(diff([], ["newdir/"]), one_step.diff);
    assert_eq!(diff([], ["newdir/"]), two_steps.diff);
    assert_eq!(one_step.current_dir, two_steps.current_dir);
    assert_eq!(one_step.listing(), two_steps.listing());
}

#[test]
fn test_enter_new_file_fails() {
    assert!(matches!(
        update_test_dir(">newfile\n"),
        Err(KoilError::NotADirectory(_))
    ));
}

// ── nested paths ─────────────────────────────────────────────────────────────

#[test]
fn test_nested_create_with_parent() {
    let koil = update_test_dir(
        "\
        newdir/\n\
        newdir/A",
    )
    .unwrap();
    assert_eq!(diff([], ["newdir/", "newdir/A"]), koil.diff);
}

#[test]
fn test_nested_create_without_parent() {
    // same as writing `newdir/` too
    let koil = update_test_dir("newdir/A").unwrap();
    assert_eq!(diff([], ["newdir/", "newdir/A"]), koil.diff);
}

#[test]
fn test_nested_create_many() {
    let mut koil = update_test_dir(
        "\
        newdir/A\n\
        newdir/B",
    )
    .unwrap();
    assert_eq!(diff([], ["newdir/", "newdir/A", "newdir/B"]), koil.diff);

    // `newdir/` is shown here, and its files are shown inside it
    assert_eq!(format!("{TEST_DIR_LISTING}newdir/"), koil.listing());
    koil.open("newdir").unwrap();
    let mut listing: Vec<_> = koil.listing().lines().map(str::to_string).collect();
    listing.sort();
    assert_eq!(vec!["A", "B"], listing);

    assert_eq!(
        vec![
            Action::CreateDir(test_path("newdir")),
            Action::CreateFile(test_path("newdir/A")),
            Action::CreateFile(test_path("newdir/B")),
        ],
        koil.compute_actions()
    );
}

#[test]
fn test_nested_create_deep() {
    let koil = update_test_dir("a/b/c/").unwrap();
    assert_eq!(diff([], ["a/", "a/b/", "a/b/c/"]), koil.diff);
}

#[test]
fn test_nested_create_in_existing_dir() {
    // `dir/` already exists, so only the file is created
    let koil = update_test_dir("dir/A").unwrap();
    assert_eq!(diff([], ["dir/A"]), koil.diff);
}

#[test]
fn test_nested_move() {
    let mut koil = test_koil();
    // move qwerty into newdir
    let listing = "\
        :d0n6oe dir/\n\
        :52updl file\n\
        :tdffoi file2\n\
        :75ra32 newdir/qwerty\n";
    koil.update(listing).unwrap();
    assert_eq!(
        diff([(2, "qwerty", &["newdir/qwerty"])], ["newdir/"]),
        koil.diff
    );

    // the same listing again does not change anything
    koil.update(listing).unwrap();
    assert_eq!(
        diff([(2, "qwerty", &["newdir/qwerty"])], ["newdir/"]),
        koil.diff
    );
}

#[test]
fn test_nested_create_in_renamed_dir() {
    let mut koil = test_koil();
    koil.update(
        "\
        :d0n6oe dir2/\n\
        :52updl file\n\
        :tdffoi file2\n\
        :75ra32 qwerty\n\
        dir2/A\n",
    )
    .unwrap();
    // `dir2/` comes from the rename, so it is not created
    assert_eq!(diff([(3, "dir", &["dir2"])], ["dir2/A"]), koil.diff);
}

#[test]
fn test_nested_create_inside_file_fails() {
    // `file` exists, and is not a dir
    assert!(matches!(
        update_test_dir("file/A"),
        Err(KoilError::NotADirectory(name)) if name == "file"
    ));
    // `new` is a new file, and is not a dir
    assert!(matches!(
        update_test_dir(
            "\
            new\n\
            new/A"
        ),
        Err(KoilError::NotADirectory(name)) if name == "new"
    ));
}

#[test]
fn test_invalid_names_fail() {
    for name in ["../A", "/A", "./A", "a/../b", "a/./b"] {
        assert!(
            matches!(
                update_test_dir(&format!("{name}\n")),
                Err(KoilError::InvalidName(_))
            ),
            "{name} should be invalid"
        );
    }
}

#[test]
fn test_nested_duplicates_fail() {
    let duplicates = [
        "\
        new/\n\
        new\n",
        "\
        new/A\n\
        new/A\n",
        // `dir/` is already in the listing
        "dir\n",
    ];
    for extra in duplicates {
        assert!(
            matches!(update_test_dir(extra), Err(KoilError::DuplicatePath(_))),
            "{extra:?} should be a duplicate"
        );
    }
}

#[test]
fn test_ids_round_trip() {
    let koil = test_koil();
    for index in 0..4 {
        assert_eq!(index, koil.id_to_index(&koil.to_id(index)).unwrap());
    }
}

#[test]
fn test_invalid_ids_fail() {
    // Typos of `d0n6oe`, an ID not in the listing, and the old hex format
    for id in ["d0n6of", "d0n6o", "d0n6oee", "D0N6OE", "f0djx0", "000003"] {
        assert!(
            matches!(
                update_test_dir(&format!(":{id} new\n")),
                Err(KoilError::InvalidID(_))
            ),
            "{id} should be invalid"
        );
    }
}

#[test]
fn test_refresh() {
    let mut koil = update_test_dir("new").unwrap();
    koil.refresh().unwrap();
    assert_eq!(Diff::default(), koil.diff);
    assert_eq!(TEST_DIR_LISTING.trim_end(), koil.listing());
}
