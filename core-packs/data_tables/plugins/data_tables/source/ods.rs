//! OpenDocument spreadsheets: each sheet is a table, read as a stream of rows.
//!
//! Cells keep the type the file gives them: float, percentage and currency
//! cells are numbers (integers when written without a fraction), date cells are
//! dates or timestamps, boolean cells are booleans and string cells are text
//! with their paragraphs joined by a line break. Repeated empty rows and cells
//! cost nothing; a repeated non-empty row is capped at the spreadsheet limit.
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::Res;
use crate::sheet::{Cell, SheetRows};
use crate::typed::{parse_date, parse_timestamp};
use crate::xlsx::{attr, number, xml_error};
use crate::zipread::Zip;

const CONTENT: &str = "content.xml";
/// The most copies of one non-empty row or cell a file may ask for.
const MAX_REPEAT: u64 = 1_048_576;
const MAX_CELL_REPEAT: u64 = 16_384;

/// An ODS workbook.
pub struct Book {
    path: PathBuf,
    /// The sheet names in order.
    pub sheets: Vec<String>,
}

fn stream(path: &Path) -> Res<Reader<BufReader<Box<dyn std::io::Read + Send>>>> {
    let mut zip = Zip::open(path)?;
    let s = zip.stream(CONTENT)?;
    Ok(Reader::from_reader(BufReader::with_capacity(128 * 1024, s)))
}

impl Book {
    /// The workbook's file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lists the sheets of the workbook at `path`.
    // vertexia: a sheet list needs one streamed pass over content.xml (flat memory, time that
    // grows with the file), run for every .ods in a folder at listing time; an index kept
    // beside the file, or a lazy listing, would make a folder of large ODS files free until named.
    pub fn open(path: &Path) -> Res<Self> {
        let mut reader = stream(path)?;
        let (mut sheets, mut buf, mut depth) = (Vec::new(), Vec::new(), 0u32);
        loop {
            buf.clear();
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) if e.local_name().as_ref() == "table" => {
                    depth += 1;
                    if depth == 1 {
                        sheets.push(
                            attr(&e, "name")
                                .unwrap_or_else(|| format!("Sheet{}", sheets.len() + 1)),
                        );
                    }
                }
                Ok(Event::End(e)) if e.local_name().as_ref() == "table" => {
                    depth = depth.saturating_sub(1)
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(xml_error(path, "the spreadsheet content", &e)),
                _ => {}
            }
        }
        if sheets.is_empty() {
            return Err(format!("{} has no sheets to read", path.display()));
        }
        Ok(Self {
            path: path.to_path_buf(),
            sheets,
        })
    }

    /// A stream over the rows of the sheet named `name`.
    pub fn rows(&self, name: &str, warn: &Arc<crate::table::Warnings>) -> Res<SheetRows> {
        Ok(Box::new(Rows {
            reader: stream(&self.path)?,
            path: self.path.clone(),
            want: name.to_string(),
            state: Scan::Seeking,
            repeat: 0,
            last: Vec::new(),
            warn: Arc::clone(warn),
        }))
    }
}

#[derive(Clone, Copy)]
enum Scan {
    Seeking,
    In,
    Done,
}

struct Rows {
    reader: Reader<BufReader<Box<dyn std::io::Read + Send>>>,
    path: PathBuf,
    want: String,
    state: Scan,
    repeat: u64,
    last: Vec<Cell>,
    warn: Arc<crate::table::Warnings>,
}

#[derive(Default)]
struct Pending {
    kind: String,
    value: Option<String>,
    repeat: u64,
    text: String,
    paragraphs: u32,
    in_p: bool,
}

fn cell_of(p: &Pending, warn: &crate::table::Warnings) -> Cell {
    let v = p.value.as_deref().unwrap_or("");
    match p.kind.as_str() {
        "float" | "percentage" | "currency" => number(v).unwrap_or(Cell::Empty),
        "boolean" => Cell::Bool(v == "true"),
        "date" => match (parse_date(v.as_bytes()), parse_timestamp(v.as_bytes())) {
            (Some(d), _) => Cell::Date(d),
            (None, Some(t)) => Cell::Stamp(t.micros),
            _ => Cell::Text(v.to_string()),
        },
        "time" => Cell::Text(v.to_string()),
        "string" => Cell::Text(p.value.clone().unwrap_or_else(|| p.text.clone())),
        "" if !p.text.is_empty() => Cell::Text(p.text.clone()),
        "" => Cell::Empty,
        _ => {
            warn.once(
                "ods-kind",
                "cells of an unknown value type were read as text".into(),
            );
            Cell::Text(p.text.clone())
        }
    }
}

fn start_cell(e: &BytesStart<'_>) -> Pending {
    let kind = attr(e, "value-type").unwrap_or_default();
    let value = match kind.as_str() {
        "date" => attr(e, "date-value"),
        "time" => attr(e, "time-value"),
        "boolean" => attr(e, "boolean-value"),
        "string" => attr(e, "string-value"),
        _ => attr(e, "value"),
    };
    Pending {
        kind,
        value,
        repeat: attr(e, "number-columns-repeated")
            .and_then(|r| r.parse().ok())
            .unwrap_or(1),
        ..Pending::default()
    }
}

impl Rows {
    fn row(&mut self) -> Res<Option<Vec<Cell>>> {
        if self.repeat > 0 {
            self.repeat -= 1;
            return Ok(Some(self.last.clone()));
        }
        let mut buf = Vec::new();
        let (mut row, mut row_repeat, mut gap) = (Vec::<Cell>::new(), 1u64, 0u64);
        let (mut cell, mut depth): (Option<Pending>, u32) = (None, 0);
        loop {
            buf.clear();
            let event = self.reader.read_event_into(&mut buf);
            let event = event.map_err(|e| xml_error(&self.path, "the spreadsheet content", &e))?;
            match (self.state, event) {
                (_, Event::Eof) => return Ok(None),
                (Scan::Done, _) => return Ok(None),
                (Scan::Seeking, Event::Start(e)) if e.local_name().as_ref() == "table" => {
                    depth += 1;
                    if depth == 1 && attr(&e, "name").as_deref() == Some(self.want.as_str()) {
                        self.state = Scan::In;
                    }
                }
                (Scan::Seeking, Event::End(e)) if e.local_name().as_ref() == "table" => {
                    depth = depth.saturating_sub(1)
                }
                (Scan::Seeking, _) => {}
                (Scan::In, Event::End(e)) if e.local_name().as_ref() == "table" => {
                    self.state = Scan::Done;
                    return Ok(None);
                }
                (Scan::In, Event::Start(e)) => match e.local_name().as_ref() {
                    "table-row" => {
                        row.clear();
                        gap = 0;
                        row_repeat = attr(&e, "number-rows-repeated")
                            .and_then(|r| r.parse().ok())
                            .unwrap_or(1);
                    }
                    "table-cell" | "covered-table-cell" => cell = Some(start_cell(&e)),
                    "p" => {
                        if let Some(c) = cell.as_mut() {
                            if c.paragraphs > 0 {
                                c.text.push('\n');
                            }
                            c.paragraphs += 1;
                            c.in_p = true;
                        }
                    }
                    _ => {}
                },
                (Scan::In, Event::Empty(e)) => match e.local_name().as_ref() {
                    "table-cell" | "covered-table-cell" => {
                        let p = start_cell(&e);
                        push_cell(&mut row, &mut gap, &p, &self.warn, &self.path)?;
                    }
                    "table-row" => {}
                    "s" => {
                        if let Some(c) = cell.as_mut().filter(|c| c.in_p) {
                            let n: usize = attr(&e, "c")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1)
                                .min(10_000);
                            c.text.push_str(&" ".repeat(n));
                        }
                    }
                    "tab" => {
                        if let Some(c) = cell.as_mut().filter(|c| c.in_p) {
                            c.text.push('\t');
                        }
                    }
                    "line-break" => {
                        if let Some(c) = cell.as_mut().filter(|c| c.in_p) {
                            c.text.push('\n');
                        }
                    }
                    _ => {}
                },
                (Scan::In, Event::Text(t)) => {
                    if let Some(c) = cell.as_mut().filter(|c| c.in_p) {
                        c.text.push_str(&t.xml10_content());
                    }
                }
                (Scan::In, Event::GeneralRef(r)) => {
                    if let Some(c) = cell.as_mut().filter(|c| c.in_p) {
                        let ch = r.resolve_char_ref().ok().flatten().or(match r.as_ref() {
                            "amp" => Some('&'),
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            _ => None,
                        });
                        c.text.extend(ch);
                    }
                }
                (Scan::In, Event::End(e)) => match e.local_name().as_ref() {
                    "p" => {
                        if let Some(c) = cell.as_mut() {
                            c.in_p = false;
                        }
                    }
                    "table-cell" | "covered-table-cell" => {
                        if let Some(p) = cell.take() {
                            push_cell(&mut row, &mut gap, &p, &self.warn, &self.path)?;
                        }
                    }
                    "table-row" => {
                        while row.last() == Some(&Cell::Empty) {
                            row.pop();
                        }
                        if row.is_empty() {
                            continue;
                        }
                        if row_repeat > MAX_REPEAT {
                            return Err(format!(
                                "a sheet of {} repeats one row {row_repeat} times, over the limit of {MAX_REPEAT}",
                                self.path.display()
                            ));
                        }
                        self.repeat = row_repeat.saturating_sub(1);
                        self.last = row.clone();
                        return Ok(Some(row));
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// Adds a cell (repeated) to `row`. Empty cells only widen `gap`, which becomes padding when a
/// value follows it, so a long empty tail costs nothing and a long empty head is refused.
fn push_cell(
    row: &mut Vec<Cell>,
    gap: &mut u64,
    p: &Pending,
    warn: &crate::table::Warnings,
    path: &Path,
) -> Res<()> {
    let cell = cell_of(p, warn);
    if cell == Cell::Empty {
        *gap = gap.saturating_add(p.repeat);
        return Ok(());
    }
    let width = (row.len() as u64)
        .saturating_add(*gap)
        .saturating_add(p.repeat);
    if width > MAX_CELL_REPEAT {
        return Err(format!(
            "a sheet of {} is wider than {} columns",
            path.display(),
            MAX_CELL_REPEAT
        ));
    }
    row.resize(row.len() + *gap as usize, Cell::Empty);
    *gap = 0;
    row.extend(std::iter::repeat_n(cell, p.repeat as usize));
    Ok(())
}

impl Iterator for Rows {
    type Item = Res<Vec<Cell>>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.row() {
            Ok(Some(r)) => Some(Ok(r)),
            Ok(None) => None,
            Err(e) => {
                self.state = Scan::Done;
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Dir;
    use crate::testkit::fixtures::{self, O};

    fn read(bytes: &[u8]) -> (Vec<String>, Vec<Vec<Vec<Cell>>>, Dir) {
        let dir = Dir::new();
        let p = dir.put("b.ods", bytes);
        let book = Book::open(&p).unwrap();
        let warn = Arc::new(crate::table::Warnings::default());
        let all = book
            .sheets
            .iter()
            .map(|s| book.rows(s, &warn).unwrap().map(|r| r.unwrap()).collect())
            .collect();
        (book.sheets.clone(), all, dir)
    }

    #[test]
    fn typed_cells_repeats_and_blank_rows() {
        let (names, sheets, _d) = read(&fixtures::ods(&[
            (
                "A & B",
                vec![
                    (
                        1,
                        vec![
                            O::S("text"),
                            O::F("1200"),
                            O::F("2.5"),
                            O::B(true),
                            O::D("2024-01-31"),
                            O::D("2024-02-01T09:30:00"),
                            O::P("0.2"),
                        ],
                    ),
                    (3, vec![O::S("r"), O::Gap(2), O::F("9")]),
                    (1, vec![O::Gap(5)]),
                    (1000000, vec![O::Gap(3)]),
                    (1, vec![O::S("last")]),
                ],
            ),
            ("Second", vec![(1, vec![O::F("1")])]),
        ]));
        assert_eq!(names, ["A & B", "Second"]);
        let first = vec![
            Cell::Text("text".into()),
            Cell::Int(1200),
            Cell::Float(2.5),
            Cell::Bool(true),
            Cell::Date(19753),
            Cell::Stamp(19754 * 86_400_000_000 + 34_200_000_000),
            Cell::Float(0.2),
        ];
        let repeated = vec![
            Cell::Text("r".into()),
            Cell::Empty,
            Cell::Empty,
            Cell::Int(9),
        ];
        assert_eq!(
            sheets[0],
            vec![
                first,
                repeated.clone(),
                repeated.clone(),
                repeated,
                vec![Cell::Text("last".into())]
            ]
        );
        assert_eq!(sheets[1], vec![vec![Cell::Int(1)]]);
    }

    #[test]
    fn multiline_text_keeps_its_breaks_and_spaces() {
        let (_, sheets, _d) = read(&fixtures::ods(&[("S", vec![(1, vec![O::S("a\nb  c")])])]));
        assert_eq!(sheets[0][0][0], Cell::Text("a\nb  c".into()));
    }

    #[test]
    fn a_sheet_that_asks_for_absurd_repeats_is_refused() {
        let bytes = fixtures::ods(&[("S", vec![(2_000_000, vec![O::S("x")])])]);
        let dir = Dir::new();
        let p = dir.put("r.ods", bytes);
        let book = Book::open(&p).unwrap();
        let err = book
            .rows("S", &Arc::new(crate::table::Warnings::default()))
            .unwrap()
            .find_map(Result::err)
            .unwrap();
        assert!(err.contains("repeats one row"), "{err}");
        let wide = fixtures::ods(&[(
            "S",
            vec![(1, vec![O::F("1"), O::Gap(100_000)]), (1, vec![O::F("2")])],
        )]);
        let p = dir.put("w.ods", wide);
        let book = Book::open(&p).unwrap();
        // Trailing empty cells are trimmed, so a huge empty tail is not an error.
        let rows: Vec<_> = book
            .rows("S", &Arc::new(crate::table::Warnings::default()))
            .unwrap()
            .collect();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(Result::is_ok));
        let wide_data = fixtures::ods(&[("S", vec![(1, vec![O::Gap(100_000), O::F("1")])])]);
        let p = dir.put("w2.ods", wide_data);
        let book = Book::open(&p).unwrap();
        let err = book
            .rows("S", &Arc::new(crate::table::Warnings::default()))
            .unwrap()
            .find_map(Result::err)
            .unwrap();
        assert!(err.contains("is wider than"), "{err}");
    }

    #[test]
    fn broken_files_are_one_sentence() {
        let dir = Dir::new();
        let good = fixtures::ods(&[("S", vec![(1, vec![O::S("a")])])]);
        for (name, bytes) in [
            ("truncated", good[..80].to_vec()),
            ("empty", Vec::new()),
            ("xlsx", fixtures::xlsx(&[("S", vec![])])),
        ] {
            let p = dir.put(&format!("{name}.ods"), bytes);
            let err = Book::open(&p)
                .err()
                .unwrap_or_else(|| panic!("{name} opened"));
            assert!(!err.contains('\n') && err.len() < 240, "{name}: {err}");
        }
    }
}
