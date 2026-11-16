//! Result values for the model and for other nodes: exact type names, JSON
//! values that never lose precision, and the bounded Markdown table.
//!
//! Integers and floats are JSON numbers; an `int64` or `uint64` beyond
//! +-2^53 (which a JSON reader may round) and every decimal are strings, the
//! column `type` saying which. Floats print in the shortest form that reads
//! back to the same value; NaN and the infinities, which JSON cannot hold,
//! are the strings `"NaN"`, `"Infinity"` and `"-Infinity"`. A SQL NULL is
//! `null` and an empty string is `""`. Dates and timestamps are ISO-8601.
use arrow::array::{Array, ArrayRef, AsArray, BinaryArray, LargeBinaryArray, StructArray};
use arrow::datatypes::{
    DataType, Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type, UInt8Type,
    UInt16Type, UInt32Type, UInt64Type,
};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use serde_json::{Map, Value, json};

/// The largest integer a JSON reader holds exactly as a double.
const SAFE_INT: u64 = 1 << 53;
/// Cells longer than this many characters are cut in the Markdown table.
const MARKDOWN_CELL_CHARS: usize = 200;

/// The name a column's type is reported under.
pub fn type_name(dt: &DataType) -> String {
    match dt {
        DataType::Null => "null".into(),
        DataType::Boolean => "bool".into(),
        DataType::Int8 => "int8".into(),
        DataType::Int16 => "int16".into(),
        DataType::Int32 => "int32".into(),
        DataType::Int64 => "int64".into(),
        DataType::UInt8 => "uint8".into(),
        DataType::UInt16 => "uint16".into(),
        DataType::UInt32 => "uint32".into(),
        DataType::UInt64 => "uint64".into(),
        DataType::Float16 | DataType::Float32 => "float32".into(),
        DataType::Float64 => "float64".into(),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "text".into(),
        DataType::Binary
        | DataType::LargeBinary
        | DataType::FixedSizeBinary(_)
        | DataType::BinaryView => "binary".into(),
        DataType::Date32 | DataType::Date64 => "date".into(),
        DataType::Time32(_) | DataType::Time64(_) => "time".into(),
        DataType::Timestamp(_, None) => "timestamp".into(),
        DataType::Timestamp(_, Some(_)) => "timestamp_tz".into(),
        DataType::Duration(_) => "duration".into(),
        DataType::Interval(_) => "interval".into(),
        DataType::Decimal128(p, s) | DataType::Decimal256(p, s) => format!("decimal({p},{s})"),
        DataType::List(f) | DataType::LargeList(f) | DataType::FixedSizeList(f, _) => {
            format!("list<{}>", type_name(f.data_type()))
        }
        DataType::Struct(_) => "struct".into(),
        DataType::Map(..) => "map".into(),
        DataType::Dictionary(_, v) => type_name(v),
        other => format!("{other}").to_lowercase(),
    }
}

/// A float as JSON text: shortest round-trip digits, or the name of a non-finite value.
pub fn float_text(v: f64) -> String {
    match float_json(v) {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

fn float_json(v: f64) -> Value {
    if v.is_nan() {
        return json!("NaN");
    }
    if v.is_infinite() {
        return json!(if v > 0.0 { "Infinity" } else { "-Infinity" });
    }
    serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
}

/// The shortest decimal form of a 32-bit float, read back as the double it prints as.
fn f32_json(v: f32) -> Value {
    if !v.is_finite() {
        return float_json(f64::from(v));
    }
    float_json(v.to_string().parse().unwrap_or(f64::from(v)))
}

fn shown(array: &dyn Array, i: usize) -> Value {
    let opts = FormatOptions::default().with_display_error(true);
    match ArrayFormatter::try_new(array, &opts) {
        Ok(f) => Value::String(f.value(i).to_string()),
        Err(_) => Value::Null,
    }
}

fn hex(bytes: &[u8]) -> Value {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    Value::String(s)
}

fn int_json(v: i128) -> Value {
    if v.unsigned_abs() <= u128::from(SAFE_INT) {
        json!(v as i64)
    } else {
        Value::String(v.to_string())
    }
}

/// How much of one cell may still be turned into JSON. Once it runs out the rest of the cell
/// (the tail of a text, the later elements of a list) is left unread, so a huge nested value is
/// never built whole.
struct Budget {
    left: usize,
    cut: bool,
}

impl Budget {
    /// Charges up to `n` bytes and returns how many were granted; fewer than `n` marks the
    /// cell as cut and spends the budget.
    fn grant(&mut self, n: usize) -> usize {
        let granted = n.min(self.left);
        self.left -= granted;
        self.cut |= granted < n;
        granted
    }

    /// Whether the budget is spent, which marks the cell as cut: asked of a cell that has more
    /// to give.
    fn spent(&mut self) -> bool {
        self.cut |= self.left == 0;
        self.left == 0
    }

    fn text(&mut self, s: &str) -> Value {
        let granted = self.grant(s.len());
        Value::String(cut_at(s, granted).to_string())
    }
}

/// `s` cut to at most `n` bytes on a character boundary.
pub fn cut_at(s: &str, n: usize) -> &str {
    let mut at = n.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    &s[..at]
}

/// The JSON value of row `i` of `a`, whole.
#[cfg(test)]
pub fn value_json(a: &ArrayRef, i: usize) -> Value {
    value(
        a,
        i,
        &mut Budget {
            left: usize::MAX,
            cut: false,
        },
    )
}

/// The JSON value of row `i` of `a` with at most about `cap` bytes of it built. A value over
/// the cap becomes the first `cap` bytes of its JSON text, as a string, and the flag is true;
/// a list or record cut this way is never half built.
pub fn cell_json(a: &ArrayRef, i: usize, cap: usize) -> (Value, bool) {
    let mut budget = Budget {
        left: cap,
        cut: false,
    };
    let v = value(a, i, &mut budget);
    match v {
        _ if !budget.cut => (v, false),
        Value::String(_) => (v, true),
        other => (
            Value::String(cut_at(&other.to_string(), cap).to_string()),
            true,
        ),
    }
}

fn value(a: &ArrayRef, i: usize, b: &mut Budget) -> Value {
    if a.is_null(i) {
        return Value::Null;
    }
    // A number or flag costs the bytes its JSON text takes, and a comma.
    let mut small = |v: Value| {
        b.grant(v.to_string().len() + 1);
        v
    };
    match a.data_type() {
        DataType::Null => Value::Null,
        DataType::Boolean => small(Value::Bool(a.as_boolean().value(i))),
        DataType::Int8 => small(json!(a.as_primitive::<Int8Type>().value(i))),
        DataType::Int16 => small(json!(a.as_primitive::<Int16Type>().value(i))),
        DataType::Int32 => small(json!(a.as_primitive::<Int32Type>().value(i))),
        DataType::Int64 => small(int_json(i128::from(a.as_primitive::<Int64Type>().value(i)))),
        DataType::UInt8 => small(json!(a.as_primitive::<UInt8Type>().value(i))),
        DataType::UInt16 => small(json!(a.as_primitive::<UInt16Type>().value(i))),
        DataType::UInt32 => small(json!(a.as_primitive::<UInt32Type>().value(i))),
        DataType::UInt64 => small(int_json(i128::from(
            a.as_primitive::<UInt64Type>().value(i),
        ))),
        DataType::Float32 => small(f32_json(a.as_primitive::<Float32Type>().value(i))),
        DataType::Float64 => small(float_json(a.as_primitive::<Float64Type>().value(i))),
        DataType::Utf8 => b.text(a.as_string::<i32>().value(i)),
        DataType::LargeUtf8 => b.text(a.as_string::<i64>().value(i)),
        DataType::Utf8View => b.text(a.as_string_view().value(i)),
        DataType::Binary => hex_capped(
            a.as_any()
                .downcast_ref::<BinaryArray>()
                .map_or(&[][..], |x| x.value(i)),
            b,
        ),
        DataType::LargeBinary => hex_capped(
            a.as_any()
                .downcast_ref::<LargeBinaryArray>()
                .map_or(&[][..], |x| x.value(i)),
            b,
        ),
        DataType::List(_) => list_json(&a.as_list::<i32>().value(i), b),
        DataType::LargeList(_) => list_json(&a.as_list::<i64>().value(i), b),
        DataType::FixedSizeList(..) => list_json(&a.as_fixed_size_list().value(i), b),
        DataType::Struct(_) => struct_json(a.as_struct(), i, b),
        DataType::Dictionary(..) => match a.as_any_dictionary().normalized_keys().get(i) {
            Some(&k) => value(a.as_any_dictionary().values(), k, b),
            None => Value::Null,
        },
        _ => match shown(a.as_ref(), i) {
            Value::String(s) => b.text(&s),
            other => other,
        },
    }
}

fn hex_capped(bytes: &[u8], b: &mut Budget) -> Value {
    let granted = b.grant(bytes.len().saturating_mul(2));
    hex(&bytes[..granted / 2])
}

fn list_json(values: &ArrayRef, b: &mut Budget) -> Value {
    let mut out = Vec::new();
    for j in 0..values.len() {
        if b.spent() {
            break;
        }
        out.push(value(values, j, b));
    }
    Value::Array(out)
}

fn struct_json(s: &StructArray, i: usize, b: &mut Budget) -> Value {
    let mut map = Map::new();
    for (field, column) in s.fields().iter().zip(s.columns()) {
        if b.spent() {
            break;
        }
        b.grant(field.name().len() + 4);
        map.insert(field.name().clone(), value(column, i, b));
    }
    Value::Object(map)
}

/// A Markdown table of up to `limit` of `rows`, and whether rows were left out.
/// A NULL shows as `NULL`, an empty string as an empty cell, `|` is escaped and a line
/// break shows as `<br>`.
pub fn markdown(columns: &[(String, String)], rows: &[Vec<Value>], limit: usize) -> (String, bool) {
    let (md, shown) = markdown_capped(columns, rows, limit, usize::MAX);
    (md, rows.len() > shown)
}

/// As [`markdown`], also stopping once the table is `max_bytes` long, and returning how many
/// rows it shows.
pub fn markdown_capped(
    columns: &[(String, String)],
    rows: &[Vec<Value>],
    limit: usize,
    max_bytes: usize,
) -> (String, usize) {
    if columns.is_empty() {
        return (String::new(), 0);
    }
    let mut out = String::new();
    let head: Vec<String> = columns.iter().map(|(n, _)| cell_text(n)).collect();
    out.push_str(&format!("| {} |\n", head.join(" | ")));
    out.push_str(&format!("|{}\n", " --- |".repeat(columns.len())));
    let mut shown = 0;
    for row in rows.iter().take(limit) {
        if out.len() >= max_bytes {
            break;
        }
        let cells: Vec<String> = row.iter().map(|v| cell_text(&value_text(v))).collect();
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
        shown += 1;
    }
    (out.trim_end().to_string(), shown)
}

fn value_text(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn cell_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MARKDOWN_CELL_CHARS + 3));
    for (n, c) in s.chars().enumerate() {
        if n == MARKDOWN_CELL_CHARS {
            out.push_str("...");
            break;
        }
        match c {
            '|' => out.push_str("\\|"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{
        BinaryArray, Date32Array, Decimal128Array, Float32Array, Float64Array, Int64Array,
        ListArray, StringArray, TimestampMicrosecondArray, UInt64Array,
    };
    use arrow::datatypes::Int32Type;
    use std::sync::Arc;

    fn arr(a: impl Array + 'static) -> ArrayRef {
        Arc::new(a)
    }

    #[test]
    fn integers_past_two_to_the_53_are_strings_and_the_rest_are_numbers() {
        let a = arr(Int64Array::from(vec![
            Some(0),
            Some(9_007_199_254_740_992),
            Some(9_007_199_254_740_993),
            Some(-9_007_199_254_740_993),
            Some(i64::MAX),
            Some(i64::MIN),
            None,
        ]));
        let got: Vec<Value> = (0..a.len()).map(|i| value_json(&a, i)).collect();
        assert_eq!(
            got,
            vec![
                json!(0),
                json!(9_007_199_254_740_992_i64),
                json!("9007199254740993"),
                json!("-9007199254740993"),
                json!("9223372036854775807"),
                json!("-9223372036854775808"),
                Value::Null
            ]
        );
        let u = arr(UInt64Array::from(vec![u64::MAX, 5]));
        assert_eq!(value_json(&u, 0), json!("18446744073709551615"));
        assert_eq!(value_json(&u, 1), json!(5));
    }

    #[test]
    fn floats_print_shortest_and_non_finite_values_are_named() {
        let a = arr(Float64Array::from(vec![
            0.1,
            100.0,
            1e21,
            1.5e-7,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.0,
        ]));
        let got: Vec<String> = (0..a.len())
            .map(|i| value_json(&a, i).to_string())
            .collect();
        assert_eq!(
            got,
            [
                "0.1",
                "100.0",
                "1e+21",
                "1.5e-7",
                "\"NaN\"",
                "\"Infinity\"",
                "\"-Infinity\"",
                "-0.0"
            ]
        );
        let f = arr(Float32Array::from(vec![0.1_f32, 16_777_216.0]));
        assert_eq!(value_json(&f, 0).to_string(), "0.1");
        assert_eq!(value_json(&f, 1).to_string(), "16777216.0");
    }

    #[test]
    fn null_and_empty_string_stay_distinct() {
        let a = arr(StringArray::from(vec![Some(""), None, Some("x")]));
        assert_eq!(value_json(&a, 0), json!(""));
        assert_eq!(value_json(&a, 1), Value::Null);
    }

    #[test]
    fn decimals_dates_timestamps_and_binary() {
        let d = arr(Decimal128Array::from(vec![12345_i128, -5])
            .with_precision_and_scale(10, 2)
            .unwrap());
        assert_eq!(value_json(&d, 0), json!("123.45"));
        assert_eq!(value_json(&d, 1), json!("-0.05"));
        assert_eq!(type_name(d.data_type()), "decimal(10,2)");
        let date = arr(Date32Array::from(vec![19782]));
        assert_eq!(value_json(&date, 0), json!("2024-02-29"));
        let ts = arr(TimestampMicrosecondArray::from(vec![
            1_709_209_845_500_000_i64,
        ]));
        assert_eq!(value_json(&ts, 0), json!("2024-02-29T12:30:45.500"));
        let tz = arr(
            TimestampMicrosecondArray::from(vec![1_709_209_845_000_000_i64]).with_timezone("UTC"),
        );
        assert_eq!(value_json(&tz, 0), json!("2024-02-29T12:30:45Z"));
        let bin = arr(BinaryArray::from_vec(vec![&[0u8, 255, 16][..]]));
        assert_eq!(value_json(&bin, 0), json!("00ff10"));
    }

    #[test]
    fn nested_values_keep_their_shape() {
        let list = ListArray::from_iter_primitive::<Int32Type, _, _>(vec![
            Some(vec![Some(1), None, Some(3)]),
            None,
        ]);
        let a = arr(list);
        assert_eq!(value_json(&a, 0), json!([1, null, 3]));
        assert_eq!(value_json(&a, 1), Value::Null);
        assert_eq!(type_name(a.data_type()), "list<int32>");
    }

    #[test]
    fn type_names() {
        assert_eq!(type_name(&DataType::Utf8), "text");
        assert_eq!(type_name(&DataType::Boolean), "bool");
        assert_eq!(
            type_name(&DataType::Timestamp(
                arrow::datatypes::TimeUnit::Nanosecond,
                None
            )),
            "timestamp"
        );
        assert_eq!(
            type_name(&DataType::Timestamp(
                arrow::datatypes::TimeUnit::Nanosecond,
                Some("UTC".into())
            )),
            "timestamp_tz"
        );
    }

    #[test]
    fn markdown_escapes_marks_null_and_says_when_rows_were_left_out() {
        let cols = vec![
            ("a|b".to_string(), "text".to_string()),
            ("n".to_string(), "int64".to_string()),
        ];
        let rows = vec![
            vec![json!("x|y\nz"), json!(1)],
            vec![Value::Null, json!(2)],
            vec![json!(""), json!(3)],
        ];
        let (md, cut) = markdown(&cols, &rows, 2);
        assert_eq!(
            md,
            "| a\\|b | n |\n| --- | --- |\n| x\\|y<br>z | 1 |\n| NULL | 2 |"
        );
        assert!(cut);
        let (md, cut) = markdown(&cols, &rows, 3);
        assert!(!cut);
        assert!(md.ends_with("|  | 3 |"));
        let long = vec![vec![json!("é".repeat(300)), json!(1)]];
        let (md, _) = markdown(&cols, &long, 1);
        assert!(md.contains(&format!("{}...", "é".repeat(200))));
        assert!(!md.contains(&"é".repeat(201)));
    }

    #[test]
    fn floats_as_text_for_cells() {
        assert_eq!(float_text(1.5), "1.5");
        assert_eq!(float_text(f64::NAN), "NaN");
        assert_eq!(float_text(f64::NEG_INFINITY), "-Infinity");
    }
}
