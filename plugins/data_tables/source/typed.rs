//! Column types for text and spreadsheet sources: which type a column of
//! tokens or cells is, how a token parses into it, and the builders that turn
//! parsed values into Arrow arrays.
//!
//! The rules are deliberately narrow, so a value is never read as something
//! it is not: an integer needs no leading zero (a zip code stays text), no
//! sign but `-`, and fits `i64`; a float needs digits and a decimal point or
//! an exponent; a date is `YYYY-MM-DD`; a timestamp adds `T` or a space and
//! `HH:MM[:SS[.ffffff]]`, with an optional `Z` or offset. A column that is all
//! integers stays `Int64`; it becomes `Float64` only when a float is in it.
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanBuilder, Date32Builder, Float64Builder, Int64Builder, StringBuilder,
    TimestampMicrosecondBuilder,
};
use arrow::datatypes::{DataType, TimeUnit};
use chrono::{Datelike, NaiveDate};

/// Possibilities of a token, as bits.
pub const BOOL: u8 = 1;
/// The token is an integer.
pub const INT: u8 = 2;
/// The token is a number a float holds exactly.
pub const FLOAT: u8 = 4;
/// The token is a calendar date.
pub const DATE: u8 = 8;
/// The token is a timestamp without an offset (a date is midnight).
pub const TS: u8 = 16;
/// The token is a timestamp with an offset or `Z`.
pub const TSZ: u8 = 32;

/// The longest integer, in digits, a float holds exactly.
const EXACT_FLOAT_DIGITS: usize = 15;
const MICROS_PER_DAY: i64 = 86_400_000_000;
const UNIX_DAYS_FROM_CE: i32 = 719_163;

/// The type of one column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColType {
    /// `true` or `false`.
    Bool,
    /// A 64-bit integer.
    Int,
    /// A 64-bit float.
    Float,
    /// A calendar date.
    Date,
    /// A timestamp without a zone, to the microsecond.
    Timestamp,
    /// A timestamp with an offset, stored as UTC, to the microsecond.
    TimestampUtc,
    /// Text.
    Text,
}

impl ColType {
    /// The Arrow type this column is stored as.
    pub fn data_type(self) -> DataType {
        match self {
            Self::Bool => DataType::Boolean,
            Self::Int => DataType::Int64,
            Self::Float => DataType::Float64,
            Self::Date => DataType::Date32,
            Self::Timestamp => DataType::Timestamp(TimeUnit::Microsecond, None),
            Self::TimestampUtc => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            Self::Text => DataType::Utf8,
        }
    }

    /// The type a set of surviving possibilities settles on.
    pub fn from_flags(flags: u8) -> Self {
        [
            (BOOL, Self::Bool),
            (INT, Self::Int),
            (FLOAT, Self::Float),
            (DATE, Self::Date),
            (TS, Self::Timestamp),
            (TSZ, Self::TimestampUtc),
        ]
        .into_iter()
        .find(|(bit, _)| flags & bit != 0)
        .map_or(Self::Text, |(_, t)| t)
    }
}

fn digits(b: &[u8]) -> bool {
    !b.is_empty() && b.iter().all(u8::is_ascii_digit)
}

fn num(b: &[u8]) -> u32 {
    b.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0'))
}

/// Parses an integer token: `0` or `-0`, or digits with no leading zero, with an optional `-`.
pub fn parse_int(tok: &[u8]) -> Option<i64> {
    let body = tok.strip_prefix(b"-").unwrap_or(tok);
    if !digits(body) || (body.len() > 1 && body[0] == b'0') || body.len() > 19 {
        return None;
    }
    std::str::from_utf8(tok).ok()?.parse().ok()
}

/// Parses a float token: an optional `-`, an integer part with no leading
/// zero, an optional fraction, an optional exponent; the result must be finite.
pub fn parse_float(tok: &[u8]) -> Option<f64> {
    let body = tok.strip_prefix(b"-").unwrap_or(tok);
    let (mantissa, exp) = match body.iter().position(|&b| b == b'e' || b == b'E') {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (int, frac) = match mantissa.iter().position(|&b| b == b'.') {
        Some(i) => (&mantissa[..i], Some(&mantissa[i + 1..])),
        None => (mantissa, None),
    };
    if !digits(int) || (int.len() > 1 && int[0] == b'0') {
        return None;
    }
    if frac.is_some_and(|f| !f.is_empty() && !digits(f)) {
        return None;
    }
    if frac.is_none() && exp.is_none() {
        return None;
    }
    if let Some(e) = exp {
        let e = e
            .strip_prefix(b"+")
            .or_else(|| e.strip_prefix(b"-"))
            .unwrap_or(e);
        if !digits(e) {
            return None;
        }
    }
    let v: f64 = std::str::from_utf8(tok).ok()?.parse().ok()?;
    v.is_finite().then_some(v)
}

/// Parses `true` or `false` in any case.
pub fn parse_bool(tok: &[u8]) -> Option<bool> {
    if tok.eq_ignore_ascii_case(b"true") {
        Some(true)
    } else if tok.eq_ignore_ascii_case(b"false") {
        Some(false)
    } else {
        None
    }
}

/// Days since 1970-01-01 of `YYYY-MM-DD`.
pub fn parse_date(tok: &[u8]) -> Option<i32> {
    if tok.len() != 10 || tok[4] != b'-' || tok[7] != b'-' {
        return None;
    }
    let (y, m, d) = (&tok[..4], &tok[5..7], &tok[8..]);
    if !(digits(y) && digits(m) && digits(d)) {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(num(y) as i32, num(m), num(d))?;
    Some(date.num_days_from_ce() - UNIX_DAYS_FROM_CE)
}

/// A parsed timestamp: microseconds since the epoch and whether it carried an offset.
pub struct Stamp {
    /// Microseconds since 1970-01-01T00:00:00, in UTC when `zoned`.
    pub micros: i64,
    /// Whether the token named an offset (`Z`, `+hh:mm`, `-hhmm`).
    pub zoned: bool,
}

/// Parses `YYYY-MM-DD[T ]HH:MM[:SS[.f{1,6}]]` with an optional `Z` or `+hh:mm`.
pub fn parse_timestamp(tok: &[u8]) -> Option<Stamp> {
    if tok.len() < 16 || !matches!(tok[10], b'T' | b' ') {
        return None;
    }
    let days = i64::from(parse_date(&tok[..10])?);
    let rest = &tok[11..];
    if rest.len() < 5 || rest[2] != b':' || !digits(&rest[..2]) || !digits(&rest[3..5]) {
        return None;
    }
    let (hour, minute) = (num(&rest[..2]), num(&rest[3..5]));
    let mut at = 5;
    let (mut second, mut micro) = (0u32, 0u32);
    if rest.get(at) == Some(&b':') {
        let s = rest.get(at + 1..at + 3)?;
        if !digits(s) {
            return None;
        }
        second = num(s);
        at += 3;
        if rest.get(at) == Some(&b'.') {
            let frac_len = rest[at + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if frac_len == 0 || frac_len > 6 {
                return None;
            }
            micro = num(&rest[at + 1..at + 1 + frac_len]) * 10u32.pow(6 - frac_len as u32);
            at += 1 + frac_len;
        }
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut offset_minutes = 0i64;
    let tail = &rest[at..];
    let zoned = match tail {
        [] => false,
        b"Z" | b"z" => true,
        [sign @ (b'+' | b'-'), zone @ ..] => {
            let (h, m) = match zone {
                [h1, h2, b':', m1, m2] if digits(&[*h1, *h2, *m1, *m2]) => {
                    (num(&[*h1, *h2]), num(&[*m1, *m2]))
                }
                [h1, h2, m1, m2] if digits(&[*h1, *h2, *m1, *m2]) => {
                    (num(&[*h1, *h2]), num(&[*m1, *m2]))
                }
                _ => return None,
            };
            if h > 14 || m > 59 {
                return None;
            }
            offset_minutes = i64::from(h * 60 + m) * if *sign == b'-' { -1 } else { 1 };
            true
        }
        _ => return None,
    };
    let seconds =
        days * 86_400 + i64::from(hour * 3600 + minute * 60 + second) - offset_minutes * 60;
    Some(Stamp {
        micros: seconds * 1_000_000 + i64::from(micro),
        zoned,
    })
}

/// What a non-empty token could be, as a set of [`BOOL`] to [`TSZ`] bits.
/// Zero means it is text and nothing else.
pub fn classify(tok: &[u8]) -> u8 {
    let Some(&first) = tok.first() else { return 0 };
    if !(first.is_ascii_digit() || first == b'-' || first == b'.') {
        return if parse_bool(tok).is_some() { BOOL } else { 0 };
    }
    if parse_int(tok).is_some() {
        let digits = tok.len() - usize::from(first == b'-');
        return INT
            | if digits <= EXACT_FLOAT_DIGITS {
                FLOAT
            } else {
                0
            };
    }
    if parse_float(tok).is_some() {
        return FLOAT;
    }
    if parse_date(tok).is_some() {
        return DATE | TS;
    }
    match parse_timestamp(tok) {
        Some(s) if s.zoned => TSZ,
        Some(_) => TS,
        None => 0,
    }
}

/// Settles column types from observed possibilities.
#[derive(Clone)]
pub struct Inferrer {
    flags: Vec<u8>,
    seen: Vec<bool>,
}

impl Inferrer {
    /// An inferrer for `columns` columns that have seen nothing.
    pub fn new(columns: usize) -> Self {
        Self {
            flags: vec![u8::MAX; columns],
            seen: vec![false; columns],
        }
    }

    /// Records that column `col` holds a value with these possibilities.
    pub fn observe(&mut self, col: usize, flags: u8) {
        self.flags[col] &= flags;
        self.seen[col] = true;
    }

    /// The settled type of every column; a column that never held a value is text.
    pub fn types(&self) -> Vec<ColType> {
        self.flags
            .iter()
            .zip(&self.seen)
            .map(|(&f, &seen)| {
                if seen {
                    ColType::from_flags(f)
                } else {
                    ColType::Text
                }
            })
            .collect()
    }
}

/// A value did not fit the type its column was inferred as.
#[derive(Debug)]
pub struct Mismatch;

/// Builds one column.
pub enum Builder {
    /// Booleans.
    Bool(BooleanBuilder),
    /// Integers.
    Int(Int64Builder),
    /// Floats.
    Float(Float64Builder),
    /// Dates.
    Date(Date32Builder),
    /// Timestamps without a zone.
    Timestamp(TimestampMicrosecondBuilder),
    /// Timestamps in UTC.
    TimestampUtc(TimestampMicrosecondBuilder),
    /// Text.
    Text(StringBuilder),
}

impl Builder {
    /// A builder for `t` expecting about `rows` values.
    pub fn new(t: ColType, rows: usize) -> Self {
        match t {
            ColType::Bool => Self::Bool(BooleanBuilder::with_capacity(rows)),
            ColType::Int => Self::Int(Int64Builder::with_capacity(rows)),
            ColType::Float => Self::Float(Float64Builder::with_capacity(rows)),
            ColType::Date => Self::Date(Date32Builder::with_capacity(rows)),
            ColType::Timestamp => Self::Timestamp(TimestampMicrosecondBuilder::with_capacity(rows)),
            ColType::TimestampUtc => Self::TimestampUtc(
                TimestampMicrosecondBuilder::with_capacity(rows).with_timezone("UTC"),
            ),
            ColType::Text => Self::Text(StringBuilder::with_capacity(rows, rows * 8)),
        }
    }

    /// Appends a NULL.
    pub fn null(&mut self) {
        match self {
            Self::Bool(b) => b.append_null(),
            Self::Int(b) => b.append_null(),
            Self::Float(b) => b.append_null(),
            Self::Date(b) => b.append_null(),
            Self::Timestamp(b) | Self::TimestampUtc(b) => b.append_null(),
            Self::Text(b) => b.append_null(),
        }
    }

    /// Parses `tok` as this column's type and appends it.
    pub fn token(&mut self, tok: &[u8]) -> Result<(), Mismatch> {
        match self {
            Self::Bool(b) => b.append_value(parse_bool(tok).ok_or(Mismatch)?),
            Self::Int(b) => b.append_value(parse_int(tok).ok_or(Mismatch)?),
            Self::Float(b) => {
                let v = parse_int(tok)
                    .map(|i| i as f64)
                    .or_else(|| parse_float(tok))
                    .ok_or(Mismatch)?;
                b.append_value(v);
            }
            Self::Date(b) => b.append_value(parse_date(tok).ok_or(Mismatch)?),
            Self::Timestamp(b) => b.append_value(stamp_of(tok, false)?),
            Self::TimestampUtc(b) => b.append_value(stamp_of(tok, true)?),
            Self::Text(b) => b.append_value(String::from_utf8_lossy(tok)),
        }
        Ok(())
    }

    /// Appends a boolean cell.
    pub fn bool(&mut self, v: bool) -> Result<(), Mismatch> {
        match self {
            Self::Bool(b) => b.append_value(v),
            Self::Text(b) => b.append_value(if v { "true" } else { "false" }),
            _ => return Err(Mismatch),
        }
        Ok(())
    }

    /// Appends an integer cell.
    pub fn int(&mut self, v: i64) -> Result<(), Mismatch> {
        match self {
            Self::Int(b) => b.append_value(v),
            Self::Float(b) if v.unsigned_abs() < (1 << 53) => b.append_value(v as f64),
            Self::Text(b) => b.append_value(v.to_string()),
            _ => return Err(Mismatch),
        }
        Ok(())
    }

    /// Appends a float cell.
    pub fn float(&mut self, v: f64) -> Result<(), Mismatch> {
        match self {
            Self::Float(b) => b.append_value(v),
            Self::Text(b) => b.append_value(crate::render::float_text(v)),
            _ => return Err(Mismatch),
        }
        Ok(())
    }

    /// Appends a text cell.
    pub fn text(&mut self, v: &str) -> Result<(), Mismatch> {
        match self {
            Self::Text(b) => b.append_value(v),
            _ => return Err(Mismatch),
        }
        Ok(())
    }

    /// Appends a date or timestamp cell as microseconds since the epoch.
    pub fn micros(&mut self, v: i64) -> Result<(), Mismatch> {
        match self {
            Self::Date(b) if v % MICROS_PER_DAY == 0 => b.append_value((v / MICROS_PER_DAY) as i32),
            Self::Timestamp(b) | Self::TimestampUtc(b) => b.append_value(v),
            _ => return Err(Mismatch),
        }
        Ok(())
    }

    /// Finishes the column and resets the builder.
    pub fn finish(&mut self) -> ArrayRef {
        match self {
            Self::Bool(b) => Arc::new(b.finish()),
            Self::Int(b) => Arc::new(b.finish()),
            Self::Float(b) => Arc::new(b.finish()),
            Self::Date(b) => Arc::new(b.finish()),
            Self::Timestamp(b) | Self::TimestampUtc(b) => Arc::new(b.finish()),
            Self::Text(b) => Arc::new(b.finish()),
        }
    }
}

/// A timestamp column takes timestamps of its own kind and dates (as midnight).
fn stamp_of(tok: &[u8], zoned: bool) -> Result<i64, Mismatch> {
    if let Some(days) = parse_date(tok) {
        return Ok(i64::from(days) * MICROS_PER_DAY);
    }
    match parse_timestamp(tok) {
        Some(s) if s.zoned == zoned => Ok(s.micros),
        _ => Err(Mismatch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn integers_follow_the_narrow_rules() {
        for (tok, want) in [
            ("0", Some(0)),
            ("-0", Some(0)),
            ("42", Some(42)),
            ("-17", Some(-17)),
            ("9223372036854775807", Some(i64::MAX)),
            ("-9223372036854775808", Some(i64::MIN)),
            ("9223372036854775808", None),
            ("007", None),
            ("-007", None),
            ("+5", None),
            ("1_000", None),
            (" 5", None),
            ("5 ", None),
            ("", None),
            ("-", None),
            ("1.0", None),
            ("0x10", None),
        ] {
            assert_eq!(parse_int(tok.as_bytes()), want, "{tok:?}");
        }
    }

    #[test]
    fn floats_follow_the_narrow_rules() {
        for (tok, want) in [
            ("1.5", Some(1.5)),
            ("-0.25", Some(-0.25)),
            ("1e3", Some(1000.0)),
            ("2.5E-2", Some(0.025)),
            ("3.", Some(3.0)),
            ("10.50", Some(10.5)),
            ("1e400", None),
            ("42", None),
            ("00.5", None),
            (".5", None),
            ("1.2.3", None),
            ("1e", None),
            ("NaN", None),
            ("inf", None),
            ("1,5", None),
        ] {
            assert_eq!(parse_float(tok.as_bytes()), want, "{tok:?}");
        }
    }

    #[test]
    fn dates_and_timestamps() {
        assert_eq!(parse_date(b"1970-01-01"), Some(0));
        assert_eq!(parse_date(b"2024-02-29"), Some(19782));
        assert_eq!(parse_date(b"2023-02-29"), None);
        assert_eq!(parse_date(b"2024-13-01"), None);
        assert_eq!(parse_date(b"2024-1-01"), None);
        assert_eq!(parse_date(b"01/02/2024"), None);
        let t = parse_timestamp(b"2024-02-29 12:30:45.5").unwrap();
        assert_eq!(
            (t.micros, t.zoned),
            (
                19782 * MICROS_PER_DAY + (12 * 3600 + 30 * 60 + 45) * 1_000_000 + 500_000,
                false
            )
        );
        let z = parse_timestamp(b"2024-02-29T12:30:45Z").unwrap();
        assert!(z.zoned);
        let off = parse_timestamp(b"2024-02-29T12:30+02:00").unwrap();
        assert_eq!(
            off.micros,
            19782 * MICROS_PER_DAY + (10 * 3600 + 30 * 60) * 1_000_000
        );
        assert_eq!(
            parse_timestamp(b"2024-02-29T12:30-0130").unwrap().micros,
            19782 * MICROS_PER_DAY + (14 * 3600) * 1_000_000
        );
        for bad in [
            "2024-02-29T24:00:00",
            "2024-02-29T12:60",
            "2024-02-29T12:30:45.1234567",
            "2024-02-29T12:30:45+25:00",
            "2024-02-29X12:30",
            "2024-02-29T12:30:45.",
            "2024-02-29T12:30:45 UTC",
        ] {
            assert!(parse_timestamp(bad.as_bytes()).is_none(), "{bad}");
        }
    }

    #[test]
    fn classification_picks_the_possibilities() {
        assert_eq!(classify(b"true"), BOOL);
        assert_eq!(classify(b"FALSE"), BOOL);
        assert_eq!(classify(b"12"), INT | FLOAT);
        assert_eq!(classify(b"1234567890123456"), INT);
        assert_eq!(classify(b"1.5"), FLOAT);
        assert_eq!(classify(b"2024-01-31"), DATE | TS);
        assert_eq!(classify(b"2024-01-31T10:00"), TS);
        assert_eq!(classify(b"2024-01-31T10:00Z"), TSZ);
        for text in [
            "hello",
            "007",
            "",
            "yes",
            "12abc",
            "+33612345678",
            "NaN",
            "1,5",
            "-",
        ] {
            assert_eq!(classify(text.as_bytes()), 0, "{text}");
        }
    }

    fn infer(columns: &[&[&str]]) -> Vec<ColType> {
        let mut inf = Inferrer::new(columns.len());
        for (i, col) in columns.iter().enumerate() {
            for tok in *col {
                if !tok.is_empty() {
                    inf.observe(i, classify(tok.as_bytes()));
                }
            }
        }
        inf.types()
    }

    #[test]
    fn a_clean_integer_column_stays_integer_and_a_float_widens_it() {
        use ColType::*;
        assert_eq!(infer(&[&["1", "2", "3"]]), [Int]);
        assert_eq!(infer(&[&["1", "2.5"]]), [Float]);
        assert_eq!(infer(&[&["1", "x"]]), [Text]);
        assert_eq!(
            infer(&[
                &["true", "false"],
                &["2024-01-01", "2024-01-02"],
                &["2024-01-01", "2024-01-02 10:00"]
            ]),
            [Bool, Date, Timestamp]
        );
        assert_eq!(infer(&[&["2024-01-01T10:00Z", "2024-01-01T10:00"]]), [Text]);
        assert_eq!(infer(&[&["", ""]]), [Text]);
        // A 16 digit integer is exact only as an integer, so beside a float it is text.
        assert_eq!(infer(&[&["1234567890123456", "1.5"]]), [Text]);
        assert_eq!(infer(&[&["007", "8"]]), [Text]);
    }

    #[test]
    fn a_builder_rejects_a_value_of_another_type() {
        let mut b = Builder::new(ColType::Int, 4);
        b.token(b"5").unwrap();
        assert!(b.token(b"5.5").is_err());
        assert!(b.token(b"x").is_err());
        b.null();
        let a = b.finish();
        assert_eq!(a.len(), 2);
        assert_eq!(a.null_count(), 1);
        let mut f = Builder::new(ColType::Float, 2);
        f.token(b"7").unwrap();
        f.token(b"7.5").unwrap();
        assert!(f.int(1 << 60).is_err());
        let mut d = Builder::new(ColType::Date, 2);
        assert!(d.micros(1).is_err());
        d.micros(MICROS_PER_DAY).unwrap();
    }

    proptest! {
        #[test]
        fn every_i64_is_an_exact_integer(n in any::<i64>()) {
            let s = n.to_string();
            prop_assert_eq!(parse_int(s.as_bytes()), Some(n));
            prop_assert!(classify(s.as_bytes()) & INT != 0);
        }

        #[test]
        fn a_column_of_integers_never_becomes_float(ns in prop::collection::vec(any::<i64>(), 1..50)) {
            let toks: Vec<String> = ns.iter().map(i64::to_string).collect();
            let col: Vec<&str> = toks.iter().map(String::as_str).collect();
            prop_assert_eq!(infer(&[&col]), [ColType::Int]);
        }

        #[test]
        fn the_float_parser_agrees_with_std_on_its_own_output(x in any::<f64>().prop_filter("finite", |v| v.is_finite())) {
            let s = format!("{x:?}");
            if let Some(v) = parse_float(s.as_bytes()) {
                prop_assert_eq!(v.to_bits(), x.to_bits());
            }
        }

        #[test]
        fn parsers_never_panic(data in prop::collection::vec(any::<u8>(), 0..40)) {
            let _ = (classify(&data), parse_int(&data), parse_float(&data), parse_date(&data), parse_timestamp(&data));
        }
    }
}
