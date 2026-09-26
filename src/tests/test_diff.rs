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
    // open test_dir, to load all the IDs
    koil.open("test_dir").unwrap();
    // open the dir, to also load all of its IDs
    koil.open("dir").unwrap();
    // 000004 is ID that I loaded from `dir`
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

#[test]
fn test_copy_then_delete_original() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        >:000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n",
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

    koil.open("..").unwrap();
    // delete qwerty
    koil.update(
        "\
        >:000003 dir/\n\
        :000001 file\n\
        :000000 file2\n",
    )
    .unwrap();

    assert_eq!(diff([(2, "qwerty", &["dir/qwerty"])], []), koil.diff);
}

fn test_path(s: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test_dir")
        .join(s)
}

#[test]
fn test_parse_select_new_dir() {
    let parsed = parse::parse_listing(">newdir/\n:000001 file\n");
    assert_eq!(
        Some(parse::Selected::New("newdir/".into())),
        parsed.selected
    );
    assert_eq!(vec!["newdir/".to_string()], parsed.without_id);

    let parsed = parse::parse_listing(">:000003 dir/\n");
    assert_eq!(Some(parse::Selected::Id("000003".into())), parsed.selected);
    // a new dir, even if its name starts with something that looks like an ID
    let parsed = parse::parse_listing(">ab cd/\n");
    assert_eq!(Some(parse::Selected::New("ab cd/".into())), parsed.selected);
    assert_eq!(vec!["ab cd/".to_string()], parsed.without_id);

    let parsed = parse::parse_listing(">:ab cd/\n");
    assert_eq!(Some(parse::Selected::Id("ab".into())), parsed.selected);
    assert!(parsed.without_id.is_empty());
}

#[test]
fn test_enter_new_dir() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n\
        >newdir/\n",
    )
    .unwrap();
    assert_eq!(test_path("newdir"), koil.current_dir);
    assert_eq!("", koil.listing());
    assert_eq!(diff([], ["newdir/"]), koil.diff);

    // copy qwerty into the new dir
    koil.update(":000002 qwerty\n").unwrap();
    assert_eq!(":000002 qwerty", koil.listing());
    assert_eq!(
        diff([(2, "qwerty", &["qwerty", "newdir/qwerty"])], ["newdir/"]),
        koil.diff
    );

    // go back, and delete the original, so it becomes a move
    koil.open("..").unwrap();
    assert_eq!(
        ":000003 dir/\n:000001 file\n:000000 file2\n:000002 qwerty\nnewdir/",
        koil.listing()
    );
    koil.update(
        "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
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
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    // `made/up` does not exist, and is not in the listing, so go as far as possible
    let warning = koil
        .update(&format!(
            "===\n\
        {}\n\
        ===\n\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n",
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
    let listing = "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n\
        newdir/\n";
    let with_settings = |dir: &str| format!("===\n{}\n===\n{listing}", test_path(dir).display());

    // a new dir written in the same listing can be entered
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();

    assert_eq!(None, koil.update(&with_settings("newdir")).unwrap());
    assert_eq!(test_path("newdir"), koil.current_dir);
    assert_eq!(diff([], ["newdir/"]), koil.diff);

    // a new dir written in an earlier listing can be entered too,
    // but a made up dir inside it is not created
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(listing).unwrap();
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
    let listing = "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n";

    // enter the new dir right away
    let mut one_step = Koil::builder().show_settings(false).build();
    one_step.open("test_dir").unwrap();
    one_step
        .update(&format!(
            "\
        {listing}\
        >newdir/\n"
        ))
        .unwrap();

    // create the new dir first, then enter it
    let mut two_steps = Koil::builder().show_settings(false).build();
    two_steps.open("test_dir").unwrap();
    two_steps.update(&format!("{listing}newdir/\n")).unwrap();
    assert_eq!(format!("{listing}newdir/"), two_steps.listing());
    assert_eq!(diff([], ["newdir/"]), two_steps.diff);
    two_steps.update(&format!("{listing}>newdir/\n")).unwrap();

    assert_eq!(diff([], ["newdir/"]), one_step.diff);
    assert_eq!(diff([], ["newdir/"]), two_steps.diff);
    assert_eq!(one_step.current_dir, two_steps.current_dir);
    assert_eq!(one_step.listing(), two_steps.listing());
}

#[test]
fn test_enter_new_file_fails() {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    assert!(matches!(
        koil.update(">newfile\n"),
        Err(KoilError::NotADirectory(_))
    ));
}

// ── nested paths ─────────────────────────────────────────────────────────────

/// The unchanged `test_dir` listing
const TEST_DIR_LISTING: &str = "\
    :000003 dir/\n\
    :000001 file\n\
    :000000 file2\n\
    :000002 qwerty\n";

/// Open `test_dir`, and update it with the unchanged listing + `extra`
fn update_test_dir(extra: &str) -> Result<Koil, KoilError> {
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(&format!("{TEST_DIR_LISTING}{extra}"))?;
    Ok(koil)
}

#[test]
fn test_nested_create_with_parent() {
    let koil = update_test_dir(
        "newdir/\n\
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
        "newdir/A\n\
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
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    // move qwerty into newdir
    let listing = "\
        :000003 dir/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 newdir/qwerty\n";
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
    let mut koil = Koil::builder().show_settings(false).build();
    koil.open("test_dir").unwrap();
    koil.update(
        "\
        :000003 dir2/\n\
        :000001 file\n\
        :000000 file2\n\
        :000002 qwerty\n\
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
        update_test_dir("new\nnew/A"),
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
    for extra in ["new/\nnew\n", "new/A\nnew/A\n", "dir\n"] {
        assert!(
            matches!(update_test_dir(extra), Err(KoilError::DuplicatePath(_))),
            "{extra:?} should be a duplicate"
        );
    }
}
