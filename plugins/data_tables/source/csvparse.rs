//! A streaming CSV tokenizer: records one at a time out of any reader, with
//! flat memory. It keeps what the usual parsers throw away, whether a field
//! was quoted, so an empty field (NULL) and an empty quoted field (the empty
//! string) stay distinct. Quotes, doubled quotes, embedded newlines, `\n`,
//! `\r\n` and a lone `\r` as record ends and blank lines are handled; a
//! record never grows past [`MAX_RECORD_BYTES`].
use std::io::Read;

use crate::Res;

/// The most bytes one record may take before the file is refused.
pub const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
const CHUNK: usize = 256 * 1024;

/// One record: the unescaped bytes of every field, back to back.
#[derive(Default)]
pub struct Record {
    data: Vec<u8>,
    /// End offset into `data` and whether the field was quoted, per field.
    ends: Vec<(usize, bool)>,
    /// The 1-based line the record started on.
    pub line: u64,
}

impl Record {
    /// The number of fields.
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// The bytes the record holds.
    pub fn bytes(&self) -> usize {
        self.data.len()
    }

    /// The bytes of field `i` and whether it was quoted, or `None` past the end.
    pub fn field(&self, i: usize) -> Option<(&[u8], bool)> {
        let &(end, quoted) = self.ends.get(i)?;
        let start = if i == 0 { 0 } else { self.ends[i - 1].0 };
        Some((&self.data[start..end], quoted))
    }

    fn clear(&mut self) {
        self.data.clear();
        self.ends.clear();
    }
}

/// Reads records from `R`.
pub struct Reader<R: Read> {
    src: R,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    eof: bool,
    delim: u8,
    line: u64,
    started: bool,
}

/// What ended a field.
enum Stop {
    Delim,
    Newline,
    Eof,
}

impl<R: Read> Reader<R> {
    /// A reader over `src` splitting fields on `delim`.
    pub fn new(src: R, delim: u8) -> Self {
        Self {
            src,
            buf: vec![0; CHUNK],
            pos: 0,
            end: 0,
            eof: false,
            delim,
            line: 1,
            started: false,
        }
    }

    fn fill(&mut self) -> Res<bool> {
        if self.eof {
            return Ok(false);
        }
        loop {
            match self.src.read(&mut self.buf) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(false);
                }
                Ok(n) => {
                    self.pos = 0;
                    self.end = n;
                    return Ok(true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("cannot read the file: {e}")),
            }
        }
    }

    fn peek(&mut self) -> Res<Option<u8>> {
        if self.pos >= self.end && !self.fill()? {
            return Ok(None);
        }
        Ok(Some(self.buf[self.pos]))
    }

    fn too_long(&self, line: u64) -> String {
        format!(
            "a row on line {line} is over {} MiB; check the quoting and the delimiter",
            MAX_RECORD_BYTES / 1024 / 1024
        )
    }

    /// Reads the next record into `rec`; `Ok(false)` at the end of the input.
    pub fn read(&mut self, rec: &mut Record) -> Res<bool> {
        rec.clear();
        if !self.started {
            self.started = true;
            self.skip_bom()?;
        }
        // Blank lines are not records.
        loop {
            match self.peek()? {
                None => return Ok(false),
                Some(b'\n') => {
                    self.pos += 1;
                    self.line += 1;
                }
                Some(b'\r') => {
                    self.pos += 1;
                    if self.peek()? == Some(b'\n') {
                        self.pos += 1;
                    }
                    self.line += 1;
                }
                Some(_) => break,
            }
        }
        rec.line = self.line;
        loop {
            let (quoted, stop) = self.field(rec)?;
            rec.ends.push((rec.data.len(), quoted));
            match stop {
                Stop::Delim => {}
                Stop::Newline | Stop::Eof => return Ok(true),
            }
        }
    }

    fn skip_bom(&mut self) -> Res<()> {
        // The three bytes of a UTF-8 byte order mark may straddle two reads.
        let mut seen = 0;
        while seen < 3 {
            match self.peek()? {
                Some(b) if b == [0xEF, 0xBB, 0xBF][seen] => {
                    self.pos += 1;
                    seen += 1;
                }
                _ => break,
            }
        }
        if seen > 0 && seen < 3 {
            return Err("the file starts with a partial byte order mark".into());
        }
        Ok(())
    }

    fn field(&mut self, rec: &mut Record) -> Res<(bool, Stop)> {
        let start_line = self.line;
        if self.peek()? == Some(b'"') {
            self.pos += 1;
            self.quoted(rec, start_line)?;
            // Bytes after the closing quote and before the delimiter are kept as they are.
            let stop = self.plain(rec, start_line)?;
            return Ok((true, stop));
        }
        let stop = self.plain(rec, start_line)?;
        Ok((false, stop))
    }

    /// Appends an unquoted run up to the delimiter, a newline or the end.
    fn plain(&mut self, rec: &mut Record, line: u64) -> Res<Stop> {
        loop {
            if self.pos >= self.end && !self.fill()? {
                return Ok(Stop::Eof);
            }
            let window = &self.buf[self.pos..self.end];
            let hit = memchr::memchr3(self.delim, b'\n', b'\r', window);
            let take = hit.unwrap_or(window.len());
            if rec.data.len() + take > MAX_RECORD_BYTES {
                return Err(self.too_long(line));
            }
            rec.data.extend_from_slice(&window[..take]);
            self.pos += take;
            let Some(_) = hit else { continue };
            let byte = self.buf[self.pos];
            self.pos += 1;
            if byte == self.delim {
                return Ok(Stop::Delim);
            }
            if byte == b'\r' && self.peek()? == Some(b'\n') {
                self.pos += 1;
            }
            self.line += 1;
            return Ok(Stop::Newline);
        }
    }

    /// Appends the inside of a quoted field and consumes its closing quote.
    fn quoted(&mut self, rec: &mut Record, line: u64) -> Res<()> {
        loop {
            if self.pos >= self.end && !self.fill()? {
                return Err(format!(
                    "the quoted field on line {line} is never closed; check the quoting"
                ));
            }
            let window = &self.buf[self.pos..self.end];
            let hit = memchr::memchr2(b'"', b'\n', window);
            let take = hit.unwrap_or(window.len());
            if rec.data.len() + take > MAX_RECORD_BYTES {
                return Err(self.too_long(line));
            }
            rec.data.extend_from_slice(&window[..take]);
            self.pos += take;
            let Some(_) = hit else { continue };
            let byte = self.buf[self.pos];
            self.pos += 1;
            if byte == b'\n' {
                rec.data.push(b'\n');
                self.line += 1;
                continue;
            }
            if self.peek()? == Some(b'"') {
                self.pos += 1;
                rec.data.push(b'"');
                continue;
            }
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    fn parse(input: &[u8], delim: u8) -> Res<Vec<Vec<(Vec<u8>, bool)>>> {
        let mut reader = Reader::new(Cursor::new(input.to_vec()), delim);
        let mut rec = Record::default();
        let mut out = Vec::new();
        while reader.read(&mut rec)? {
            out.push(
                (0..rec.len())
                    .map(|i| {
                        let (b, q) = rec.field(i).unwrap();
                        (b.to_vec(), q)
                    })
                    .collect(),
            );
        }
        Ok(out)
    }

    fn texts(input: &str) -> Vec<Vec<String>> {
        parse(input.as_bytes(), b',')
            .unwrap()
            .into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|(b, _)| String::from_utf8(b).unwrap())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn splits_plain_records_on_every_line_ending() {
        let want = vec![vec!["a", "b"], vec!["c", "d"], vec!["e", "f"]];
        assert_eq!(texts("a,b\nc,d\ne,f"), want);
        assert_eq!(texts("a,b\r\nc,d\r\ne,f\r\n"), want);
        assert_eq!(texts("a,b\rc,d\re,f\r"), want);
    }

    #[test]
    fn keeps_quoted_commas_quotes_and_newlines() {
        assert_eq!(
            texts("\"a,b\",\"say \"\"hi\"\"\",\"l1\nl2\"\n"),
            vec![vec!["a,b", "say \"hi\"", "l1\nl2"]]
        );
        assert_eq!(texts("\"a\r\nb\"\n"), vec![vec!["a\r\nb"]]);
    }

    #[test]
    fn tells_an_empty_field_from_an_empty_quoted_field() {
        let rows = parse(b"a,,\"\",b\n", b',').unwrap();
        let flags: Vec<(usize, bool)> = rows[0].iter().map(|(b, q)| (b.len(), *q)).collect();
        assert_eq!(flags, vec![(1, false), (0, false), (0, true), (1, false)]);
    }

    #[test]
    fn skips_blank_lines_and_a_byte_order_mark() {
        assert_eq!(
            texts("\u{feff}a,b\n\n\r\n,\nc,d\n"),
            vec![vec!["a", "b"], vec!["", ""], vec!["c", "d"]]
        );
    }

    #[test]
    fn a_final_record_without_a_newline_and_a_trailing_delimiter() {
        assert_eq!(texts("a,b,"), vec![vec!["a", "b", ""]]);
        assert!(texts("").is_empty());
        assert!(texts("\n\n").is_empty());
    }

    #[test]
    fn text_after_a_closing_quote_is_kept() {
        assert_eq!(texts("\"ab\"cd,e\n"), vec![vec!["abcd", "e"]]);
    }

    #[test]
    fn a_bare_quote_inside_an_unquoted_field_is_literal() {
        assert_eq!(texts("5\" pipe,x\n"), vec![vec!["5\" pipe", "x"]]);
    }

    #[test]
    fn an_unclosed_quote_names_its_line() {
        let err = parse(b"a,b\nc,\"oops\nmore\n", b',').unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("never closed"), "{err}");
    }

    #[test]
    fn line_numbers_count_newlines_inside_quotes() {
        let mut reader = Reader::new(Cursor::new(b"a\n\"x\ny\"\nb\n".to_vec()), b',');
        let mut rec = Record::default();
        let mut lines = Vec::new();
        while reader.read(&mut rec).unwrap() {
            lines.push(rec.line);
        }
        assert_eq!(lines, vec![1, 2, 4]);
    }

    #[test]
    fn a_record_over_the_limit_is_refused() {
        let mut data = vec![b'"'];
        data.extend(std::iter::repeat_n(b'x', MAX_RECORD_BYTES + 10));
        data.push(b'"');
        let err = parse(&data, b',').unwrap_err();
        assert!(err.contains("over 16 MiB"), "{err}");
        let err = parse(&vec![b'y'; MAX_RECORD_BYTES + 10], b',').unwrap_err();
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn works_across_chunk_boundaries() {
        // A quoted field and a delimiter pair that straddle the 256 KiB chunk size.
        let big = "x".repeat(CHUNK - 3);
        let input = format!("\"{big}\"\"y\",z\n{big},\"q\"\n");
        let rows = texts(&input);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].len(), CHUNK - 3 + 2);
        assert_eq!(rows[0][1], "z");
        assert_eq!(rows[1][1], "q");
    }

    #[test]
    fn a_byte_order_mark_split_across_reads_is_still_removed() {
        struct Dribble(Vec<u8>);
        impl Read for Dribble {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(0);
                }
                out[0] = self.0.remove(0);
                Ok(1)
            }
        }
        let mut reader = Reader::new(Dribble(b"\xEF\xBB\xBFa,b\n".to_vec()), b',');
        let mut rec = Record::default();
        assert!(reader.read(&mut rec).unwrap());
        assert_eq!(rec.field(0).unwrap().0, b"a");
    }

    #[test]
    fn other_delimiters_work() {
        let rows = parse(b"a\tb\n\"c\td\"\te\n", b'\t').unwrap();
        assert_eq!(rows[1][0].0, b"c\td");
        assert_eq!(rows[1].len(), 2);
    }

    /// Writes fields the way an exporter must for them to read back unchanged.
    fn encode(rows: &[Vec<Option<String>>]) -> String {
        let mut out = String::new();
        for row in rows {
            let cells: Vec<String> = row
                .iter()
                .map(|c| match c {
                    None => String::new(),
                    Some(s) if s.is_empty() => "\"\"".to_string(),
                    Some(s) if s.contains([',', '"', '\n', '\r']) => {
                        format!("\"{}\"", s.replace('"', "\"\""))
                    }
                    Some(s) => s.clone(),
                })
                .collect();
            out.push_str(&cells.join(","));
            out.push('\n');
        }
        out
    }

    proptest! {
        #[test]
        fn encoded_records_read_back_exactly(rows in prop::collection::vec(
            prop::collection::vec(prop::option::of("[ -~\n\r]{0,12}"), 2..5), 1..12)) {
            // A row of one NULL would be a blank line; every row here has two or more fields.
            let encoded = encode(&rows);
            let parsed = parse(encoded.as_bytes(), b',').unwrap();
            prop_assert_eq!(parsed.len(), rows.len());
            for (got, want) in parsed.iter().zip(&rows) {
                prop_assert_eq!(got.len(), want.len());
                for ((bytes, quoted), cell) in got.iter().zip(want) {
                    match cell {
                        None => prop_assert!(bytes.is_empty() && !quoted),
                        Some(s) => {
                            prop_assert_eq!(bytes.as_slice(), s.as_bytes());
                            prop_assert_eq!(*quoted, s.is_empty() || s.contains([',', '"', '\n', '\r']));
                        }
                    }
                }
            }
        }

        #[test]
        fn arbitrary_bytes_never_panic(data in prop::collection::vec(any::<u8>(), 0..400)) {
            let _ = parse(&data, b',');
        }
    }
}
