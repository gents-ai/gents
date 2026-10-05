//! CSV, TSV and other delimited text: delimiter and header detection, type
//! inference and the streaming scan.
//!
//! The delimiter is the one among `,` `;` tab and `|` that splits the first
//! rows into the most consistent number of columns. The first row is a header
//! when every cell is text and either a later row of some column is typed or
//! the cells are distinct and non-empty. UTF-8 and UTF-16 (with a byte order
//! mark) are read. An empty field is NULL and an empty quoted field is the
//! empty string. Rows with too few fields are padded with NULL and rows with
//! too many lose the extra values, each said once in the warnings.
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{Field, Schema, SchemaRef};
use arrow::record_batch::{RecordBatch, RecordBatchOptions};

use crate::Res;
use crate::csvparse::{Reader, Record};
use crate::table::{
    BATCH_BYTES, BATCH_ROWS, Batches, Infer, SAMPLE_ROWS, ScanError, TableSource, Warnings,
    file_fingerprint,
};
use crate::typed::{Builder, ColType, Inferrer, classify};

const SNIFF_BYTES: u64 = 256 * 1024;
const SNIFF_ROWS: usize = 100;
const CANDIDATES: [u8; 4] = [b',', b';', b'\t', b'|'];
/// The most columns a table may have.
pub const MAX_COLUMNS: usize = 100_000;

/// What a caller may fix instead of detecting.
#[derive(Clone, Copy, Default, Debug)]
pub struct Options {
    /// The field delimiter.
    pub delimiter: Option<u8>,
    /// Whether the first row is a header.
    pub header: Option<bool>,
}

/// A delimited text file read as a table.
pub struct CsvTable {
    path: PathBuf,
    table: String,
    delim: u8,
    header: bool,
    schema: SchemaRef,
    types: Vec<ColType>,
    rows: Option<u64>,
    warn: Arc<Warnings>,
    fp: u64,
}

/// Opens `path` as UTF-8 text, converting UTF-16 when a byte order mark says so.
pub fn open_text(path: &Path) -> Res<Box<dyn Read + Send>> {
    let mut f = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut head = [0u8; 2];
    let n = f
        .read(&mut head)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let le = match head[..n] {
        [0xFF, 0xFE] => Some(true),
        [0xFE, 0xFF] => Some(false),
        _ => None,
    };
    match le {
        Some(le) => Ok(Box::new(Utf16 {
            inner: f,
            le,
            pending: Vec::new(),
            out: Vec::new(),
            at: 0,
        })),
        None => Ok(Box::new(Cursor::new(head[..n].to_vec()).chain(f))),
    }
}

/// Transcodes UTF-16 to UTF-8 as it is read; unpaired surrogates become U+FFFD.
struct Utf16 {
    inner: File,
    le: bool,
    /// Bytes of a unit or surrogate pair the last read ended inside.
    pending: Vec<u8>,
    out: Vec<u8>,
    at: usize,
}

impl Utf16 {
    fn units(&self, raw: &[u8]) -> Vec<u16> {
        raw.chunks_exact(2)
            .map(|p| {
                if self.le {
                    u16::from_le_bytes([p[0], p[1]])
                } else {
                    u16::from_be_bytes([p[0], p[1]])
                }
            })
            .collect()
    }
}

impl Read for Utf16 {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.out.len() {
            let mut raw = std::mem::take(&mut self.pending);
            let before = raw.len();
            let mut chunk = vec![0u8; 64 * 1024];
            let n = self.inner.read(&mut chunk)?;
            raw.extend_from_slice(&chunk[..n]);
            let at_end = n == 0;
            if !at_end {
                let odd = raw.len() % 2;
                let mut keep = raw.split_off(raw.len() - odd);
                if raw.len() >= 2 {
                    let last = self.units(&raw[raw.len() - 2..])[0];
                    if (0xD800..0xDC00).contains(&last) {
                        let mut lead = raw.split_off(raw.len() - 2);
                        lead.append(&mut keep);
                        keep = lead;
                    }
                }
                self.pending = keep;
            } else if before == 0 {
                return Ok(0);
            }
            let mut text: String = char::decode_utf16(self.units(&raw))
                .map(|r| r.unwrap_or('\u{FFFD}'))
                .collect();
            if at_end && raw.len() % 2 == 1 {
                text.push('\u{FFFD}');
            }
            self.out = text.into_bytes();
            self.at = 0;
            if at_end && self.out.is_empty() {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.out.len() - self.at);
        buf[..n].copy_from_slice(&self.out[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

type Rows = Vec<Vec<(Vec<u8>, bool)>>;

fn own(rec: &Record) -> Vec<(Vec<u8>, bool)> {
    (0..rec.len())
        .filter_map(|i| rec.field(i))
        .map(|(b, q)| (b.to_vec(), q))
        .collect()
}

/// Up to [`SNIFF_ROWS`] complete records of `bytes`; a cut or broken tail is dropped.
fn sample_rows(bytes: &[u8], delim: u8, complete: bool) -> Rows {
    let mut reader = Reader::new(Cursor::new(bytes), delim);
    let mut rec = Record::default();
    let mut rows = Rows::new();
    while rows.len() < SNIFF_ROWS && matches!(reader.read(&mut rec), Ok(true)) {
        rows.push(own(&rec));
    }
    // Without the whole file, the last record may have been cut mid-row.
    if !complete && rows.len() < SNIFF_ROWS {
        rows.pop();
    }
    rows
}

/// The candidate delimiter that splits the sample into the most consistent columns.
pub fn sniff_delimiter(bytes: &[u8], complete: bool) -> u8 {
    let mut best = (0usize, 0usize, b',');
    for d in CANDIDATES {
        let rows = sample_rows(bytes, d, complete);
        let mut freq: HashMap<usize, usize> = HashMap::new();
        for r in &rows {
            *freq.entry(r.len()).or_default() += 1;
        }
        let Some((&cols, &agree)) = freq.iter().max_by_key(|&(&c, &n)| (n, c)) else {
            continue;
        };
        if cols >= 2 && (agree, cols) > (best.0, best.1) {
            best = (agree, cols, d);
        }
    }
    best.2
}

/// Whether the first row of `rows` is a header.
pub fn detect_header(rows: &Rows) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    let texty = |cell: &(Vec<u8>, bool)| !cell.0.is_empty() && classify(&cell.0) == 0;
    if !first.iter().all(texty) {
        return false;
    }
    let typed_below = (0..first.len()).any(|c| {
        rows[1..]
            .iter()
            .filter_map(|r| r.get(c))
            .any(|(b, _)| !b.is_empty() && classify(b) != 0)
    });
    let distinct: HashSet<&[u8]> = first.iter().map(|c| c.0.as_slice()).collect();
    typed_below || distinct.len() == first.len()
}

/// Column names from a header row: trimmed, empty ones numbered by position and repeats suffixed.
pub fn column_names(
    header: Option<&[(Vec<u8>, bool)]>,
    count: usize,
    warn: &Warnings,
) -> Vec<String> {
    let mut taken: HashSet<String> = HashSet::new();
    let mut names = Vec::with_capacity(count);
    let (mut renamed_empty, mut renamed_dup) = (0usize, 0usize);
    for i in 0..count {
        let raw = header
            .and_then(|h| h.get(i))
            .map(|(b, _)| String::from_utf8_lossy(b).trim().to_string())
            .unwrap_or_default();
        let base = if raw.is_empty() {
            if header.is_some() {
                renamed_empty += 1;
            }
            format!("column_{}", i + 1)
        } else {
            raw
        };
        let mut name = base.clone();
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        if name != base {
            renamed_dup += 1;
        }
        taken.insert(name.clone());
        names.push(name);
    }
    if renamed_empty > 0 {
        warn.once(
            "empty-names",
            format!("{renamed_empty} empty column names were replaced by column_<position>"),
        );
    }
    if renamed_dup > 0 {
        warn.once(
            "dup-names",
            format!("{renamed_dup} repeated column names were numbered (name_2, name_3)"),
        );
    }
    names
}

fn ragged_warning(width: usize, line: u64, short: bool) -> String {
    if short {
        format!("rows with fewer than {width} fields were padded with NULL (first on line {line})")
    } else {
        format!("rows with more than {width} fields lost their extra values (first on line {line})")
    }
}

impl CsvTable {
    /// Reads the head of `path` and infers its layout and types.
    pub fn open(
        path: &Path,
        table: &str,
        opts: Options,
        infer: Infer,
        warn: &Arc<Warnings>,
    ) -> Res<Self> {
        let mut head = Vec::new();
        open_text(path)?
            .take(SNIFF_BYTES)
            .read_to_end(&mut head)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let complete = (head.len() as u64) < SNIFF_BYTES;
        let delim = opts
            .delimiter
            .unwrap_or_else(|| sniff_delimiter(&head, complete));
        let rows = sample_rows(&head, delim, complete);
        if rows.is_empty() {
            return Err(format!("{} has no rows to read", path.display()));
        }
        let header = opts.header.unwrap_or_else(|| detect_header(&rows));
        let width = if header {
            rows[0].len()
        } else {
            rows.iter().map(Vec::len).max().unwrap_or(1)
        };
        if width > MAX_COLUMNS {
            return Err(format!(
                "{} has {width} columns, over the {MAX_COLUMNS} one table may have",
                path.display()
            ));
        }
        let names = column_names(header.then(|| rows[0].as_slice()), width, warn);
        let mut inf = Inferrer::new(width);
        let mut reader = Reader::new(open_text(path)?, delim);
        let mut rec = Record::default();
        let (mut count, mut skip, mut at_end) = (0u64, header, true);
        while reader.read(&mut rec)? {
            if skip {
                skip = false;
                continue;
            }
            if infer == Infer::Sample && count >= SAMPLE_ROWS {
                at_end = false;
                break;
            }
            count += 1;
            if rec.len() != width {
                warn.once("ragged", ragged_warning(width, rec.line, rec.len() < width));
            }
            for c in 0..width.min(rec.len()) {
                if let Some((b, _)) = rec.field(c) {
                    // An empty field is NULL, and a quoted empty one is NULL in a typed
                    // column and the empty string in a text column: neither sets the type.
                    if !b.is_empty() {
                        inf.observe(c, classify(b));
                    }
                }
            }
        }
        let types = inf.types();
        let fields: Vec<Field> = names
            .iter()
            .zip(&types)
            .map(|(n, t)| Field::new(n, t.data_type(), true))
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            table: table.to_string(),
            delim,
            header,
            schema: Arc::new(Schema::new(fields)),
            types,
            rows: at_end.then_some(count),
            warn: Arc::clone(warn),
            fp: file_fingerprint(path)?,
        })
    }
}

impl TableSource for CsvTable {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn row_count(&self) -> Option<u64> {
        self.rows
    }

    fn fingerprint(&self) -> u64 {
        self.fp
    }

    fn details(&self) -> Option<serde_json::Value> {
        let delimiter = (self.delim as char).to_string();
        Some(serde_json::json!({"delimiter": delimiter, "header": self.header}))
    }

    fn scan(&self, projection: Option<&[usize]>) -> Batches {
        let all: Vec<usize> = (0..self.types.len()).collect();
        let columns = projection.map_or(all, <[usize]>::to_vec);
        let fields: Vec<Field> = columns
            .iter()
            .map(|&c| self.schema.field(c).clone())
            .collect();
        let reader = match open_text(&self.path) {
            Ok(r) => r,
            Err(e) => return Box::new(std::iter::once(Err(ScanError::Failed(e)))),
        };
        Box::new(Scan {
            reader: Reader::new(reader, self.delim),
            rec: Record::default(),
            skip: self.header,
            table: self.table.clone(),
            schema: Arc::new(Schema::new(fields)),
            types: columns.iter().map(|&c| self.types[c]).collect(),
            names: columns
                .iter()
                .map(|&c| self.schema.field(c).name().clone())
                .collect(),
            columns,
            width: self.types.len(),
            warn: Arc::clone(&self.warn),
            done: false,
        })
    }
}

struct Scan {
    reader: Reader<Box<dyn Read + Send>>,
    rec: Record,
    skip: bool,
    table: String,
    schema: SchemaRef,
    types: Vec<ColType>,
    names: Vec<String>,
    columns: Vec<usize>,
    width: usize,
    warn: Arc<Warnings>,
    done: bool,
}

impl Scan {
    fn batch(&mut self) -> Result<Option<RecordBatch>, ScanError> {
        let mut builders: Vec<Builder> = self
            .types
            .iter()
            .map(|&t| Builder::new(t, BATCH_ROWS))
            .collect();
        let (mut rows, mut bytes) = (0usize, 0usize);
        while rows < BATCH_ROWS && bytes < BATCH_BYTES {
            if !self.reader.read(&mut self.rec)? {
                self.done = true;
                break;
            }
            if self.skip {
                self.skip = false;
                continue;
            }
            let rec = &self.rec;
            if rec.len() != self.width {
                self.warn.once(
                    "ragged",
                    ragged_warning(self.width, rec.line, rec.len() < self.width),
                );
            }
            for (slot, builder) in builders.iter_mut().enumerate() {
                let Some((b, quoted)) = rec.field(self.columns[slot]) else {
                    builder.null();
                    continue;
                };
                if b.is_empty() && (!quoted || self.types[slot] != ColType::Text) {
                    builder.null();
                } else if self.types[slot] == ColType::Text {
                    let stored = match String::from_utf8_lossy(b) {
                        std::borrow::Cow::Borrowed(s) => builder.text(s),
                        std::borrow::Cow::Owned(s) => {
                            self.warn.once(
                                "utf8",
                                "some text was not valid UTF-8 and the bad bytes were replaced"
                                    .into(),
                            );
                            builder.text(&s)
                        }
                    };
                    stored.map_err(|_| ScanError::Failed("text could not be stored".into()))?;
                } else if builder.token(b).is_err() {
                    return Err(ScanError::Conflict {
                        table: self.table.clone(),
                        column: self.names[slot].clone(),
                        line: rec.line,
                    });
                }
            }
            rows += 1;
            bytes += rec.bytes();
        }
        if rows == 0 {
            return Ok(None);
        }
        let columns = builders.iter_mut().map(Builder::finish).collect();
        let options = RecordBatchOptions::new().with_row_count(Some(rows));
        RecordBatch::try_new_with_options(Arc::clone(&self.schema), columns, &options)
            .map(Some)
            .map_err(|e| ScanError::Failed(format!("building rows failed: {e}")))
    }
}

impl Iterator for Scan {
    type Item = Result<RecordBatch, ScanError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.batch() {
            Ok(Some(b)) => Some(Ok(b)),
            Ok(None) => None,
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
                "2 repeated column names were numbered (name_2, name_3)",
                "1 empty column names were replaced by column_<position>"
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
    fn ten_thousand_columns_are_one_table_and_a_million_are_refused() {
        let width = 10_000;
        let header: Vec<String> = (0..width).map(|i| format!("c{i}")).collect();
        let row: Vec<String> = (0..width).map(|i| i.to_string()).collect();
        let (t, _, _d) = open(&format!("{}\n{}\n", header.join(","), row.join(",")));
        assert_eq!(columns(&t).len(), width);
        assert_eq!(
            collect(&t, Some(&[9999, 0])).unwrap(),
            vec![vec![json!(9999), json!(0)]]
        );
        let dir = Dir::new();
        let p = dir.put("w.csv", format!("{}\n", ",".repeat(MAX_COLUMNS)));
        let err = CsvTable::open(&p, "w", Options::default(), Infer::Sample, &Warnings::new())
            .err()
            .unwrap();
        assert!(err.contains("one table may have"), "{err}");
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
}
