use super::*;
use diff::Diff;

/// Path to `s` inside `test_dir`
fn test_path(s: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test_dir")
        .join(s)
}

/// Koil with `test_dir` opened
fn test_koil() -> Koil {
    let mut koil = Koil::default();
    koil.open("test_dir").unwrap();
    koil
}

/// The unchanged `test_dir` listing
fn test_dir_listing(koil: &Koil) -> Vec<Entry> {
    vec![
        keep(koil, "dir/"),
        keep(koil, "file"),
        keep(koil, "file2"),
        keep(koil, "qwerty"),
    ]
}

/// Open `test_dir`, and update it with the unchanged listing + new entries `extra`
fn update_test_dir(extra: &[&str]) -> Result<Koil, UpdateError> {
    let mut koil = test_koil();
    let mut entries = test_dir_listing(&koil);
    entries.extend(extra.iter().map(|name| without_id(name)));
    koil.update(&entries)?;
    Ok(koil)
}

/// The ID of `s` inside `test_dir`
fn test_id(koil: &Koil, s: &str) -> Id {
    koil.id_of(&test_path(s)).unwrap()
}

/// A diff, with paths relative to `test_dir`
/// `with_id` has the path before and the paths after, and in `without_id` a trailing `/`
/// marks a dir
fn diff<const A: usize, const B: usize>(
    koil: &Koil,
    with_id: [(&str, &[&str]); A],
    without_id: [&str; B],
) -> Diff {
    Diff {
        with_id: with_id
            .into_iter()
            .map(|(before, afters)| {
                (
                    test_id(koil, before).0 as usize,
                    (
                        test_path(before),
                        afters.iter().map(|&p| test_path(p)).collect(),
                    ),
                )
            })
            .collect(),
        without_id: without_id
            .into_iter()
            .map(|s| {
                let (name, is_dir) = split_dir(s);
                (test_path(name.to_str().unwrap()), is_dir)
            })
            .collect(),
    }
}

/// The error of each entry, as (entry, kind)
fn errors(result: Result<Koil, UpdateError>) -> Vec<(usize, EntryErrorKind)> {
    let err = result.expect_err("the update should fail");
    err.errors.into_iter().map(|e| (e.entry, e.kind)).collect()
}

#[test]
fn test_generated_diff_no_change() {
    let koil = update_test_dir(&[]).unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_generated_diff_add() {
    let koil = update_test_dir(&["new"]).unwrap();
    assert_eq!(diff(&koil, [], ["new"]), koil.diff);
}

#[test]
fn test_generated_diff_delete() {
    let mut koil = test_koil();
    let entries = [
        keep(&koil, "dir/"),
        keep(&koil, "file"),
        keep(&koil, "file2"),
    ];
    koil.update(&entries).unwrap();
    assert_eq!(diff(&koil, [("qwerty", &[])], []), koil.diff);
}

#[test]
fn test_generated_diff_rename() {
    let mut koil = test_koil();
    let entries = [
        keep(&koil, "dir/"),
        keep(&koil, "file"),
        keep(&koil, "file2"),
        with_id(id(&koil, "qwerty"), "qwerty1"),
    ];
    koil.update(&entries).unwrap();
    assert_eq!(diff(&koil, [("qwerty", &["qwerty1"])], []), koil.diff);
}

#[test]
fn test_generated_diff_copy() {
    let mut koil = test_koil();
    let entries = [
        keep(&koil, "dir/"),
        keep(&koil, "file"),
        keep(&koil, "file2"),
        keep(&koil, "qwerty"),
        with_id(id(&koil, "qwerty"), "qwerty2"),
    ];
    koil.update(&entries).unwrap();
    assert_eq!(
        diff(&koil, [("qwerty", &["qwerty", "qwerty2"])], []),
        koil.diff
    );
}

#[test]
fn test_generated_diff_undo() {
    let mut koil = test_koil();
    let entries = [
        keep(&koil, "dir/"),
        with_id(id(&koil, "file2"), "file3"),
        with_id(id(&koil, "qwerty"), "poppy"),
        with_id(id(&koil, "qwerty"), "another"),
        without_id("new"),
    ];
    koil.update(&entries).unwrap();

    assert_eq!(
        diff(
            &koil,
            [
                ("file2", &["file3"]),
                ("file", &[]),
                ("qwerty", &["poppy", "another"])
            ],
            ["new"]
        ),
        koil.diff
    );
    // undo what I just did, and diff clears
    koil.update(&test_dir_listing(&koil)).unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_generated_diff_copy_cross_dir() {
    // open test_dir, to load all the IDs
    let mut koil = test_koil();
    let qwerty = id(&koil, "qwerty");
    // open the dir, to also load all of its IDs
    koil.open("dir").unwrap();
    koil.update(&[keep(&koil, "inside")]).unwrap();
    assert_eq!(Diff::default(), koil.diff);

    koil.update(&[keep(&koil, "inside"), with_id(qwerty, "qwerty")])
        .unwrap();
    assert_eq!(
        diff(&koil, [("qwerty", &["qwerty", "dir/qwerty"])], []),
        koil.diff
    );

    koil.update(&[keep(&koil, "inside")]).unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_copy_then_delete_original() {
    let mut koil = test_koil();
    let qwerty = id(&koil, "qwerty");
    koil.open("dir").unwrap();
    koil.update(&[keep(&koil, "inside"), with_id(qwerty, "qwerty")])
        .unwrap();
    assert_eq!(
        diff(&koil, [("qwerty", &["qwerty", "dir/qwerty"])], []),
        koil.diff
    );

    koil.open("..").unwrap();
    // delete qwerty
    let entries = [
        keep(&koil, "dir/"),
        keep(&koil, "file"),
        keep(&koil, "file2"),
    ];
    koil.update(&entries).unwrap();

    assert_eq!(diff(&koil, [("qwerty", &["dir/qwerty"])], []), koil.diff);
}

#[test]
fn test_enter_new_dir() {
    let mut koil = update_test_dir(&["newdir/"]).unwrap();
    let qwerty = id(&koil, "qwerty");
    assert_eq!(None, koil.open("newdir").unwrap());
    assert_eq!(test_path("newdir"), koil.current_dir);
    assert_eq!(Vec::<Entry>::new(), koil.listing());
    assert_eq!(diff(&koil, [], ["newdir/"]), koil.diff);

    // copy qwerty into the new dir
    koil.update(&[with_id(qwerty, "qwerty")]).unwrap();
    assert_eq!(vec![with_id(qwerty, "qwerty")], koil.listing());
    assert_eq!(
        diff(
            &koil,
            [("qwerty", &["qwerty", "newdir/qwerty"])],
            ["newdir/"]
        ),
        koil.diff
    );

    // go back, and delete the original, so it becomes a move
    koil.open("..").unwrap();
    let mut listing = test_dir_listing(&koil);
    listing.push(without_id("newdir/"));
    assert_eq!(listing, koil.listing());
    let entries = [
        keep(&koil, "dir/"),
        keep(&koil, "file"),
        keep(&koil, "file2"),
        without_id("newdir/"),
    ];
    koil.update(&entries).unwrap();
    assert_eq!(
        diff(&koil, [("qwerty", &["newdir/qwerty"])], ["newdir/"]),
        koil.diff
    );
}

#[test]
fn test_open_made_up_dir() {
    let mut koil = test_koil();
    // `made/up` does not exist, and is not in the listing, so go as far as possible
    assert_eq!(
        Some(Warning::DirNotFound {
            requested: test_path("dir/made/up"),
            opened: test_path("dir"),
        }),
        koil.open(test_path("dir/made/up")).unwrap()
    );
    assert_eq!(test_path("dir"), koil.current_dir);
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_open_made_up_dir_in_new_dir() {
    // a new dir can be entered, but a made up dir inside it is not created
    let mut koil = update_test_dir(&["newdir/"]).unwrap();
    assert_eq!(
        Some(Warning::DirNotFound {
            requested: test_path("newdir/made/up"),
            opened: test_path("newdir"),
        }),
        koil.open(test_path("newdir/made/up")).unwrap()
    );
    assert_eq!(test_path("newdir"), koil.current_dir);
    assert_eq!(diff(&koil, [], ["newdir/"]), koil.diff);
}

#[test]
fn test_open_new_file() {
    // a new file is not a dir, so the dir it is in is opened
    let mut koil = update_test_dir(&["newfile"]).unwrap();
    assert_eq!(
        Some(Warning::DirNotFound {
            requested: test_path("newfile"),
            opened: test_path(""),
        }),
        koil.open("newfile").unwrap()
    );
}

// ── nested paths ─────────────────────────────────────────────────────────────

#[test]
fn test_nested_create_with_parent() {
    let koil = update_test_dir(&["newdir/", "newdir/A"]).unwrap();
    assert_eq!(diff(&koil, [], ["newdir/", "newdir/A"]), koil.diff);
}

#[test]
fn test_nested_create_without_parent() {
    // `newdir/` is created too, and shown, but it is not written in the diff
    let koil = update_test_dir(&["newdir/A"]).unwrap();
    assert_eq!(diff(&koil, [], ["newdir/A"]), koil.diff);
    let mut listing = test_dir_listing(&koil);
    listing.push(without_id("newdir/"));
    assert_eq!(listing, koil.listing());
    assert_eq!(
        vec![
            Action::CreateDir(test_path("newdir")),
            Action::CreateFile(test_path("newdir/A")),
        ],
        koil.compute_actions()
    );
}

#[test]
fn test_nested_create_removed_with_parent() {
    let mut koil = update_test_dir(&["newdir/A"]).unwrap();
    koil.open("newdir").unwrap();
    // without `A`, nothing is inside `newdir`, so it is not created
    koil.update(&[]).unwrap();
    assert_eq!(Vec::<Action>::new(), koil.compute_actions());
    koil.open("..").unwrap();
    assert_eq!(test_dir_listing(&koil), koil.listing());
}

#[test]
fn test_nested_create_many() {
    let mut koil = update_test_dir(&["newdir/A", "newdir/B"]).unwrap();
    assert_eq!(diff(&koil, [], ["newdir/A", "newdir/B"]), koil.diff);

    // `newdir/` is shown here, and its files are shown inside it
    let mut listing = test_dir_listing(&koil);
    listing.push(without_id("newdir/"));
    assert_eq!(listing, koil.listing());
    koil.open("newdir").unwrap();
    assert_eq!(vec![without_id("A"), without_id("B")], koil.listing());

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
    let koil = update_test_dir(&["a/b/c/"]).unwrap();
    assert_eq!(diff(&koil, [], ["a/b/c/"]), koil.diff);
    assert_eq!(
        vec![
            Action::CreateDir(test_path("a")),
            Action::CreateDir(test_path("a/b")),
            Action::CreateDir(test_path("a/b/c")),
        ],
        koil.compute_actions()
    );
}

#[test]
fn test_nested_create_in_existing_dir() {
    // `dir/` already exists, so only the file is created
    let koil = update_test_dir(&["dir/A"]).unwrap();
    assert_eq!(diff(&koil, [], ["dir/A"]), koil.diff);
}

#[test]
fn test_nested_move() {
    let mut koil = test_koil();
    // move qwerty into newdir
    let entries = [
        keep(&koil, "dir/"),
        keep(&koil, "file"),
        keep(&koil, "file2"),
        with_id(id(&koil, "qwerty"), "newdir/qwerty"),
    ];
    koil.update(&entries).unwrap();
    assert_eq!(diff(&koil, [("qwerty", &["newdir/qwerty"])], []), koil.diff);
    assert_eq!(
        vec![
            Action::CreateDir(test_path("newdir")),
            Action::Rename(test_path("qwerty"), test_path("newdir/qwerty")),
        ],
        koil.compute_actions()
    );

    // the same entries again do not change anything
    koil.update(&entries).unwrap();
    assert_eq!(diff(&koil, [("qwerty", &["newdir/qwerty"])], []), koil.diff);
}

#[test]
fn test_nested_create_in_renamed_dir() {
    let mut koil = test_koil();
    let entries = [
        with_id(id(&koil, "dir"), "dir2/"),
        keep(&koil, "file"),
        keep(&koil, "file2"),
        keep(&koil, "qwerty"),
        without_id("dir2/A"),
    ];
    koil.update(&entries).unwrap();
    // `dir2/` comes from the rename, so it is not created
    assert_eq!(diff(&koil, [("dir", &["dir2"])], ["dir2/A"]), koil.diff);
}

#[test]
fn test_nested_create_inside_file_fails() {
    // `file` exists, and is not a dir
    assert_eq!(
        vec![(4, EntryErrorKind::NotADirectory("file".into()))],
        errors(update_test_dir(&["file/A"]))
    );
    // `new` is a new file, and is not a dir
    assert_eq!(
        vec![(5, EntryErrorKind::NotADirectory("new".into()))],
        errors(update_test_dir(&["new", "new/A"]))
    );
}

#[test]
fn test_invalid_names_fail() {
    for name in ["../A", "/A", "./A", "a/../b", "a/./b", ""] {
        assert_eq!(
            vec![(4, EntryErrorKind::InvalidName(name.into()))],
            errors(update_test_dir(&[name])),
            "{name} should be invalid"
        );
    }
}

#[test]
fn test_nested_duplicates_fail() {
    let duplicate = |path: &str, first| EntryErrorKind::Duplicate {
        path: path.into(),
        first,
    };
    assert_eq!(
        vec![(5, duplicate("new", 4))],
        errors(update_test_dir(&["new/", "new"]))
    );
    assert_eq!(
        vec![(5, duplicate("new/A", 4))],
        errors(update_test_dir(&["new/A", "new/A"]))
    );
    // `dir/` is already the first entry
    assert_eq!(
        vec![(4, duplicate("dir", 0))],
        errors(update_test_dir(&["dir"]))
    );
}

#[test]
fn test_unknown_ids_fail() {
    // `dir/inside` has an ID only once `dir` is opened
    for unknown in [Id(4), Id(999), Id(u64::MAX)] {
        let mut koil = test_koil();
        let mut entries = test_dir_listing(&koil);
        entries.push(with_id(unknown, "new"));
        assert_eq!(
            vec![(4, EntryErrorKind::UnknownId(unknown))],
            errors(koil.update(&entries).map(|()| koil.clone()))
        );
    }
}

#[test]
fn test_every_error_is_reported() {
    let mut koil = test_koil();
    let entries = [
        keep(&koil, "dir/"),
        with_id(Id(999), "x"),
        without_id("../a"),
        without_id("dir"),
        keep(&koil, "file"),
        without_id("file/B"),
        keep(&koil, "qwerty"),
    ];
    assert_eq!(
        vec![
            (1, EntryErrorKind::UnknownId(Id(999))),
            (2, EntryErrorKind::InvalidName("../a".into())),
            (
                3,
                EntryErrorKind::Duplicate {
                    path: "dir".into(),
                    first: 0
                }
            ),
            (5, EntryErrorKind::NotADirectory("file".into())),
        ],
        errors(koil.update(&entries).map(|()| koil.clone()))
    );
}

#[test]
fn test_listing_ids_point_to_paths() {
    let koil = test_koil();
    for entry in koil.listing() {
        let path = koil.path_of(entry.id.unwrap()).unwrap();
        assert_eq!(test_path(entry.name.to_str().unwrap()), path);
        assert_eq!(entry.id, koil.id_of(path));
    }
    assert_eq!(None, koil.path_of(Id(999)));
    assert_eq!(None, koil.id_of(&test_path("made/up")));
}

#[test]
fn test_path_of_renamed_is_the_original() {
    let mut koil = test_koil();
    let qwerty = id(&koil, "qwerty");
    let entries = [
        keep(&koil, "dir/"),
        keep(&koil, "file"),
        keep(&koil, "file2"),
        with_id(qwerty, "renamed"),
    ];
    koil.update(&entries).unwrap();
    assert!(koil.listing().contains(&with_id(qwerty, "renamed")));
    assert_eq!(Some(test_path("qwerty").as_path()), koil.path_of(qwerty));
}

// ── hidden entries ───────────────────────────────────────────────────────────

/// Koil with `test_dir` opened, and hidden entries shown
fn hidden_koil() -> Koil {
    let mut koil = Koil::builder()
        .settings(Settings {
            show_hidden: true,
            ..Settings::default()
        })
        .build();
    koil.open("test_dir").unwrap();
    koil
}

/// The unchanged `test_dir` listing, with hidden entries shown
fn hidden_listing(koil: &Koil) -> Vec<Entry> {
    vec![
        Entry::parent(),
        keep(koil, "dir/"),
        keep(koil, ".hidden"),
        keep(koil, "file"),
        keep(koil, "file2"),
        keep(koil, "qwerty"),
    ]
}

#[test]
fn test_hidden_not_shown() {
    // `test_dir/.hidden` is not in the listing, so it is not deleted either
    let koil = update_test_dir(&[]).unwrap();
    assert_eq!(test_dir_listing(&koil), koil.listing());
    assert_eq!(None, koil.id_of(&test_path(".hidden")));
    assert_eq!(Vec::<Action>::new(), koil.compute_actions());
}

#[test]
fn test_show_hidden() {
    let mut koil = hidden_koil();
    assert_eq!(hidden_listing(&koil), koil.listing());
    koil.update(&hidden_listing(&koil)).unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_set_show_hidden() {
    let mut koil = test_koil();
    assert_eq!(
        None,
        koil.set_settings(Settings {
            show_hidden: true,
            ..Settings::default()
        })
        .unwrap()
    );
    assert_eq!(hidden_listing(&koil), koil.listing());
    koil.set_settings(Settings::default()).unwrap();
    assert_eq!(test_dir_listing(&koil), koil.listing());
}

#[test]
fn test_parent_entry_is_ignored() {
    let mut koil = hidden_koil();
    // without `..`
    koil.update(&hidden_listing(&koil)[1..]).unwrap();
    assert_eq!(Diff::default(), koil.diff);
    // with `..` written twice
    let mut entries = hidden_listing(&koil);
    entries.push(Entry::parent());
    koil.update(&entries).unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_no_parent_entry_in_root() {
    let mut koil = hidden_koil();
    koil.open("/").unwrap();
    assert!(!koil.listing().contains(&Entry::parent()));
}

#[test]
fn test_hide_renamed_hidden() {
    let mut koil = hidden_koil();
    let hidden = id(&koil, ".hidden");
    let mut entries = hidden_listing(&koil);
    entries[2] = with_id(hidden, ".renamed");
    koil.update(&entries).unwrap();

    // the renamed entry is still shown, so updating keeps it a rename
    koil.set_settings(Settings::default()).unwrap();
    let mut listing = test_dir_listing(&koil);
    listing.insert(1, with_id(hidden, ".renamed"));
    assert_eq!(listing, koil.listing());
    koil.update(&listing).unwrap();
    assert_eq!(diff(&koil, [(".hidden", &[".renamed"])], []), koil.diff);
}

#[test]
fn test_hide_deleted_hidden() {
    let mut koil = hidden_koil();
    let mut entries = hidden_listing(&koil);
    entries.remove(2);
    koil.update(&entries).unwrap();

    // the deleted entry is not shown, and updating keeps it deleted
    koil.set_settings(Settings::default()).unwrap();
    assert_eq!(test_dir_listing(&koil), koil.listing());
    koil.update(&test_dir_listing(&koil)).unwrap();
    assert_eq!(diff(&koil, [(".hidden", &[])], []), koil.diff);
}

#[test]
fn test_new_hidden_is_shown() {
    // user wrote it, so it is shown even if hidden entries are not
    let koil = update_test_dir(&[".new"]).unwrap();
    let mut listing = test_dir_listing(&koil);
    listing.push(without_id(".new"));
    assert_eq!(listing, koil.listing());
}

#[test]
fn test_settings_are_saved() {
    let koil = hidden_koil();
    let loaded = Koil::load_state(&koil.save_state()).unwrap();
    assert_eq!(
        &Settings {
            show_hidden: true,
            ..Settings::default()
        },
        loaded.settings()
    );
}

// ── globs ────────────────────────────────────────────────────────────────────

/// Koil with the glob `glob` opened inside `test_dir`
fn glob_koil(glob: &str) -> Koil {
    let mut koil = test_koil();
    assert_eq!(None, koil.open(glob).unwrap());
    koil
}

/// Every entry of `names` in the open dir, unchanged
fn keep_all(koil: &Koil, names: &[&str]) -> Vec<Entry> {
    names.iter().map(|name| keep(koil, name)).collect()
}

#[test]
fn test_glob_open() {
    let koil = glob_koil("**/*");
    assert_eq!(test_path(""), koil.current_dir());
    assert_eq!(Some("**/*"), koil.pattern().map(Pattern::as_str));
    assert_eq!(test_path("**/*"), koil.location());
    // only files, and names are relative to the base dir
    assert_eq!(
        keep_all(&koil, &["dir/inside", "file", "file2", "qwerty"]),
        koil.listing()
    );
}

#[test]
fn test_glob_patterns() {
    let glob_names = |glob: &str| -> Vec<PathBuf> {
        let koil = glob_koil(glob);
        koil.listing().into_iter().map(|e| e.name).collect()
    };
    // `*` does not match `/`
    assert_eq!(
        vec![PathBuf::from("file"), "file2".into(), "qwerty".into()],
        glob_names("*")
    );
    assert_eq!(vec![PathBuf::from("dir/inside")], glob_names("d*/*"));
    assert_eq!(
        vec![PathBuf::from("file"), "file2".into()],
        glob_names("{file,file2}")
    );
    assert_eq!(vec![PathBuf::from("file2")], glob_names("file?"));
    assert_eq!(Vec::<PathBuf>::new(), glob_names("*.rs"));
}

#[test]
fn test_glob_absolute() {
    let mut koil = Koil::default();
    koil.open(test_path("*2")).unwrap();
    assert_eq!(test_path(""), koil.current_dir());
    assert_eq!(keep_all(&koil, &["file2"]), koil.listing());
}

#[test]
fn test_glob_unchanged() {
    let mut koil = glob_koil("**/*");
    koil.update(&koil.listing()).unwrap();
    assert_eq!(Diff::default(), koil.diff);
}

#[test]
fn test_glob_edit() {
    let mut koil = glob_koil("**/*");
    let entries = [
        with_id(id(&koil, "dir/inside"), "dir/inside2"),
        with_id(id(&koil, "file"), "dir/file"),
        keep(&koil, "file2"),
        without_id("new/x"),
    ];
    koil.update(&entries).unwrap();
    assert_eq!(
        diff(
            &koil,
            [
                ("dir/inside", &["dir/inside2"]),
                ("file", &["dir/file"]),
                ("qwerty", &[])
            ],
            ["new/x"]
        ),
        koil.diff
    );
    // every file is still shown where it was written, and `new/` is not, since it is a dir
    let mut listing = entries.to_vec();
    listing.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(listing, koil.listing());
}

#[test]
fn test_glob_rename_out_of_view() {
    let mut koil = glob_koil("file*");
    let file2 = id(&koil, "file2");
    koil.update(&[keep(&koil, "file"), with_id(file2, "other")])
        .unwrap();
    // `other` does not match, so it is not shown, but it is still renamed
    assert_eq!(keep_all(&koil, &["file"]), koil.listing());
    koil.update(&koil.listing()).unwrap();
    assert_eq!(diff(&koil, [("file2", &["other"])], []), koil.diff);
}

#[test]
fn test_glob_edit_seen_in_dir() {
    let mut koil = glob_koil("**/*");
    let inside = id(&koil, "dir/inside");
    let mut entries = koil.listing();
    entries[0] = with_id(inside, "dir/renamed");
    koil.update(&entries).unwrap();

    koil.open(test_path("dir")).unwrap();
    assert_eq!(None, koil.pattern().map(Pattern::as_str));
    assert_eq!(vec![with_id(inside, "renamed")], koil.listing());
}

#[test]
fn test_glob_hidden() {
    let mut koil = hidden_koil();
    koil.open("**/*").unwrap();
    let mut listing = vec![Entry::parent()];
    listing.extend(keep_all(
        &koil,
        &[".hidden", "dir/inside", "file", "file2", "qwerty"],
    ));
    assert_eq!(listing, koil.listing());
}

#[test]
fn test_glob_in_new_dir() {
    let mut koil = update_test_dir(&["newdir/A", "newdir/B/"]).unwrap();
    koil.open("newdir/*").unwrap();
    assert_eq!(test_path("newdir"), koil.current_dir());
    assert_eq!(vec![without_id("A")], koil.listing());
}

#[test]
fn test_glob_missing_base() {
    let mut koil = test_koil();
    assert_eq!(
        Some(Warning::DirNotFound {
            requested: test_path("made/up"),
            opened: test_path(""),
        }),
        koil.open("made/up/*").unwrap()
    );
    assert_eq!(None, koil.pattern().map(Pattern::as_str));
    assert_eq!(test_dir_listing(&koil), koil.listing());
}

#[test]
fn test_glob_invalid() {
    let mut koil = test_koil();
    for glob in ["[", "{a"] {
        assert!(
            matches!(koil.open(glob), Err(OpenError::InvalidGlob { glob: g, .. }) if g == glob),
            "{glob} should be invalid"
        );
    }
    // nothing changed
    assert_eq!(test_dir_listing(&koil), koil.listing());
}

#[test]
fn test_glob_like_dir_name() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("a[1]")).unwrap();
    fs::write(temp.path().join("a[1]/x"), "").unwrap();
    let mut koil = Koil::default();

    // an existing dir is never a glob
    koil.open(temp.path().join("a[1]")).unwrap();
    assert_eq!(None, koil.pattern().map(Pattern::as_str));
    koil.open(temp.path().join("a[1]/*")).unwrap();
    assert_eq!(Some("*"), koil.pattern().map(Pattern::as_str));
    assert_eq!(keep_all(&koil, &["x"]), koil.listing());
}

#[test]
fn test_glob_kept_on_refresh() {
    let mut koil = glob_koil("*");
    koil.refresh().unwrap();
    assert_eq!(Some("*"), koil.pattern().map(Pattern::as_str));
    let loaded = Koil::load_state(&koil.save_state()).unwrap();
    assert_eq!(Some("*"), loaded.pattern().map(Pattern::as_str));
}

// ── regex ────────────────────────────────────────────────────────────────────

fn regex() -> Settings {
    Settings {
        regex: true,
        ..Settings::default()
    }
}

/// Koil with the regex `pattern` opened inside `test_dir`
fn regex_koil(pattern: &str) -> Koil {
    let mut koil = Koil::builder().settings(regex()).build();
    koil.open("test_dir").unwrap();
    assert_eq!(None, koil.open(pattern).unwrap());
    koil
}

#[test]
fn test_regex_open() {
    let koil = regex_koil(".*");
    assert_eq!(test_path(""), koil.current_dir());
    assert_eq!(Some(&Pattern::Regex(".*".into())), koil.pattern());
    assert_eq!(test_path(".*"), koil.location());
    // `.*` matches `/` too, but still only files are shown
    assert_eq!(
        keep_all(&koil, &["dir/inside", "file", "file2", "qwerty"]),
        koil.listing()
    );
}

#[test]
fn test_regex_patterns() {
    let regex_names = |regex: &str| -> Vec<PathBuf> {
        let koil = regex_koil(regex);
        koil.listing().into_iter().map(|e| e.name).collect()
    };
    assert_eq!(
        vec![PathBuf::from("file"), "file2".into(), "qwerty".into()],
        regex_names("[^/]*")
    );
    assert_eq!(
        vec![PathBuf::from("file"), "file2".into()],
        regex_names(r"file\d?")
    );
    assert_eq!(
        vec![PathBuf::from("file"), "qwerty".into()],
        regex_names("(file|qwerty)")
    );
    assert_eq!(vec![PathBuf::from("dir/inside")], regex_names("d.*"));
    // the whole path must match
    assert_eq!(Vec::<PathBuf>::new(), regex_names("il."));
}

#[test]
fn test_expand_commas() {
    let cases = [
        (r",*\.rs", r"[^/]*\.rs"),
        // escaped
        (r"a\,b", r"a\,b"),
        (r"a\\,b", r"a\\[^/]b"),
        // repetitions and classes keep their `,`
        ("a{1,2},", "a{1,2}[^/]"),
        ("[,a],", "[,a][^/]"),
        ("[]],", "[]][^/]"),
        ("[^],],", "[^],][^/]"),
        ("[[:alpha:],],", "[[:alpha:],][^/]"),
    ];
    for (regex, expanded) in cases {
        assert_eq!(expanded, expand_commas(regex), "{regex}");
    }
}

#[test]
fn test_regex_commas() {
    let regex_names = |regex: &str| -> Vec<PathBuf> {
        let koil = regex_koil(regex);
        koil.listing().into_iter().map(|e| e.name).collect()
    };
    // `,` never matches `/`
    assert_eq!(
        vec![PathBuf::from("file"), "file2".into(), "qwerty".into()],
        regex_names(",*")
    );
    assert_eq!(vec![PathBuf::from("dir/inside")], regex_names("d,*/,*"));
    assert_eq!(Vec::<PathBuf>::new(), regex_names("d,*"));

    // `\,` is a `,`
    let temp = tempfile::tempdir().unwrap();
    for file in ["a,b", "axb"] {
        fs::write(temp.path().join(file), "").unwrap();
    }
    let mut koil = Koil::builder().settings(regex()).build();
    let names =
        |koil: &Koil| -> Vec<PathBuf> { koil.listing().into_iter().map(|e| e.name).collect() };
    koil.open(temp.path().join(r"a\,b")).unwrap();
    assert_eq!(vec![PathBuf::from("a,b")], names(&koil));
    koil.open(temp.path().join("a,b")).unwrap();
    assert_eq!(vec![PathBuf::from("a,b"), "axb".into()], names(&koil));
}

#[test]
fn test_regex_edit() {
    let mut koil = regex_koil("file.*");
    let file2 = id(&koil, "file2");
    koil.update(&[keep(&koil, "file"), with_id(file2, "dir/file2")])
        .unwrap();
    assert_eq!(diff(&koil, [("file2", &["dir/file2"])], []), koil.diff);
}

#[test]
fn test_regex_invalid() {
    let mut koil = Koil::builder().settings(regex()).build();
    koil.open("test_dir").unwrap();
    assert!(matches!(
        koil.open("(file"),
        Err(OpenError::InvalidRegex { regex, .. }) if regex == "(file"
    ));
    // nothing changed
    assert_eq!(None, koil.pattern());
    assert_eq!(test_dir_listing(&koil), koil.listing());
}

#[test]
fn test_regex_dirs() {
    let mut koil = Koil::builder().settings(regex()).build();
    koil.open("test_dir").unwrap();
    // a path without special characters is a dir
    assert_eq!(None, koil.open("dir").unwrap());
    assert_eq!(None, koil.pattern());
    assert_eq!(
        Some(Warning::DirNotFound {
            requested: test_path("dir/made/up"),
            opened: test_path("dir"),
        }),
        koil.open("made/up").unwrap()
    );

    // an existing dir is never a regex, even with a `.` in its name
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("v1.0")).unwrap();
    fs::write(temp.path().join("v1.0/x"), "").unwrap();
    koil.open(temp.path().join("v1.0")).unwrap();
    assert_eq!(None, koil.pattern());
    koil.open(temp.path().join("v1.0/.*")).unwrap();
    assert_eq!(keep_all(&koil, &["x"]), koil.listing());
}

#[test]
fn test_regex_setting_keeps_open_pattern() {
    let mut koil = glob_koil("**/*");
    koil.set_settings(regex()).unwrap();
    // the open glob stays a glob
    assert_eq!(Some(&Pattern::Glob("**/*".into())), koil.pattern());
    assert_eq!(
        keep_all(&koil, &["dir/inside", "file", "file2", "qwerty"]),
        koil.listing()
    );
    // but opening it again reads it as a regex
    assert!(matches!(
        koil.open(koil.location()),
        Err(OpenError::InvalidRegex { .. })
    ));
}

// ── gitignore ────────────────────────────────────────────────────────────────

/// A temp dir, with a `.gitignore` that ignores `target/` and `*.log`, and:
/// `a.log`, `file`, `src/main.rs`, `src/debug.log`, `target/out`
/// It is a git repo, only if `git` is true
fn gitignore_temp_dir(git: bool) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    if git {
        fs::create_dir(root.join(".git")).unwrap();
    }
    fs::create_dir(root.join("src")).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(
        root.join(".gitignore"),
        "\
        target/\n\
        *.log\n",
    )
    .unwrap();
    for file in [
        "a.log",
        "file",
        "src/main.rs",
        "src/debug.log",
        "target/out",
    ] {
        fs::write(root.join(file), "").unwrap();
    }
    temp
}

/// Koil with `location` opened inside `temp`
fn temp_koil(temp: &tempfile::TempDir, settings: Settings, location: &str) -> Koil {
    let mut koil = Koil::builder().settings(settings).build();
    assert_eq!(None, koil.open(temp.path().join(location)).unwrap());
    koil
}

/// The names in the listing, a trailing `/` marks a dir
fn names(koil: &Koil) -> Vec<String> {
    let name = |e: Entry| {
        let slash = if e.is_dir { "/" } else { "" };
        format!("{}{slash}", e.name.display())
    };
    koil.listing().into_iter().map(name).collect()
}

fn gitignore() -> Settings {
    Settings {
        respect_gitignore: true,
        ..Settings::default()
    }
}

#[test]
fn test_gitignore_off_by_default() {
    let temp = gitignore_temp_dir(true);
    let koil = temp_koil(&temp, Settings::default(), "");
    assert_eq!(vec!["src/", "target/", "a.log", "file"], names(&koil));
    let koil = temp_koil(&temp, Settings::default(), "**/*");
    assert_eq!(
        vec![
            "a.log",
            "file",
            "src/debug.log",
            "src/main.rs",
            "target/out"
        ],
        names(&koil)
    );
}

#[test]
fn test_gitignore() {
    let temp = gitignore_temp_dir(true);
    let koil = temp_koil(&temp, gitignore(), "");
    assert_eq!(vec!["src/", "file"], names(&koil));
    let koil = temp_koil(&temp, gitignore(), "**/*");
    assert_eq!(vec!["file", "src/main.rs"], names(&koil));
    // the `.gitignore` of the repo root is used in its subdirs too
    let koil = temp_koil(&temp, gitignore(), "src");
    assert_eq!(vec!["main.rs"], names(&koil));
}

#[test]
fn test_gitignore_hides_git_dir() {
    let temp = gitignore_temp_dir(true);
    let settings = Settings {
        show_hidden: true,
        respect_gitignore: true,
        ..Settings::default()
    };
    let koil = temp_koil(&temp, settings, "");
    assert_eq!(vec!["../", "src/", ".gitignore", "file"], names(&koil));
}

#[test]
fn test_gitignore_outside_repo() {
    // like git, `.gitignore` does nothing outside a repo
    let temp = gitignore_temp_dir(false);
    let koil = temp_koil(&temp, gitignore(), "");
    assert_eq!(vec!["src/", "target/", "a.log", "file"], names(&koil));
}

#[test]
fn test_open_ignored_or_hidden_dir() {
    // the open dir itself is shown, even if it is ignored or hidden
    let temp = gitignore_temp_dir(true);
    let koil = temp_koil(&temp, gitignore(), "target");
    assert_eq!(vec!["out"], names(&koil));
    fs::create_dir(temp.path().join(".dotdir")).unwrap();
    fs::write(temp.path().join(".dotdir/x"), "").unwrap();
    let koil = temp_koil(&temp, Settings::default(), ".dotdir");
    assert_eq!(vec!["x"], names(&koil));
}

#[test]
fn test_gitignore_keeps_changes() {
    let temp = gitignore_temp_dir(true);
    let mut koil = temp_koil(&temp, Settings::default(), "");
    let log = id(&koil, "a.log");
    let mut entries = koil.listing();
    entries[2] = with_id(log, "b.log");
    koil.update(&entries).unwrap();

    // `b.log` is ignored, but it was renamed, so it is still shown and stays renamed
    koil.set_settings(gitignore()).unwrap();
    assert_eq!(vec!["src/", "b.log", "file"], names(&koil));
    koil.update(&koil.listing()).unwrap();
    let root = koil.current_dir().to_path_buf();
    assert_eq!(
        vec![Action::Rename(root.join("a.log"), root.join("b.log"))],
        koil.compute_actions()
    );
}

#[test]
fn test_refresh() {
    let mut koil = update_test_dir(&["new"]).unwrap();
    koil.refresh().unwrap();
    assert_eq!(Diff::default(), koil.diff);
    assert_eq!(test_dir_listing(&koil), koil.listing());
}

#[test]
fn test_undo_last_apply_first() {
    let mut koil = test_koil();
    koil.push_undo(vec![Undo::Trash("a".into())]);
    koil.push_undo(vec![]);
    koil.push_undo(vec![Undo::Rename("b".into(), "c".into())]);
    assert_eq!(
        Some(&[Undo::Rename("b".into(), "c".into())][..]),
        koil.undo_steps().unwrap()
    );
}

#[test]
fn test_undo_trashes_dir_with_its_contents() {
    let mut koil = test_koil();
    koil.push_undo(vec![
        Undo::Trash("new/dir/y".into()),
        Undo::Trash("new/x".into()),
        Undo::Rename("new2/b".into(), "b".into()),
        Undo::Trash("new".into()),
        Undo::Trash("new2".into()),
    ]);
    assert_eq!(
        Some(
            &[
                Undo::Rename("new2/b".into(), "b".into()),
                Undo::Trash("new".into()),
                Undo::Trash("new2".into()),
            ][..]
        ),
        koil.undo_steps().unwrap()
    );
}

#[test]
fn test_undo_is_saved() {
    let mut koil = test_koil();
    koil.push_undo(vec![Undo::Trash("a".into())]);
    let loaded = Koil::load_state(&koil.save_state()).unwrap();
    assert_eq!(
        Some(&[Undo::Trash("a".into())][..]),
        loaded.undo_steps().unwrap()
    );
}

#[test]
fn test_undo_with_pending_changes_fails() {
    let mut koil = update_test_dir(&["new"]).unwrap();
    koil.push_undo(vec![Undo::Trash("a".into())]);
    assert!(matches!(koil.undo_steps(), Err(KoilError::PendingChanges)));
    assert!(matches!(koil.undo(), Err(KoilError::PendingChanges)));
}

#[test]
fn test_nothing_to_undo() {
    let mut koil = test_koil();
    assert_eq!(None, koil.undo_steps().unwrap());
    assert!(matches!(koil.undo(), Err(KoilError::NothingToUndo)));
}

#[test]
fn test_invalid_update_changes_nothing() {
    let mut koil = test_koil();
    let before = koil.save_state();
    // `file` is deleted, before the invalid ID is found
    let entries = [keep(&koil, "dir/"), with_id(Id(999), "new")];
    assert!(koil.update(&entries).is_err());
    assert_eq!(before, koil.save_state());
}
