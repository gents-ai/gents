//! XLSX workbooks: each worksheet is a table, read as a stream of rows.
//!
//! Shared strings and cell formats are read up front (and capped); sheet
//! XML is streamed. Number cells whose format is a date or time become dates
//! and timestamps, other numbers are integers when written without a fraction
//! or exponent. Formula cells read as the value Excel stored; error cells
//! (`#DIV/0!` and so on) read as NULL, said once in the warnings.
use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::Res;
use crate::sheet::{Cell, SheetRows};
use crate::table::Warnings;
use crate::typed::{parse_date, parse_int, parse_timestamp};
use crate::zipread::Zip;

const SHARED_STRINGS_CAP: u64 = 512 * 1024 * 1024;
const SMALL_PART_CAP: u64 = 64 * 1024 * 1024;
/// Days from 1899-12-30 (Excel's day 0, which absorbs its 1900 leap year bug) to 1970-01-01.
const EXCEL_EPOCH_TO_UNIX: f64 = 25569.0;
/// Days between the 1900 system's day 0 and the 1904 system's.
const EXCEL_1904_OFFSET: f64 = 1462.0;

/// A sheet of a workbook.
#[derive(Clone, Debug)]
pub struct SheetRef {
    /// The name the workbook gives it.
    pub name: String,
    /// The part holding its cells.
    pub part: String,
}

/// What every sheet of a workbook shares.
pub struct Book {
    /// The workbook's file.
    pub path: PathBuf,
    /// Its sheets, in workbook order.
    pub sheets: Vec<SheetRef>,
    strings: Arc<Vec<String>>,
    date_styles: Arc<Vec<bool>>,
    date1904: bool,
}

pub(crate) fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

pub(crate) fn xml_error(path: &Path, what: &str, e: &dyn std::fmt::Display) -> String {
    format!(
        "{what} of {} is not valid XML ({e}); the workbook is corrupt",
        path.display()
    )
}

/// Whether the number format `id` (with its custom `code`, if any) shows dates or times.
fn is_date_format(id: u32, code: Option<&str>) -> bool {
    if let Some(code) = code {
        let (mut quoted, mut bracket, mut escaped) = (false, false, false);
        for c in code.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' | '_' | '*' => escaped = true,
                '"' => quoted = !quoted,
                '[' if !quoted => bracket = true,
                ']' if !quoted => bracket = false,
                'y' | 'm' | 'd' | 'h' | 's' | 'Y' | 'M' | 'D' | 'H' | 'S'
                    if !quoted && !bracket =>
                {
                    return true;
                }
                _ => {}
            }
        }
        return false;
    }
    matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58)
}

impl Book {
    /// Reads the workbook's sheet list, shared strings and cell formats.
    pub fn open(path: &Path) -> Res<Self> {
        let mut zip = Zip::open(path)?;
        let workbook = zip.read("xl/workbook.xml", SMALL_PART_CAP)?;
        let rels = zip.read("xl/_rels/workbook.xml.rels", SMALL_PART_CAP)?;
        let mut targets: HashMap<String, String> = HashMap::new();
        let mut reader = Reader::from_reader(rels.as_slice());
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Empty(e) | Event::Start(e))
                    if e.local_name().as_ref() == "Relationship" =>
                {
                    if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                        let full = match target.strip_prefix('/') {
                            Some(abs) => abs.to_string(),
                            None => format!("xl/{target}"),
                        };
                        targets.insert(id, full);
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(xml_error(path, "the relationships", &e)),
                _ => {}
            }
            buf.clear();
        }
        let mut sheets = Vec::new();
        let mut date1904 = false;
        let mut reader = Reader::from_reader(workbook.as_slice());
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Empty(e) | Event::Start(e)) => match e.local_name().as_ref() {
                    "sheet" => {
                        let (Some(name), Some(rid)) = (attr(&e, "name"), attr(&e, "id")) else {
                            continue;
                        };
                        if let Some(part) = targets.get(&rid).filter(|p| p.contains("worksheets/"))
                        {
                            sheets.push(SheetRef {
                                name,
                                part: part.clone(),
                            });
                        }
                    }
                    "workbookPr" => {
                        date1904 = matches!(attr(&e, "date1904").as_deref(), Some("1" | "true"))
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(xml_error(path, "the workbook", &e)),
                _ => {}
            }
            buf.clear();
        }
        if sheets.is_empty() {
            return Err(format!("{} has no worksheets to read", path.display()));
        }
        let strings = if zip.has("xl/sharedStrings.xml") {
            shared_strings(&mut zip, path)?
        } else {
            Vec::new()
        };
        let date_styles = if zip.has("xl/styles.xml") {
            styles(&mut zip, path)?
        } else {
            Vec::new()
        };
        Ok(Self {
            path: path.to_path_buf(),
            sheets,
            strings: Arc::new(strings),
            date_styles: Arc::new(date_styles),
            date1904,
        })
    }

    /// A stream over the rows of `sheet`.
    pub fn rows(&self, sheet: &SheetRef, warn: &Arc<Warnings>) -> Res<SheetRows> {
        let mut zip = Zip::open(&self.path)?;
        let stream = zip.stream(&sheet.part)?;
        let mut reader = Reader::from_reader(BufReader::with_capacity(128 * 1024, stream));
        reader.config_mut().trim_text(false);
        Ok(Box::new(Rows {
            reader,
            buf: Vec::new(),
            path: self.path.clone(),
            strings: Arc::clone(&self.strings),
            date_styles: Arc::clone(&self.date_styles),
            date1904: self.date1904,
            warn: Arc::clone(warn),
            done: false,
        }))
    }
}

fn shared_strings(zip: &mut Zip, path: &Path) -> Res<Vec<String>> {
    let bytes = zip.read("xl/sharedStrings.xml", SHARED_STRINGS_CAP)?;
    let mut reader = Reader::from_reader(bytes.as_slice());
    let (mut out, mut buf) = (Vec::new(), Vec::new());
    let (mut current, mut in_t, mut in_phonetic) = (String::new(), false, false);
    let mut total = 0usize;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "si" => current.clear(),
                "t" if !in_phonetic => in_t = true,
                "rPh" => in_phonetic = true,
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "si" => {
                    total += current.len();
                    if total as u64 > SHARED_STRINGS_CAP {
                        return Err(format!(
                            "the shared strings of {} are too large to read",
                            path.display()
                        ));
                    }
                    out.push(std::mem::take(&mut current));
                }
                "t" => in_t = false,
                "rPh" => in_phonetic = false,
                _ => {}
            },
            Ok(Event::Text(t)) if in_t => current.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) if in_t => {
                if let Some(c) = r.resolve_char_ref().ok().flatten() {
                    current.push(c);
                } else {
                    match r.as_ref() {
                        "amp" => current.push('&'),
                        "lt" => current.push('<'),
                        "gt" => current.push('>'),
                        "quot" => current.push('"'),
                        "apos" => current.push('\''),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(xml_error(path, "the shared strings", &e)),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// For each cell format index, whether it shows a date or time.
fn styles(zip: &mut Zip, path: &Path) -> Res<Vec<bool>> {
    let bytes = zip.read("xl/styles.xml", SMALL_PART_CAP)?;
    let mut reader = Reader::from_reader(bytes.as_slice());
    let (mut codes, mut xfs, mut buf) =
        (HashMap::<u32, String>::new(), Vec::<u32>::new(), Vec::new());
    let mut in_cell_xfs = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.local_name().as_ref() == "cellXfs" => in_cell_xfs = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == "cellXfs" => in_cell_xfs = false,
            Ok(Event::Empty(e) | Event::Start(e)) => match e.local_name().as_ref() {
                "numFmt" => {
                    if let (Some(id), Some(code)) = (attr(&e, "numFmtId"), attr(&e, "formatCode"))
                        && let Ok(id) = id.parse()
                    {
                        codes.insert(id, code);
                    }
                }
                "xf" if in_cell_xfs => xfs.push(
                    attr(&e, "numFmtId")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                ),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(xml_error(path, "the styles", &e)),
            _ => {}
        }
        buf.clear();
    }
    Ok(xfs
        .into_iter()
        .map(|id| is_date_format(id, codes.get(&id).map(String::as_str)))
        .collect())
}

/// The 0-based column of a cell reference like `AB12`.
fn column_of(reference: &str) -> Option<usize> {
    let letters = reference.bytes().take_while(u8::is_ascii_alphabetic);
    let mut n = 0usize;
    let mut any = false;
    for b in letters {
        n = n
            .checked_mul(26)?
            .checked_add(usize::from(b.to_ascii_uppercase() - b'A') + 1)?;
        any = true;
    }
    (any && n <= 16_384).then(|| n - 1)
}

/// A number cell's value: an integer unless it was written with a fraction or exponent.
pub(crate) fn number(text: &str) -> Option<Cell> {
    let t = text.trim();
    if let Some(i) = parse_int(t.as_bytes()) {
        return Some(Cell::Int(i));
    }
    let f: f64 = t.parse().ok().filter(|f: &f64| f.is_finite())?;
    // `5.0` is how some writers store an integer.
    if f.fract() == 0.0 && f.abs() < 1e15 && !t.contains(['e', 'E']) {
        return Some(Cell::Int(f as i64));
    }
    Some(Cell::Float(f))
}

/// An Excel serial date as a date (a whole day) or a timestamp.
fn serial(value: f64, date1904: bool) -> Cell {
    let days = value + if date1904 { EXCEL_1904_OFFSET } else { 0.0 } - EXCEL_EPOCH_TO_UNIX;
    let micros = (days * 86_400_000_000.0).round();
    if micros.abs() > 9.0e18 {
        return Cell::Float(value);
    }
    let micros = micros as i64;
    if micros % 86_400_000_000 == 0 {
        Cell::Date((micros / 86_400_000_000) as i32)
    } else {
        Cell::Stamp(micros)
    }
}

struct Rows {
    reader: Reader<BufReader<Box<dyn std::io::Read + Send>>>,
    buf: Vec<u8>,
    path: PathBuf,
    strings: Arc<Vec<String>>,
    date_styles: Arc<Vec<bool>>,
    date1904: bool,
    warn: Arc<Warnings>,
    done: bool,
}

#[derive(Default)]
struct CellState {
    column: usize,
    kind: String,
    style: usize,
    value: String,
    inline: String,
    in_v: bool,
    in_t: bool,
    in_is: bool,
}

impl Rows {
    fn finish(&self, c: &CellState) -> Cell {
        let v = c.value.as_str();
        match c.kind.as_str() {
            "s" => match v
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|i| self.strings.get(i))
            {
                Some(s) => Cell::Text(s.clone()),
                None => Cell::Empty,
            },
            "str" => {
                if v.is_empty() {
                    Cell::Empty
                } else {
                    Cell::Text(v.to_string())
                }
            }
            "inlineStr" => {
                if c.inline.is_empty() {
                    Cell::Empty
                } else {
                    Cell::Text(c.inline.clone())
                }
            }
            "b" => Cell::Bool(v.trim() == "1" || v.trim().eq_ignore_ascii_case("true")),
            "e" => {
                self.warn.once(
                    "xlsx-errors",
                    "cells holding spreadsheet errors (such as #DIV/0!) were read as NULL".into(),
                );
                Cell::Empty
            }
            "d" => match (
                parse_date(v.trim().as_bytes()),
                parse_timestamp(v.trim().as_bytes()),
            ) {
                (Some(d), _) => Cell::Date(d),
                (None, Some(t)) => Cell::Stamp(t.micros),
                _ => Cell::Text(v.to_string()),
            },
            _ if v.trim().is_empty() => Cell::Empty,
            _ => match number(v) {
                Some(cell) if self.date_styles.get(c.style).copied().unwrap_or(false) => match cell
                {
                    Cell::Int(i) => serial(i as f64, self.date1904),
                    Cell::Float(f) => serial(f, self.date1904),
                    other => other,
                },
                Some(cell) => cell,
                None => Cell::Text(v.to_string()),
            },
        }
    }

    fn next_row(&mut self) -> Res<Option<Vec<Cell>>> {
        let mut row: Vec<Cell> = Vec::new();
        let mut in_row = false;
        let mut cell: Option<CellState> = None;
        loop {
            self.buf.clear();
            let event = self.reader.read_event_into(&mut self.buf);
            match event {
                Err(e) => return Err(xml_error(&self.path, "a worksheet", &e)),
                Ok(Event::Eof) => return Ok(None),
                Ok(Event::Start(e)) => match e.local_name().as_ref() {
                    "row" => {
                        in_row = true;
                        row.clear();
                    }
                    "c" if in_row => {
                        let column = attr(&e, "r")
                            .as_deref()
                            .and_then(column_of)
                            .unwrap_or(row.len());
                        cell = Some(CellState {
                            column,
                            kind: attr(&e, "t").unwrap_or_default(),
                            style: attr(&e, "s").and_then(|s| s.parse().ok()).unwrap_or(0),
                            ..CellState::default()
                        });
                    }
                    "v" => {
                        if let Some(c) = cell.as_mut() {
                            c.in_v = true;
                        }
                    }
                    "is" => {
                        if let Some(c) = cell.as_mut() {
                            c.in_is = true;
                        }
                    }
                    "t" => {
                        if let Some(c) = cell.as_mut().filter(|c| c.in_is) {
                            c.in_t = true;
                        }
                    }
                    _ => {}
                },
                Ok(Event::Empty(e)) => {
                    if e.local_name().as_ref() == "row" {
                        return Ok(Some(Vec::new()));
                    }
                }
                Ok(Event::Text(t)) => {
                    if let Some(c) = cell.as_mut() {
                        let text = t.xml10_content();
                        if c.in_v {
                            c.value.push_str(&text);
                        } else if c.in_t {
                            c.inline.push_str(&text);
                        }
                    }
                }
                Ok(Event::GeneralRef(r)) => {
                    if let Some(c) = cell.as_mut() {
                        let ch = r.resolve_char_ref().ok().flatten().or(match r.as_ref() {
                            "amp" => Some('&'),
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            _ => None,
                        });
                        if let Some(ch) = ch {
                            if c.in_v {
                                c.value.push(ch);
                            } else if c.in_t {
                                c.inline.push(ch);
                            }
                        }
                    }
                }
                Ok(Event::End(e)) => match e.local_name().as_ref() {
                    "v" => {
                        if let Some(c) = cell.as_mut() {
                            c.in_v = false;
                        }
                    }
                    "t" => {
                        if let Some(c) = cell.as_mut() {
                            c.in_t = false;
                        }
                    }
                    "is" => {
                        if let Some(c) = cell.as_mut() {
                            c.in_is = false;
                        }
                    }
                    "c" => {
                        if let Some(c) = cell.take() {
                            let value = self.finish(&c);
                            if c.column >= 16_384 {
                                return Err(format!(
                                    "a worksheet of {} has a cell beyond column XFD",
                                    self.path.display()
                                ));
                            }
                            while row.len() < c.column {
                                row.push(Cell::Empty);
                            }
                            if row.len() == c.column {
                                row.push(value);
                            } else {
                                row[c.column] = value;
                            }
                        }
                    }
                    "row" => {
                        while row.last() == Some(&Cell::Empty) {
                            row.pop();
                        }
                        return Ok(Some(std::mem::take(&mut row)));
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

impl Iterator for Rows {
    type Item = Res<Vec<Cell>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_row() {
            Ok(Some(r)) => Some(Ok(r)),
            Ok(None) => {
                self.done = true;
                None
            }
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
    use crate::testkit::Dir;
    use crate::testkit::fixtures::{self, X};

    #[test]
    fn column_letters_to_index() {
        for (r, want) in [
            ("A1", Some(0)),
            ("Z9", Some(25)),
            ("AA1", Some(26)),
            ("AB12", Some(27)),
            ("XFD1", Some(16383)),
            ("XFE1", None),
            ("12", None),
            ("", None),
            ("AAAAAAAAAAAAAAAAAAAAA1", None),
        ] {
            assert_eq!(column_of(r), want, "{r}");
        }
    }

    #[test]
    fn numbers_are_integers_unless_written_with_a_fraction_or_exponent() {
        for (text, want) in [
            ("5", Some(Cell::Int(5))),
            ("-12", Some(Cell::Int(-12))),
            ("5.0", Some(Cell::Int(5))),
            ("2.5", Some(Cell::Float(2.5))),
            ("1E3", Some(Cell::Float(1000.0))),
            ("1.5e-3", Some(Cell::Float(0.0015))),
            ("99999999999999999999", Some(Cell::Float(1e20))),
            ("1e400", None),
            ("abc", None),
            (" 7 ", Some(Cell::Int(7))),
        ] {
            assert_eq!(number(text), want, "{text:?}");
        }
    }

    #[test]
    fn serial_dates_follow_the_1900_and_1904_systems() {
        assert_eq!(serial(45292.0, false), Cell::Date(19723));
        assert_eq!(serial(25569.0, false), Cell::Date(0));
        assert_eq!(
            serial(45294.5, false),
            Cell::Stamp(19725 * 86_400_000_000 + 43_200_000_000)
        );
        assert_eq!(serial(45292.0 - 1462.0, true), Cell::Date(19723));
        assert_eq!(serial(1e300, false), Cell::Float(1e300));
    }

    #[test]
    fn date_formats() {
        for id in [14, 15, 22, 27, 36, 45, 47, 50, 58] {
            assert!(is_date_format(id, None), "{id}");
        }
        for id in [0, 1, 2, 9, 10, 11, 12, 37, 49, 59, 164] {
            assert!(!is_date_format(id, None), "{id}");
        }
        for code in [
            "yyyy-mm-dd",
            "dd/mm/yyyy hh:mm",
            "[$-409]mmmm d, yyyy",
            "h:mm AM/PM",
            "hh:mm:ss",
        ] {
            assert!(is_date_format(164, Some(code)), "{code}");
        }
        for code in ["0.00", "#,##0", "\"day\"0", "0.00\\d", "[Red]0.0", "0%"] {
            assert!(!is_date_format(164, Some(code)), "{code}");
        }
    }

    fn rows_of(bytes: &[u8]) -> (Book, Vec<Vec<Vec<Cell>>>, Dir) {
        let dir = Dir::new();
        let p = dir.put("b.xlsx", bytes);
        let book = Book::open(&p).unwrap();
        let warn = Warnings::new();
        let all = book
            .sheets
            .iter()
            .map(|s| book.rows(s, &warn).unwrap().map(|r| r.unwrap()).collect())
            .collect();
        (book, all, dir)
    }

    #[test]
    fn a_workbook_reads_typed_cells_row_by_row() {
        let (book, sheets, _d) = rows_of(&fixtures::xlsx(&[(
            "S",
            vec![
                vec![
                    X::S("a"),
                    X::N("1"),
                    X::B(true),
                    X::D("45292"),
                    X::T("45292.25"),
                    X::I("in<line>"),
                ],
                vec![X::Empty, X::N("2.5")],
                vec![],
                vec![X::Empty, X::Empty, X::Empty, X::N("7")],
                vec![X::F("f"), X::E("#N/A")],
            ],
        )]));
        assert_eq!(
            book.sheets
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["S"]
        );
        assert_eq!(
            sheets[0],
            vec![
                vec![
                    Cell::Text("a".into()),
                    Cell::Int(1),
                    Cell::Bool(true),
                    Cell::Date(19723),
                    Cell::Stamp(19723 * 86_400_000_000 + 21_600_000_000),
                    Cell::Text("in<line>".into())
                ],
                vec![Cell::Empty, Cell::Float(2.5)],
                vec![],
                vec![Cell::Empty, Cell::Empty, Cell::Empty, Cell::Int(7)],
                vec![Cell::Text("f".into())],
            ]
        );
    }

    #[test]
    fn the_1904_date_system_is_honoured() {
        let (_, sheets, _d) = rows_of(&fixtures::xlsx_with(
            &[("S", vec![vec![X::D("43830")]])],
            true,
        ));
        assert_eq!(sheets[0][0][0], Cell::Date(19723));
    }

    #[test]
    fn several_sheets_keep_their_order_and_names() {
        let (book, sheets, _d) = rows_of(&fixtures::xlsx(&[
            ("One & Two", vec![vec![X::N("1")]]),
            ("Z", vec![vec![X::N("2")]]),
        ]));
        assert_eq!(
            book.sheets
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["One & Two", "Z"]
        );
        assert_eq!(sheets[1][0][0], Cell::Int(2));
    }

    #[test]
    fn broken_workbooks_are_one_sentence() {
        let good = fixtures::xlsx(&[("S", vec![vec![X::S("a")]])]);
        let dir = Dir::new();
        for (name, bytes) in [
            ("truncated", good[..100].to_vec()),
            ("empty", Vec::new()),
            ("text", b"hello".to_vec()),
            ("zip of nothing", fixtures::ods(&[("S", vec![])])),
        ] {
            let p = dir.put(&format!("{name}.xlsx"), bytes);
            let err = Book::open(&p)
                .err()
                .unwrap_or_else(|| panic!("{name} opened"));
            assert!(!err.contains('\n') && err.len() < 220, "{name}: {err}");
        }
    }

    #[test]
    fn a_decompression_bomb_is_refused_before_it_is_read() {
        let dir = Dir::new();
        let p = dir.put("bomb.xlsx", fixtures::zip_bomb());
        let book = Book::open(&p).unwrap();
        let err = book.rows(&book.sheets[0], &Warnings::new()).err().unwrap();
        assert!(err.contains("decompression bomb"), "{err}");
    }

    #[test]
    fn a_worksheet_with_broken_xml_fails_the_stream_with_a_sentence() {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let good = fixtures::xlsx(&[("S", vec![vec![X::S("a")]])]);
        let mut src = zip::ZipArchive::new(std::io::Cursor::new(good)).unwrap();
        for i in 0..src.len() {
            let mut f = src.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut data).unwrap();
            if name.ends_with("sheet1.xml") {
                data =
                    b"<worksheet><sheetData><row><c r=\"A1\"><v>1</v></row></sheetData>".to_vec();
            }
            std::io::Write::flush(&mut zw).unwrap();
            zw.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zw, &data).unwrap();
        }
        let bytes = zw.finish().unwrap().into_inner();
        let dir = Dir::new();
        let p = dir.put("x.xlsx", bytes);
        let book = Book::open(&p).unwrap();
        let first = book
            .rows(&book.sheets[0], &Warnings::new())
            .unwrap()
            .find_map(Result::err);
        assert!(first.is_some_and(|e| e.contains("is not valid XML")));
    }
}
