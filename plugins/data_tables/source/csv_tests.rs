//! Tests of delimited text: delimiter and header detection, names, types, wide files and the
//! scan. A child module of `csv` so it can reach the private helpers.
use super::*;
use crate::testkit::{Dir, collect, cols, columns};
use serde_json::{Value, json};

fn open_bytes(bytes: &[u8], opts: Options, infer: Infer) -> (CsvTable, Arc<Warnings>, Dir) {
    let dir = Dir::new();
    let p = dir.put("t.csv", bytes);
    let warn = Warnings::new();
    let t = CsvTable::open(&p, "t", opts, infer, &warn).unwrap();
    (t, warn, dir)
}

fn open(text: &str) -> (CsvTable, Arc<Warnings>, Dir) {
    open_bytes(text.as_bytes(), Options::default(), Infer::Sample)
}

#[test]
fn reads_the_people_fixture_with_exact_types_and_values() {
    let (t, warn, _d) = open(
        "name,score,joined,active\nAna,9,2024-01-05,true\n\"Bo, Jr.\",7,2024-02-10,false\nCy,,2024-03-01,true\n",
    );
    assert_eq!(
        columns(&t),
        cols(&[
            ("name", "text"),
            ("score", "int64"),
            ("joined", "date"),
            ("active", "bool")
        ])
    );
    assert_eq!(t.row_count(), Some(3));
    assert_eq!(
        collect(&t, None).unwrap(),
        vec![
            vec![json!("Ana"), json!(9), json!("2024-01-05"), json!(true)],
            vec![
                json!("Bo, Jr."),
                json!(7),
                json!("2024-02-10"),
                json!(false)
            ],
            vec![json!("Cy"), Value::Null, json!("2024-03-01"), json!(true)],
        ]
    );
    assert!(warn.list().is_empty());
    assert_eq!(t.details(), Some(json!({"delimiter": ",", "header": true})));
}

#[test]
fn sniffs_each_delimiter() {
    for (text, delim) in [
        ("a;b;c\n1;2;3\n4;5;6\n", b';'),
        ("a\tb\n1\t2\n", b'\t'),
        ("a|b|c\n1|2|3\n", b'|'),
        ("a,b\n1,2\n", b','),
        ("x\ny\nz\n", b','),
        // A tie in agreement and width goes to the earlier candidate: `,` over `;`, and
        // `;` over `|`.
        ("a,b;c\nd,e;f\n", b','),
        ("a;b|c\nd;e|f\n", b';'),
    ] {
        assert_eq!(sniff_delimiter(text.as_bytes(), true), delim, "{text:?}");
    }
    // Commas inside quotes do not outvote the real delimiter.
    assert_eq!(
        sniff_delimiter(b"a;b\n\"1,5,5\";2\n\"2,5,5\";3\n", true),
        b';'
    );
    // The most consistent candidate wins: every row splits in two on `;`, but not on `,`.
    assert_eq!(sniff_delimiter(b"x;1,5\ny;2,5\nz;3\n", true), b';');
}

#[test]
fn a_wrong_guess_can_be_overridden() {
    let (t, _, _d) = open_bytes(
        b"a,b;c\n1,2;3\n",
        Options {
            delimiter: Some(b','),
            header: None,
        },
        Infer::Sample,
    );
    assert_eq!(columns(&t).len(), 2);
    assert_eq!(columns(&t)[1].0, "b;c");
}

#[test]
fn header_detection() {
    let rows = |r: &[&[&str]]| -> Rows {
        r.iter()
            .map(|row| row.iter().map(|c| (c.as_bytes().to_vec(), false)).collect())
            .collect()
    };
    // Text over typed values.
    assert!(detect_header(&rows(&[&["name", "age"], &["Ana", "9"]])));
    // All text but distinct: a header.
    assert!(detect_header(&rows(&[&["name", "city"], &["Ana", "Oslo"]])));
    // All text and a repeated cell: data.
    assert!(!detect_header(&rows(&[&["x", "x"], &["a", "b"]])));
    // A number in the first row: data.
    assert!(!detect_header(&rows(&[&["1", "2"], &["3", "4"]])));
    // An empty cell in the first row: data.
    assert!(!detect_header(&rows(&[&["a", ""], &["b", "c"]])));
    assert!(!detect_header(&Rows::new()));
}

#[test]
fn a_file_without_a_header_gets_numbered_columns_and_keeps_its_first_row() {
    let (t, _, _d) = open("1,2\n3,4\n");
    assert_eq!(
        columns(&t),
        cols(&[("column_1", "int64"), ("column_2", "int64")])
    );
    assert_eq!(
        collect(&t, None).unwrap(),
        vec![vec![json!(1), json!(2)], vec![json!(3), json!(4)]]
    );
}

#[test]
fn the_header_option_overrides_detection_both_ways() {
    let (t, _, _d) = open_bytes(
        b"1,2\n3,4\n",
        Options {
            delimiter: None,
            header: Some(true),
        },
        Infer::Sample,
    );
    assert_eq!(columns(&t), cols(&[("1", "int64"), ("2", "int64")]));
    assert_eq!(t.row_count(), Some(1));
    let (t, _, _d) = open_bytes(
        b"name,age\nAna,9\n",
        Options {
            delimiter: None,
            header: Some(false),
        },
        Infer::Sample,
    );
    assert_eq!(
        columns(&t),
        cols(&[("column_1", "text"), ("column_2", "text")])
    );
    assert_eq!(t.row_count(), Some(2));
}

#[test]
fn empty_and_repeated_names_are_replaced_and_said() {
    // A first row with an empty cell is not detected as a header, so it is asked for.
    let (t, warn, _d) = open_bytes(
        b"a,a,,a\n1,2,3,4\n",
        Options {
            delimiter: None,
            header: Some(true),
        },
        Infer::Sample,
    );
    assert_eq!(
        columns(&t).iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["a", "a_2", "column_3", "a_3"]
    );
    assert_eq!(
        warn.list(),
        [
            "2 repeated column names got a number added (name_2, name_3)",
            "1 empty column name was named column_<position>"
        ]
    );
}

#[test]
fn a_numbered_name_never_collides_with_a_real_one() {
    let (t, _, _d) = open("a,a_2,a\n1,2,3\n");
    assert_eq!(
        columns(&t).iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["a", "a_2", "a_3"]
    );
}

#[test]
fn null_and_the_empty_string_are_different_values() {
    let (t, _, _d) = open("a,b\n1,\"\"\n2,\n3,x\n");
    assert_eq!(columns(&t)[1], ("b".to_string(), "text".to_string()));
    assert_eq!(
        collect(&t, Some(&[1])).unwrap(),
        vec![vec![json!("")], vec![Value::Null], vec![json!("x")]]
    );
}

#[test]
fn a_quoted_empty_field_in_a_number_column_is_null() {
    let (t, _, _d) = open("a,b\n1,5\n2,\"\"\n");
    assert_eq!(columns(&t)[1].1, "int64");
    assert_eq!(
        collect(&t, Some(&[1])).unwrap(),
        vec![vec![json!(5)], vec![Value::Null]]
    );
}

#[test]
fn leading_zero_numbers_stay_text_and_clean_integers_stay_integers() {
    let (t, _, _d) = open("zip,n,big\n02134,1,9223372036854775807\n10001,2,-5\n");
    assert_eq!(
        columns(&t),
        cols(&[("zip", "text"), ("n", "int64"), ("big", "int64")])
    );
    let rows = collect(&t, None).unwrap();
    assert_eq!(
        rows[0],
        vec![json!("02134"), json!(1), json!("9223372036854775807")]
    );
    assert_eq!(rows[1], vec![json!("10001"), json!(2), json!(-5)]);
}

#[test]
fn a_float_widens_only_its_own_column() {
    let (t, _, _d) = open("i,f\n1,1\n2,2.5\n");
    assert_eq!(columns(&t), cols(&[("i", "int64"), ("f", "float64")]));
    assert_eq!(
        collect(&t, Some(&[1])).unwrap(),
        vec![vec![json!(1.0)], vec![json!(2.5)]]
    );
}

#[test]
fn ragged_rows_are_padded_or_cut_and_said_once() {
    let (t, warn, _d) = open("a,b,c\n1,2,3\n4,5\n6,7,8,9\n");
    assert_eq!(
        collect(&t, None).unwrap(),
        vec![
            vec![json!(1), json!(2), json!(3)],
            vec![json!(4), json!(5), Value::Null],
            vec![json!(6), json!(7), json!(8)]
        ]
    );
    assert_eq!(
        warn.list(),
        ["rows with fewer than 3 fields were padded with NULL (first on line 3)"]
    );
}

#[test]
fn extra_fields_are_dropped_with_a_warning() {
    let (_, warn, _d) = open("a,b\n1,2,3\n");
    assert_eq!(
        warn.list(),
        ["rows with more than 2 fields lost their extra values (first on line 2)"]
    );
}

#[test]
fn embedded_newlines_quotes_and_crlf() {
    let (t, _, _d) = open("id,note\r\n1,\"line one\nline two\"\r\n2,\"say \"\"hi\"\"\"\r\n");
    assert_eq!(
        collect(&t, Some(&[1])).unwrap(),
        vec![vec![json!("line one\nline two")], vec![json!("say \"hi\"")]]
    );
    assert_eq!(t.row_count(), Some(2));
}

#[test]
fn a_byte_order_mark_is_not_part_of_the_first_name() {
    let (t, _, _d) = open_bytes(
        "\u{feff}k,v\n1,one\n".as_bytes(),
        Options::default(),
        Infer::Sample,
    );
    assert_eq!(columns(&t)[0].0, "k");
}

#[test]
fn utf16_files_with_a_byte_order_mark_are_read() {
    for le in [true, false] {
        let mut bytes = if le {
            vec![0xFF, 0xFE]
        } else {
            vec![0xFE, 0xFF]
        };
        for u in "k,v\n1,é\n2,😀\n".encode_utf16() {
            bytes.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        let (t, _, _d) = open_bytes(&bytes, Options::default(), Infer::Sample);
        assert_eq!(
            collect(&t, Some(&[1])).unwrap(),
            vec![vec![json!("é")], vec![json!("😀")]],
            "le={le}"
        );
    }
}

#[test]
fn a_long_utf16_file_with_pairs_at_every_read_boundary_decodes_exactly() {
    let line = "😀é,x\n";
    let text: String = std::iter::repeat_n(line, 40_000).collect();
    let mut bytes = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let dir = Dir::new();
    let p = dir.put("w.csv", &bytes);
    let mut got = String::new();
    open_text(&p).unwrap().read_to_string(&mut got).unwrap();
    assert_eq!(got, text);
}

#[test]
fn invalid_utf8_is_repaired_and_said() {
    let (t, warn, _d) = open_bytes(
        b"a,b\nok,1\nbad\xff\xfe,2\n",
        Options::default(),
        Infer::Sample,
    );
    let rows = collect(&t, Some(&[0])).unwrap();
    assert_eq!(rows[1][0], json!("bad\u{fffd}\u{fffd}"));
    assert_eq!(
        warn.list(),
        ["some text was not valid UTF-8 and the bad bytes were replaced"]
    );
}

#[test]
fn projection_reads_only_the_named_columns_and_an_empty_one_counts_rows() {
    let (t, _, _d) = open("a,b,c\n1,x,2.5\n2,y,3.5\n");
    assert_eq!(
        collect(&t, Some(&[0, 2])).unwrap(),
        vec![vec![json!(1), json!(2.5)], vec![json!(2), json!(3.5)]]
    );
    let batches: Vec<_> = t.scan(Some(&[])).collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 2);
    assert_eq!(batches[0].num_columns(), 0);
}

#[test]
fn a_header_only_file_is_an_empty_text_table() {
    let (t, _, _d) = open("a,b\n");
    assert_eq!(columns(&t), cols(&[("a", "text"), ("b", "text")]));
    assert_eq!(t.row_count(), Some(0));
    assert!(collect(&t, None).unwrap().is_empty());
}

#[test]
fn an_empty_file_is_refused_with_a_sentence() {
    let dir = Dir::new();
    let p = dir.put("e.csv", "");
    let err = CsvTable::open(&p, "e", Options::default(), Infer::Sample, &Warnings::new())
        .err()
        .unwrap();
    assert!(err.contains("has no rows to read"), "{err}");
    let err = CsvTable::open(
        &dir.path().join("missing.csv"),
        "e",
        Options::default(),
        Infer::Sample,
        &Warnings::new(),
    )
    .err()
    .unwrap();
    assert!(err.contains("cannot read"), "{err}");
}

#[test]
fn a_value_past_the_sample_that_breaks_the_type_is_a_conflict_that_full_inference_resolves() {
    let mut text = String::from("v\n");
    for i in 0..(SAMPLE_ROWS + 10) {
        text.push_str(&format!("{i}\n"));
    }
    text.push_str("not a number\n");
    let (t, _, _d) = open(&text);
    assert_eq!(columns(&t)[0].1, "int64");
    assert_eq!(t.row_count(), None);
    let err = collect(&t, None).unwrap_err();
    let ScanError::Conflict {
        table,
        column,
        line,
    } = err
    else {
        panic!("{err}")
    };
    assert_eq!(
        (table.as_str(), column.as_str(), line),
        ("t", "v", SAMPLE_ROWS + 12)
    );
    let (full, _, _d) = open_bytes(text.as_bytes(), Options::default(), Infer::Full);
    assert_eq!(columns(&full)[0].1, "text");
    assert_eq!(full.row_count(), Some(SAMPLE_ROWS + 11));
    assert_eq!(
        collect(&full, None).unwrap().last().unwrap(),
        &vec![json!("not a number")]
    );
}

#[test]
fn the_row_count_is_known_only_when_the_whole_file_was_read() {
    let mut text = String::from("v\n");
    for i in 0..SAMPLE_ROWS {
        text.push_str(&format!("{i}\n"));
    }
    let (t, _, _d) = open(&text);
    assert_eq!(t.row_count(), Some(SAMPLE_ROWS));
    text.push_str("1\n");
    let (t, _, _d) = open(&text);
    assert_eq!(t.row_count(), None);
    assert_eq!(collect(&t, None).unwrap().len() as u64, SAMPLE_ROWS + 1);
}

#[test]
fn the_widest_table_is_one_table_and_one_column_more_is_refused() {
    let width = MAX_COLUMNS;
    let header: Vec<String> = (0..width).map(|i| format!("c{i}")).collect();
    let row: Vec<String> = (0..width).map(|i| i.to_string()).collect();
    let (t, _, _d) = open(&format!("{}\n{}\n", header.join(","), row.join(",")));
    assert_eq!(columns(&t).len(), width);
    assert_eq!(
        collect(&t, Some(&[width - 1, 0])).unwrap(),
        vec![vec![json!(width - 1), json!(0)]]
    );
    let dir = Dir::new();
    for wide in [width + 1, 50_000] {
        let p = dir.put("w.csv", format!("{}\n", ",".repeat(wide - 1)));
        let err = CsvTable::open(&p, "w", Options::default(), Infer::Sample, &Warnings::new())
            .err()
            .unwrap();
        assert_eq!(
            err,
            format!(
                "{} has {wide} columns, over the {MAX_COLUMNS} one table may have",
                p.display()
            )
        );
    }
}

#[test]
fn a_header_longer_than_the_first_sniff_window_is_still_read() {
    // 1,500 names of 250 bytes: a 375 KB header, past the 256 KiB first window.
    let header: Vec<String> = (0..1_500).map(|i| format!("{i:0>250}")).collect();
    let row: Vec<String> = (0..1_500).map(|i| i.to_string()).collect();
    let text = format!(
        "{}\n{}\n{}\n",
        header.join(","),
        row.join(","),
        row.join(",")
    );
    assert!(text.len() as u64 > SNIFF_BYTES + 100_000);
    let (t, _, _d) = open(&text);
    assert_eq!(columns(&t).len(), 1_500);
    assert_eq!(columns(&t)[7].0, format!("{:0>250}", 7));
    assert_eq!(t.row_count(), Some(2));
    // The same shape without a header, whose first record is also longer than the window.
    let (t, _, _d) = open(&format!("{}\n{}\n", row.join(","), row.join(",")));
    assert_eq!(columns(&t).len(), 1_500);
}

#[test]
fn fifty_thousand_repeated_names_are_numbered_in_linear_time() {
    let start = std::time::Instant::now();
    let header = vec!["x"; 50_000].join(",");
    let names = column_names(
        Some(
            &header
                .split(',')
                .map(|n| (n.as_bytes().to_vec(), false))
                .collect::<Vec<_>>(),
        ),
        50_000,
        &Warnings::new(),
    );
    assert_eq!(names[0], "x");
    assert_eq!(names[1], "x_2");
    assert_eq!(names[49_999], "x_50000");
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
}

#[test]
fn a_pandas_style_header_with_an_empty_first_cell_is_a_header() {
    let (t, warn, _d) = open(",a,b\n0,1,2\n1,3,4\n");
    assert_eq!(
        columns(&t),
        cols(&[("column_1", "int64"), ("a", "int64"), ("b", "int64")])
    );
    assert_eq!(
        collect(&t, None).unwrap(),
        vec![
            vec![json!(0), json!(1), json!(2)],
            vec![json!(1), json!(3), json!(4)]
        ]
    );
    assert_eq!(
        warn.list(),
        ["1 empty column name was named column_<position>"]
    );
    // Text under an empty-celled first row gives no typed evidence: still data.
    let (t, _, _d) = open(",a\nx,y\n");
    assert_eq!(t.row_count(), Some(2));
    // A row of nothing but empty cells is never a header.
    let rows: Rows = vec![
        vec![(vec![], false), (vec![], false)],
        vec![(b"1".to_vec(), false), (b"2".to_vec(), false)],
    ];
    assert!(!detect_header(&rows));
}

#[test]
fn a_batch_never_holds_more_than_the_batch_size() {
    let mut text = String::from("v\n");
    for i in 0..(BATCH_ROWS * 2 + 5) {
        text.push_str(&format!("{i}\n"));
    }
    let (t, _, _d) = open(&text);
    let sizes: Vec<usize> = t.scan(None).map(|b| b.unwrap().num_rows()).collect();
    assert_eq!(sizes, vec![BATCH_ROWS, BATCH_ROWS, 5]);
}

#[test]
fn dates_and_timestamps_become_typed_columns() {
    let (t, _, _d) = open(
        "d,t,z\n2024-02-29,2024-02-29 12:30:45,2024-02-29T12:30:45+02:00\n2024-03-01,2024-03-01T00:00,2024-03-01T00:00Z\n",
    );
    assert_eq!(
        columns(&t),
        cols(&[("d", "date"), ("t", "timestamp"), ("z", "timestamp_tz")])
    );
    let rows = collect(&t, None).unwrap();
    assert_eq!(
        rows[0],
        vec![
            json!("2024-02-29"),
            json!("2024-02-29T12:30:45"),
            json!("2024-02-29T10:30:45Z")
        ]
    );
    assert_eq!(rows[1][1], json!("2024-03-01T00:00:00"));
}
