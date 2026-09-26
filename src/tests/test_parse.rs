use super::*;

#[test]
fn test_parse_select_new_dir() {
    let parsed = parse::parse_listing(
        "\
        >newdir/\n\
        :000001 file\n",
    );
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
