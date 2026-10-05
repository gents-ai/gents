//! The in-memory table every source is read into: column-major, one 16-byte
//! cell per value, strings pooled. A builder enforces the byte and row bounds
//! while a source streams into it, so a table never outgrows its budget no
//! matter how large the input is.

use std::collections::HashMap;

/// One value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cell {
    /// Empty or null.
    Null,
    /// A finite number.
    Num(f64),
    /// Text, an index into the table's pool.
    Str(u32),
}

/// Rows read at most, whatever the byte budget says.
pub const MAX_ROWS: usize = 2_000_000;
/// Bytes of cells and text a table may hold.
pub const MAX_TABLE_BYTES: usize = 192 * 1024 * 1024;
/// Columns kept at most.
pub const MAX_COLUMNS: usize = 512;
/// Longest cell of text, in bytes.
pub const MAX_CELL_BYTES: usize = 65_536;

/// Deduplicated strings.
#[derive(Debug, Default)]
pub struct Pool {
    strings: Vec<String>,
    index: HashMap<String, u32>,
    bytes: usize,
}

impl Pool {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(i) = self.index.get(s) {
            return *i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_owned());
        self.index.insert(s.to_owned(), i);
        self.bytes += 2 * s.len() + 64;
        i
    }

    /// The text of string `i`.
    pub fn get(&self, i: u32) -> &str {
        &self.strings[i as usize]
    }
}

/// Why reading stopped before the end of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The row limit was reached.
    Rows,
    /// The byte budget was reached.
    Bytes,
}

/// A table of named columns.
#[derive(Debug, Default)]
pub struct Table {
    /// Column names, in source order.
    pub names: Vec<String>,
    /// Column cells, one vector per name, all of length `rows`.
    pub cols: Vec<Vec<Cell>>,
    /// Row count.
    pub rows: usize,
    /// Text of every `Cell::Str`.
    pub pool: Pool,
    /// Set when the source had more data than the bounds allowed.
    pub stopped: Option<Stop>,
    /// Values that were not scalars (lists or objects) and became empty.
    pub nested: usize,
    /// Rows with more or fewer fields than the header.
    pub ragged: usize,
    /// Bytes that were not valid UTF-8 and were replaced.
    pub bad_utf8: bool,
    /// Columns left out because the table already holds the column limit.
    pub dropped_columns: usize,
}

impl Table {
    /// Index of column `name`.
    pub fn col(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// Text of a cell, if it is text.
    pub fn text(&self, cell: Cell) -> Option<&str> {
        match cell {
            Cell::Str(i) => Some(self.pool.get(i)),
            _ => None,
        }
    }

    /// Approximate bytes held.
    pub fn bytes(&self) -> usize {
        self.cols.len() * self.rows * std::mem::size_of::<Cell>() + self.pool.bytes
    }
}

/// Parses `[+-]digits[.digits][e[+-]digits]` (or `.digits`) as a finite
/// number; text with a leading zero before more digits is not a number.
pub fn plain_number(t: &str) -> Option<f64> {
    let b = t.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let int_start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let int_len = i - int_start;
    if int_len > 1 && b[int_start] == b'0' {
        return None;
    }
    let mut frac_len = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let s = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        frac_len = i - s;
    }
    if int_len == 0 && frac_len == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let s = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == s {
            return None;
        }
    }
    if i != b.len() {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Trims header names, names empty ones `column N` and makes repeated ones
/// unique by appending `_2`, `_3`.
pub fn normalize_header(header: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(header.len());
    for (i, raw) in header.iter().enumerate() {
        let base = match raw.trim() {
            "" => format!("column {}", i + 1),
            t => t.to_owned(),
        };
        let mut name = base.clone();
        let mut n = 2;
        while out.contains(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        out.push(name);
    }
    out
}

/// Reads rows into a [`Table`] within the bounds.
#[derive(Debug)]
pub struct Builder {
    table: Table,
    keep: Option<Vec<String>>,
    /// For header-driven sources: the table column of each source column.
    slots: Vec<Option<usize>>,
    max_rows: usize,
    max_bytes: usize,
}

impl Builder {
    /// A builder keeping only the columns in `keep` (all when `None`).
    pub fn new(keep: Option<Vec<String>>) -> Self {
        Self::with_limits(keep, MAX_ROWS, MAX_TABLE_BYTES)
    }

    /// A builder with explicit bounds.
    pub fn with_limits(keep: Option<Vec<String>>, max_rows: usize, max_bytes: usize) -> Self {
        Self {
            table: Table::default(),
            keep,
            slots: Vec::new(),
            max_rows,
            max_bytes,
        }
    }

    /// True when a column of this name would be kept.
    pub fn wants(&self, name: &str) -> bool {
        self.keep
            .as_ref()
            .is_none_or(|k| k.iter().any(|n| n == name.trim()))
    }

    fn column(&mut self, name: &str) -> Option<usize> {
        if let Some(i) = self.table.col(name) {
            return Some(i);
        }
        if !self.wants(name) {
            return None;
        }
        if self.table.names.len() >= MAX_COLUMNS {
            self.table.dropped_columns += 1;
            return None;
        }
        self.table.names.push(name.to_owned());
        self.table.cols.push(vec![Cell::Null; self.table.rows]);
        Some(self.table.names.len() - 1)
    }

    /// Declares the header of a positional source (CSV, `columns`).
    pub fn set_header(&mut self, header: &[String]) {
        let names = normalize_header(header);
        self.slots = names.iter().map(|name| self.column(name)).collect();
    }

    /// The table column for source column `i`, if kept.
    pub fn slot(&self, i: usize) -> Option<usize> {
        self.slots.get(i).copied().flatten()
    }

    /// Number of source columns declared by the header.
    pub fn header_len(&self) -> usize {
        self.slots.len()
    }

    /// True when reading already stopped at a bound.
    pub fn is_stopped(&self) -> bool {
        self.table.stopped.is_some()
    }

    /// True once no further row may be added; records why.
    pub fn is_full(&mut self) -> bool {
        if self.table.stopped.is_some() {
            return true;
        }
        if self.table.rows >= self.max_rows {
            self.table.stopped = Some(Stop::Rows);
        } else if self.table.bytes() >= self.max_bytes {
            self.table.stopped = Some(Stop::Bytes);
        }
        self.table.stopped.is_some()
    }

    /// Text becomes a number when it is plain decimal notation, a null when
    /// it is empty or a null marker, and stays text otherwise. Leading zeros
    /// keep an identifier like `007` as text.
    pub fn text_cell(&mut self, raw: &str) -> Cell {
        let t = raw.trim();
        if t.is_empty()
            || ["null", "na", "n/a", "nan", "none"]
                .iter()
                .any(|m| t.eq_ignore_ascii_case(m))
        {
            return Cell::Null;
        }
        match plain_number(t) {
            Some(v) => Cell::Num(v),
            None => Cell::Str(self.table.pool.intern(t)),
        }
    }

    /// A number cell; non-finite numbers become null.
    pub fn num_cell(&self, v: f64) -> Cell {
        if v.is_finite() {
            Cell::Num(v)
        } else {
            Cell::Null
        }
    }

    /// Records a value that was a list or an object.
    pub fn note_nested(&mut self) {
        self.table.nested += 1;
    }

    /// Marks that a row had a different number of fields than the header.
    pub fn note_ragged(&mut self) {
        self.table.ragged += 1;
    }

    /// Marks bytes that were not valid UTF-8.
    pub fn note_bad_utf8(&mut self) {
        self.table.bad_utf8 = true;
    }

    fn begin_row(&mut self) -> usize {
        for col in &mut self.table.cols {
            col.push(Cell::Null);
        }
        self.table.rows += 1;
        self.table.rows - 1
    }

    /// Adds one row by name (records).
    pub fn push_record(&mut self, fields: Vec<(String, Cell)>) {
        let row = self.begin_row();
        for (name, cell) in fields {
            if let Some(i) = self.column(name.trim()) {
                self.table.cols[i][row] = cell;
            }
        }
    }

    /// Adds one row by source column position (header-driven sources).
    pub fn push_positional(&mut self, cells: &[Cell]) {
        let row = self.begin_row();
        for (i, cell) in cells.iter().enumerate() {
            if let Some(slot) = self.slot(i) {
                self.table.cols[slot][row] = *cell;
            }
        }
    }

    /// The finished table.
    pub fn finish(self) -> Table {
        self.table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_driven_table_keeps_only_the_wanted_columns() {
        let mut b = Builder::new(Some(vec!["b".into(), "d".into()]));
        b.set_header(&["a".into(), "b".into(), "c".into(), "d".into()]);
        for i in 0..3 {
            let cells = vec![
                Cell::Num(1.0),
                Cell::Num(f64::from(i)),
                Cell::Num(9.0),
                Cell::Null,
            ];
            b.push_positional(&cells);
        }
        let t = b.finish();
        assert_eq!(t.names, ["b", "d"]);
        assert_eq!(t.rows, 3);
        assert_eq!(t.cols[0], [Cell::Num(0.0), Cell::Num(1.0), Cell::Num(2.0)]);
        assert_eq!(t.cols[1], [Cell::Null; 3]);
    }

    #[test]
    fn short_rows_are_padded_with_nulls() {
        let mut b = Builder::new(None);
        b.set_header(&["a".into(), "b".into()]);
        b.push_positional(&[Cell::Num(1.0)]);
        b.push_positional(&[Cell::Num(2.0), Cell::Num(3.0), Cell::Num(4.0)]);
        let t = b.finish();
        assert_eq!(t.cols[1], [Cell::Null, Cell::Num(3.0)]);
        assert_eq!(t.rows, 2);
    }

    #[test]
    fn records_grow_columns_and_backfill_earlier_rows() {
        let mut b = Builder::new(None);
        b.push_record(vec![("a".into(), Cell::Num(1.0))]);
        b.push_record(vec![
            ("a".into(), Cell::Num(2.0)),
            ("b".into(), Cell::Num(5.0)),
        ]);
        b.push_record(vec![("b".into(), Cell::Num(6.0))]);
        let t = b.finish();
        assert_eq!(t.names, ["a", "b"]);
        assert_eq!(t.cols[0], [Cell::Num(1.0), Cell::Num(2.0), Cell::Null]);
        assert_eq!(t.cols[1], [Cell::Null, Cell::Num(5.0), Cell::Num(6.0)]);
    }

    #[test]
    fn text_cells_are_pooled_and_null_markers_become_null() {
        let mut b = Builder::new(None);
        let a = b.text_cell("north");
        let again = b.text_cell(" north ");
        assert_eq!(a, again);
        for marker in ["", "  ", "null", "NULL", "NA", "n/a", "NaN", "None"] {
            assert_eq!(b.text_cell(marker), Cell::Null, "{marker:?}");
        }
        assert_eq!(b.text_cell("12.5"), Cell::Num(12.5));
    }

    #[test]
    fn plain_numbers_parse_and_everything_else_stays_text() {
        for (t, v) in [
            ("0", 0.0),
            ("12", 12.0),
            ("-3.5", -3.5),
            ("+4", 4.0),
            (".5", 0.5),
            ("5.", 5.0),
            ("1e3", 1000.0),
            ("2.5E-2", 0.025),
            ("0.75", 0.75),
            ("-0", -0.0),
        ] {
            assert_eq!(plain_number(t), Some(v), "{t}");
        }
        for t in [
            "", "-", ".", "e5", "007", "1,234", "1_000", "0x10", "inf", "nan", "1e", "1e+",
            "12abc", "1.2.3", " 1", "--1", "1e999", "$5", "5%",
        ] {
            assert_eq!(plain_number(t), None, "{t}");
        }
    }

    #[test]
    fn numeric_text_cells_become_numbers_and_identifiers_stay_text() {
        let mut b = Builder::new(None);
        assert_eq!(b.text_cell(" 42 "), Cell::Num(42.0));
        assert!(matches!(b.text_cell("007"), Cell::Str(_)));
        assert!(matches!(b.text_cell("2024-01-02"), Cell::Str(_)));
    }

    #[test]
    fn headers_are_trimmed_named_and_made_unique() {
        let h = normalize_header(&[" a ".into(), "".into(), "a".into(), "a".into(), "  ".into()]);
        assert_eq!(h, ["a", "column 2", "a_2", "a_3", "column 5"]);
    }

    #[test]
    fn non_finite_numbers_become_null() {
        let b = Builder::new(None);
        assert_eq!(b.num_cell(f64::NAN), Cell::Null);
        assert_eq!(b.num_cell(f64::INFINITY), Cell::Null);
        assert_eq!(b.num_cell(2.5), Cell::Num(2.5));
    }

    #[test]
    fn the_row_limit_stops_the_builder_and_says_why() {
        let mut b = Builder::with_limits(None, 3, usize::MAX);
        b.set_header(&["a".into()]);
        let mut pushed = 0;
        while !b.is_full() {
            b.push_positional(&[Cell::Num(1.0)]);
            pushed += 1;
        }
        let t = b.finish();
        assert_eq!((pushed, t.rows, t.stopped), (3, 3, Some(Stop::Rows)));
    }

    #[test]
    fn the_byte_budget_stops_the_builder_before_the_row_limit() {
        let mut b = Builder::with_limits(None, usize::MAX, 16 * 100);
        b.set_header(&["a".into()]);
        while !b.is_full() {
            b.push_positional(&[Cell::Num(1.0)]);
        }
        let t = b.finish();
        assert_eq!(t.stopped, Some(Stop::Bytes));
        assert_eq!(t.rows, 100);
    }

    #[test]
    fn text_bytes_count_against_the_budget() {
        let mut b = Builder::with_limits(None, usize::MAX, 4096);
        b.set_header(&["a".into()]);
        let mut i = 0;
        while !b.is_full() {
            let c = b.text_cell(&format!("value-{i}"));
            b.push_positional(&[c]);
            i += 1;
        }
        assert!(i < 100, "{i} rows fit a 4 KiB budget of distinct strings");
    }

    #[test]
    fn the_column_limit_drops_extra_columns_and_counts_them() {
        let mut b = Builder::new(None);
        let header: Vec<String> = (0..MAX_COLUMNS + 5).map(|i| format!("c{i}")).collect();
        b.set_header(&header);
        let t = b.finish();
        assert_eq!(t.names.len(), MAX_COLUMNS);
        assert_eq!(t.dropped_columns, 5);
    }

    #[test]
    fn column_lookup_and_text_access() {
        let mut b = Builder::new(None);
        b.set_header(&[" x ".into()]);
        let c = b.text_cell("hello");
        b.push_positional(&[c]);
        let t = b.finish();
        assert_eq!(t.col("x"), Some(0));
        assert_eq!(t.col("y"), None);
        assert_eq!(t.text(t.cols[0][0]), Some("hello"));
        assert_eq!(t.text(Cell::Num(1.0)), None);
    }
}
