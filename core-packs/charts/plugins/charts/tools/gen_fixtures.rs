//! Writes the plugin's test fixtures. Everything is derived from a fixed
//! formula, so running it again gives the same bytes; a test regenerates the
//! files in memory and compares them with the committed ones.
//!
//! Usage: cargo run --example gen_fixtures [directory]
//! (default: tests/fixtures next to this crate).

use std::path::{Path, PathBuf};

/// Every fixture as (path below the fixtures folder, bytes).
pub fn files() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut add = |name: &str, body: String| out.push((name.to_owned(), body.into_bytes()));

    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut sales = String::from("month,region,revenue,units\n");
    for (i, m) in months.iter().enumerate() {
        for (r, region) in ["North", "South"].iter().enumerate() {
            let revenue = 100 + i * 9 + (i * 7 + r * 13) % 23 + r * 15;
            let units = 40 + (i * 5 + r * 11) % 17;
            sales.push_str(&format!("{m},{region},{revenue},{units}\n"));
        }
    }
    add("data/sales.csv", sales);

    let mut scores = String::from(r#"{"columns":["student","math","art","music"],"rows":["#);
    let rows: Vec<String> = (0..12)
        .map(|i| {
            format!(
                r#"["s{:02}",{},{},{}]"#,
                i + 1,
                55 + (i * 7) % 40,
                60 + (i * 11) % 35,
                50 + (i * 13) % 45
            )
        })
        .collect();
    scores.push_str(&rows.join(","));
    scores.push_str("]}\n");
    add("data/scores.json", scores);

    let mut events = String::new();
    for i in 0..20 {
        let service = if i % 3 == 0 { "db" } else { "api" };
        let latency = 12 + (i * 17) % 31 + if service == "db" { 20 } else { 0 };
        events.push_str(&format!(
            "{{\"t\":\"2024-03-01T{:02}:{:02}:00Z\",\"service\":\"{service}\",\"latency\":{latency}}}\n",
            8 + i / 6,
            (i % 6) * 10
        ));
    }
    add("data/events.ndjson", events);

    // A European file: semicolons, decimal commas, and the labels NA (a country
    // code) and None, which are categories here and not missing values.
    add(
        "data/european.csv",
        "country;gdp;growth\nNA;1.234,5;2,5\nUS;21000;1,9\nNone;0,75;-0,5\nDE;3800;0,3\n".into(),
    );

    add(
        "data/semicolon.csv",
        "city;population;area\nOslo;709;454.0\nBergen;286;465.3\nTrondheim;212;342.2\nStavanger;144;71.0\nTromso;78;2521.3\nDrammen;102;137.7\n".into(),
    );
    add(
        "data/tabs.tsv",
        "team\tscore\nred\t12\nblue\t18\ngreen\t9\nyellow\t15\n".into(),
    );

    let mut dates = String::from("day,visits,signups\n");
    for i in 0..30u32 {
        let visits = 200 + i * 6 + (i * 37) % 41;
        let signups = 20 + (i * 5) % 17 + i / 3;
        dates.push_str(&format!("2024-04-{:02},{visits},{signups}\n", i + 1));
    }
    add("data/dates.csv", dates);

    add(
        "data/gaps.csv",
        "x,y,z\n1,10,5\n2,,6\n3,12,n/a\n4,unknown,8\n5,15,NA\n6,17,10\n7,18,11\n".into(),
    );
    add("data/looks.png", "k,v\nalpha,3\nbeta,5\ngamma,4\n".into());
    add("data/table", "k,v\nalpha,3\nbeta,5\ngamma,4\n".into());
    add(
        "data/objects.json",
        r#"[{"fruit":"apple","kg":12.5,"price":1.2},{"fruit":"pear","kg":8,"price":1.8},{"fruit":"plum","kg":null,"price":2.4},{"fruit":"fig","kg":3.25,"price":4.1}]"#
            .to_owned()
            + "\n",
    );

    let mut big = String::from("t,value\n");
    for i in 0..1800u32 {
        let value = (i * 37) % 101 + i / 40;
        big.push_str(&format!("{i},{value}\n"));
    }
    add("data/big_line.csv", big);

    let mut ragged = b"k,v,w\nalpha,1,2\nbeta,3\ngamma,4,5,6\ndelta\xff,7,8\n".to_vec();
    ragged.extend_from_slice(b"\nepsilon,9,10\n");
    out.push(("data/ragged.csv".to_owned(), ragged));

    out.push(("hostile/empty.csv".to_owned(), Vec::new()));
    out.push((
        "hostile/truncated.json".to_owned(),
        br#"{"columns":["a","b"],"rows":[[1,2],[3,"#.to_vec(),
    ));
    out.push((
        "hostile/garbage.json".to_owned(),
        b"{not json at all".to_vec(),
    ));
    out.push((
        "hostile/binary.csv".to_owned(),
        vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
        ],
    ));
    out.push(("hostile/header_only.csv".to_owned(), b"a,b\n".to_vec()));
    out.push((
        "hostile/text.csv".to_owned(),
        b"k,v\nalpha,x\nbeta,y\n".to_vec(),
    ));
    let mut long_cell = b"k,v\n".to_vec();
    long_cell.extend(std::iter::repeat_n(b'x', 70_000));
    out.push(("hostile/long_cell.csv".to_owned(), long_cell));
    out
}

/// Writes every fixture below `dir`.
pub fn write(dir: &Path) -> std::io::Result<()> {
    for (name, bytes) in files() {
        let path = dir.join(&name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
    }
    #[cfg(unix)]
    {
        let link = dir.join("hostile/link.csv");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("../data/sales.csv", link)?;
    }
    Ok(())
}

#[allow(dead_code)]
fn main() -> std::io::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"));
    write(&dir)
}
