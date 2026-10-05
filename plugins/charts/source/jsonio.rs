//! Streaming JSON table reader. Rows are read one at a time straight from the
//! text, never as a whole document, so the table size is bounded by the
//! builder and not by the input.
//!
//! Accepted shapes: an array of objects; `{"columns": [...], "rows": [[...]]}`
//! (the shape the data_tables pack returns), with rows as lists or as objects;
//! either of those wrapped in a `response`, `result`, `table`, `data` or
//! `output` key; and one object per line (JSON Lines).

use std::fmt;
use std::io::{BufRead, Read};

use serde::Deserializer;
use serde::de::{DeserializeSeed, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::err::{Res, fail};
use crate::table::{Builder, Cell, MAX_CELL_BYTES};

const STOP: &str = "the row limit was reached";
const WRAPPERS: [&str; 6] = ["response", "result", "results", "table", "data", "output"];
const MAX_LINE: usize = 16 * 1024 * 1024;

struct CellSeed<'a>(&'a mut Builder);

impl<'de> DeserializeSeed<'de> for CellSeed<'_> {
    type Value = Cell;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Cell, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for CellSeed<'_> {
    type Value = Cell;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a value")
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Cell, E> {
        Ok(self.0.text_cell(if v { "true" } else { "false" }))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Cell, E> {
        Ok(self.0.num_cell(v as f64))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Cell, E> {
        Ok(self.0.num_cell(v as f64))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Cell, E> {
        Ok(self.0.num_cell(v))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Cell, E> {
        if v.len() > MAX_CELL_BYTES {
            return Err(E::custom(
                "a cell is longer than 65536 bytes, so this is not a table; check the file",
            ));
        }
        Ok(self.0.text_cell(v))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Cell, E> {
        Ok(Cell::Null)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Cell, E> {
        Ok(Cell::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Cell, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        self.0.note_nested();
        Ok(Cell::Null)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Cell, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        self.0.note_nested();
        Ok(Cell::Null)
    }
}

/// A key that is only copied when the builder keeps its column.
struct KeySeed<'a>(&'a Builder);

impl<'de> DeserializeSeed<'de> for KeySeed<'_> {
    type Value = Option<String>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for KeySeed<'_> {
    type Value = Option<String>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a key")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(self.0.wants(v).then(|| v.to_owned()))
    }
}

struct Row<'a>(&'a mut Builder);

impl<'de> DeserializeSeed<'de> for Row<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Row<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a row")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut fields = Vec::new();
        while let Some(key) = map.next_key_seed(KeySeed(self.0))? {
            match key {
                Some(name) => {
                    let cell = map.next_value_seed(CellSeed(self.0))?;
                    fields.push((name, cell));
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        self.0.push_record(fields);
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if self.0.header_len() == 0 {
            return Err(A::Error::custom(
                "the rows are lists, so the data also needs its columns before the rows",
            ));
        }
        let mut cells = Vec::with_capacity(self.0.header_len());
        let mut pos = 0;
        loop {
            // Cells of columns that are not kept are read and dropped.
            let keep = self.0.slot(pos).is_some();
            let cell = if keep {
                seq.next_element_seed(CellSeed(self.0))?
            } else {
                seq.next_element::<IgnoredAny>()?.map(|_| Cell::Null)
            };
            match cell {
                Some(c) => cells.push(c),
                None => break,
            }
            pos += 1;
        }
        if pos != self.0.header_len() {
            self.0.note_ragged();
        }
        self.0.push_positional(&cells);
        Ok(())
    }
}

struct Rows<'a>(&'a mut Builder);

impl<'de> DeserializeSeed<'de> for Rows<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Rows<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of rows")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        loop {
            if self.0.is_full() {
                return Err(A::Error::custom(STOP));
            }
            if seq.next_element_seed(Row(self.0))?.is_none() {
                return Ok(());
            }
        }
    }
}

struct Columns<'a>(&'a mut Builder);

impl<'de> DeserializeSeed<'de> for Columns<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

struct Name(String);

impl<'de> serde::Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Name;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a column name or an object with a name")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Name, E> {
                Ok(Name(v.to_owned()))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Name, A::Error> {
                let mut name = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "name" {
                        name = Some(map.next_value::<String>()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                name.map(Name)
                    .ok_or_else(|| A::Error::custom("a column object needs a name"))
            }
        }
        d.deserialize_any(V)
    }
}

impl<'de> Visitor<'de> for Columns<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of column names")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut names = Vec::new();
        while let Some(Name(n)) = seq.next_element()? {
            names.push(n);
        }
        self.0.set_header(&names);
        Ok(())
    }
}

struct Root<'a> {
    builder: &'a mut Builder,
    nested: bool,
}

impl<'de> DeserializeSeed<'de> for Root<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Root<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rows of data")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        loop {
            if self.builder.is_full() {
                return Err(A::Error::custom(STOP));
            }
            if seq.next_element_seed(Row(self.builder))?.is_none() {
                return Ok(());
            }
        }
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut found = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "columns" => map.next_value_seed(Columns(self.builder))?,
                "rows" | "records" => {
                    found = true;
                    map.next_value_seed(Rows(self.builder))?;
                }
                k if WRAPPERS.contains(&k) && !found => {
                    map.next_value_seed(Root {
                        builder: self.builder,
                        nested: true,
                    })?;
                    found = true;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if !found && !self.nested {
            return Err(A::Error::custom(
                "the data object has no rows; send {\"columns\": [...], \"rows\": [[...]]} or a list of objects",
            ));
        }
        Ok(())
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        self.lenient("nothing")
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        self.lenient("text")
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        self.lenient("a boolean")
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        self.lenient("a number")
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        self.lenient("a number")
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        self.lenient("a number")
    }
}

impl Root<'_> {
    fn lenient<E: serde::de::Error>(&self, what: &str) -> Result<(), E> {
        if self.nested {
            Ok(())
        } else {
            Err(E::custom(format!(
                "the data is {what}, not rows; send a list of objects or columns and rows"
            )))
        }
    }
}

fn sentence(e: &serde_json::Error, builder: &Builder) -> Option<String> {
    if builder.is_stopped() {
        return None;
    }
    let text = e.to_string();
    let text = text.split(" at line ").next().unwrap_or(&text);
    if e.is_eof() {
        return Some("the JSON data ends early; it looks truncated".into());
    }
    if text.contains("recursion limit") {
        return Some("the JSON data is nested too deeply to be a table".into());
    }
    Some(format!(
        "the JSON data cannot be read as rows: {text} (line {}, column {})",
        e.line(),
        e.column()
    ))
}

/// Reads one JSON document from `text`.
pub fn read_str(text: &str, builder: &mut Builder) -> Res<()> {
    let mut de = serde_json::Deserializer::from_str(text);
    run(&mut de, builder)
}

/// Reads one JSON document from `r`.
pub fn read_reader<R: Read>(r: R, builder: &mut Builder) -> Res<()> {
    let mut de = serde_json::Deserializer::from_reader(r);
    run(&mut de, builder)
}

fn run<'de, R: serde_json::de::Read<'de>>(
    de: &mut serde_json::Deserializer<R>,
    builder: &mut Builder,
) -> Res<()> {
    let first = Root {
        builder,
        nested: false,
    }
    .deserialize(&mut *de);
    match first {
        Ok(()) => {}
        Err(e) => {
            return match sentence(&e, builder) {
                Some(s) => fail(s),
                None => Ok(()),
            };
        }
    }
    if let Err(e) = de.end() {
        return if e.is_eof() {
            Ok(())
        } else {
            fail(
                "the file holds more than one JSON value; for one row per line name it with a .jsonl extension",
            )
        };
    }
    Ok(())
}

/// Reads one JSON object per line.
pub fn read_lines<R: BufRead>(mut r: R, builder: &mut Builder) -> Res<()> {
    let mut line = Vec::new();
    let mut number = 0_usize;
    loop {
        line.clear();
        let n = r
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| format!("the file could not be read: {e}"))?;
        if n == 0 {
            return Ok(());
        }
        number += 1;
        if line.len() > MAX_LINE {
            return fail("a line is longer than 16 MiB, so this is not one JSON object per line");
        }
        let text = match std::str::from_utf8(&line) {
            Ok(t) => t.trim(),
            Err(_) => return fail(format!("line {number} is not valid UTF-8 text")),
        };
        if text.is_empty() {
            continue;
        }
        if builder.is_full() {
            return Ok(());
        }
        let mut de = serde_json::Deserializer::from_str(text);
        if let Err(e) = Row(builder).deserialize(&mut de).and_then(|()| de.end()) {
            return fail(format!(
                "line {number} is not a JSON object: {}",
                e.to_string().split(" at line ").next().unwrap_or("invalid")
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Stop, Table};

    fn load(json: &str) -> Table {
        let mut b = Builder::new(None);
        read_str(json, &mut b).unwrap_or_else(|e| panic!("{e}"));
        b.finish()
    }

    fn fail_msg(json: &str) -> String {
        let mut b = Builder::new(None);
        read_str(json, &mut b).unwrap_err().0
    }

    fn cell(t: &Table, col: &str, row: usize) -> String {
        match t.cols[t.col(col).unwrap()][row] {
            Cell::Null => "<null>".into(),
            Cell::Num(v) => format!("#{v}"),
            Cell::Str(i) => t.pool.get(i).to_owned(),
        }
    }

    #[test]
    fn an_array_of_objects_becomes_a_table() {
        let t = load(r#"[{"a":1,"b":"x"},{"a":2.5,"b":null},{"b":"z","c":true}]"#);
        assert_eq!(t.names, ["a", "b", "c"]);
        assert_eq!(cell(&t, "a", 1), "#2.5");
        assert_eq!(cell(&t, "b", 1), "<null>");
        assert_eq!(cell(&t, "a", 2), "<null>");
        assert_eq!(cell(&t, "c", 2), "true");
    }

    #[test]
    fn columns_and_rows_with_list_rows() {
        let t =
            load(r#"{"columns":["month","sales"],"rows":[["Jan",10],["Feb",12.5],["Mar",null]]}"#);
        assert_eq!(t.names, ["month", "sales"]);
        assert_eq!(t.rows, 3);
        assert_eq!(cell(&t, "month", 2), "Mar");
        assert_eq!(cell(&t, "sales", 1), "#12.5");
        assert_eq!(cell(&t, "sales", 2), "<null>");
    }

    #[test]
    fn column_objects_with_types_are_accepted() {
        let t = load(r#"{"columns":[{"name":"a","type":"int"},{"name":"b"}],"rows":[[1,2]]}"#);
        assert_eq!(t.names, ["a", "b"]);
        assert_eq!(cell(&t, "b", 0), "#2");
    }

    #[test]
    fn extra_keys_next_to_columns_and_rows_are_ignored() {
        let t =
            load(r#"{"columns":["a"],"rows":[[1],[2]],"next":{"cursor":"x"},"warnings":["w"]}"#);
        assert_eq!(t.rows, 2);
    }

    #[test]
    fn a_wrapped_table_is_found_inside_response_or_result() {
        for key in ["response", "result", "table", "data", "output"] {
            let t = load(&format!(r#"{{"{key}":{{"columns":["a"],"rows":[[7]]}}}}"#));
            assert_eq!(cell(&t, "a", 0), "#7", "{key}");
        }
        let t = load(r#"{"response":[{"a":1}],"parts":[{"type":"image"}]}"#);
        assert_eq!(t.rows, 1);
    }

    #[test]
    fn row_objects_inside_columns_and_rows_use_their_keys() {
        let t = load(r#"{"columns":["a","b"],"rows":[{"a":1,"b":2},{"b":3}]}"#);
        assert_eq!(cell(&t, "b", 1), "#3");
        assert_eq!(cell(&t, "a", 1), "<null>");
    }

    #[test]
    fn nested_values_become_empty_and_are_counted() {
        let t = load(r#"[{"a":[1,2,3],"b":{"x":1},"c":5}]"#);
        assert_eq!(t.nested, 2);
        assert_eq!(cell(&t, "a", 0), "<null>");
        assert_eq!(cell(&t, "c", 0), "#5");
    }

    #[test]
    fn short_and_long_list_rows_are_counted_as_ragged() {
        let t = load(r#"{"columns":["a","b"],"rows":[[1],[1,2,3],[1,2]]}"#);
        assert_eq!(t.ragged, 2);
        assert_eq!(t.rows, 3);
        assert_eq!(cell(&t, "b", 0), "<null>");
        assert_eq!(cell(&t, "b", 1), "#2");
    }

    #[test]
    fn projection_skips_columns_that_are_not_wanted() {
        let mut b = Builder::new(Some(vec!["b".into()]));
        read_str(r#"[{"a":1,"b":2,"c":[1]},{"a":3,"b":4}]"#, &mut b).unwrap();
        let t = b.finish();
        assert_eq!(t.names, ["b"]);
        assert_eq!(
            t.nested, 0,
            "an unwanted column is skipped without being read as a value"
        );
        let mut b = Builder::new(Some(vec!["b".into()]));
        read_str(r#"{"columns":["a","b"],"rows":[[1,2],[3,4]]}"#, &mut b).unwrap();
        assert_eq!(b.finish().cols[0], [Cell::Num(2.0), Cell::Num(4.0)]);
    }

    #[test]
    fn numeric_strings_become_numbers_and_ids_stay_text() {
        let t = load(r#"[{"a":"12.5","b":"007","c":"2024-01-01"}]"#);
        assert_eq!(cell(&t, "a", 0), "#12.5");
        assert_eq!(cell(&t, "b", 0), "007");
        assert_eq!(cell(&t, "c", 0), "2024-01-01");
    }

    #[test]
    fn the_row_limit_stops_a_stream_without_an_error() {
        let rows: Vec<String> = (0..100).map(|i| format!("[{i}]")).collect();
        let json = format!(r#"{{"columns":["a"],"rows":[{}]}}"#, rows.join(","));
        let mut b = Builder::with_limits(None, 5, usize::MAX);
        read_str(&json, &mut b).unwrap();
        let t = b.finish();
        assert_eq!((t.rows, t.stopped), (5, Some(Stop::Rows)));
        let mut b = Builder::with_limits(None, 5, usize::MAX);
        read_str(&format!("[{}]", vec!["{\"a\":1}"; 50].join(",")), &mut b).unwrap();
        assert_eq!(b.finish().rows, 5);
    }

    #[test]
    fn list_rows_without_columns_are_refused_with_a_hint() {
        assert!(fail_msg(r#"{"rows":[[1,2]]}"#).contains("columns"));
        assert!(fail_msg(r#"[[1,2],[3,4]]"#).contains("columns"));
    }

    #[test]
    fn data_that_is_not_rows_is_refused_in_one_sentence() {
        for (json, needle) in [
            ("42", "a number"),
            (r#""hello""#, "text"),
            ("true", "a boolean"),
            ("null", "nothing"),
            (r#"{"a":1}"#, "no rows"),
            (r#"{"columns":"a","rows":[]}"#, "cannot be read"),
        ] {
            let m = fail_msg(json);
            assert!(m.contains(needle), "{json}: {m}");
            assert!(!m.contains('\n'));
        }
    }

    #[test]
    fn rows_that_are_neither_objects_nor_lists_are_refused() {
        assert!(fail_msg("[1,2,3]").contains("cannot be read"));
    }

    #[test]
    fn malformed_and_truncated_json_say_so() {
        assert!(fail_msg(r#"[{"a":1},"#).contains("truncated"));
        assert!(fail_msg(r#"[{"a":1}"#).contains("truncated"));
        assert!(fail_msg(r#"[{"a":}]"#).contains("cannot be read"));
        assert!(fail_msg("").contains("truncated"));
        assert!(fail_msg("[{\"a\":1}] trailing").contains("more than one JSON value"));
    }

    #[test]
    fn deeply_nested_structure_is_refused_not_overflowed() {
        let deep = format!("{}1{}", r#"{"data":"#.repeat(5000), "}".repeat(5000));
        assert!(fail_msg(&deep).contains("nested too deeply"));
    }

    #[test]
    fn a_deeply_nested_cell_is_skipped_without_recursion_and_counted() {
        let deep_cell = format!(r#"[{{"a":{}1{}}}]"#, "[".repeat(5000), "]".repeat(5000));
        let t = load(&deep_cell);
        assert_eq!(t.nested, 1);
        assert_eq!(t.rows, 1);
    }

    #[test]
    fn a_cell_over_the_limit_is_refused() {
        let json = format!(r#"[{{"a":"{}"}}]"#, "x".repeat(MAX_CELL_BYTES + 1));
        assert!(fail_msg(&json).contains("longer than 65536 bytes"));
    }

    #[test]
    fn a_reader_source_gives_the_same_table_as_a_string() {
        let json = r#"{"columns":["a","b"],"rows":[[1,"x"],[2,"y"]]}"#;
        let mut b = Builder::new(None);
        read_reader(json.as_bytes(), &mut b).unwrap();
        let t = b.finish();
        assert_eq!(t.rows, 2);
        assert_eq!(cell(&t, "b", 1), "y");
    }

    #[test]
    fn json_lines_read_one_object_per_line() {
        let mut b = Builder::new(None);
        read_lines(
            "{\"a\":1}\n\n{\"a\":2,\"b\":\"x\"}\r\n{\"a\":3}".as_bytes(),
            &mut b,
        )
        .unwrap();
        let t = b.finish();
        assert_eq!(t.rows, 3);
        assert_eq!(cell(&t, "b", 1), "x");
    }

    #[test]
    fn json_lines_name_the_bad_line() {
        let mut b = Builder::new(None);
        let e = read_lines("{\"a\":1}\n{\"a\":\n".as_bytes(), &mut b).unwrap_err();
        assert!(e.0.starts_with("line 2 "), "{e}");
        let mut b = Builder::new(None);
        assert!(
            read_lines(&b"\xff\n"[..], &mut b)
                .unwrap_err()
                .0
                .contains("line 1")
        );
        let mut b = Builder::new(None);
        assert!(read_lines(&b"{\"a\":1} {\"a\":2}\n"[..], &mut b).is_err());
    }

    #[test]
    fn json_lines_stop_at_the_row_limit() {
        let mut b = Builder::with_limits(None, 2, usize::MAX);
        read_lines("{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n".as_bytes(), &mut b).unwrap();
        assert_eq!(b.finish().rows, 2);
    }
}
