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
use crate::csvparse::{MAX_RECORD_BYTES, Reader, Record};
use crate::names::{Taken, unique};
use crate::table::{
    BATCH_BYTES, BATCH_ROWS, Batches, Infer, SAMPLE_ROWS, ScanError, TableSource, Warnings,
    file_fingerprint,
};
use crate::typed::{Builder, ColType, Inferrer, classify};

const SNIFF_BYTES: u64 = 256 * 1024;
const SNIFF_ROWS: usize = 100;
const CANDIDATES: [u8; 4] = *b",;\t|";
/// The most columns a table may have. The SQL engine plans a wide table in time that grows
/// faster than its width (a `SELECT *` over 10,000 columns took minutes in the sandbox), so a
/// wider table is refused with a sentence instead of running into the wall-clock limit.
pub const MAX_COLUMNS: usize = 2_000;

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
        raw.as_chunks::<2>()
            .0
            .iter()
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
    let mut ended_cleanly = false;
    while rows.len() < SNIFF_ROWS {
        match reader.read(&mut rec) {
            Ok(true) => rows.push(own(&rec)),
            Ok(false) => {
                ended_cleanly = true;
                break;
            }
            // A quoted record cut by the end of the window never made it into `rows`.
            Err(_) => break,
        }
    }
    // Without the whole file, an unquoted last record may have been cut mid-row.
    if !complete && ended_cleanly {
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

/// Whether the first row of `rows` is a header: every non-empty cell is text and a later row
/// of some column is typed, or (when no cell is empty) the cells are distinct. A row with an
/// empty cell needs the typed evidence, because a data row of text with a gap looks the same.
pub fn detect_header(rows: &Rows) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    let filled: Vec<&[u8]> = first
        .iter()
        .map(|c| c.0.as_slice())
        .filter(|b| !b.is_empty())
        .collect();
    if filled.is_empty() || !filled.iter().all(|b| classify(b) == 0) {
        return false;
    }
    let typed_below = (0..first.len()).any(|c| {
        rows[1..]
            .iter()
            .filter_map(|r| r.get(c))
            .any(|(b, _)| !b.is_empty() && classify(b) != 0)
    });
    let distinct: HashSet<&[u8]> = filled.iter().copied().collect();
    typed_below || (filled.len() == first.len() && distinct.len() == first.len())
}

/// Column names from a header row: trimmed, empty ones numbered by position and repeats suffixed.
pub fn column_names(
    header: Option<&[(Vec<u8>, bool)]>,
    count: usize,
    warn: &Warnings,
) -> Vec<String> {
    let mut taken = Taken::default();
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
        let name = unique(&base, &mut taken);
        if name != base {
            renamed_dup += 1;
        }
        names.push(name);
    }
    if renamed_empty > 0 {
        warn.once(
            "empty-names",
            format!(
                "{renamed_empty} empty column {} named column_<position>",
                if renamed_empty == 1 {
                    "name was"
                } else {
                    "names were"
                }
            ),
        );
    }
    if renamed_dup > 0 {
        warn.once(
            "dup-names",
            format!(
                "{renamed_dup} repeated column {} a number added (name_2, name_3)",
                if renamed_dup == 1 {
                    "name got"
                } else {
                    "names got"
                }
            ),
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
        // The sniff window grows until it holds two whole records, so a long header row is
        // never cut: a table of many columns reads like any other.
        let (mut window, mut head) = (SNIFF_BYTES, Vec::new());
        let (delim, rows) = loop {
            head.clear();
            open_text(path)?
                .take(window)
                .read_to_end(&mut head)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let complete = (head.len() as u64) < window;
            let delim = opts
                .delimiter
                .unwrap_or_else(|| sniff_delimiter(&head, complete));
            let rows = sample_rows(&head, delim, complete);
            if complete || rows.len() >= 2 || window > MAX_RECORD_BYTES as u64 {
                break (delim, rows);
            }
            window *= 4;
        };
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
#[path = "csv_tests.rs"]
mod tests;
