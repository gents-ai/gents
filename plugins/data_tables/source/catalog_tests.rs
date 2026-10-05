//! Tests of the catalog: what a folder lists, how formats and names are decided, and what
//! is refused or skipped. A child module of `catalog` so it can reach the private helpers.
use super::*;
use crate::testkit::fixtures::{self, O, X};
use crate::testkit::{Dir, collect, cols, columns};
use parquet::basic::Compression;
use serde_json::json;

fn discover(dir: &Dir) -> Catalog {
    Catalog::discover(Some(&dir.s()), None, None, Options::default()).unwrap()
}

fn names(c: &Catalog) -> Vec<String> {
    c.specs().iter().map(|s| s.name.clone()).collect()
}

fn book_xlsx() -> Vec<u8> {
    fixtures::xlsx(&[
        (
            "Sales",
            vec![
                vec![
                    X::S("region"),
                    X::S("units"),
                    X::S("price"),
                    X::S("day"),
                    X::S("ok"),
                ],
                vec![
                    X::S("north"),
                    X::N("10"),
                    X::N("2.5"),
                    X::D("45292"),
                    X::B(true),
                ],
                vec![
                    X::S("south"),
                    X::N("7"),
                    X::N("3"),
                    X::D("45293"),
                    X::B(false),
                ],
                vec![X::Empty, X::Empty, X::Empty, X::Empty, X::Empty],
                vec![
                    X::I("east"),
                    X::N("5.0"),
                    X::E("#DIV/0!"),
                    X::T("45294.5"),
                    X::Empty,
                ],
            ],
        ),
        (
            "Notes",
            vec![
                vec![X::S("note")],
                vec![X::F("computed")],
                vec![X::S("a & b <c>")],
            ],
        ),
    ])
}

fn book_ods() -> Vec<u8> {
    fixtures::ods(&[
        (
            "Budget",
            vec![
                (
                    1,
                    vec![O::S("item"), O::S("cost"), O::S("due"), O::S("paid")],
                ),
                (
                    1,
                    vec![O::S("rent"), O::F("1200"), O::D("2024-01-31"), O::B(true)],
                ),
                (
                    3,
                    vec![
                        O::S("tea"),
                        O::F("2.5"),
                        O::D("2024-02-01T09:30:00"),
                        O::B(false),
                    ],
                ),
                (1_048_000, vec![O::Gap(4)]),
            ],
        ),
        (
            "Rates",
            vec![
                (1, vec![O::S("kind"), O::S("rate")]),
                (1, vec![O::S("tax"), O::P("0.2")]),
            ],
        ),
    ])
}

#[test]
fn a_folder_lists_every_readable_file_and_sheet_in_order_and_says_what_it_skipped() {
    let d = Dir::new();
    d.put("a.csv", "x\n1\n");
    d.put("b.json", "[{\"y\":1}]");
    d.put(
        "c.parquet",
        fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY),
    );
    d.put("d.xlsx", book_xlsx());
    d.put("e.ods", book_ods());
    d.put("readme.md", "# hi\n");
    d.put("skip.bin", [0u8, 1, 2, 3]);
    d.put(".hidden.csv", "x\n1\n");
    d.put(".git/config.csv", "x\n1\n");
    d.put("sub/f.csv", "x\n1\n");
    d.put("sub/deep/g.csv", "x\n1\n");
    let c = discover(&d);
    assert_eq!(
        names(&c),
        [
            "a",
            "b",
            "c",
            "d_Sales",
            "d_Notes",
            "e_Budget",
            "e_Rates",
            "sub_deep_g",
            "sub_f"
        ]
    );
    let listed: Vec<(&str, &str, Option<&str>)> = c
        .specs()
        .iter()
        .map(|s| (s.source.as_str(), s.format, s.sheet.as_deref()))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("a.csv", "csv", None),
            ("b.json", "json", None),
            ("c.parquet", "parquet", None),
            ("d.xlsx", "xlsx", Some("Sales")),
            ("d.xlsx", "xlsx", Some("Notes")),
            ("e.ods", "ods", Some("Budget")),
            ("e.ods", "ods", Some("Rates")),
            ("sub/deep/g.csv", "csv", None),
            ("sub/f.csv", "csv", None),
        ]
    );
    assert_eq!(
        c.listing,
        [
            "2 files were skipped: readme.md: not a supported data file name; skip.bin: a binary file that is not a supported data format"
        ]
    );
}

#[test]
fn xlsx_sheets_read_as_typed_tables() {
    let d = Dir::new();
    d.put("book.xlsx", book_xlsx());
    let c = discover(&d);
    let sales = c.open("book_Sales").unwrap();
    assert_eq!(
        columns(sales.as_ref()),
        cols(&[
            ("region", "text"),
            ("units", "int64"),
            ("price", "float64"),
            ("day", "timestamp"),
            ("ok", "bool")
        ])
    );
    assert_eq!(sales.row_count(), Some(3));
    assert_eq!(
        collect(sales.as_ref(), None).unwrap(),
        vec![
            vec![
                json!("north"),
                json!(10),
                json!(2.5),
                json!("2024-01-01T00:00:00"),
                json!(true)
            ],
            vec![
                json!("south"),
                json!(7),
                json!(3.0),
                json!("2024-01-02T00:00:00"),
                json!(false)
            ],
            vec![
                json!("east"),
                json!(5),
                json!(null),
                json!("2024-01-03T12:00:00"),
                json!(null)
            ],
        ]
    );
    assert_eq!(
        c.warn.list(),
        ["cells holding spreadsheet errors (such as #DIV/0!) were read as NULL"]
    );
    let notes = c.open("book_Notes").unwrap();
    assert_eq!(columns(notes.as_ref()), cols(&[("note", "text")]));
    assert_eq!(
        collect(notes.as_ref(), None).unwrap(),
        vec![vec![json!("computed")], vec![json!("a & b <c>")]]
    );
}

#[test]
fn ods_sheets_read_as_typed_tables_and_skip_the_million_blank_rows() {
    let d = Dir::new();
    d.put("book.ods", book_ods());
    let c = discover(&d);
    let t = c.open("book_Budget").unwrap();
    assert_eq!(
        columns(t.as_ref()),
        cols(&[
            ("item", "text"),
            ("cost", "float64"),
            ("due", "timestamp"),
            ("paid", "bool")
        ])
    );
    assert_eq!(t.row_count(), Some(4));
    assert_eq!(
        collect(t.as_ref(), None).unwrap(),
        vec![
            vec![
                json!("rent"),
                json!(1200.0),
                json!("2024-01-31T00:00:00"),
                json!(true)
            ],
            vec![
                json!("tea"),
                json!(2.5),
                json!("2024-02-01T09:30:00"),
                json!(false)
            ],
            vec![
                json!("tea"),
                json!(2.5),
                json!("2024-02-01T09:30:00"),
                json!(false)
            ],
            vec![
                json!("tea"),
                json!(2.5),
                json!("2024-02-01T09:30:00"),
                json!(false)
            ],
        ]
    );
    let rates = c.open("book_Rates").unwrap();
    assert_eq!(
        collect(rates.as_ref(), None).unwrap(),
        vec![vec![json!("tax"), json!(0.2)]]
    );
}

#[test]
fn a_single_file_is_its_stem_and_a_sheet_adds_its_name() {
    let d = Dir::new();
    let p = d.put("My Report 2024.csv", "x\n1\n");
    let c = Catalog::discover(Some(&p.to_string_lossy()), None, None, Options::default()).unwrap();
    assert_eq!(names(&c), ["My_Report_2024"]);
    c.open("My_Report_2024").unwrap();
    assert_eq!(
        c.warn.list(),
        ["My Report 2024.csv is the table My_Report_2024"]
    );
}

#[test]
fn names_are_made_safe_and_collisions_are_numbered_and_said() {
    let d = Dir::new();
    d.put("data.csv", "x\n1\n");
    d.put("data.json", "[{\"x\":1}]");
    d.put("2024.csv", "x\n1\n");
    d.put("a b.csv", "x\n1\n");
    d.put("a_b.csv", "x\n1\n");
    let c = discover(&d);
    assert_eq!(names(&c), ["t_2024", "a_b", "a_b_2", "data", "data_2"]);
    assert!(
        c.warn.list().is_empty(),
        "naming is said when a table is used, not when it is listed"
    );
    for n in names(&c) {
        c.open(&n).unwrap();
    }
    assert_eq!(
        c.warn.list(),
        [
            "a b.csv is the table a_b",
            "a_b.csv is the table a_b_2",
            "data.json is the table data_2",
            "2024.csv is the table t_2024"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
    );
}

#[test]
fn content_decides_the_format_not_the_name() {
    let d = Dir::new();
    d.put(
        "really_parquet.csv",
        fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY),
    );
    d.put("really_xlsx.txt", book_xlsx());
    d.put("fake.parquet", "name,score\nAna,9\n");
    d.put("fake.xlsx", "not a zip");
    d.put("noext", "a,b\n1,2\n");
    let c = discover(&d);
    let formats: Vec<(&str, &str)> = c
        .specs()
        .iter()
        .map(|s| (s.name.as_str(), s.format))
        .collect();
    assert_eq!(
        formats,
        vec![
            ("really_parquet", "parquet"),
            ("really_xlsx_Sales", "xlsx"),
            ("really_xlsx_Notes", "xlsx")
        ]
    );
    let w = &c.listing[0];
    assert!(w.starts_with("3 files were skipped: fake.parquet: named like Parquet but does not start like a Parquet file; fake.xlsx: named like a spreadsheet"), "{w}");
    assert!(w.contains("noext: not a supported data file name"), "{w}");
}

#[test]
fn a_named_file_is_read_by_content_even_without_a_known_name_and_refused_with_a_reason_when_it_is_not_data()
 {
    let d = Dir::new();
    d.put("noext", "a,b\n1,2\n");
    d.put("records", "[{\"a\":1}]");
    d.put("fake.parquet", "name,score\n");
    d.put("blob.bin", [0u8, 255, 0]);
    let c = Catalog::discover(
        Some(&d.s()),
        Some(&["noext".into(), "records".into()]),
        None,
        Options::default(),
    )
    .unwrap();
    assert_eq!(
        c.specs().iter().map(|s| s.format).collect::<Vec<_>>(),
        ["csv", "json"]
    );
    for bad in ["fake.parquet", "blob.bin"] {
        let err = Catalog::discover(Some(&d.s()), Some(&[bad.into()]), None, Options::default())
            .err()
            .unwrap();
        assert!(
            err.starts_with(&format!("{bad} cannot be read as a table: ")),
            "{err}"
        );
    }
}

#[test]
fn files_keeps_the_given_order_and_refuses_paths_that_leave_the_folder() {
    let d = Dir::new();
    d.put("b.csv", "x\n1\n");
    d.put("a.csv", "x\n1\n");
    d.put("sub/c.csv", "x\n1\n");
    let c = Catalog::discover(
        Some(&d.s()),
        Some(&["b.csv".into(), "sub/c.csv".into(), "a.csv".into()]),
        None,
        Options::default(),
    )
    .unwrap();
    assert_eq!(names(&c), ["b", "sub_c", "a"]);
    for bad in [
        "../x.csv",
        "/etc/passwd",
        "",
        "a/../../x",
        "sub/../../x.csv",
        "x\0.csv",
        "C:\\x.csv",
    ] {
        let err = Catalog::discover(Some(&d.s()), Some(&[bad.into()]), None, Options::default())
            .err()
            .unwrap_or_else(|| panic!("{bad:?} accepted"));
        assert!(
            err.contains("not a path inside the folder") || err.contains("cannot read"),
            "{bad:?}: {err}"
        );
    }
    let err = Catalog::discover(
        Some(&d.s()),
        Some(&["missing.csv".into()]),
        None,
        Options::default(),
    )
    .err()
    .unwrap();
    assert!(err.contains("cannot read"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_file_in_a_listing_that_cannot_be_opened_is_skipped_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let d = Dir::new();
    d.put("ok.csv", "x\n1\n");
    let locked = d.put("locked.csv", "x\n1\n");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::File::open(&locked).is_ok();
    let c = discover(&d);
    let named = Catalog::discover(
        Some(&d.s()),
        Some(&["locked.csv".into()]),
        None,
        Options::default(),
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    if !readable {
        assert_eq!(names(&c), ["ok"]);
        assert!(
            c.listing[0].contains("locked.csv: cannot read"),
            "{:?}",
            c.listing
        );
        assert!(named.err().unwrap().contains("cannot read"));
    }
}

#[cfg(unix)]
#[test]
fn links_that_leave_the_folder_are_not_followed() {
    let outside = Dir::new();
    outside.put("secret.csv", "k\ntop\n");
    outside.put("dir/x.csv", "k\n1\n");
    let d = Dir::new();
    d.put("ok.csv", "x\n1\n");
    std::os::unix::fs::symlink(outside.path().join("secret.csv"), d.path().join("leak.csv"))
        .unwrap();
    std::os::unix::fs::symlink(outside.path().join("dir"), d.path().join("leakdir")).unwrap();
    std::os::unix::fs::symlink("ok.csv", d.path().join("inside.csv")).unwrap();
    let c = discover(&d);
    assert_eq!(names(&c), ["inside", "ok"]);
    assert!(
        c.listing[0]
            .contains("leak.csv: a link that leads outside the folder or to a folder, not read"),
        "{:?}",
        c.listing
    );
    assert!(c.listing[0].contains("leakdir"));
    let err = Catalog::discover(
        Some(&d.s()),
        Some(&["leak.csv".into()]),
        None,
        Options::default(),
    )
    .err()
    .unwrap();
    assert!(
        err.contains("a link that leads outside the folder"),
        "{err}"
    );
    let err = Catalog::discover(
        Some(&d.s()),
        Some(&["leakdir/x.csv".into()]),
        None,
        Options::default(),
    )
    .err()
    .unwrap();
    assert!(
        err.contains("a link that leads outside the folder"),
        "{err}"
    );
}

#[test]
fn deep_folders_and_huge_folders_are_bounded_and_said() {
    let d = Dir::new();
    let mut rel = String::new();
    for i in 0..20 {
        rel.push_str(&format!("d{i}/"));
    }
    d.put(&format!("{rel}deep.csv"), "x\n1\n");
    d.put("top.csv", "x\n1\n");
    let c = discover(&d);
    assert_eq!(names(&c), ["top"]);
    assert!(
        c.listing[0].contains("folders nest too deep to list"),
        "{:?}",
        c.listing
    );

    let many = Dir::new();
    for i in 0..(MAX_FILES + 3) {
        many.put(&format!("f{i:05}.csv"), "x\n1\n");
    }
    let c = discover(&many);
    assert_eq!(c.specs().len(), MAX_FILES);
    assert!(c.listing.contains(&format!(
        "only the first {MAX_FILES} of {} files are listed; name the files to read in files",
        MAX_FILES + 3
    )));
}

#[test]
fn what_is_asked_for_must_exist() {
    // Nothing named is an empty catalog; the modes that need data refuse it (see main).
    let c = Catalog::discover(None, None, None, Options::default()).unwrap();
    assert!(c.specs().is_empty());
    let err = Catalog::discover(Some("/definitely/not/here"), None, None, Options::default())
        .err()
        .unwrap();
    assert!(err.starts_with("cannot read /definitely/not/here"), "{err}");
    let d = Dir::new();
    let p = d.put("a.csv", "x\n1\n");
    let err = Catalog::discover(
        Some(&p.to_string_lossy()),
        Some(&["a.csv".into()]),
        None,
        Options::default(),
    )
    .err()
    .unwrap();
    assert_eq!(err, "files lists paths inside a folder, but path is a file");
    let empty = Dir::new();
    let c = discover(&empty);
    assert!(c.specs().is_empty());
    assert!(
        c.open("x")
            .err()
            .unwrap()
            .contains("there is no table named x")
    );
}

#[test]
fn inline_tables_join_the_files_and_share_the_namespace() {
    let d = Dir::new();
    d.put("t.csv", "x\n1\n");
    let inline =
        json!([{"name": "t", "rows": [[1]]}, {"name": "other", "columns": ["a"], "rows": [[2]]}]);
    let c = Catalog::discover(Some(&d.s()), None, Some(&inline), Options::default()).unwrap();
    assert_eq!(names(&c), ["t", "t_2", "other"]);
    let t = c.open("other").unwrap();
    assert_eq!(collect(t.as_ref(), None).unwrap(), vec![vec![json!(2)]]);
    let only = Catalog::discover(None, None, Some(&inline), Options::default()).unwrap();
    assert_eq!(names(&only), ["t", "other"]);
}

#[test]
fn opening_is_cached_widening_reopens_and_the_fingerprint_follows_the_data() {
    let d = Dir::new();
    d.put("t.csv", "x\n1\n");
    let c = discover(&d);
    let a = c.open("t").unwrap();
    let b = c.open("t").unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    let before = c.opened_fingerprint();
    assert!(c.widen("t"));
    assert!(!c.widen("t"));
    let again = c.open("t").unwrap();
    assert!(!Arc::ptr_eq(&a, &again));
    assert_eq!(before, c.opened_fingerprint());
    d.put("t.csv", "x\n1\n2\n");
    let c2 = discover(&d);
    c2.open("t").unwrap();
    assert_ne!(before, c2.opened_fingerprint());
    let unopened = discover(&d);
    assert_eq!(unopened.opened_fingerprint(), Fnv::default().0);
}

#[test]
fn a_bad_file_fails_only_when_its_table_is_opened() {
    let d = Dir::new();
    d.put("good.csv", "x\n1\n");
    d.put("bad.parquet", "PAR1 not really");
    let c = discover(&d);
    assert_eq!(names(&c), ["bad", "good"]);
    assert!(c.open("good").is_ok());
    let err = c.open("bad").err().unwrap();
    assert!(err.contains("is not a readable Parquet file"), "{err}");
}

#[test]
fn safe_join_accepts_plain_relative_paths_only() {
    let root = Path::new("/tmp");
    for ok in ["a.csv", "sub/a.csv", "./a.csv", "a b.csv"] {
        assert!(safe_join(root, ok).is_ok(), "{ok}");
    }
    for bad in ["", "..", "../a", "a/..", "/abs", "a\0b"] {
        assert!(safe_join(root, bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_file_is_listed_at_exactly_sixteen_folders_down_and_skipped_at_seventeen() {
    let nest = |n: usize| "d/".repeat(n);
    let d = Dir::new();
    d.put(&format!("{}at16.csv", nest(16)), "x\n1\n");
    d.put("top.csv", "x\n1\n");
    let c = discover(&d);
    assert_eq!(names(&c), ["d_d_d_d_d_d_d_d_d_d_d_d_d_d_d_d_at16", "top"]);
    assert!(c.listing.is_empty(), "{:?}", c.listing);
    let d = Dir::new();
    d.put(&format!("{}at17.csv", nest(17)), "x\n1\n");
    let c = discover(&d);
    assert!(c.specs().is_empty());
    assert_eq!(
        c.listing,
        ["1 files were skipped: d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d: folders nest too deep to list"]
    );
}

#[test]
fn a_csv_that_starts_like_a_binary_format_is_still_a_csv() {
    let d = Dir::new();
    d.put("parts.csv", "PAR1,qty\nbolt,4\n");
    d.put("marks.csv", "ARROW1,qty\nbolt,4\n");
    d.put("ff.csv", b"\xFF\xFF\xFF\xFF,qty\nbolt,4\n");
    let c = discover(&d);
    assert!(c.listing.is_empty(), "{:?}", c.listing);
    assert_eq!(names(&c), ["ff", "marks", "parts"]);
    for name in ["parts", "marks"] {
        let t = c.open(name).unwrap();
        assert_eq!(
            columns(t.as_ref())[0].0,
            if name == "parts" { "PAR1" } else { "ARROW1" }
        );
    }
    // The real magic still wins: a Parquet file by its closing marker whatever its name, and
    // Arrow files by the full file magic or a stream marker with a sane length.
    let d = Dir::new();
    let parquet = fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY);
    d.put("real.dat", &parquet);
    d.put("file.dat", b"ARROW1\0\0 and then data");
    let mut stream = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x20, 0, 0, 0];
    stream.extend_from_slice(&[1; 32]);
    d.put("stream.dat", &stream);
    let c = Catalog::discover(
        Some(&d.s()),
        Some(&[
            "real.dat".to_string(),
            "file.dat".to_string(),
            "stream.dat".to_string(),
        ]),
        None,
        Options::default(),
    );
    let err = c.err().unwrap();
    assert!(err.contains("Arrow files are not supported"), "{err}");
    assert!(
        detect(&d.path().join("real.dat"), false)
            .unwrap()
            .is_ok_and(|f| f == Fmt::Parquet)
    );
    for arrow in ["file.dat", "stream.dat"] {
        assert!(
            detect(&d.path().join(arrow), false)
                .unwrap()
                .unwrap_err()
                .contains("Arrow files are not supported"),
            "{arrow}"
        );
    }
    // A damaged file named .parquet is still handed to the Parquet reader for its own reason.
    d.put("cut.parquet", &parquet[..60]);
    assert_eq!(
        detect(&d.path().join("cut.parquet"), true).unwrap(),
        Ok(Fmt::Parquet)
    );
}

/// `xlsx` with the part `name` replaced by `data`.
fn with_part(xlsx: &[u8], name: &str, data: &[u8]) -> Vec<u8> {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    let mut read = zip::ZipArchive::new(Cursor::new(xlsx)).unwrap();
    let mut write = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..read.len() {
        let mut part = read.by_index(i).unwrap();
        let part_name = part.name().to_string();
        write
            .start_file(&part_name, SimpleFileOptions::default())
            .unwrap();
        if part_name == name {
            write.write_all(data).unwrap();
        } else {
            std::io::copy(&mut part, &mut write).unwrap();
        }
    }
    write.finish().unwrap().into_inner()
}

#[test]
fn a_workbook_beside_a_csv_is_not_read_until_a_query_names_one_of_its_sheets() {
    let d = Dir::new();
    d.put("small.csv", "x\n1\n2\n");
    // The shared strings part is not even XML: reading it fails, so any read of it is seen.
    let broken = with_part(&book_xlsx(), "xl/sharedStrings.xml", b"<sst><si><t>a</sst>");
    for i in 0..12 {
        d.put(&format!("book{i:02}.xlsx"), &broken);
    }
    // Listing and a query on the CSV read none of the 12 workbooks beyond their sheet lists.
    let c = discover(&d);
    assert_eq!(c.specs().len(), 1 + 12 * 2);
    assert!(c.listing.is_empty(), "{:?}", c.listing);
    assert_eq!(
        crate::testkit::collect(c.open("small").unwrap().as_ref(), None).unwrap(),
        vec![vec![json!(1)], vec![json!(2)]]
    );
    assert!(c.warn.list().is_empty());
    let r = crate::testkit::query(&d, "SELECT count(*) AS n FROM small").unwrap();
    assert_eq!(r["rows"], json!([[2]]));
    assert_eq!(r["warnings"], json!([]));
    // Naming a sheet reads its workbook's shared strings, and the failure names the workbook.
    let e = crate::testkit::query(&d, "SELECT * FROM book03_Sales").unwrap_err();
    assert!(
        e.starts_with("the shared strings of book03.xlsx is not valid XML"),
        "{e}"
    );
    // A part cut off inside a string is refused too, not read as missing values.
    d.put(
        "cut.xlsx",
        with_part(
            &book_xlsx(),
            "xl/sharedStrings.xml",
            b"<sst><si><t>never closed",
        ),
    );
    let e = crate::testkit::query(&d, "SELECT * FROM cut_Sales").unwrap_err();
    assert!(
        e.starts_with("the shared strings of cut.xlsx is not valid XML (it ends inside a string)"),
        "{e}"
    );
}

#[test]
fn a_header_with_an_empty_cell_is_a_header_in_a_sheet_too() {
    let d = Dir::new();
    let e = Dir::new();
    d.put(
        "pandas.xlsx",
        fixtures::xlsx(&[(
            "S",
            vec![
                vec![X::Empty, X::S("a"), X::S("b")],
                vec![X::N("0"), X::N("1"), X::N("2")],
                vec![X::N("1"), X::N("3"), X::N("4")],
            ],
        )]),
    );
    e.put(
        "pandas.ods",
        fixtures::ods(&[(
            "S",
            vec![
                (1, vec![O::Gap(1), O::S("a"), O::S("b")]),
                (1, vec![O::F("0"), O::F("1"), O::F("2")]),
                (1, vec![O::F("1"), O::F("3"), O::F("4")]),
            ],
        )]),
    );
    for dir in [&d, &e] {
        let c = discover(dir);
        let t = c.open("pandas_S").unwrap();
        assert_eq!(
            columns(t.as_ref()),
            cols(&[("column_1", "int64"), ("a", "int64"), ("b", "int64")]),
            "{}",
            dir.s()
        );
        assert_eq!(
            collect(t.as_ref(), None).unwrap(),
            vec![
                vec![json!(0), json!(1), json!(2)],
                vec![json!(1), json!(3), json!(4)]
            ]
        );
        assert_eq!(
            c.warn.list(),
            ["1 empty column name was named column_<position>"]
        );
    }
    // Text under a gappy first row is no evidence of a header: it stays data.
    let d = Dir::new();
    d.put(
        "t.xlsx",
        fixtures::xlsx(&[(
            "S",
            vec![
                vec![X::S("x"), X::Empty, X::S("z")],
                vec![X::S("p"), X::S("q"), X::S("r")],
            ],
        )]),
    );
    let c = discover(&d);
    let t = c.open("t_S").unwrap();
    assert_eq!(t.row_count(), Some(2));
}
