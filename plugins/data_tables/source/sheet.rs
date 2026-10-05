//! A spreadsheet sheet as a table: rows of typed cells, a header row when the
//! top row looks like one, and column types from the cells themselves (a cell
//! typed as text stays text, a number cell is an integer unless it has a
//! fraction). Completely empty rows are dropped, so a sheet that declares a
//! million trailing blank rows costs nothing.
use std::collections::HashSet;
use std::sync::Arc;

use arrow::datatypes::{Field, Schema, SchemaRef};
use arrow::record_batch::{RecordBatch, RecordBatchOptions};

use crate::Res;
use crate::csv::{MAX_COLUMNS, column_names};
use crate::table::{BATCH_ROWS, Batches, Infer, SAMPLE_ROWS, ScanError, TableSource, Warnings};
use crate::typed::{BOOL, Builder, ColType, DATE, FLOAT, INT, Inferrer, TS};

/// One cell.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    /// Nothing in the cell.
    Empty,
    /// A boolean.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A number with a fraction or beyond the integer range.
    Float(f64),
    /// Text.
    Text(String),
    /// A date, in days since 1970-01-01.
    Date(i32),
    /// A date and time, in microseconds since 1970-01-01.
    Stamp(i64),
}

/// The rows of one sheet.
pub type SheetRows = Box<dyn Iterator<Item = Res<Vec<Cell>>> + Send>;

/// Opens a fresh stream over a sheet's rows.
pub type Opener = Box<dyn Fn() -> Res<SheetRows> + Send + Sync>;

/// A sheet read as a table.
pub struct SheetTable {
    open: Opener,
    table: String,
    header: bool,
    types: Vec<ColType>,
    schema: SchemaRef,
    rows: Option<u64>,
    fp: u64,
    warn: Arc<Warnings>,
}

/// A cell as text, for a column that holds text.
pub fn cell_text(c: &Cell) -> String {
    use chrono::DateTime;
    match c {
        Cell::Empty => String::new(),
        Cell::Bool(v) => v.to_string(),
        Cell::Int(v) => v.to_string(),
        Cell::Float(v) => crate::render::float_text(*v),
        Cell::Text(s) => s.clone(),
        Cell::Date(d) => DateTime::from_timestamp(i64::from(*d) * 86_400, 0)
            .map_or_else(String::new, |t| t.format("%Y-%m-%d").to_string()),
        Cell::Stamp(m) => DateTime::from_timestamp_micros(*m).map_or_else(String::new, |t| {
            let fmt = if m % 1_000_000 == 0 {
                "%Y-%m-%dT%H:%M:%S"
            } else {
                "%Y-%m-%dT%H:%M:%S%.6f"
            };
            t.format(fmt).to_string()
        }),
    }
}

fn blank(row: &[Cell]) -> bool {
    row.iter().all(|c| *c == Cell::Empty)
}

fn flags(c: &Cell) -> Option<u8> {
    match c {
        Cell::Empty => None,
        Cell::Bool(_) => Some(BOOL),
        Cell::Int(i) => Some(
            INT | if i.unsigned_abs() < (1 << 53) {
                FLOAT
            } else {
                0
            },
        ),
        Cell::Float(_) => Some(FLOAT),
        Cell::Text(_) => Some(0),
        Cell::Date(_) => Some(DATE | TS),
        Cell::Stamp(_) => Some(TS),
    }
}

fn header_text(row: &[Cell]) -> Option<Vec<(Vec<u8>, bool)>> {
    row.iter()
        .map(|c| match c {
            Cell::Text(s) if !s.trim().is_empty() => Some((s.as_bytes().to_vec(), true)),
            _ => None,
        })
        .collect()
}

impl SheetTable {
    /// Reads the head of the sheet `open` streams and infers its types.
    pub fn open(
        open: Opener,
        table: &str,
        infer: Infer,
        fp: u64,
        warn: &Arc<Warnings>,
    ) -> Res<Self> {
        let mut rows = open()?;
        let mut first: Option<Vec<Cell>> = None;
        let mut sample: Vec<Vec<Cell>> = Vec::new();
        let mut inf: Option<(Inferrer, usize)> = None;
        let (mut count, mut at_end, mut header) = (0u64, true, false);
        let mut widest = 0usize;
        for row in rows.by_ref() {
            let row = row?;
            if blank(&row) {
                continue;
            }
            if first.is_none() {
                first = Some(row.clone());
            }
            if sample.len() < 100 {
                sample.push(row);
                continue;
            }
            // The head is complete: settle the layout, then stream the rest of the sample.
            if inf.is_none() {
                let (h, w) = layout(&sample, &mut header);
                widest = w;
                inf = Some((Inferrer::new(widest), widest));
                for r in sample.drain(..).skip(usize::from(h)) {
                    observe(&mut inf, &r, &mut count, warn);
                }
            }
            if infer == Infer::Sample && count >= SAMPLE_ROWS {
                at_end = false;
                break;
            }
            observe(&mut inf, &row, &mut count, warn);
        }
        if inf.is_none() {
            if sample.is_empty() {
                return Err("the sheet has no rows to read".into());
            }
            let (h, w) = layout(&sample, &mut header);
            widest = w;
            inf = Some((Inferrer::new(widest), widest));
            for r in sample.drain(..).skip(usize::from(h)) {
                observe(&mut inf, &r, &mut count, warn);
            }
        }
        if widest > MAX_COLUMNS {
            return Err(format!(
                "the sheet has {widest} columns, over the {MAX_COLUMNS} one table may have"
            ));
        }
        let names = column_names(
            header
                .then(|| first.as_deref().and_then(header_text))
                .flatten()
                .as_deref(),
            widest,
            warn,
        );
        let types = inf.map(|(i, _)| i.types()).unwrap_or_default();
        let fields: Vec<Field> = names
            .iter()
            .zip(&types)
            .map(|(n, t)| Field::new(n, t.data_type(), true))
            .collect();
        Ok(Self {
            open,
            table: table.to_string(),
            header,
            types,
            schema: Arc::new(Schema::new(fields)),
            rows: at_end.then_some(count),
            fp,
            warn: Arc::clone(warn),
        })
    }
}

/// Decides whether the first row is a header and how wide the table is.
fn layout(sample: &[Vec<Cell>], header: &mut bool) -> (bool, usize) {
    let first = &sample[0];
    let texty = header_text(first).is_some();
    let typed_below = (0..first.len()).any(|c| {
        sample[1..]
            .iter()
            .filter_map(|r| r.get(c))
            .any(|cell| !matches!(cell, Cell::Empty | Cell::Text(_)))
    });
    let names: HashSet<&str> = first
        .iter()
        .filter_map(|c| {
            if let Cell::Text(s) = c {
                Some(s.trim())
            } else {
                None
            }
        })
        .collect();
    *header = texty && (typed_below || names.len() == first.len());
    let widest = if *header {
        first.len()
    } else {
        sample.iter().map(Vec::len).max().unwrap_or(1)
    };
    (*header, widest)
}

fn observe(inf: &mut Option<(Inferrer, usize)>, row: &[Cell], count: &mut u64, warn: &Warnings) {
    let Some((inf, width)) = inf else { return };
    *count += 1;
    if row.len() > *width {
        warn.once(
            "wide",
            format!("cells right of column {width} were ignored (first in data row {count})"),
        );
    }
    for (c, cell) in row.iter().take(*width).enumerate() {
        if let Some(f) = flags(cell) {
            inf.observe(c, f);
        }
    }
}

impl TableSource for SheetTable {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn row_count(&self) -> Option<u64> {
        self.rows
    }

    fn fingerprint(&self) -> u64 {
        self.fp
    }

    fn scan(&self, projection: Option<&[usize]>) -> Batches {
        let columns: Vec<usize> =
            projection.map_or_else(|| (0..self.types.len()).collect(), <[usize]>::to_vec);
        let rows = match (self.open)() {
            Ok(r) => r,
            Err(e) => return Box::new(std::iter::once(Err(ScanError::Failed(e)))),
        };
        let fields: Vec<Field> = columns
            .iter()
            .map(|&c| self.schema.field(c).clone())
            .collect();
        Box::new(Scan {
            rows,
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
            row: 0,
            done: false,
        })
    }
}

struct Scan {
    rows: SheetRows,
    skip: bool,
    table: String,
    schema: SchemaRef,
    types: Vec<ColType>,
    names: Vec<String>,
    columns: Vec<usize>,
    width: usize,
    warn: Arc<Warnings>,
    row: u64,
    done: bool,
}

impl Scan {
    fn batch(&mut self) -> Result<Option<RecordBatch>, ScanError> {
        let mut builders: Vec<Builder> = self
            .types
            .iter()
            .map(|&t| Builder::new(t, BATCH_ROWS))
            .collect();
        let mut n = 0usize;
        while n < BATCH_ROWS {
            let Some(row) = self.rows.next() else {
                self.done = true;
                break;
            };
            let row = row?;
            if blank(&row) {
                continue;
            }
            if self.skip {
                self.skip = false;
                continue;
            }
            self.row += 1;
            if row.len() > self.width {
                self.warn.once(
                    "wide",
                    format!(
                        "cells right of column {} were ignored (first in data row {})",
                        self.width, self.row
                    ),
                );
            }
            for (slot, b) in builders.iter_mut().enumerate() {
                let cell = row.get(self.columns[slot]).unwrap_or(&Cell::Empty);
                let ok = match cell {
                    c if self.types[slot] == ColType::Text && *c != Cell::Empty => {
                        b.text(&cell_text(c))
                    }
                    Cell::Empty => {
                        b.null();
                        Ok(())
                    }
                    Cell::Bool(v) => b.bool(*v),
                    Cell::Int(v) => b.int(*v),
                    Cell::Float(v) => b.float(*v),
                    Cell::Text(v) => b.text(v),
                    Cell::Date(d) => b.micros(i64::from(*d) * 86_400_000_000),
                    Cell::Stamp(m) => b.micros(*m),
                };
                if ok.is_err() {
                    return Err(ScanError::Conflict {
                        table: self.table.clone(),
                        column: self.names[slot].clone(),
                        line: self.row,
                    });
                }
            }
            n += 1;
        }
        if n == 0 {
            return Ok(None);
        }
        let arrays = builders.iter_mut().map(Builder::finish).collect();
        let options = RecordBatchOptions::new().with_row_count(Some(n));
        RecordBatch::try_new_with_options(Arc::clone(&self.schema), arrays, &options)
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
