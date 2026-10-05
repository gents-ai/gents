//! The plugin's input: every field checked once, at the edge.
use serde::Deserialize;
use serde_json::Value;

use crate::Res;
use crate::csv::Options;

/// The most rows one page returns.
pub const MAX_ROWS_LIMIT: u64 = 100_000;
/// Default rows per page.
pub const DEFAULT_MAX_ROWS: u64 = 200;
/// Default bytes of row data per page.
pub const DEFAULT_MAX_BYTES: u64 = 1_000_000;
/// The most bytes of row data one page may hold: the host's output limit leaves room for the rest.
pub const MAX_BYTES_LIMIT: u64 = 3_000_000;
/// Default rows shown in the Markdown table.
pub const DEFAULT_MARKDOWN_ROWS: u64 = 50;
/// The most rows `describe` reads to compute its statistics.
pub const MAX_SAMPLE_ROWS: u64 = 10_000_000;
/// Default rows `describe` reads.
pub const DEFAULT_SAMPLE_ROWS: u64 = 100_000;
/// The most columns `describe` reports per call.
pub const MAX_DESCRIBE_COLUMNS: usize = 200;

/// What a call does.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// List the tables.
    Tables,
    /// Schema, statistics and a sample.
    Describe,
    /// Run a SELECT.
    Query,
    /// Run a SELECT and write the result to a file.
    Export,
}

/// The format an export writes.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// Comma separated text.
    Csv,
    /// Parquet.
    Parquet,
}

/// One call's input.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub mode: Option<Mode>,
    pub path: Option<String>,
    /// Set by the host to the real path of what `path` names; the plugin ignores it.
    #[allow(dead_code)]
    pub path_original: Option<String>,
    pub files: Option<Vec<String>>,
    pub tables: Option<Value>,
    pub table: Option<String>,
    pub sql: Option<String>,
    pub max_rows: Option<u64>,
    pub max_bytes: Option<u64>,
    pub markdown_rows: Option<u64>,
    pub cursor: Option<String>,
    pub delimiter: Option<String>,
    pub header: Option<bool>,
    pub sample_rows: Option<u64>,
    pub columns: Option<Vec<String>>,
    pub output: Option<String>,
    pub format: Option<ExportFormat>,
    pub overwrite: Option<bool>,
}

fn within(name: &str, v: Option<u64>, lo: u64, hi: u64, default: u64) -> Res<u64> {
    match v {
        None => Ok(default),
        Some(n) if (lo..=hi).contains(&n) => Ok(n),
        Some(_) => Err(format!("{name} must be a whole number from {lo} to {hi}")),
    }
}

impl Input {
    /// The mode: the one given, else `query` when there is SQL, else `tables`.
    pub fn mode(&self) -> Mode {
        self.mode.unwrap_or(if self.sql.is_some() {
            Mode::Query
        } else {
            Mode::Tables
        })
    }

    /// Rows per page.
    pub fn max_rows(&self) -> Res<usize> {
        within(
            "max_rows",
            self.max_rows,
            1,
            MAX_ROWS_LIMIT,
            DEFAULT_MAX_ROWS,
        )
        .map(|n| n as usize)
    }

    /// Bytes of row data per page.
    pub fn max_bytes(&self) -> Res<usize> {
        within(
            "max_bytes",
            self.max_bytes,
            4096,
            MAX_BYTES_LIMIT,
            DEFAULT_MAX_BYTES,
        )
        .map(|n| n as usize)
    }

    /// Rows in the Markdown table.
    pub fn markdown_rows(&self) -> Res<usize> {
        within(
            "markdown_rows",
            self.markdown_rows,
            0,
            1000,
            DEFAULT_MARKDOWN_ROWS,
        )
        .map(|n| n as usize)
    }

    /// Rows `describe` reads.
    pub fn sample_rows(&self) -> Res<u64> {
        within(
            "sample_rows",
            self.sample_rows,
            1,
            MAX_SAMPLE_ROWS,
            DEFAULT_SAMPLE_ROWS,
        )
    }

    /// The CSV options the call fixes.
    pub fn csv_options(&self) -> Res<Options> {
        let delimiter = match self.delimiter.as_deref() {
            None => None,
            Some("\\t" | "tab") => Some(b'\t'),
            Some(d) if d.len() == 1 && d.is_ascii() && !matches!(d, "\"" | "\n" | "\r") => {
                Some(d.as_bytes()[0])
            }
            Some(_) => return Err(
                "delimiter must be one ASCII character other than a quote or a line break, or tab"
                    .into(),
            ),
        };
        Ok(Options {
            delimiter,
            header: self.header,
        })
    }

    /// The SQL, which a query or an export needs.
    pub fn sql(&self) -> Res<&str> {
        match self.sql.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => Ok(s),
            _ => Err("sql is required: give the SELECT to run".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: serde_json::Value) -> Res<Input> {
        serde_json::from_value(v).map_err(|e| format!("invalid input: {e}"))
    }

    #[test]
    fn the_mode_follows_the_sql_unless_given() {
        assert_eq!(parse(serde_json::json!({})).unwrap().mode(), Mode::Tables);
        assert_eq!(
            parse(serde_json::json!({"sql": "select 1"}))
                .unwrap()
                .mode(),
            Mode::Query
        );
        assert_eq!(
            parse(serde_json::json!({"sql": "select 1", "mode": "describe"}))
                .unwrap()
                .mode(),
            Mode::Describe
        );
    }

    #[test]
    fn unknown_fields_and_bad_modes_are_refused() {
        assert!(parse(serde_json::json!({"nope": 1})).is_err());
        assert!(parse(serde_json::json!({"mode": "drop"})).is_err());
    }

    #[test]
    fn limits_are_enforced_with_a_sentence() {
        let i = parse(serde_json::json!({"max_rows": 0})).unwrap();
        assert!(i.max_rows().unwrap_err().contains("from 1 to 100000"));
        assert!(
            parse(serde_json::json!({"max_rows": 100001}))
                .unwrap()
                .max_rows()
                .is_err()
        );
        assert_eq!(
            parse(serde_json::json!({"max_rows": 100000}))
                .unwrap()
                .max_rows(),
            Ok(100_000)
        );
        assert!(
            parse(serde_json::json!({"max_bytes": 4095}))
                .unwrap()
                .max_bytes()
                .is_err()
        );
        assert!(
            parse(serde_json::json!({"max_bytes": 3000001}))
                .unwrap()
                .max_bytes()
                .is_err()
        );
        assert!(parse(serde_json::json!({"max_rows": -1})).is_err());
        assert_eq!(parse(serde_json::json!({})).unwrap().max_rows(), Ok(200));
    }

    #[test]
    fn delimiters() {
        let d = |v: &str| {
            parse(serde_json::json!({"delimiter": v}))
                .unwrap()
                .csv_options()
                .map(|o| o.delimiter)
        };
        assert_eq!(d(";"), Ok(Some(b';')));
        assert_eq!(d("tab"), Ok(Some(b'\t')));
        assert_eq!(d("\\t"), Ok(Some(b'\t')));
        assert!(d("ab").is_err());
        assert!(d("\"").is_err());
        assert!(d("é").is_err());
        assert!(d("").is_err());
    }

    #[test]
    fn sql_must_not_be_blank() {
        assert!(
            parse(serde_json::json!({"sql": "  "}))
                .unwrap()
                .sql()
                .is_err()
        );
        assert_eq!(
            parse(serde_json::json!({"sql": " select 1 "}))
                .unwrap()
                .sql(),
            Ok("select 1")
        );
        assert!(parse(serde_json::json!({})).unwrap().sql().is_err());
    }
}
