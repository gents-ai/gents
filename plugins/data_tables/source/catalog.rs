//! The tables a call can see: every readable file below the bound path (or the
//! files named), every sheet of every workbook, and the inline tables of the
//! request. Tables are listed cheaply and opened only when a query names them.
//!
//! A table is named after its file's path without the extension (`sales.csv`
//! is `sales`, `2024/q1.csv` is `t_2024_q1`), a sheet after its file and its
//! name (`report_Summary`). Names are made of letters, digits and underscores,
//! and a name taken twice gets `_2`, `_3`; every renaming is a warning.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::csv::{CsvTable, Options};
use crate::inline::{InlineTable, parse_tables};
use crate::json::JsonTable;
use crate::names::{Taken, sanitize, unique};
use crate::parquet_src::ParquetTable;
use crate::sheet::SheetTable;
use crate::table::{Fnv, Infer, TableSource, Warnings, file_fingerprint};
use crate::zipread::Zip;
use crate::{Res, ods, xlsx};

/// Files listed below a folder at most.
pub const MAX_FILES: usize = 10_000;
const MAX_DEPTH: usize = 16;

/// A kind of file a table can come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    /// Delimited text.
    Csv,
    /// JSON records.
    Json,
    /// Parquet.
    Parquet,
    /// Excel workbook.
    Xlsx,
    /// OpenDocument spreadsheet.
    Ods,
}

impl Fmt {
    fn name(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Parquet => "parquet",
            Self::Xlsx => "xlsx",
            Self::Ods => "ods",
        }
    }
}

enum Kind {
    Csv(PathBuf),
    Json(PathBuf),
    Parquet(PathBuf),
    Xlsx(Arc<xlsx::Book>, xlsx::SheetRef),
    Ods(Arc<ods::Book>, String),
    Inline(Arc<InlineTable>),
}

/// One table that can be opened.
pub struct Spec {
    /// The name SQL uses.
    pub name: String,
    /// The file it comes from, relative to the bound folder; empty for an inline table.
    pub source: String,
    /// The sheet, for a workbook.
    pub sheet: Option<String>,
    /// The kind of source.
    pub format: &'static str,
    /// How the name was made, when it is not the file's own: said when the table is used.
    note: Option<String>,
    kind: Kind,
}

/// What a call can read.
pub struct Catalog {
    specs: Vec<Spec>,
    index: BTreeMap<String, usize>,
    opts: Options,
    /// What the call noticed about its data.
    pub warn: Arc<Warnings>,
    /// What listing the files noticed: files skipped and caps reached.
    pub listing: Vec<String>,
    full: Mutex<HashSet<String>>,
    opened: Mutex<HashMap<String, Arc<dyn TableSource>>>,
}

/// Why a file is not a table.
fn unsupported(rel: &str, why: &str) -> String {
    format!("{rel}: {why}")
}

fn head(path: &Path) -> Res<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(4096).read_to_end(&mut buf))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(buf)
}

/// Whether the file ends with the Parquet magic, so a CSV that merely starts with `PAR1,` is not one.
fn ends_with_par1(path: &Path) -> bool {
    use std::io::{Seek, SeekFrom};
    let mut tail = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| {
            f.seek(SeekFrom::End(-4))?;
            f.read_exact(&mut tail)
        })
        .is_ok()
        && &tail == b"PAR1"
}

/// Whether `head` opens an Arrow IPC file (`ARROW1` and two zero bytes) or stream (a
/// continuation marker and a plausible message length), not text that starts alike.
fn is_arrow(head: &[u8]) -> bool {
    if head.starts_with(b"ARROW1\0\0") {
        return true;
    }
    head.starts_with(&[0xFF, 0xFF, 0xFF, 0xFF])
        && head
            .get(4..8)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .is_some_and(|b| u32::from_le_bytes(b) <= 64 * 1024 * 1024)
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// The format of the file at `path` by its content (and, among text formats, its name).
/// `strict` is true while walking a folder, where a file of an unknown name is skipped
/// instead of guessed at.
fn detect(path: &Path, strict: bool) -> Res<Result<Fmt, String>> {
    let head = head(path)?;
    let ext = extension(path);
    // A name that says Parquet is trusted (a damaged file then fails with its own reason); any
    // other name needs the closing magic too, so a CSV that starts `PAR1,` stays text.
    if head.starts_with(b"PAR1")
        && (matches!(ext.as_str(), "parquet" | "pq") || ends_with_par1(path))
    {
        return Ok(Ok(Fmt::Parquet));
    }
    if is_arrow(&head) {
        return Ok(Err(
            "Arrow files are not supported; save the data as Parquet or CSV".into(),
        ));
    }
    if head.starts_with(b"PK\x03\x04") {
        let mut zip = match Zip::open(path) {
            Ok(z) => z,
            Err(e) => return Ok(Err(e)),
        };
        if zip.has("xl/workbook.xml") {
            return Ok(Ok(Fmt::Xlsx));
        }
        let ods = zip.has("content.xml")
            && zip
                .read("mimetype", 1024)
                .is_ok_and(|m| m.starts_with(b"application/vnd.oasis.opendocument.spreadsheet"));
        return Ok(if ods {
            Ok(Fmt::Ods)
        } else {
            Err("a ZIP container that is not a spreadsheet".to_string())
        });
    }
    match ext.as_str() {
        "parquet" | "pq" => {
            return Ok(Err(
                "named like Parquet but does not start like a Parquet file".into(),
            ));
        }
        "xlsx" | "xlsm" | "ods" => {
            return Ok(Err(
                "named like a spreadsheet but is not a spreadsheet file".into(),
            ));
        }
        "arrow" | "feather" | "ipc" => {
            return Ok(Err(
                "Arrow files are not supported; save the data as Parquet or CSV".into(),
            ));
        }
        _ => {}
    }
    let utf16 = head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]);
    if !utf16 && head.contains(&0) {
        return Ok(Err(
            "a binary file that is not a supported data format".into()
        ));
    }
    Ok(match ext.as_str() {
        "csv" | "tsv" | "tab" | "txt" | "psv" => Ok(Fmt::Csv),
        "json" | "jsonl" | "ndjson" => Ok(Fmt::Json),
        _ if strict => Err("not a supported data file name".into()),
        _ => {
            let first = head
                .iter()
                .find(|b| !b.is_ascii_whitespace() && **b != 0xEF && **b != 0xBB && **b != 0xBF);
            if matches!(first, Some(b'[' | b'{')) {
                Ok(Fmt::Json)
            } else {
                Ok(Fmt::Csv)
            }
        }
    })
}

/// The path `rel` under `root`, refused when it names something outside it.
pub fn safe_join(root: &Path, rel: &str) -> Res<PathBuf> {
    let bad = || {
        format!(
            "{rel:?} is not a path inside the folder; give a relative path without .. or a leading /"
        )
    };
    if rel.is_empty() || rel.contains('\0') {
        return Err(bad());
    }
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(bad());
    }
    let joined = root.join(p);
    if let (Ok(r), Ok(j)) = (std::fs::canonicalize(root), std::fs::canonicalize(&joined))
        && !j.starts_with(&r)
    {
        return Err(format!(
            "{rel} is a link that leads outside the folder and is not read"
        ));
    }
    Ok(joined)
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>, notes: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        notes.push(unsupported(&rel_of(root, dir), "the folder cannot be read"));
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let rel = rel_of(root, &path);
        let Ok(link) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if link.is_dir() {
            if depth >= MAX_DEPTH {
                notes.push(unsupported(&rel, "folders nest too deep to list"));
            } else {
                walk(root, &path, depth + 1, out, notes);
            }
        } else if link.is_symlink() {
            if safe_join(root, &rel).is_err() || std::fs::metadata(&path).is_ok_and(|m| m.is_dir())
            {
                notes.push(unsupported(
                    &rel,
                    "a link that leads outside the folder or to a folder, not read",
                ));
            } else {
                out.push(rel);
            }
        } else if link.is_file() {
            out.push(rel);
        }
    }
}

fn rel_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn stem(rel: &str) -> String {
    let p = Path::new(rel);
    let without = p.with_extension("");
    let s: Vec<String> = without
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    s.join("_")
}

impl Catalog {
    /// Lists the tables of `path` (a file or a folder, `files` picking inside a folder) and of the
    /// inline `tables`. Nothing is read beyond what naming the tables takes.
    pub fn discover(
        path: Option<&str>,
        files: Option<&[String]>,
        tables: Option<&Value>,
        opts: Options,
    ) -> Res<Self> {
        let warn = Warnings::new();
        let mut cat = Self {
            specs: Vec::new(),
            index: BTreeMap::new(),
            opts,
            warn: Arc::clone(&warn),
            listing: Vec::new(),
            full: Mutex::default(),
            opened: Mutex::default(),
        };
        let mut taken = Taken::default();
        let mut notes: Vec<String> = Vec::new();
        if let Some(path) = path {
            let root = Path::new(path);
            let meta = std::fs::metadata(root).map_err(|e| {
                format!(
                    "cannot read {path}: {e}; the path must be a file or folder this tool may read"
                )
            })?;
            crate::table::set_root(if meta.is_file() {
                root.parent().unwrap_or(root)
            } else {
                root
            });
            let (base, rels, strict) = if meta.is_file() {
                if files.is_some() {
                    return Err("files lists paths inside a folder, but path is a file".into());
                }
                let name = root
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                (
                    root.parent().unwrap_or(Path::new("")).to_path_buf(),
                    vec![name],
                    false,
                )
            } else if let Some(files) = files {
                (root.to_path_buf(), files.to_vec(), false)
            } else {
                let mut rels = Vec::new();
                walk(root, root, 0, &mut rels, &mut notes);
                (root.to_path_buf(), rels, true)
            };
            if rels.len() > MAX_FILES {
                cat.listing.push(format!("only the first {MAX_FILES} of {} files are listed; name the files to read in files", rels.len()));
            }
            for rel in rels.into_iter().take(MAX_FILES) {
                let full = if meta.is_file() {
                    root.to_path_buf()
                } else {
                    safe_join(&base, &rel)?
                };
                let detected = match detect(&full, strict) {
                    Ok(d) => d,
                    // A file in a listing that cannot be opened (a link out of the folder, a
                    // permission) is skipped and said; one that was asked for is an error.
                    Err(why) if strict => {
                        notes.push(unsupported(&rel, &why));
                        continue;
                    }
                    Err(why) => return Err(why),
                };
                match detected {
                    Err(why) if strict => notes.push(unsupported(&rel, &why)),
                    Err(why) => return Err(format!("{rel} cannot be read as a table: {why}")),
                    Ok(fmt) => cat.add_file(&rel, &full, fmt, &mut taken, &mut notes, !strict)?,
                }
            }
        }
        if let Some(v) = tables {
            for (name, t) in parse_tables(v, &warn)? {
                let name = unique(&name, &mut taken);
                cat.push(Spec {
                    name,
                    note: None,
                    source: String::new(),
                    sheet: None,
                    format: "inline",
                    kind: Kind::Inline(Arc::new(t)),
                });
            }
        }
        if !notes.is_empty() {
            let shown: Vec<String> = notes.iter().take(5).cloned().collect();
            let more = notes.len().saturating_sub(5);
            let tail = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            cat.listing.push(format!(
                "{} files were skipped: {}{tail}",
                notes.len(),
                shown.join("; ")
            ));
        }
        Ok(cat)
    }

    fn push(&mut self, spec: Spec) {
        self.index.insert(spec.name.clone(), self.specs.len());
        self.specs.push(spec);
    }

    fn name_for(raw: String, rel: &str, taken: &mut Taken) -> (String, Option<String>) {
        let name = unique(&sanitize(&raw), taken);
        let note = (name != raw).then(|| format!("{rel} is the table {name}"));
        (name, note)
    }

    fn add_file(
        &mut self,
        rel: &str,
        full: &Path,
        fmt: Fmt,
        taken: &mut Taken,
        notes: &mut Vec<String>,
        explicit: bool,
    ) -> Res<()> {
        match fmt {
            Fmt::Xlsx => {
                let book = match xlsx::Book::open(full) {
                    Ok(b) => Arc::new(b),
                    Err(e) if !explicit => {
                        notes.push(unsupported(rel, &e));
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                for sheet in book.sheets.clone() {
                    let (name, note) =
                        Self::name_for(format!("{}_{}", stem(rel), sheet.name), rel, taken);
                    self.push(Spec {
                        name,
                        note,
                        source: rel.to_string(),
                        sheet: Some(sheet.name.clone()),
                        format: fmt.name(),
                        kind: Kind::Xlsx(Arc::clone(&book), sheet),
                    });
                }
            }
            Fmt::Ods => {
                let book = match ods::Book::open(full) {
                    Ok(b) => Arc::new(b),
                    Err(e) if !explicit => {
                        notes.push(unsupported(rel, &e));
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                for sheet in book.sheets.clone() {
                    let (name, note) =
                        Self::name_for(format!("{}_{}", stem(rel), sheet), rel, taken);
                    self.push(Spec {
                        name,
                        note,
                        source: rel.to_string(),
                        sheet: Some(sheet.clone()),
                        format: fmt.name(),
                        kind: Kind::Ods(Arc::clone(&book), sheet),
                    });
                }
            }
            Fmt::Csv | Fmt::Json | Fmt::Parquet => {
                let path = full.to_path_buf();
                let kind = match fmt {
                    Fmt::Csv => Kind::Csv(path),
                    Fmt::Json => Kind::Json(path),
                    _ => Kind::Parquet(path),
                };
                let (name, note) = Self::name_for(stem(rel), rel, taken);
                self.push(Spec {
                    name,
                    note,
                    source: rel.to_string(),
                    sheet: None,
                    format: fmt.name(),
                    kind,
                });
            }
        }
        Ok(())
    }

    /// The tables, in listing order.
    pub fn specs(&self) -> &[Spec] {
        &self.specs
    }

    /// The table named `name`.
    pub fn spec(&self, name: &str) -> Option<&Spec> {
        self.index.get(name).map(|&i| &self.specs[i])
    }

    /// Whether `name` is read in full to infer its types.
    fn infer_of(&self, name: &str) -> Infer {
        if self.full.lock().is_ok_and(|f| f.contains(name)) {
            Infer::Full
        } else {
            Infer::Sample
        }
    }

    /// Reads every row of `name` to infer its types, the next time it is opened.
    /// False when it already was, so a second conflict is a real error.
    pub fn widen(&self, name: &str) -> bool {
        let fresh = self
            .full
            .lock()
            .is_ok_and(|mut f| f.insert(name.to_string()));
        if fresh && let Ok(mut o) = self.opened.lock() {
            o.remove(name);
        }
        fresh
    }

    /// Reads every row of each opened table whose types came from a sample, the next time it is
    /// opened, so that every page of a paged result has the same column types. False when no
    /// opened table was sampled.
    pub fn settle(&self) -> bool {
        let sampled: Vec<String> = self.opened.lock().map_or_else(
            |_| Vec::new(),
            |o| {
                o.iter()
                    .filter(|(_, t)| t.row_count().is_none())
                    .map(|(n, _)| n.clone())
                    .collect()
            },
        );
        sampled.iter().fold(false, |any, n| self.widen(n) | any)
    }

    /// The names of the tables read in full to infer their types, in name order.
    pub fn full_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .full
            .lock()
            .map(|f| f.iter().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Opens the table `name`, once; later calls return the same table.
    pub fn open(&self, name: &str) -> Res<Arc<dyn TableSource>> {
        if let Some(t) = self.opened.lock().ok().and_then(|o| o.get(name).cloned()) {
            return Ok(t);
        }
        let spec = self
            .spec(name)
            .ok_or_else(|| format!("there is no table named {name}"))?;
        if let Some(note) = &spec.note {
            self.warn.once(&format!("name-{name}"), note.clone());
        }
        let infer = self.infer_of(name);
        let table: Arc<dyn TableSource> = match &spec.kind {
            Kind::Csv(path) => Arc::new(CsvTable::open(path, name, self.opts, infer, &self.warn)?),
            Kind::Json(path) => Arc::new(JsonTable::open(path, name, infer, &self.warn)?),
            Kind::Parquet(path) => Arc::new(ParquetTable::open(path)?),
            Kind::Xlsx(book, sheet) => {
                let (book, sheet, warn) = (Arc::clone(book), sheet.clone(), Arc::clone(&self.warn));
                let fp = file_fingerprint(&book.path)? ^ sheet_hash(&sheet.name);
                let opener: crate::sheet::Opener = {
                    let (book, sheet, warn) = (Arc::clone(&book), sheet.clone(), Arc::clone(&warn));
                    Box::new(move || book.rows(&sheet, &warn))
                };
                Arc::new(SheetTable::open(opener, name, infer, fp, &warn)?)
            }
            Kind::Ods(book, sheet) => {
                let warn = Arc::clone(&self.warn);
                let fp = file_fingerprint(book.path())? ^ sheet_hash(sheet);
                let opener: crate::sheet::Opener = {
                    let (book, sheet, warn) = (Arc::clone(book), sheet.clone(), Arc::clone(&warn));
                    Box::new(move || book.rows(&sheet, &warn))
                };
                Arc::new(SheetTable::open(opener, name, infer, fp, &warn)?)
            }
            Kind::Inline(t) => Arc::clone(t) as Arc<dyn TableSource>,
        };
        if let Ok(mut o) = self.opened.lock() {
            o.insert(name.to_string(), Arc::clone(&table));
        }
        Ok(table)
    }

    /// A hash of the names and contents of the tables opened so far, in name order.
    pub fn opened_fingerprint(&self) -> u64 {
        let mut h = Fnv::default();
        if let Ok(o) = self.opened.lock() {
            let mut names: Vec<&String> = o.keys().collect();
            names.sort();
            for n in names {
                h.text(n);
                h.num(o[n].fingerprint());
            }
        }
        h.0
    }

    /// The CSV options in force.
    pub fn options(&self) -> Options {
        self.opts
    }
}

fn sheet_hash(name: &str) -> u64 {
    let mut h = Fnv::default();
    h.text(name);
    h.0
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
