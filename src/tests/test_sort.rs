use super::*;
use std::fs;
use std::time::Duration;
use tempfile::TempDir;

/// A temp dir with the files `b.txt` (3 bytes), `A.rs` (10 bytes), `c` (1 byte) and `a10.txt`
/// (5 bytes), and the dirs `big` (3 entries, one hidden) and `small` (empty), and its path as
/// koil reads it (canonical, like `/private/var` on macOS)
/// Each file is a minute older than the one before it
fn setup() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let now = SystemTime::now();
    for (i, (name, size)) in [("b.txt", 3), ("A.rs", 10), ("c", 1), ("a10.txt", 5)]
        .into_iter()
        .enumerate()
    {
        let path = root.join(name);
        fs::write(&path, "x".repeat(size)).unwrap();
        let time = now - Duration::from_secs(60 * (i as u64 + 1));
        let times = fs::FileTimes::new().set_modified(time).set_accessed(time);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(times)
            .unwrap();
    }
    fs::create_dir(root.join("big")).unwrap();
    for name in ["1", "2", ".3"] {
        fs::write(root.join("big").join(name), "").unwrap();
    }
    fs::create_dir(root.join("small")).unwrap();
    (tmp, root)
}

/// Koil with `dir` open, sorted by `by` (the other way round if `reverse`)
fn open_sorted(dir: &Path, by: SortBy, reverse: bool) -> Koil {
    let mut koil = Koil::builder()
        .settings(Settings {
            sort: Sort { by, reverse },
            ..Settings::default()
        })
        .build();
    koil.open(dir).unwrap();
    koil
}

/// The names of the listing, with `/` after a dir's
fn names(koil: &Koil) -> Vec<String> {
    let name = |e: Entry| {
        let slash = if e.is_dir { "/" } else { "" };
        format!("{}{slash}", e.name.display())
    };
    koil.listing().into_iter().map(name).collect()
}

#[test]
fn test_sort_by_name() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Name, false);
    assert_eq!(
        vec!["big/", "small/", "A.rs", "a10.txt", "b.txt", "c"],
        names(&koil)
    );
}

#[test]
fn test_sort_by_name_reversed() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Name, true);
    // dirs still come first
    assert_eq!(
        vec!["small/", "big/", "c", "b.txt", "a10.txt", "A.rs"],
        names(&koil)
    );
}

#[test]
fn test_sort_natural() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Natural, false);
    assert_eq!(
        vec!["big/", "small/", "A.rs", "a10.txt", "b.txt", "c"],
        names(&koil)
    );
}

#[test]
fn test_natural_order() {
    let mut names = vec!["a10", "a2", "B", "a1", "a02b", "a2a", "", "10", "9", "a"];
    names.sort_by(|a, b| natural_order(a, b).then_with(|| a.cmp(b)));
    assert_eq!(
        vec!["", "9", "10", "a", "a1", "a2", "a2a", "a02b", "a10", "B"],
        names
    );
}

#[test]
fn test_sort_by_extension() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Extension, false);
    // no extension first, and then by name
    assert_eq!(
        vec!["big/", "small/", "c", "A.rs", "a10.txt", "b.txt"],
        names(&koil)
    );
}

#[test]
fn test_sort_by_size() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Size, false);
    // a dir by its entries, without hidden ones
    assert_eq!(
        vec!["big/", "small/", "A.rs", "a10.txt", "b.txt", "c"],
        names(&koil)
    );
    let big = koil.metadata(id(&koil, "big")).unwrap();
    assert_eq!((true, Some(2)), (big.is_dir, big.entries));
    let a = koil.metadata(id(&koil, "A.rs")).unwrap();
    assert_eq!((false, 10, None), (a.is_dir, a.size, a.entries));
}

#[test]
fn test_sort_by_size_reversed() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Size, true);
    assert_eq!(
        vec!["small/", "big/", "c", "b.txt", "a10.txt", "A.rs"],
        names(&koil)
    );
}

#[test]
fn test_sort_by_size_counts_hidden_entries_when_shown() {
    let (_tmp, root) = setup();
    let mut koil = open_sorted(&root, SortBy::Size, false);
    let settings = Settings {
        show_hidden: true,
        ..koil.settings().clone()
    };
    koil.set_settings(settings).unwrap();
    let big = koil.metadata(id(&koil, "big")).unwrap();
    assert_eq!(Some(3), big.entries);
}

#[test]
fn test_entries_only_counted_when_sorting_by_size() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Modified, false);
    assert_eq!(None, koil.metadata(id(&koil, "big")).unwrap().entries);
}

#[test]
fn test_sort_by_disk_size() {
    let (_tmp, root) = setup();
    fs::write(root.join("written"), vec![1; 100_000]).unwrap();
    let koil = open_sorted(&root, SortBy::Disk, false);
    // a dir by its entries, as by size
    assert_eq!(vec!["big/", "small/", "written"], names(&koil)[..3]);
    let big = koil.metadata(id(&koil, "big")).unwrap();
    assert_eq!(Some(2), big.entries);
    let written = koil.metadata(id(&koil, "written")).unwrap();
    assert!(written.disk_size.unwrap() >= 100_000, "{written:?}");
    let koil = open_sorted(&root, SortBy::Disk, true);
    assert_eq!(vec!["small/", "big/"], names(&koil)[..2]);
    assert_eq!(Some("written"), names(&koil).last().map(String::as_str));
}

/// A sparse file is bigger than what it takes on disk
#[cfg(unix)]
#[test]
fn test_sort_by_disk_size_not_size() {
    let (_tmp, root) = setup();
    fs::write(root.join("written"), vec![1; 100_000]).unwrap();
    let sparse = fs::File::create(root.join("sparse")).unwrap();
    sparse.set_len(10_000_000).unwrap();
    let files = |by| -> Vec<String> {
        let koil = open_sorted(&root, by, false);
        names(&koil).into_iter().skip(2).collect()
    };
    assert_eq!(files(SortBy::Size)[..2], ["sparse", "written"]);
    let by_disk = files(SortBy::Disk);
    assert_eq!(
        (by_disk.first(), by_disk.last()),
        (Some(&"written".into()), Some(&"sparse".into()))
    );
}

#[test]
fn test_disk_size_only_read_when_sorting_by_it() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Size, false);
    assert_eq!(None, koil.metadata(id(&koil, "A.rs")).unwrap().disk_size);
}

#[test]
fn test_disk_size() {
    let (_tmp, root) = setup();
    let path = root.join("written");
    fs::write(&path, vec![1; 100_000]).unwrap();
    let size = disk_size(&path, &path.symlink_metadata().unwrap()).unwrap();
    // in whole blocks
    assert!((100_000..200_000).contains(&size), "{size}");
}

#[test]
fn test_sort_by_modified() {
    let (_tmp, root) = setup();
    let files = |reverse| -> Vec<String> {
        let koil = open_sorted(&root, SortBy::Modified, reverse);
        names(&koil).into_iter().skip(2).collect()
    };
    assert_eq!(vec!["b.txt", "A.rs", "c", "a10.txt"], files(false));
    assert_eq!(vec!["a10.txt", "c", "A.rs", "b.txt"], files(true));
}

#[test]
fn test_sort_by_accessed() {
    let (_tmp, root) = setup();
    let koil = open_sorted(&root, SortBy::Accessed, false);
    let files: Vec<String> = names(&koil).into_iter().skip(2).collect();
    assert_eq!(vec!["b.txt", "A.rs", "c", "a10.txt"], files);
}

#[test]
fn test_sort_changed_by_settings() {
    let (_tmp, root) = setup();
    let mut koil = open_sorted(&root, SortBy::Name, false);
    let settings = Settings {
        sort: Sort {
            by: SortBy::Size,
            reverse: false,
        },
        ..Settings::default()
    };
    let listing = koil.listing();
    let updated = koil.update_and_open(&listing, settings, None).unwrap();
    // the same entries, in another order
    assert!(!updated.moved);
    assert_eq!(
        vec!["big/", "small/", "A.rs", "a10.txt", "b.txt", "c"],
        names(&koil)
    );
    assert_eq!(Some(2), koil.metadata(id(&koil, "big")).unwrap().entries);
}

#[test]
fn test_sort_keeps_new_entries_last() {
    let (_tmp, root) = setup();
    let mut koil = open_sorted(&root, SortBy::Size, false);
    let mut entries = koil.listing();
    entries.extend([without_id("z"), without_id("new/"), without_id("m")]);
    // renamed: sorted by its new name among ones of the same size
    entries[2] = with_id(id(&koil, "A.rs"), "renamed.rs");
    koil.update(&entries).unwrap();
    assert_eq!(
        vec![
            "big/",
            "small/",
            "renamed.rs",
            "a10.txt",
            "b.txt",
            "c",
            "m",
            "new/",
            "z"
        ],
        names(&koil)
    );
    // still last, by name the other way round
    let settings = Settings {
        sort: Sort {
            by: SortBy::Size,
            reverse: true,
        },
        ..Settings::default()
    };
    koil.set_settings(settings).unwrap();
    let new: Vec<String> = names(&koil).into_iter().skip(6).collect();
    assert_eq!(vec!["z", "new/", "m"], new);
}

#[test]
fn test_sort_parent_first() {
    let (_tmp, root) = setup();
    let mut koil = open_sorted(&root, SortBy::Size, true);
    let settings = Settings {
        show_hidden: true,
        ..koil.settings().clone()
    };
    koil.set_settings(settings).unwrap();
    assert_eq!(Entry::parent(), koil.listing()[0]);
    let listing = koil.listing();
    assert!(koil.compare(&listing[0], &listing[1]).is_lt());
}

#[test]
fn test_compare_is_listing_order() {
    let (_tmp, root) = setup();
    for by in [
        SortBy::Name,
        SortBy::Natural,
        SortBy::Extension,
        SortBy::Size,
        SortBy::Modified,
        SortBy::Created,
        SortBy::Accessed,
    ] {
        for reverse in [false, true] {
            let mut koil = open_sorted(&root, by, reverse);
            let mut entries = koil.listing();
            entries.extend([without_id("new"), without_id("a/")]);
            koil.update(&entries).unwrap();
            let listing = koil.listing();
            let mut sorted = listing.clone();
            sorted.reverse();
            sorted.sort_by(|a, b| koil.compare(a, b));
            assert_eq!(listing, sorted, "{by:?}, reverse: {reverse}");
        }
    }
}

#[test]
fn test_metadata_of_id_from_another_dir() {
    let (_tmp, root) = setup();
    let mut koil = open_sorted(&root, SortBy::Size, false);
    let a = id(&koil, "A.rs");
    koil.open(root.join("big")).unwrap();
    // not in the listing, so read from disk
    assert_eq!(10, koil.metadata(a).unwrap().size);
    fs::write(root.join("A.rs"), "longer than before").unwrap();
    assert_eq!(18, koil.metadata(a).unwrap().size);
}

#[test]
fn test_settings_without_sort_load() {
    let settings: Settings = serde_json::from_str(r#"{"show_hidden": true}"#).unwrap();
    assert_eq!(Sort::default(), settings.sort);
    let sort: Sort = serde_json::from_str(r#"{"by": "size"}"#).unwrap();
    assert_eq!((SortBy::Size, false), (sort.by, sort.reverse));
}
