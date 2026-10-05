//! Finds the data a request names and reads it into a [`Table`]: inline JSON
//! or CSV text, or a file below the bound folder. A named file is resolved
//! with every component checked, so `..`, absolute paths and symbolic links
//! cannot leave the folder the call was granted.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};

use crate::csvio;
use crate::err::{fail, Res};
use crate::jsonio;
use crate::table::{Builder, Table};

const SNIFF_BYTES: usize = 8192;

/// Resolves `rel` below `root` and returns the path, refusing anything that
/// leaves `root`: absolute paths, `..`, backslashes and symbolic links.
pub fn resolve(root: &Path, rel: &str) -> Res<PathBuf> {
    if rel.trim().is_empty() {
        return fail("the file name is empty; name a file inside the folder");
    }
    if rel.contains(['\0', '\\']) || rel.starts_with('/') || rel.as_bytes().get(1) == Some(&b':') {
        return fail(format!("{rel:?} is not a path inside the folder; use a relative path like data/sales.csv"));
    }
    let mut path = root.to_path_buf();
    for part in Path::new(rel).components() {
        match part {
            Component::Normal(name) => {
                path.push(name);
                if let Ok(meta) = std::fs::symlink_metadata(&path) {
                    if meta.file_type().is_symlink() {
                        return fail(format!("{rel:?} goes through a symbolic link; name the real file inside the folder"));
                    }
                }
            }
            Component::CurDir => {}
            _ => return fail(format!("{rel:?} leaves the folder; use a path inside it")),
        }
    }
    Ok(path)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Csv,
    Json,
    Lines,
}

fn format_of(path: &Path, head: &[u8]) -> Res<Format> {
    if head.contains(&0) {
        return fail("the file looks binary, not CSV or JSON data; give a .csv or .json file");
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    Ok(match ext.as_str() {
        "csv" | "tsv" => Format::Csv,
        "json" => Format::Json,
        "jsonl" | "ndjson" => Format::Lines,
        _ => {
            let body = head.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(head);
            match body.iter().find(|b| !b.is_ascii_whitespace()) {
                Some(b'[' | b'{') => Format::Json,
                _ => Format::Csv,
            }
        }
    })
}

/// Reads the data file at `path` into a table with the `keep` columns.
pub fn file(path: &Path, keep: Option<Vec<String>>) -> Res<Table> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{} does not exist; check the file name", name_of(path)),
        _ => format!("{} cannot be read: {e}", name_of(path)),
    })?;
    if !meta.is_file() {
        return fail(format!("{} is a folder; name a CSV or JSON file", name_of(path)));
    }
    if meta.len() == 0 {
        return fail(format!("{} is empty; it needs a header row and data", name_of(path)));
    }
    let f = File::open(path).map_err(|e| format!("{} cannot be opened: {e}", name_of(path)))?;
    let mut r = BufReader::with_capacity(64 * 1024, f);
    let head = r.fill_buf().map_err(|e| format!("{} cannot be read: {e}", name_of(path)))?;
    let head = &head[..head.len().min(SNIFF_BYTES)];
    let fmt = format_of(path, head)?;
    let mut builder = Builder::new(keep);
    match fmt {
        Format::Csv => csvio::read(r, &mut builder)?,
        Format::Json => jsonio::read_reader(skip_bom(r)?, &mut builder)?,
        Format::Lines => jsonio::read_lines(r, &mut builder)?,
    }
    Ok(builder.finish())
}

fn skip_bom<R: BufRead>(mut r: R) -> Res<R> {
    let bom = r.fill_buf().map_err(|e| format!("the file cannot be read: {e}"))?.starts_with(&[0xef, 0xbb, 0xbf]);
    if bom {
        r.consume(3);
    }
    Ok(r)
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| "the file".into(), |n| n.to_string_lossy().into_owned())
}

/// Reads inline data: JSON text, or CSV text when it does not start with `[`
/// or `{`.
pub fn inline(text: &str, keep: Option<Vec<String>>) -> Res<Table> {
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut builder = Builder::new(keep);
    match body.trim_start().as_bytes().first() {
        None => return fail("data is empty; send rows or a CSV with a header line"),
        Some(b'[' | b'{') => jsonio::read_str(body, &mut builder)?,
        Some(_) => csvio::read(body.as_bytes(), &mut builder)?,
    }
    Ok(builder.finish())
}

/// Reads at most `limit` bytes of `r`, to size-check inline input.
pub fn read_limited<R: Read>(r: R, limit: usize) -> Res<String> {
    let mut buf = Vec::new();
    r.take(limit as u64 + 1).read_to_end(&mut buf).map_err(|e| format!("reading the request failed: {e}"))?;
    if buf.len() > limit {
        return fail(format!("the request is larger than {} MiB; send a file in a folder instead of inline data", limit / (1024 * 1024)));
    }
    String::from_utf8(buf).or_else(|_| fail("the request is not valid UTF-8 text"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Cell;
    use crate::testutil::TempDir;

    #[test]
    fn resolve_joins_plain_relative_paths() {
        let d = TempDir::new();
        d.write("sub/a.csv", b"x");
        assert_eq!(resolve(d.path(), "sub/a.csv").unwrap(), d.path().join("sub/a.csv"));
        assert_eq!(resolve(d.path(), "./a.csv").unwrap(), d.path().join("a.csv"));
    }

    #[test]
    fn resolve_refuses_every_way_out_of_the_folder() {
        let d = TempDir::new();
        for bad in ["../x.csv", "a/../../x.csv", "/etc/passwd", "..", "a/..", "C:/x.csv", "a\\b.csv", "a\0b", "", "   "] {
            let e = resolve(d.path(), bad).unwrap_err().0;
            assert!(!e.is_empty() && !e.contains('\n'), "{bad:?}: {e}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_refuses_symbolic_links_to_files_and_folders() {
        let d = TempDir::new();
        let outside = TempDir::new();
        outside.write("secret.csv", b"a\n1\n");
        std::os::unix::fs::symlink(outside.path().join("secret.csv"), d.path().join("link.csv")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("dir")).unwrap();
        assert!(resolve(d.path(), "link.csv").unwrap_err().0.contains("symbolic link"));
        assert!(resolve(d.path(), "dir/secret.csv").unwrap_err().0.contains("symbolic link"));
    }

    #[test]
    fn a_csv_file_loads_by_extension() {
        let d = TempDir::new();
        let p = d.write("s.csv", b"a,b\n1,x\n2,y\n");
        let t = file(&p, None).unwrap();
        assert_eq!((t.rows, t.names.len()), (2, 2));
    }

    #[test]
    fn json_json_lines_and_sniffed_files_load() {
        let d = TempDir::new();
        let j = file(&d.write("a.json", br#"[{"a":1},{"a":2}]"#), None).unwrap();
        let l = file(&d.write("b.jsonl", b"{\"a\":1}\n{\"a\":2}\n"), None).unwrap();
        let n = file(&d.write("c.ndjson", b"{\"a\":1}\n"), None).unwrap();
        let sniffed_json = file(&d.write("d.dat", br#"{"columns":["a"],"rows":[[1],[2],[3]]}"#), None).unwrap();
        let sniffed_csv = file(&d.write("e.txt", b"a,b\n1,2\n"), None).unwrap();
        let bom = file(&d.write("f.json", b"\xef\xbb\xbf[{\"a\":1}]"), None).unwrap();
        assert_eq!((j.rows, l.rows, n.rows, sniffed_json.rows, sniffed_csv.rows, bom.rows), (2, 2, 1, 3, 1, 1));
    }

    #[test]
    fn a_wrong_extension_does_not_decide_the_format_of_an_unknown_extension() {
        let d = TempDir::new();
        let t = file(&d.write("data.png", b"a,b\n1,2\n"), None).unwrap();
        assert_eq!(t.names, ["a", "b"]);
    }

    #[test]
    fn binary_files_are_refused_whatever_their_name() {
        let d = TempDir::new();
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R'];
        for name in ["x.csv", "x.json", "x.png", "x"] {
            let e = file(&d.write(name, &png), None).unwrap_err().0;
            assert!(e.contains("binary"), "{name}: {e}");
        }
    }

    #[test]
    fn empty_missing_and_folder_paths_are_refused_with_the_name() {
        let d = TempDir::new();
        let empty = d.write("empty.csv", b"");
        assert!(file(&empty, None).unwrap_err().0.contains("empty.csv is empty"));
        assert!(file(&d.path().join("nope.csv"), None).unwrap_err().0.contains("nope.csv does not exist"));
        std::fs::create_dir(d.path().join("folder")).unwrap();
        assert!(file(&d.path().join("folder"), None).unwrap_err().0.contains("is a folder"));
    }

    #[test]
    fn a_json_file_that_is_not_json_says_so() {
        let d = TempDir::new();
        let p = d.write("bad.json", b"a,b\n1,2\n");
        assert!(file(&p, None).unwrap_err().0.contains("JSON"));
    }

    #[test]
    fn inline_text_is_json_or_csv() {
        assert_eq!(inline(r#"[{"a":1}]"#, None).unwrap().rows, 1);
        assert_eq!(inline("  {\"columns\":[\"a\"],\"rows\":[[1]]}", None).unwrap().rows, 1);
        let t = inline("a,b\n1,2\n3,4\n", None).unwrap();
        assert_eq!(t.rows, 2);
        assert_eq!(t.cols[0][1], Cell::Num(3.0));
        assert!(inline("   ", None).unwrap_err().0.contains("empty"));
        assert_eq!(inline("\u{feff}a\n1\n", None).unwrap().names, ["a"]);
    }

    #[test]
    fn read_limited_refuses_input_over_the_limit() {
        assert_eq!(read_limited(&b"abc"[..], 3).unwrap(), "abc");
        assert!(read_limited(&b"abcd"[..], 3).unwrap_err().0.contains("larger than"));
        assert!(read_limited(&b"\xff\xfe"[..], 10).unwrap_err().0.contains("UTF-8"));
    }

    #[test]
    fn projection_reaches_the_file_readers() {
        let d = TempDir::new();
        let p = d.write("s.csv", b"a,b,c\n1,2,3\n");
        let t = file(&p, Some(vec!["c".into()])).unwrap();
        assert_eq!(t.names, ["c"]);
        let q = d.write("s.json", br#"[{"a":1,"c":3}]"#);
        assert_eq!(file(&q, Some(vec!["c".into()])).unwrap().names, ["c"]);
    }
}
