//! Writes the committed fixtures under `tests/fixtures`. Deterministic: run it
//! again and nothing changes. `cargo run --example gen_fixtures` from the plugin folder.
#[path = "fixtures.rs"]
mod fixtures;

use std::path::Path;

use fixtures::{O, X};
use parquet::basic::Compression;

fn put(root: &Path, rel: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
}

fn main() {
    let root = Path::new("tests/fixtures");
    let batch = fixtures::sample_batch();
    put(
        root,
        "formats/people.csv",
        "name,score,joined,active\nAna,9,2024-01-05,true\n\"Bo, Jr.\",7,2024-02-10,false\nCy,,2024-03-01,true\n",
    );
    put(
        root,
        "formats/semi.csv",
        "id;city;pop\n1;Oslo;709\n2;Bergen;286\n",
    );
    put(
        root,
        "formats/tabs.tsv",
        "id\tnote\n1\tfirst\n2\t\"two\nlines\"\n",
    );
    put(root, "formats/pipes.txt", "a|b\n1|x\n2|y\n");
    put(root, "formats/bom.csv", "\u{feff}k,v\n1,one\n2,two\n");
    put(root, "formats/utf16le.csv", {
        let mut b = vec![0xFF, 0xFE];
        for u in "k,v\n1,é\n2,😀\n".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b
    });
    put(root, "formats/nulls.csv", "a,b\n1,\"\"\n2,\n3,x\n");
    put(root, "formats/ragged.csv", "a,b,c\n1,2,3\n4,5\n6,7,8,9\n");
    put(root, "formats/mixed.csv", "v\n1\n2\nthree\n");
    put(root, "formats/zip_codes.csv", "zip,n\n02134,1\n10001,2\n");
    put(root, "formats/dupes.csv", "a,a,,a\n1,2,3,4\n");
    put(
        root,
        "formats/records.json",
        "[{\"id\":1,\"name\":\"Ana\",\"tags\":[\"x\",\"y\"],\"geo\":{\"lat\":1.5,\"lon\":2}},{\"id\":2,\"name\":null,\"tags\":[],\"geo\":null},{\"id\":3,\"name\":\"Cy\",\"extra\":true}]",
    );
    put(
        root,
        "formats/events.ndjson",
        "{\"t\":1,\"kind\":\"a\"}\n{\"t\":2,\"kind\":\"b\"}\n{\"t\":3,\"kind\":\"a\"}\n",
    );
    put(
        root,
        "formats/mixed_types.json",
        "[{\"v\":1},{\"v\":\"two\"},{\"v\":[3]}]",
    );
    put(
        root,
        "formats/people.parquet",
        fixtures::parquet(&batch, Compression::SNAPPY),
    );
    // formats/people_zstd.parquet is not written here: it was produced once with the reference zstd
    // library, so the pure Rust decoder is tested against another encoder's frames.
    put(
        root,
        "formats/book.xlsx",
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
        ]),
    );
    put(
        root,
        "formats/book.ods",
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
                    (1048000, vec![O::Gap(4)]),
                ],
            ),
            (
                "Rates",
                vec![
                    (1, vec![O::S("kind"), O::S("rate")]),
                    (1, vec![O::S("tax"), O::P("0.2")]),
                ],
            ),
        ]),
    );
    put(root, "hostile/bomb.xlsx", fixtures::zip_bomb());
    put(
        root,
        "hostile/truncated.parquet",
        &fixtures::parquet(&batch, Compression::SNAPPY)[..60],
    );
    put(root, "hostile/corrupt_footer.parquet", {
        let mut b = fixtures::parquet(&batch, Compression::SNAPPY);
        let n = b.len();
        for x in &mut b[n - 40..n - 8] {
            *x = 0xAB;
        }
        b
    });
    put(
        root,
        "hostile/truncated.xlsx",
        &fixtures::xlsx(&[("S", vec![vec![X::S("a")]])])[..100],
    );
    put(root, "hostile/not_parquet.parquet", "name,score\nAna,9\n");
    put(root, "hostile/binary.bin", [0u8, 1, 2, 255, 0, 7]);
    put(root, "hostile/unclosed.csv", "a,b\n1,\"never closed\n2,3\n");
    put(root, "hostile/truncated.json", "[{\"a\":1},{\"a\":");
    put(root, "hostile/empty.csv", "");
    put(root, "hostile/header_only.csv", "a,b\n");
    put(
        root,
        "hostile/deep.json",
        format!("{}1{}", "[".repeat(300), "]".repeat(300)),
    );
}
