//! JSON arrays of records and newline-delimited (or any concatenated) JSON.
//!
//! A file is read one element at a time, so memory stays flat whatever its
//! size. Columns are the keys of the records in order of first appearance;
//! a column is `bool`, `int64`, `float64` or `text`, nested objects are
//! structs and nested arrays are lists. Values that do not agree (a number
//! and a string under one key) make the column text, a nested value in a text
//! column is its compact JSON. A top-level element that is not an object goes
//! into a single column named `value`. Strings are never read as dates.
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanBuilder, Float64Builder, Int64Builder, ListArray, StringBuilder, StructArray,
};
use arrow::buffer::{NullBuffer, OffsetBuffer};
use arrow::datatypes::{DataType, Field, Fields, Schema, SchemaRef};
use arrow::record_batch::{RecordBatch, RecordBatchOptions};
use serde_json::Value;

use crate::Res;
use crate::csv::{MAX_COLUMNS, open_text};
use crate::table::{
    BATCH_ROWS, Batches, Infer, SAMPLE_ROWS, ScanError, TableSource, Warnings, file_fingerprint,
};

/// The most bytes one element may take.
const MAX_ELEMENT_BYTES: usize = 64 * 1024 * 1024;
const BIG: u64 = 1 << 53;

/// The inferred type of a JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum JType {
    /// Only nulls so far.
    Null,
    /// `true` or `false`.
    Bool,
    /// An integer; `big` when it is past +-2^53.
    Int { big: bool },
    /// A number with a fraction or exponent.
    Float,
    /// A string, or anything that has no common type with its neighbours.
    Text,
    /// An object, its keys in order of first appearance.
    Struct(Vec<(String, JType)>),
    /// An array of one element type.
    List(Box<JType>),
}

/// The type of `v` on its own.
pub fn type_of(v: &Value) -> JType {
    match v {
        Value::Null => JType::Null,
        Value::Bool(_) => JType::Bool,
        Value::Number(n) => match n.as_i64() {
            Some(i) => JType::Int {
                big: i.unsigned_abs() > BIG,
            },
            None if n.is_u64() => JType::Text,
            None => JType::Float,
        },
        Value::String(_) => JType::Text,
        Value::Array(items) => {
            JType::List(Box::new(items.iter().map(type_of).fold(JType::Null, merge)))
        }
        Value::Object(map) => {
            JType::Struct(map.iter().map(|(k, v)| (k.clone(), type_of(v))).collect())
        }
    }
}

/// The type two values of types `a` and `b` share.
pub fn merge(a: JType, b: JType) -> JType {
    use JType::*;
    match (a, b) {
        (Null, x) | (x, Null) => x,
        (Bool, Bool) => Bool,
        (Int { big: x }, Int { big: y }) => Int { big: x || y },
        (Float, Float) => Float,
        (Int { big: false }, Float) | (Float, Int { big: false }) => Float,
        (Struct(mut a), Struct(b)) => {
            for (k, t) in b {
                match a.iter_mut().find(|(name, _)| *name == k) {
                    Some((_, slot)) => *slot = merge(std::mem::replace(slot, Null), t),
                    None => a.push((k, t)),
                }
            }
            Struct(a)
        }
        (List(a), List(b)) => List(Box::new(merge(*a, *b))),
        _ => Text,
    }
}

/// The Arrow type a JSON type is stored as.
pub fn arrow_type(t: &JType) -> DataType {
    match t {
        JType::Null | JType::Text => DataType::Utf8,
        JType::Bool => DataType::Boolean,
        JType::Int { .. } => DataType::Int64,
        JType::Float => DataType::Float64,
        JType::Struct(fields) => DataType::Struct(Fields::from(
            fields
                .iter()
                .map(|(k, t)| Field::new(k, arrow_type(t), true))
                .collect::<Vec<_>>(),
        )),
        JType::List(inner) => DataType::List(Arc::new(Field::new("item", arrow_type(inner), true))),
    }
}

/// A value does not fit the type its column was inferred as.
pub struct Mismatch(pub String);

fn miss(what: &str) -> Mismatch {
    Mismatch(what.to_string())
}

/// Builds the column of `t` out of `values` (`None` is an absent value).
pub fn build(values: &[Option<&Value>], t: &JType) -> Result<ArrayRef, Mismatch> {
    let is_null = |v: &Option<&Value>| matches!(v, None | Some(Value::Null));
    Ok(match t {
        JType::Bool => {
            let mut b = BooleanBuilder::with_capacity(values.len());
            for v in values {
                match v {
                    v if is_null(v) => b.append_null(),
                    Some(Value::Bool(x)) => b.append_value(*x),
                    _ => return Err(miss("a value is not a boolean")),
                }
            }
            Arc::new(b.finish())
        }
        JType::Int { .. } => {
            let mut b = Int64Builder::with_capacity(values.len());
            for v in values {
                match v {
                    v if is_null(v) => b.append_null(),
                    Some(Value::Number(n)) if n.as_i64().is_some() => {
                        b.append_value(n.as_i64().unwrap_or(0))
                    }
                    _ => return Err(miss("a value is not an integer")),
                }
            }
            Arc::new(b.finish())
        }
        JType::Float => {
            let mut b = Float64Builder::with_capacity(values.len());
            for v in values {
                match v {
                    v if is_null(v) => b.append_null(),
                    Some(Value::Number(n)) if n.as_f64().is_some() => {
                        b.append_value(n.as_f64().unwrap_or(0.0))
                    }
                    _ => return Err(miss("a value is not a number")),
                }
            }
            Arc::new(b.finish())
        }
        JType::Text | JType::Null => {
            let mut b = StringBuilder::with_capacity(values.len(), values.len() * 8);
            for v in values {
                match v {
                    v if is_null(v) => b.append_null(),
                    _ if *t == JType::Null => {
                        return Err(miss("a value appeared in a column that was empty so far"));
                    }
                    Some(Value::String(s)) => b.append_value(s),
                    Some(other) => b.append_value(other.to_string()),
                    None => b.append_null(),
                }
            }
            Arc::new(b.finish())
        }
        JType::Struct(fields) => {
            let mut children: Vec<Vec<Option<&Value>>> =
                vec![Vec::with_capacity(values.len()); fields.len()];
            for v in values {
                match v {
                    v if is_null(v) => children.iter_mut().for_each(|c| c.push(None)),
                    Some(Value::Object(map)) => {
                        if map.keys().any(|k| !fields.iter().any(|(f, _)| f == k)) {
                            return Err(miss("a record has a field that was not seen before"));
                        }
                        for ((name, _), c) in fields.iter().zip(children.iter_mut()) {
                            c.push(map.get(name));
                        }
                    }
                    _ => return Err(miss("a value is not an object")),
                }
            }
            let arrays = children
                .iter()
                .zip(fields)
                .map(|(c, (_, ft))| build(c, ft))
                .collect::<Result<Vec<_>, _>>()?;
            let arrow_fields = match arrow_type(t) {
                DataType::Struct(f) => f,
                _ => Fields::empty(),
            };
            let nulls = NullBuffer::from(values.iter().map(|v| !is_null(v)).collect::<Vec<_>>());
            Arc::new(
                StructArray::try_new_with_length(arrow_fields, arrays, Some(nulls), values.len())
                    .map_err(|e| Mismatch(e.to_string()))?,
            )
        }
        JType::List(inner) => {
            let mut flat: Vec<Option<&Value>> = Vec::new();
            let mut lengths = Vec::with_capacity(values.len());
            for v in values {
                match v {
                    v if is_null(v) => lengths.push(0),
                    Some(Value::Array(items)) => {
                        lengths.push(items.len());
                        flat.extend(items.iter().map(Some));
                    }
                    _ => return Err(miss("a value is not an array")),
                }
            }
            let child = build(&flat, inner)?;
            let nulls = NullBuffer::from(values.iter().map(|v| !is_null(v)).collect::<Vec<_>>());
            let field = Arc::new(Field::new("item", arrow_type(inner), true));
            Arc::new(
                ListArray::try_new(
                    field,
                    OffsetBuffer::from_lengths(lengths),
                    child,
                    Some(nulls),
                )
                .map_err(|e| Mismatch(e.to_string()))?,
            )
        }
    })
}

/// Turns a JSON array's brackets and commas into spaces so its elements read as
/// a stream of concatenated values, and refuses an element over [`MAX_ELEMENT_BYTES`].
struct Flatten<R> {
    inner: R,
    array: Option<bool>,
    depth: u32,
    in_str: bool,
    esc: bool,
    since: usize,
}

impl<R: Read> Read for Flatten<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        for b in &mut buf[..n] {
            self.since += 1;
            if self.since > MAX_ELEMENT_BYTES {
                return Err(std::io::Error::other("an element is over 64 MiB"));
            }
            if self.in_str {
                if self.esc {
                    self.esc = false;
                } else if *b == b'\\' {
                    self.esc = true;
                } else if *b == b'"' {
                    self.in_str = false;
                }
                continue;
            }
            if self.array.is_none() {
                if b.is_ascii_whitespace() {
                    continue;
                }
                self.array = Some(*b == b'[');
                if *b == b'[' {
                    *b = b' ';
                    self.since = 0;
                    continue;
                }
            }
            let array = self.array == Some(true);
            match *b {
                b'"' => self.in_str = true,
                b'{' | b'[' => self.depth += 1,
                b'}' => self.depth = self.depth.saturating_sub(1),
                b']' if self.depth == 0 && array => *b = b' ',
                b']' => self.depth = self.depth.saturating_sub(1),
                b',' if self.depth == 0 && array => {
                    *b = b' ';
                    self.since = 0;
                }
                b'\n' if self.depth == 0 && !array => self.since = 0,
                _ => {}
            }
        }
        Ok(n)
    }
}

type Elements = serde_json::StreamDeserializer<
    'static,
    serde_json::de::IoRead<BufReader<Flatten<Box<dyn Read + Send>>>>,
    Value,
>;

fn elements(path: &Path) -> Res<Elements> {
    let mut text = open_text(path)?;
    // A UTF-8 byte order mark is not JSON.
    let mut head = [0u8; 3];
    let mut got = 0;
    while got < 3 {
        match text.read(&mut head[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        }
    }
    let kept = if head[..got] == [0xEF, 0xBB, 0xBF] {
        &head[..0]
    } else {
        &head[..got]
    };
    let source: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(kept.to_vec()).chain(text));
    let flat = Flatten {
        inner: source,
        array: None,
        depth: 0,
        in_str: false,
        esc: false,
        since: 0,
    };
    Ok(
        serde_json::Deserializer::from_reader(BufReader::with_capacity(256 * 1024, flat))
            .into_iter::<Value>(),
    )
}

fn element_error(path: &Path, e: &serde_json::Error) -> String {
    let what = e.to_string();
    if what.contains("recursion limit") {
        return format!(
            "{} nests JSON deeper than 128 levels and cannot be read",
            path.display()
        );
    }
    if what.contains("over 64 MiB") {
        return format!(
            "{} has an element over 64 MiB; split it into smaller records",
            path.display()
        );
    }
    format!(
        "{} is not valid JSON at line {}, column {}; check the file is a JSON array or one record per line",
        path.display(),
        e.line(),
        e.column()
    )
}

/// A JSON file read as a table.
pub struct JsonTable {
    path: PathBuf,
    table: String,
    columns: Vec<(String, JType)>,
    single: bool,
    schema: SchemaRef,
    rows: Option<u64>,
    fp: u64,
}

impl JsonTable {
    /// Reads the head of `path` and infers its columns.
    pub fn open(path: &Path, table: &str, infer: Infer, warn: &Arc<Warnings>) -> Res<Self> {
        let mut columns: Vec<(String, JType)> = Vec::new();
        let mut single = false;
        let mut value_type = JType::Null;
        let (mut count, mut at_end) = (0u64, true);
        let mut skipped = 0u64;
        for item in elements(path)? {
            let v = item.map_err(|e| element_error(path, &e))?;
            if infer == Infer::Sample && count >= SAMPLE_ROWS {
                at_end = false;
                break;
            }
            count += 1;
            match &v {
                Value::Object(map) if !single => {
                    for (k, val) in map {
                        let t = type_of(val);
                        match columns.iter_mut().find(|(n, _)| n == k) {
                            Some((_, slot)) => {
                                *slot = merge(std::mem::replace(slot, JType::Null), t)
                            }
                            None => columns.push((k.clone(), t)),
                        }
                    }
                }
                _ => {
                    if !single {
                        // A top-level value that is not an object: everything goes under `value`.
                        single = true;
                        value_type = JType::Null;
                        for (_, t) in std::mem::take(&mut columns) {
                            value_type = merge(value_type, t);
                        }
                        if count > 1 {
                            value_type = merge(value_type, JType::Text);
                            skipped = count - 1;
                        }
                    }
                    value_type = merge(value_type, type_of(&v));
                }
            }
        }
        if skipped > 0 {
            warn.once("json-mixed", "the file mixes records and other values, so every row is read as one text column named value".into());
        }
        if single {
            columns = vec![("value".to_string(), value_type)];
        }
        if columns.is_empty() {
            return Err(format!("{} holds no JSON records to read", path.display()));
        }
        if columns.len() > MAX_COLUMNS {
            return Err(format!(
                "{} has {} fields, over the {MAX_COLUMNS} one table may have",
                path.display(),
                columns.len()
            ));
        }
        let fields: Vec<Field> = columns
            .iter()
            .map(|(n, t)| Field::new(n, arrow_type(t), true))
            .collect();
        Ok(Self {
            path: path.to_path_buf(),
            table: table.to_string(),
            columns,
            single,
            schema: Arc::new(Schema::new(fields)),
            rows: at_end.then_some(count),
            fp: file_fingerprint(path)?,
        })
    }
}

impl TableSource for JsonTable {
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
        let picked: Vec<usize> =
            projection.map_or_else(|| (0..self.columns.len()).collect(), <[usize]>::to_vec);
        let fields: Vec<Field> = picked
            .iter()
            .map(|&c| self.schema.field(c).clone())
            .collect();
        let source = match elements(&self.path) {
            Ok(e) => e,
            Err(e) => return Box::new(std::iter::once(Err(ScanError::Failed(e)))),
        };
        Box::new(Scan {
            source,
            path: self.path.clone(),
            table: self.table.clone(),
            columns: picked.iter().map(|&c| self.columns[c].clone()).collect(),
            all: self.columns.iter().map(|(n, _)| n.clone()).collect(),
            single: self.single,
            schema: Arc::new(Schema::new(fields)),
            row: 0,
            done: false,
        })
    }
}

struct Scan {
    source: Elements,
    path: PathBuf,
    table: String,
    columns: Vec<(String, JType)>,
    all: Vec<String>,
    single: bool,
    schema: SchemaRef,
    row: u64,
    done: bool,
}

impl Scan {
    fn conflict(&self, column: &str, why: &str) -> ScanError {
        let _ = why;
        ScanError::Conflict {
            table: self.table.clone(),
            column: column.to_string(),
            line: self.row,
        }
    }

    fn batch(&mut self) -> Result<Option<RecordBatch>, ScanError> {
        let mut rows: Vec<Value> = Vec::with_capacity(BATCH_ROWS);
        while rows.len() < BATCH_ROWS {
            match self.source.next() {
                None => {
                    self.done = true;
                    break;
                }
                Some(Err(e)) => return Err(ScanError::Failed(element_error(&self.path, &e))),
                Some(Ok(v)) => {
                    self.row += 1;
                    rows.push(v);
                }
            }
        }
        if rows.is_empty() {
            return Ok(None);
        }
        let mut arrays = Vec::with_capacity(self.columns.len());
        for (name, t) in &self.columns {
            let cells: Vec<Option<&Value>> = if self.single {
                rows.iter().map(Some).collect()
            } else {
                let mut cells = Vec::with_capacity(rows.len());
                for (i, r) in rows.iter().enumerate() {
                    match r {
                        Value::Object(map) => {
                            if let Some(k) = map.keys().find(|k| !self.all.contains(k)) {
                                self.row = self.row - (rows.len() - i) as u64 + 1;
                                return Err(self.conflict(k, "a field that was not seen before"));
                            }
                            cells.push(map.get(name));
                        }
                        _ => {
                            self.row = self.row - (rows.len() - i) as u64 + 1;
                            return Err(self.conflict(name, "a value that is not a record"));
                        }
                    }
                }
                cells
            };
            match build(&cells, t) {
                Ok(a) => arrays.push(a),
                Err(Mismatch(why)) => return Err(self.conflict(name, &why)),
            }
        }
        let options = RecordBatchOptions::new().with_row_count(Some(rows.len()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Dir, collect, cols, columns};
    use proptest::prelude::*;
    use serde_json::json;

    fn open_with(
        text: impl AsRef<[u8]>,
        infer: Infer,
    ) -> Result<(JsonTable, Arc<Warnings>, Dir), String> {
        let dir = Dir::new();
        let p = dir.put("t.json", text);
        let warn = Warnings::new();
        JsonTable::open(&p, "t", infer, &warn).map(|t| (t, warn, dir))
    }

    fn open(text: &str) -> (JsonTable, Arc<Warnings>, Dir) {
        open_with(text, Infer::Sample).unwrap()
    }

    #[test]
    fn records_with_nested_values_keep_their_shape_and_first_appearance_order() {
        let (t, _, _d) = open(
            "[{\"id\":1,\"name\":\"Ana\",\"tags\":[\"x\",\"y\"],\"geo\":{\"lat\":1.5,\"lon\":2}},{\"id\":2,\"name\":null,\"tags\":[],\"geo\":null},{\"id\":3,\"name\":\"Cy\",\"extra\":true}]",
        );
        assert_eq!(
            columns(&t),
            cols(&[
                ("id", "int64"),
                ("name", "text"),
                ("tags", "list<text>"),
                ("geo", "struct"),
                ("extra", "bool")
            ])
        );
        assert_eq!(t.row_count(), Some(3));
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![
                vec![
                    json!(1),
                    json!("Ana"),
                    json!(["x", "y"]),
                    json!({"lat": 1.5, "lon": 2}),
                    Value::Null
                ],
                vec![json!(2), Value::Null, json!([]), Value::Null, Value::Null],
                vec![json!(3), json!("Cy"), Value::Null, Value::Null, json!(true)],
            ]
        );
    }

    #[test]
    fn newline_delimited_and_pretty_printed_concatenated_records() {
        let (t, _, _d) =
            open("{\"t\":1,\"kind\":\"a\"}\n{\"t\":2,\"kind\":\"b\"}\n\n{\"t\":3,\"kind\":\"a\"}");
        assert_eq!(columns(&t), cols(&[("t", "int64"), ("kind", "text")]));
        assert_eq!(t.row_count(), Some(3));
        let (t, _, _d) =
            open("{\n  \"t\": 1,\n  \"kind\": \"a\"\n}\n{\n  \"t\": 2,\n  \"kind\": \"b\"\n}\n");
        assert_eq!(
            collect(&t, Some(&[1])).unwrap(),
            vec![vec![json!("a")], vec![json!("b")]]
        );
    }

    #[test]
    fn one_object_is_one_row() {
        let (t, _, _d) = open("{\"a\":1,\"b\":[1,2]}");
        assert_eq!(t.row_count(), Some(1));
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![vec![json!(1), json!([1, 2])]]
        );
    }

    #[test]
    fn values_that_disagree_become_text() {
        let (t, _, _d) =
            open("[{\"v\":1},{\"v\":\"two\"},{\"v\":[3]},{\"v\":{\"k\":true}},{\"v\":2.5}]");
        assert_eq!(columns(&t)[0].1, "text");
        let rows: Vec<Value> = collect(&t, None)
            .unwrap()
            .into_iter()
            .map(|mut r| r.remove(0))
            .collect();
        assert_eq!(
            rows,
            vec![
                json!("1"),
                json!("two"),
                json!("[3]"),
                json!("{\"k\":true}"),
                json!("2.5")
            ]
        );
    }

    #[test]
    fn disagreement_inside_a_struct_only_makes_that_field_text() {
        let (t, _, _d) = open("[{\"o\":{\"a\":1,\"b\":1}},{\"o\":{\"a\":2,\"b\":\"x\"}}]");
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![
                vec![json!({"a": 1, "b": "1"})],
                vec![json!({"a": 2, "b": "x"})]
            ]
        );
    }

    #[test]
    fn integers_and_floats_share_a_float_column_unless_an_integer_is_too_big() {
        let (t, _, _d) = open("[{\"n\":1},{\"n\":2.5}]");
        assert_eq!(columns(&t)[0].1, "float64");
        let (t, _, _d) = open("[{\"n\":9007199254740993},{\"n\":2.5}]");
        assert_eq!(columns(&t)[0].1, "text");
        let (t, _, _d) = open("[{\"n\":18446744073709551615}]");
        assert_eq!(columns(&t)[0].1, "text");
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![vec![json!("18446744073709551615")]]
        );
        let (t, _, _d) = open("[{\"n\":9007199254740993}]");
        assert_eq!(columns(&t)[0].1, "int64");
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![vec![json!("9007199254740993")]]
        );
    }

    #[test]
    fn top_level_values_that_are_not_records_go_in_one_column() {
        let (t, _, _d) = open("[1,2,3]");
        assert_eq!(columns(&t), cols(&[("value", "int64")]));
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![vec![json!(1)], vec![json!(2)], vec![json!(3)]]
        );
        let (t, warn, _d) = open("[{\"a\":1},2]");
        assert_eq!(columns(&t), cols(&[("value", "text")]));
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![vec![json!("{\"a\":1}")], vec![json!("2")]]
        );
        assert_eq!(warn.list().len(), 1);
        let (t, _, _d) = open("[[1,2],[3]]");
        assert_eq!(columns(&t), cols(&[("value", "list<int64>")]));
    }

    #[test]
    fn brackets_commas_quotes_and_escapes_inside_strings_do_not_split_elements() {
        let (t, _, _d) =
            open("[{\"a\":\"x],y\\\"z, [\"},{\"a\":\"q\\\\\"},{\"a\":\"\\u00e9\\n\"}]");
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![
                vec![json!("x],y\"z, [")],
                vec![json!("q\\")],
                vec![json!("é\n")]
            ]
        );
    }

    #[test]
    fn a_byte_order_mark_and_surrounding_whitespace_are_fine() {
        let (t, _, _d) =
            open_with("\u{feff}  \n [ {\"a\":1} , {\"a\":2} ]  \n", Infer::Sample).unwrap();
        assert_eq!(t.row_count(), Some(2));
    }

    #[test]
    fn nothing_to_read_and_broken_files_are_one_sentence() {
        for (text, want) in [
            ("[]", "holds no JSON records"),
            ("   ", "holds no JSON records"),
            ("[{\"a\":1},{\"a\":", "is not valid JSON at line"),
            ("not json", "is not valid JSON at line"),
        ] {
            let err = open_with(text, Infer::Sample).err().unwrap();
            assert!(err.contains(want), "{text:?}: {err}");
        }
    }

    #[test]
    fn json_nested_deeper_than_the_parser_allows_is_refused() {
        let text = format!("{}1{}", "[".repeat(300), "]".repeat(300));
        let err = open_with(&text, Infer::Sample).err().unwrap();
        assert!(err.contains("deeper than 128 levels"), "{err}");
        // Just inside the limit it reads, as one text-or-list value.
        let ok = format!("[{}1{}]", "[".repeat(100), "]".repeat(100));
        assert!(open_with(&ok, Infer::Sample).is_ok());
    }

    #[test]
    fn an_element_over_64_mib_is_refused() {
        let mut text = String::from("[\"");
        text.push_str(&"x".repeat(MAX_ELEMENT_BYTES + 10));
        text.push_str("\"]");
        let err = open_with(&text, Infer::Sample).err().unwrap();
        assert!(err.contains("an element over 64 MiB"), "{err}");
    }

    #[test]
    fn a_new_field_past_the_sample_is_a_conflict_that_full_inference_resolves() {
        let mut text = String::from("[");
        for i in 0..(SAMPLE_ROWS + 5) {
            text.push_str(&format!("{{\"a\":{i}}},"));
        }
        text.push_str("{\"a\":1,\"b\":2}]");
        let (t, _, _d) = open_with(&text, Infer::Sample).unwrap();
        assert_eq!(columns(&t), cols(&[("a", "int64")]));
        assert_eq!(t.row_count(), None);
        assert!(matches!(collect(&t, None), Err(ScanError::Conflict { .. })));
        let (full, _, _d) = open_with(&text, Infer::Full).unwrap();
        assert_eq!(columns(&full), cols(&[("a", "int64"), ("b", "int64")]));
        assert_eq!(
            collect(&full, None).unwrap().last().unwrap(),
            &vec![json!(1), json!(2)]
        );
    }

    #[test]
    fn a_type_change_past_the_sample_is_a_conflict_that_full_inference_resolves() {
        let mut text = String::from("[");
        for i in 0..(SAMPLE_ROWS + 5) {
            text.push_str(&format!("{{\"a\":{i}}},"));
        }
        text.push_str("{\"a\":\"late\"}]");
        let (t, _, _d) = open_with(&text, Infer::Sample).unwrap();
        let Err(ScanError::Conflict { column, line, .. }) = collect(&t, None) else {
            panic!("no conflict")
        };
        assert_eq!((column.as_str(), line), ("a", SAMPLE_ROWS + 6));
        let (full, _, _d) = open_with(&text, Infer::Full).unwrap();
        assert_eq!(columns(&full)[0].1, "text");
    }

    #[test]
    fn type_merging_rules() {
        use JType::*;
        assert_eq!(merge(Null, Bool), Bool);
        assert_eq!(
            merge(Int { big: false }, Int { big: true }),
            Int { big: true }
        );
        assert_eq!(merge(Int { big: false }, Float), Float);
        assert_eq!(merge(Int { big: true }, Float), Text);
        assert_eq!(merge(Bool, Int { big: false }), Text);
        assert_eq!(
            merge(List(Box::new(Int { big: false })), List(Box::new(Float))),
            List(Box::new(Float))
        );
        assert_eq!(
            merge(
                Struct(vec![("a".into(), Bool)]),
                Struct(vec![("b".into(), Float), ("a".into(), Bool)])
            ),
            Struct(vec![("a".into(), Bool), ("b".into(), Float)])
        );
        assert_eq!(type_of(&json!([])), List(Box::new(Null)));
        assert_eq!(
            type_of(&json!({"a": [1, 2.5]})),
            Struct(vec![("a".into(), List(Box::new(Float)))])
        );
    }

    fn arbitrary_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            (-1000i64..1000).prop_map(|n| json!(n)),
            "[a-z ]{0,6}".prop_map(Value::String),
        ]
    }

    proptest! {
        #[test]
        fn the_scan_has_one_row_per_record_and_never_panics(records in prop::collection::vec(
            prop::collection::btree_map("[abc]", arbitrary_value(), 0..4), 1..30)) {
            let text = Value::Array(records.iter().map(|m| Value::Object(m.clone().into_iter().collect())).collect()).to_string();
            let dir = Dir::new();
            let p = dir.put("p.json", &text);
            let all_empty = records.iter().all(|m| m.is_empty());
            match JsonTable::open(&p, "p", Infer::Sample, &Warnings::new()) {
                Ok(t) => {
                    prop_assert_eq!(t.row_count(), Some(records.len() as u64));
                    let rows = collect(&t, None).unwrap();
                    prop_assert_eq!(rows.len(), records.len());
                }
                Err(e) => prop_assert!(all_empty && e.contains("no JSON records"), "{}", e),
            }
        }

        #[test]
        fn clean_integer_records_come_back_unchanged(ns in prop::collection::vec(any::<i32>(), 1..40)) {
            let text = Value::Array(ns.iter().map(|n| json!({"n": n})).collect()).to_string();
            let dir = Dir::new();
            let p = dir.put("p.json", &text);
            let t = JsonTable::open(&p, "p", Infer::Sample, &Warnings::new()).unwrap();
            prop_assert_eq!(columns(&t)[0].1.clone(), "int64".to_string());
            let got: Vec<i64> = collect(&t, None).unwrap().iter().map(|r| r[0].as_i64().unwrap()).collect();
            prop_assert_eq!(got, ns.iter().map(|&n| i64::from(n)).collect::<Vec<_>>());
        }
    }
}
