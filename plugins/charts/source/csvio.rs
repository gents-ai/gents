//! A streaming CSV reader (RFC 4180 quoting, CRLF or LF, a delimiter sniffed
//! from the header line) that feeds a [`Builder`] one record at a time. It
//! never holds more than one record, and fails loudly on a cell too large to
//! be real data.

use std::io::BufRead;

use crate::err::{fail, Res};
use crate::table::{Builder, Cell, MAX_CELL_BYTES};

const MAX_FIELDS: usize = 100_000;

/// Picks the delimiter that occurs most in the first line, outside quotes.
pub fn sniff(head: &[u8]) -> u8 {
    let mut counts = [(b',', 0_usize), (b';', 0), (b'\t', 0), (b'|', 0)];
    let mut quoted = false;
    for &b in head {
        match b {
            b'"' => quoted = !quoted,
            b'\n' if !quoted => break,
            _ if !quoted => {
                if let Some(c) = counts.iter_mut().find(|(d, _)| *d == b) {
                    c.1 += 1;
                }
            }
            _ => {}
        }
    }
    counts.iter().rev().max_by_key(|(_, n)| *n).filter(|(_, n)| *n > 0).map_or(b',', |(d, _)| *d)
}

/// Byte-at-a-time record parser: state carries across buffer boundaries, so
/// a quote pair split between two reads is still one escaped quote.
struct Parser {
    delim: u8,
    field: Vec<u8>,
    fields: Vec<Vec<u8>>,
    quoted: bool,
    was_quoted: bool,
    pending_quote: bool,
    any: bool,
}

impl Parser {
    fn new(delim: u8) -> Self {
        Self { delim, field: Vec::new(), fields: Vec::new(), quoted: false, was_quoted: false, pending_quote: false, any: false }
    }

    fn end_field(&mut self) {
        if !self.was_quoted && self.field.last() == Some(&b'\r') {
            self.field.pop();
        }
        self.fields.push(std::mem::take(&mut self.field));
        self.was_quoted = false;
    }

    /// Feeds one byte; true when it completed a record, left in `fields`.
    fn feed(&mut self, b: u8) -> Res<bool> {
        self.any = true;
        if self.pending_quote {
            self.pending_quote = false;
            if b == b'"' {
                self.field.push(b'"');
                return Ok(false);
            }
            self.quoted = false;
        }
        if self.quoted {
            if b == b'"' {
                self.pending_quote = true;
            } else {
                self.field.push(b);
            }
        } else if b == b'"' && self.field.is_empty() && !self.was_quoted {
            self.quoted = true;
            self.was_quoted = true;
        } else if b == self.delim {
            self.end_field();
        } else if b == b'\n' {
            self.end_field();
            self.any = false;
            return Ok(true);
        } else {
            self.field.push(b);
        }
        if self.field.len() > MAX_CELL_BYTES {
            return fail("a cell is longer than 65536 bytes, so this is not a table; check the file");
        }
        if self.fields.len() > MAX_FIELDS {
            return fail("a row has more than 100000 columns, so this is not a table; check the file");
        }
        Ok(false)
    }

    /// The last record when the input ends without a final newline.
    fn finish(&mut self) -> bool {
        if self.pending_quote {
            self.pending_quote = false;
            self.quoted = false;
        }
        if !self.any {
            return false;
        }
        self.end_field();
        self.any = false;
        true
    }

    fn clear(&mut self) {
        self.fields.clear();
    }
}

/// Where a record goes: the header first, then the rows.
struct Sink<'a> {
    builder: &'a mut Builder,
    header_done: bool,
    cells: Vec<Cell>,
}

impl Sink<'_> {
    fn text(&mut self, f: &[u8]) -> String {
        match std::str::from_utf8(f) {
            Ok(s) => s.to_owned(),
            Err(_) => {
                self.builder.note_bad_utf8();
                String::from_utf8_lossy(f).into_owned()
            }
        }
    }

    /// Takes one record; false when the builder is full and reading must stop.
    fn record(&mut self, fields: &[Vec<u8>]) -> bool {
        if fields.len() == 1 && fields[0].iter().all(u8::is_ascii_whitespace) {
            return true;
        }
        if !self.header_done {
            let header: Vec<String> = fields.iter().map(|f| self.text(f)).collect();
            self.builder.set_header(&header);
            self.header_done = true;
            return true;
        }
        if self.builder.is_full() {
            return false;
        }
        if fields.len() != self.builder.header_len() {
            self.builder.note_ragged();
        }
        let mut cells = std::mem::take(&mut self.cells);
        cells.clear();
        for (i, f) in fields.iter().enumerate() {
            if self.builder.slot(i).is_some() {
                let s = self.text(f);
                cells.push(self.builder.text_cell(&s));
            } else {
                cells.push(Cell::Null);
            }
        }
        self.builder.push_positional(&cells);
        self.cells = cells;
        true
    }
}

/// Streams `r` into `builder`: the first record is the header.
pub fn read<R: BufRead>(mut r: R, builder: &mut Builder) -> Res<()> {
    let io = |e: std::io::Error| format!("the file could not be read: {e}");
    let head = r.fill_buf().map_err(io)?;
    let skip = if head.starts_with(&[0xef, 0xbb, 0xbf]) { 3 } else { 0 };
    let delim = sniff(&head[skip..]);
    r.consume(skip);
    let mut parser = Parser::new(delim);
    let mut sink = Sink { builder, header_done: false, cells: Vec::new() };
    loop {
        let buf = r.fill_buf().map_err(io)?;
        if buf.is_empty() {
            break;
        }
        let n = buf.len();
        for &b in buf {
            if parser.feed(b)? {
                let more = sink.record(&parser.fields);
                parser.clear();
                if !more {
                    return Ok(());
                }
            }
        }
        r.consume(n);
    }
    if parser.finish() {
        sink.record(&parser.fields);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Table;

    fn load(input: &[u8]) -> Table {
        let mut b = Builder::new(None);
        read(std::io::BufReader::with_capacity(7, input), &mut b).expect("reads");
        b.finish()
    }

    fn cell(t: &Table, col: &str, row: usize) -> String {
        match t.cols[t.col(col).unwrap()][row] {
            Cell::Null => "<null>".into(),
            Cell::Num(v) => format!("#{v}"),
            Cell::Str(i) => t.pool.get(i).to_owned(),
        }
    }

    #[test]
    fn a_plain_file_becomes_columns_and_typed_cells() {
        let t = load(b"name,score,when\nAna,9,2024-01-01\nBo,7.5,2024-02-01\n");
        assert_eq!(t.names, ["name", "score", "when"]);
        assert_eq!(t.rows, 2);
        assert_eq!(cell(&t, "name", 1), "Bo");
        assert_eq!(cell(&t, "score", 1), "#7.5");
        assert_eq!(cell(&t, "when", 0), "2024-01-01");
    }

    #[test]
    fn quoting_commas_quotes_and_newlines_inside_cells() {
        let t = load(b"a,b\n\"x, y\",\"he said \"\"hi\"\"\"\n\"line1\nline2\",3\n");
        assert_eq!(t.rows, 2);
        assert_eq!(cell(&t, "a", 0), "x, y");
        assert_eq!(cell(&t, "b", 0), "he said \"hi\"");
        assert_eq!(cell(&t, "a", 1), "line1\nline2");
        assert_eq!(cell(&t, "b", 1), "#3");
    }

    #[test]
    fn crlf_endings_bom_and_a_missing_final_newline() {
        let t = load(b"\xef\xbb\xbfa,b\r\n1,2\r\n3,4");
        assert_eq!(t.names, ["a", "b"]);
        assert_eq!((cell(&t, "b", 0), cell(&t, "b", 1)), ("#2".into(), "#4".into()));
    }

    #[test]
    fn quotes_split_across_buffer_boundaries_still_pair_up() {
        for cap in 1..16 {
            let mut b = Builder::new(None);
            let input = b"a,b\n\"p\"\"q\",\"r\"\n";
            read(std::io::BufReader::with_capacity(cap, &input[..]), &mut b).unwrap();
            let t = b.finish();
            assert_eq!(cell(&t, "a", 0), "p\"q", "capacity {cap}");
            assert_eq!(cell(&t, "b", 0), "r", "capacity {cap}");
        }
    }

    #[test]
    fn semicolon_tab_and_pipe_delimiters_are_sniffed() {
        for (d, sep) in [("semi", ';'), ("tab", '\t'), ("pipe", '|')] {
            let text = format!("a{sep}b\n1{sep}2\n");
            let t = load(text.as_bytes());
            assert_eq!(t.names, ["a", "b"], "{d}");
            assert_eq!(cell(&t, "b", 0), "#2", "{d}");
        }
    }

    #[test]
    fn a_delimiter_inside_quotes_does_not_count_when_sniffing() {
        assert_eq!(sniff(b"\"a;b;c;d\",e\n1,2"), b',');
        assert_eq!(sniff(b"abc\n"), b',');
        assert_eq!(sniff(b""), b',');
    }

    #[test]
    fn blank_lines_are_skipped_and_ragged_rows_are_counted() {
        let t = load(b"a,b\n\n1,2\n   \n3\n4,5,6\n");
        assert_eq!(t.rows, 3);
        assert_eq!(t.ragged, 2);
        assert_eq!(cell(&t, "b", 1), "<null>");
        assert_eq!(cell(&t, "b", 2), "#5");
    }

    #[test]
    fn empty_and_null_marker_cells_are_null() {
        let t = load(b"a,b\n,N/A\nNULL,x\n");
        assert_eq!(cell(&t, "a", 0), "<null>");
        assert_eq!(cell(&t, "b", 0), "<null>");
        assert_eq!(cell(&t, "a", 1), "<null>");
        assert_eq!(cell(&t, "b", 1), "x");
    }

    #[test]
    fn invalid_utf8_is_replaced_and_reported() {
        let t = load(b"a\nab\xffcd\n");
        assert!(t.bad_utf8);
        assert_eq!(cell(&t, "a", 0), "ab\u{fffd}cd");
    }

    #[test]
    fn a_header_only_file_has_columns_and_no_rows() {
        let t = load(b"a,b\n");
        assert_eq!((t.names.len(), t.rows), (2, 0));
        let empty = load(b"");
        assert_eq!((empty.names.len(), empty.rows), (0, 0));
    }

    #[test]
    fn an_oversized_cell_is_refused() {
        let mut data = b"a\n".to_vec();
        data.extend(std::iter::repeat_n(b'x', MAX_CELL_BYTES + 10));
        let mut b = Builder::new(None);
        let e = read(&data[..], &mut b).unwrap_err();
        assert!(e.0.contains("longer than 65536 bytes"), "{e}");
    }

    #[test]
    fn an_unterminated_quote_swallows_the_rest_and_is_caught_by_the_cell_limit() {
        let mut data = b"a\n\"never closed\n".to_vec();
        data.extend(std::iter::repeat_n(b'y', MAX_CELL_BYTES + 5));
        let mut b = Builder::new(None);
        assert!(read(&data[..], &mut b).is_err());
    }

    #[test]
    fn a_row_with_a_hundred_thousand_columns_is_refused() {
        let mut data = Vec::new();
        for _ in 0..100_002 {
            data.extend_from_slice(b",");
        }
        let mut b = Builder::new(None);
        assert!(read(&data[..], &mut b).unwrap_err().0.contains("100000 columns"));
    }

    #[test]
    fn the_row_limit_stops_reading_and_is_recorded() {
        let mut data = String::from("a\n");
        for i in 0..50 {
            data.push_str(&format!("{i}\n"));
        }
        let mut b = Builder::with_limits(None, 10, usize::MAX);
        read(data.as_bytes(), &mut b).unwrap();
        let t = b.finish();
        assert_eq!(t.rows, 10);
        assert_eq!(t.stopped, Some(crate::table::Stop::Rows));
    }

    #[test]
    fn projection_keeps_only_the_wanted_columns() {
        let mut b = Builder::new(Some(vec!["c".into()]));
        read(&b"a,b,c\n1,2,3\n4,5,6\n"[..], &mut b).unwrap();
        let t = b.finish();
        assert_eq!(t.names, ["c"]);
        assert_eq!(cell(&t, "c", 1), "#6");
    }

    #[test]
    fn duplicate_header_names_are_made_unique() {
        let t = load(b"a,a,\n1,2,3\n");
        assert_eq!(t.names, ["a", "a_2", "column 3"]);
    }
}
