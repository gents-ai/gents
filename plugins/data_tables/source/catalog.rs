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
use crate::names::{sanitize, unique};
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
    if head.starts_with(b"PAR1") {
        return Ok(Ok(Fmt::Parquet));
    }
    if head.starts_with(b"ARROW1") || head.starts_with(&[0xFF, 0xFF, 0xFF, 0xFF]) {
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
        let mut taken = HashSet::new();
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
        if cat.specs.is_empty() && notes.is_empty() && path.is_none() && tables.is_none() {
            return Err(
                "give path (a data file or a folder of them) or tables (rows as JSON)".into(),
            );
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

    fn name_for(raw: String, rel: &str, taken: &mut HashSet<String>) -> (String, Option<String>) {
        let name = unique(&sanitize(&raw), taken);
        let note = (name != raw).then(|| format!("{rel} is the table {name}"));
        (name, note)
    }

    fn add_file(
        &mut self,
        rel: &str,
        full: &Path,
        fmt: Fmt,
        taken: &mut HashSet<String>,
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
mod tests {
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
        let c =
            Catalog::discover(Some(&p.to_string_lossy()), None, None, Options::default()).unwrap();
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
            let err =
                Catalog::discover(Some(&d.s()), Some(&[bad.into()]), None, Options::default())
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
            let err =
                Catalog::discover(Some(&d.s()), Some(&[bad.into()]), None, Options::default())
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
            c.listing[0].contains(
                "leak.csv: a link that leads outside the folder or to a folder, not read"
            ),
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
        let err = Catalog::discover(None, None, None, Options::default())
            .err()
            .unwrap();
        assert_eq!(
            err,
            "give path (a data file or a folder of them) or tables (rows as JSON)"
        );
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
        let inline = json!([{"name": "t", "rows": [[1]]}, {"name": "other", "columns": ["a"], "rows": [[2]]}]);
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
}
