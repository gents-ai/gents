//! Tables sent inside the request: rows as JSON, in the shape this plugin's
//! own results have (`columns` and `rows`), so the output of one call or one
//! node is the input of the next.
//!
//! `tables` is a list of `{"name", "columns", "rows"}`, one such object, or an
//! object mapping names to row lists. Rows are arrays (by position) or
//! objects (by key). A column may declare its `type` with the names this
//! plugin reports; without one the type is inferred from the values.
use std::collections::HashSet;
use std::sync::Arc;

use arrow::datatypes::{Field, Schema, SchemaRef};
use arrow::record_batch::{RecordBatch, RecordBatchOptions};
use serde_json::Value;

use crate::Res;
use crate::csv::{MAX_COLUMNS, column_names};
use crate::names::{sanitize, unique};
use crate::table::{BATCH_ROWS, Batches, Fnv, TableSource, Warnings};
use crate::typed::{Builder, ColType, FLOAT, INT, Inferrer, parse_float, parse_int};

/// The most rows an inline table may carry.
pub const MAX_ROWS: usize = 1_000_000;
const MAX_CELLS: usize = 20_000_000;

/// A table held in memory.
pub struct InlineTable {
    schema: SchemaRef,
    batch: RecordBatch,
    fp: u64,
}

/// The column type a declared type name stands for; `None` is a name that means text.
fn declared(name: &str) -> ColType {
    match name.trim().to_ascii_lowercase().as_str() {
        "bool" | "boolean" => ColType::Bool,
        "int" | "integer" | "bigint" | "smallint" | "tinyint" => ColType::Int,
        t if t.starts_with("int") || t.starts_with("uint") => ColType::Int,
        "float" | "double" | "real" | "number" | "float32" | "float64" => ColType::Float,
        "date" => ColType::Date,
        "timestamp" => ColType::Timestamp,
        "timestamp_tz" => ColType::TimestampUtc,
        _ => ColType::Text,
    }
}

fn flags(v: &Value) -> Option<u8> {
    match v {
        Value::Null => None,
        Value::Bool(_) => Some(crate::typed::BOOL),
        Value::Number(n) => Some(match n.as_i64() {
            Some(i) => {
                INT | if i.unsigned_abs() < (1 << 53) {
                    FLOAT
                } else {
                    0
                }
            }
            None if n.is_f64() => FLOAT,
            None => 0,
        }),
        Value::String(_) => Some(0),
        _ => Some(0),
    }
}

fn non_finite(s: &str) -> Option<f64> {
    match s {
        "NaN" => Some(f64::NAN),
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        _ => None,
    }
}

fn push(b: &mut Builder, v: &Value, t: ColType) -> bool {
    if v.is_null() {
        b.null();
        return true;
    }
    let ok = match (t, v) {
        (ColType::Bool, Value::Bool(x)) => b.bool(*x),
        (ColType::Int, Value::Number(n)) => match n.as_i64() {
            Some(i) => b.int(i),
            None => return false,
        },
        (ColType::Int, Value::String(s)) => match parse_int(s.as_bytes()) {
            Some(i) => b.int(i),
            None => return false,
        },
        (ColType::Float, Value::Number(n)) => b.float(n.as_f64().unwrap_or(f64::NAN)),
        (ColType::Float, Value::String(s)) => match non_finite(s)
            .or_else(|| parse_float(s.as_bytes()))
            .or_else(|| parse_int(s.as_bytes()).map(|i| i as f64))
        {
            Some(f) => b.float(f),
            None => return false,
        },
        (ColType::Date | ColType::Timestamp | ColType::TimestampUtc, Value::String(s)) => {
            b.token(s.as_bytes())
        }
        (ColType::Text, Value::String(s)) => b.text(s),
        (ColType::Text, other) => b.text(&other.to_string()),
        _ => return false,
    };
    ok.is_ok()
}

/// The names and rows of one inline table value.
/// Declared column names and types, and the rows.
type Declared = (Option<Vec<(String, Option<ColType>)>>, Vec<Value>);

fn read_one(name: &str, v: &Value) -> Res<Declared> {
    let obj = v
        .as_object()
        .ok_or_else(|| format!("inline table {name} must be an object with rows"))?;
    let rows = obj
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("inline table {name} needs a rows list"))?
        .clone();
    let columns = match obj.get("columns") {
        None | Some(Value::Null) => None,
        Some(Value::Array(cols)) => Some(
            cols.iter()
                .map(|c| match c {
                    Value::String(s) => Ok((s.clone(), None)),
                    Value::Object(o) => {
                        let n = o.get("name").and_then(Value::as_str).ok_or_else(|| {
                            format!("a column of inline table {name} has no name")
                        })?;
                        Ok((
                            n.to_string(),
                            o.get("type").and_then(Value::as_str).map(declared),
                        ))
                    }
                    _ => Err(format!(
                        "columns of inline table {name} must be names or objects with a name"
                    )),
                })
                .collect::<Res<Vec<_>>>()?,
        ),
        Some(_) => return Err(format!("columns of inline table {name} must be a list")),
    };
    Ok((columns, rows))
}

/// Builds the tables the request carries, named and made unique.
pub fn parse_tables(v: &Value, warn: &Arc<Warnings>) -> Res<Vec<(String, InlineTable)>> {
    let mut items: Vec<(String, &Value)> = Vec::new();
    match v {
        Value::Array(list) => {
            for (i, t) in list.iter().enumerate() {
                let n = t
                    .get("name")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("data{}", i + 1), str::to_string);
                items.push((n, t));
            }
        }
        Value::Object(o) if o.contains_key("rows") => {
            items.push((
                o.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("data")
                    .to_string(),
                v,
            ));
        }
        Value::Object(o) => {
            for (k, t) in o {
                items.push((k.clone(), t));
            }
        }
        _ => {
            return Err("tables must be a list of tables, one table or an object of tables".into());
        }
    }
    let mut taken = HashSet::new();
    let mut out = Vec::new();
    for (raw, t) in items {
        let name = unique(&sanitize(&raw), &mut taken);
        if name != raw {
            warn.once(
                &format!("inline-name-{raw}"),
                format!("inline table {raw} is named {name} in SQL"),
            );
        }
        // A bare list of rows is a table too.
        let wrapped;
        let t = if t.is_array() {
            wrapped = serde_json::json!({"rows": t});
            &wrapped
        } else {
            t
        };
        out.push((name.clone(), build(&name, t, warn)?));
    }
    Ok(out)
}

fn build(name: &str, v: &Value, warn: &Arc<Warnings>) -> Res<InlineTable> {
    let (declared_cols, rows) = read_one(name, v)?;
    if rows.len() > MAX_ROWS {
        return Err(format!(
            "inline table {name} has {} rows, over the {MAX_ROWS} one inline table may have; bind a file instead",
            rows.len()
        ));
    }
    if let Some(r) = rows.iter().position(|r| !r.is_array() && !r.is_object()) {
        return Err(format!(
            "row {} of inline table {name} must be a list or an object",
            r + 1
        ));
    }
    let named: Vec<(String, Option<ColType>)> = match declared_cols {
        Some(c) => c,
        None => match rows.first() {
            Some(Value::Object(_)) => {
                let mut keys: Vec<String> = Vec::new();
                for r in &rows {
                    if let Value::Object(o) = r {
                        for k in o.keys() {
                            if !keys.contains(k) {
                                keys.push(k.clone());
                            }
                        }
                    }
                }
                keys.into_iter().map(|k| (k, None)).collect()
            }
            _ => {
                let width = rows
                    .iter()
                    .filter_map(|r| r.as_array().map(Vec::len))
                    .max()
                    .unwrap_or(0);
                (0..width)
                    .map(|i| (format!("column_{}", i + 1), None))
                    .collect()
            }
        },
    };
    if named.is_empty() {
        return Err(format!("inline table {name} has no columns"));
    }
    if named.len() > MAX_COLUMNS.min(10_000) || named.len().saturating_mul(rows.len()) > MAX_CELLS {
        return Err(format!(
            "inline table {name} is too large; bind a file instead"
        ));
    }
    let header: Vec<(Vec<u8>, bool)> = named
        .iter()
        .map(|(n, _)| (n.as_bytes().to_vec(), true))
        .collect();
    let names = column_names(Some(&header), named.len(), warn);
    let mut cells: Vec<Vec<&Value>> = vec![Vec::with_capacity(rows.len()); named.len()];
    for (r, row) in rows.iter().enumerate() {
        for (c, (key, _)) in named.iter().enumerate() {
            let value = match row {
                Value::Array(a) => a.get(c),
                Value::Object(o) => o.get(key),
                _ => {
                    return Err(format!(
                        "row {} of inline table {name} must be a list or an object",
                        r + 1
                    ));
                }
            };
            cells[c].push(value.unwrap_or(&Value::Null));
        }
    }
    let mut columns = Vec::with_capacity(named.len());
    let mut fields = Vec::with_capacity(named.len());
    for (c, (_, decl)) in named.iter().enumerate() {
        let t = decl.unwrap_or_else(|| {
            let mut inf = Inferrer::new(1);
            cells[c]
                .iter()
                .filter_map(|v| flags(v))
                .for_each(|f| inf.observe(0, f));
            inf.types()[0]
        });
        let mut b = Builder::new(t, rows.len());
        for (r, v) in cells[c].iter().enumerate() {
            if !push(&mut b, v, t) {
                return Err(format!(
                    "row {} of inline table {name}, column {}, does not match its type; fix the value or the declared type",
                    r + 1,
                    names[c]
                ));
            }
        }
        fields.push(Field::new(&names[c], t.data_type(), true));
        columns.push(b.finish());
    }
    let schema = Arc::new(Schema::new(fields));
    let options = RecordBatchOptions::new().with_row_count(Some(rows.len()));
    let batch = RecordBatch::try_new_with_options(Arc::clone(&schema), columns, &options)
        .map_err(|e| format!("inline table {name} could not be built: {e}"))?;
    let mut h = Fnv::default();
    h.text(&v.to_string());
    Ok(InlineTable {
        schema,
        batch,
        fp: h.0,
    })
}

impl TableSource for InlineTable {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn row_count(&self) -> Option<u64> {
        Some(self.batch.num_rows() as u64)
    }

    fn fingerprint(&self) -> u64 {
        self.fp
    }

    fn scan(&self, projection: Option<&[usize]>) -> Batches {
        let batch = match projection {
            Some(p) => match self.batch.project(p) {
                Ok(b) => b,
                Err(e) => {
                    return Box::new(std::iter::once(Err(crate::table::ScanError::Failed(
                        e.to_string(),
                    ))));
                }
            },
            None => self.batch.clone(),
        };
        let total = batch.num_rows();
        Box::new((0..total.div_ceil(BATCH_ROWS)).map(move |i| {
            let start = i * BATCH_ROWS;
            Ok(batch.slice(start, BATCH_ROWS.min(total - start)))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{collect, cols, columns};
    use serde_json::json;

    fn one(v: Value) -> Result<(InlineTable, Arc<Warnings>), String> {
        let warn = Warnings::new();
        let mut t = parse_tables(&v, &warn)?;
        Ok((t.remove(0).1, warn))
    }

    #[test]
    fn positional_rows_get_numbered_columns_and_inferred_types() {
        let (t, _) =
            one(json!({"rows": [[1, "a", true, 1.5, null], [2, "b", false, 2, null]]})).unwrap();
        assert_eq!(
            columns(&t),
            cols(&[
                ("column_1", "int64"),
                ("column_2", "text"),
                ("column_3", "bool"),
                ("column_4", "float64"),
                ("column_5", "text")
            ])
        );
        assert_eq!(
            collect(&t, None).unwrap()[1],
            vec![json!(2), json!("b"), json!(false), json!(2.0), Value::Null]
        );
        assert_eq!(t.row_count(), Some(2));
    }

    #[test]
    fn object_rows_name_their_columns_by_first_appearance_and_missing_keys_are_null() {
        let (t, _) = one(json!({"rows": [{"b": 1, "a": "x"}, {"a": "y", "c": true}]})).unwrap();
        assert_eq!(
            columns(&t),
            cols(&[("b", "int64"), ("a", "text"), ("c", "bool")])
        );
        assert_eq!(
            collect(&t, None).unwrap(),
            vec![
                vec![json!(1), json!("x"), Value::Null],
                vec![Value::Null, json!("y"), json!(true)]
            ]
        );
    }

    #[test]
    fn exact_integers_and_special_floats_come_back_as_they_went_in() {
        let (t, _) = one(json!({
            "columns": [{"name": "n", "type": "int64"}, {"name": "x", "type": "float64"}, {"name": "d", "type": "date"}, {"name": "ts", "type": "timestamp"}],
            "rows": [["9007199254740993", "NaN", "2024-02-29", "2024-02-29T12:30:45.5"], [-5, "-Infinity", null, null], [3, 2, "2024-03-01", "2024-03-01"]]
        }))
        .unwrap();
        let rows = collect(&t, None).unwrap();
        assert_eq!(
            rows[0],
            vec![
                json!("9007199254740993"),
                json!("NaN"),
                json!("2024-02-29"),
                json!("2024-02-29T12:30:45.500")
            ]
        );
        assert_eq!(
            rows[1],
            vec![json!(-5), json!("-Infinity"), Value::Null, Value::Null]
        );
        assert_eq!(
            rows[2],
            vec![
                json!(3),
                json!(2.0),
                json!("2024-03-01"),
                json!("2024-03-01T00:00:00")
            ]
        );
    }

    #[test]
    fn declared_names_without_types_infer_them_and_unknown_types_are_text() {
        let (t, _) = one(json!({"columns": ["a", {"name": "b", "type": "geometry"}], "rows": [[1, {"k": 1}], [2, [1, 2]]]})).unwrap();
        assert_eq!(columns(&t), cols(&[("a", "int64"), ("b", "text")]));
        assert_eq!(
            collect(&t, Some(&[1])).unwrap(),
            vec![vec![json!("{\"k\":1}")], vec![json!("[1,2]")]]
        );
    }

    #[test]
    fn mixed_values_make_text_and_big_integers_stay_exact() {
        let (t, _) = one(json!({"rows": [[1, 9007199254740993_i64], ["x", 2.5]]})).unwrap();
        assert_eq!(columns(&t)[0].1, "text");
        assert_eq!(columns(&t)[1].1, "text");
        let (t, _) = one(json!({"rows": [[9007199254740993_i64], [1]]})).unwrap();
        assert_eq!(columns(&t)[0].1, "int64");
        assert_eq!(
            collect(&t, None).unwrap()[0],
            vec![json!("9007199254740993")]
        );
    }

    #[test]
    fn the_accepted_table_forms() {
        let warn = Warnings::new();
        for (v, want) in [
            (
                json!([{"name": "a", "rows": [[1]]}, {"rows": [[2]]}]),
                vec!["a", "data2"],
            ),
            (json!({"name": "solo", "rows": [[1]]}), vec!["solo"]),
            (json!({"rows": [[1]]}), vec!["data"]),
            (json!({"x": {"rows": [[1]]}, "y": [[2]]}), vec!["x", "y"]),
            (json!({"a b": [[1]], "a-b": [[2]]}), vec!["a_b", "a_b_2"]),
        ] {
            let got: Vec<String> = parse_tables(&v, &warn)
                .unwrap()
                .into_iter()
                .map(|t| t.0)
                .collect();
            assert_eq!(got, want, "{v}");
        }
        assert!(
            warn.list()
                .iter()
                .any(|w| w == "inline table a b is named a_b in SQL"),
            "{:?}",
            warn.list()
        );
    }

    #[test]
    fn bad_tables_are_one_sentence() {
        let warn = Warnings::new();
        for (v, want) in [
            (json!(5), "tables must be a list of tables"),
            (json!([5]), "inline table data1 must be an object"),
            (json!({"rows": 5}), "needs a rows list"),
            (
                json!({"rows": [5]}),
                "row 1 of inline table data must be a list or an object",
            ),
            (json!({"rows": []}), "has no columns"),
            (
                json!({"columns": 3, "rows": []}),
                "columns of inline table data must be a list",
            ),
            (
                json!({"columns": [3], "rows": []}),
                "must be names or objects with a name",
            ),
            (
                json!({"columns": [{"type": "int64"}], "rows": []}),
                "has no name",
            ),
            (
                json!({"columns": [{"name": "a", "type": "int64"}], "rows": [["x"]]}),
                "row 1 of inline table data, column a, does not match its type",
            ),
            (
                json!({"columns": [{"name": "a", "type": "date"}], "rows": [["2024-13-45"]]}),
                "does not match its type",
            ),
            (
                json!({"columns": [{"name": "a", "type": "bool"}], "rows": [[1]]}),
                "does not match its type",
            ),
        ] {
            let err = parse_tables(&v, &warn)
                .err()
                .unwrap_or_else(|| panic!("{v} accepted"));
            assert!(err.contains(want), "{v}: {err}");
            assert!(!err.contains('\n'));
        }
    }

    #[test]
    fn too_many_rows_or_columns_are_refused_and_the_limit_itself_is_accepted() {
        let rows: Vec<Value> = vec![json!([1]); MAX_ROWS + 1];
        let err = parse_tables(&json!({"rows": rows}), &Warnings::new())
            .err()
            .unwrap();
        assert!(
            err.contains("over the 1000000 one inline table may have"),
            "{err}"
        );
        let rows: Vec<Value> = vec![json!([1]); MAX_ROWS];
        assert_eq!(
            parse_tables(&json!({"rows": rows}), &Warnings::new()).unwrap()[0]
                .1
                .row_count(),
            Some(MAX_ROWS as u64)
        );
        let wide = vec![json!(1); 10_001];
        let err = parse_tables(&json!({"rows": [wide]}), &Warnings::new())
            .err()
            .unwrap();
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn the_scan_pages_in_batch_sized_slices() {
        let rows: Vec<Value> = (0..(BATCH_ROWS * 2 + 3)).map(|i| json!([i])).collect();
        let (t, _) = one(json!({"rows": rows})).unwrap();
        let sizes: Vec<usize> = t.scan(None).map(|b| b.unwrap().num_rows()).collect();
        assert_eq!(sizes, vec![BATCH_ROWS, BATCH_ROWS, 3]);
        assert_eq!(
            t.scan(Some(&[]))
                .map(|b| b.unwrap().num_rows())
                .sum::<usize>(),
            BATCH_ROWS * 2 + 3
        );
    }

    #[test]
    fn the_fingerprint_follows_the_content() {
        let a = one(json!({"rows": [[1]]})).unwrap().0.fingerprint();
        assert_eq!(a, one(json!({"rows": [[1]]})).unwrap().0.fingerprint());
        assert_ne!(a, one(json!({"rows": [[2]]})).unwrap().0.fingerprint());
    }
}
