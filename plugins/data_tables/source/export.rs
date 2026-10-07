//! The `export` mode: write a query result into the bound folder as CSV or
//! Parquet. It streams, so a result of any size is written with flat memory,
//! and it writes a hidden temporary file of its own that becomes the target
//! only when the whole result was written.
//!
//! CSV keeps NULL (an empty field) apart from the empty string (`""`), so a
//! file read back gives the same rows; floats are written with their shortest
//! round-trip digits.
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, AsArray, RecordBatch};
use arrow::datatypes::{DataType, Float32Type, Float64Type};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use futures::StreamExt;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde_json::{Value, json};

use crate::Res;
use crate::catalog::Catalog;
use crate::engine::{Engine, explain, plain, scan_error};
use crate::input::{ExportFormat, Input};
use crate::query::POOL_BYTES;
use crate::render::float_text;
use crate::table::ScanError;

/// One CSV field of row `i`: `None` is NULL.
fn field(a: &ArrayRef, i: usize) -> Option<String> {
    if a.is_null(i) {
        return None;
    }
    Some(match a.data_type() {
        DataType::Utf8 => a.as_string::<i32>().value(i).to_string(),
        DataType::LargeUtf8 => a.as_string::<i64>().value(i).to_string(),
        DataType::Boolean => a.as_boolean().value(i).to_string(),
        DataType::Float64 => float_text(a.as_primitive::<Float64Type>().value(i)),
        DataType::Float32 => float_text(
            a.as_primitive::<Float32Type>()
                .value(i)
                .to_string()
                .parse()
                .unwrap_or(f64::NAN),
        ),
        _ => {
            let opts = FormatOptions::default();
            ArrayFormatter::try_new(a.as_ref(), &opts)
                .map(|f| f.value(i).to_string())
                .unwrap_or_default()
        }
    })
}

/// A CSV field as bytes: quoted when it holds a delimiter the reader may sniff (`,` `;` tab
/// `|`), a quote or a line break, or edge spaces, and quoted when it is the empty string,
/// which an empty NULL field is not. Quoting every sniffable delimiter keeps a one-column
/// table whose values hold `;` from being read back as two columns.
fn encode(value: Option<String>, out: &mut Vec<u8>) {
    match value {
        None => {}
        Some(s) if s.is_empty() => out.extend_from_slice(b"\"\""),
        Some(s)
            if s.contains([',', ';', '|', '\t', '"', '\n', '\r'])
                || s.starts_with(' ')
                || s.ends_with(' ') =>
        {
            out.push(b'"');
            out.extend_from_slice(s.replace('"', "\"\"").as_bytes());
            out.push(b'"');
        }
        Some(s) => out.extend_from_slice(s.as_bytes()),
    }
}

/// Writes `batch` as CSV rows. A NULL in a one-column file would be a blank line, which a reader
/// skips, so it is written as an empty string and counted in `blanked`.
fn write_csv(w: &mut impl Write, batch: &RecordBatch, blanked: &mut u64) -> std::io::Result<()> {
    let mut line = Vec::new();
    for i in 0..batch.num_rows() {
        line.clear();
        for (c, col) in batch.columns().iter().enumerate() {
            if c > 0 {
                line.push(b',');
            }
            let mut value = field(col, i);
            if value.is_none() && batch.num_columns() == 1 {
                *blanked += 1;
                value = Some(String::new());
            }
            encode(value, &mut line);
        }
        line.push(b'\n');
        w.write_all(&line)?;
    }
    Ok(())
}

fn header_line(names: &[String]) -> Vec<u8> {
    let mut line = Vec::new();
    for (c, n) in names.iter().enumerate() {
        if c > 0 {
            line.push(b',');
        }
        // A header is always text, even when empty.
        if n.is_empty() {
            line.extend_from_slice(b"\"\"")
        } else {
            encode(Some(n.clone()), &mut line)
        }
    }
    line.push(b'\n');
    line
}

/// The longest file name most filesystems hold.
const MAX_NAME_BYTES: usize = 255;

fn valid_name(name: &str) -> Res<()> {
    let bad = name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.starts_with('.')
        || name.contains(['/', '\\', '\0']);
    if bad {
        return Err("output must be a plain file name such as result.csv, without folders and not starting with a dot".into());
    }
    Ok(())
}

fn io_error(what: &str, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied
        || e.raw_os_error() == Some(76)
        || e.raw_os_error() == Some(63)
    {
        return format!(
            "{what}: the folder is read-only for this call; allow writing to it, or call again without output to only read"
        );
    }
    format!("{what}: {e}")
}

enum Sink {
    Csv(BufWriter<File>),
    Parquet(Box<ArrowWriter<File>>),
}

/// Why a write did not finish.
enum Stop {
    /// A table's types were widened: write again.
    Retry,
    Fail(String),
}

impl From<String> for Stop {
    fn from(s: String) -> Self {
        Self::Fail(s)
    }
}

/// Creates a temporary file in `dir` that no other call is using: created exclusively, so
/// concurrent exports and a leftover of a call that died never share one.
fn temp_file(dir: &Path) -> Res<(std::path::PathBuf, File)> {
    let mut n = 0u32;
    loop {
        let path = dir.join(format!(".data_tables-{n}.part"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
            Err(e) => return Err(io_error("cannot create the output file", &e)),
        }
    }
}

/// Makes the finished `tmp` the file `target`. Without `overwrite` a hard link publishes it only
/// while the name is free, so a file that appeared since it was checked is never replaced.
fn publish(tmp: &Path, target: &Path, overwrite: bool) -> std::io::Result<()> {
    if overwrite {
        std::fs::rename(tmp, target)
    } else {
        std::fs::hard_link(tmp, target).and_then(|()| std::fs::remove_file(tmp))
    }
}

async fn write_all(
    catalog: &Arc<Catalog>,
    sql: &str,
    format: ExportFormat,
    file: File,
) -> Result<u64, Stop> {
    let engine = Engine::new(Arc::clone(catalog), POOL_BYTES).map_err(Stop::Fail)?;
    let prepared = engine.prepare(sql).await.map_err(Stop::Fail)?;
    let mut stream = engine
        .stream(prepared)
        .await
        .map_err(|e| Stop::Fail(explain(&e)))?;
    let schema = stream.schema();
    let names: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
    let mut sink = match format {
        ExportFormat::Csv => {
            let mut w = BufWriter::new(file);
            w.write_all(&header_line(&names))
                .map_err(|e| Stop::Fail(io_error("cannot write the output file", &e)))?;
            Sink::Csv(w)
        }
        ExportFormat::Parquet => {
            let props = WriterProperties::builder()
                .set_compression(Compression::SNAPPY)
                .set_created_by("gents data_tables".to_string())
                .build();
            let plain_schema = plain(&RecordBatch::new_empty(Arc::clone(&schema)))
                .map_err(|e| Stop::Fail(e.to_string()))?
                .schema();
            Sink::Parquet(Box::new(
                ArrowWriter::try_new(file, plain_schema, Some(props)).map_err(|e| {
                    Stop::Fail(format!("the result cannot be stored as Parquet: {e}"))
                })?,
            ))
        }
    };
    let (mut rows, mut blanked) = (0u64, 0u64);
    while let Some(batch) = stream.next().await {
        let batch = match batch {
            Ok(b) => b,
            Err(e) => {
                return Err(match scan_error(&e) {
                    Some(ScanError::Conflict { table, .. }) if catalog.widen(table) => Stop::Retry,
                    _ => Stop::Fail(explain(&e)),
                });
            }
        };
        let batch = plain(&batch).map_err(|e| Stop::Fail(e.to_string()))?;
        rows += batch.num_rows() as u64;
        match &mut sink {
            Sink::Csv(w) => write_csv(w, &batch, &mut blanked)
                .map_err(|e| Stop::Fail(io_error("cannot write the output file", &e)))?,
            Sink::Parquet(w) => w
                .write(&batch)
                .map_err(|e| Stop::Fail(format!("cannot write the output file: {e}")))?,
        }
    }
    match sink {
        Sink::Csv(mut w) => w
            .flush()
            .map_err(|e| Stop::Fail(io_error("cannot write the output file", &e)))?,
        Sink::Parquet(w) => {
            w.close()
                .map_err(|e| Stop::Fail(format!("cannot write the output file: {e}")))?;
        }
    }
    if blanked > 0 {
        catalog.warn.once(
            "blank-nulls",
            format!("{blanked} NULL values in a one-column file were written as empty strings, because a blank line is not a row"),
        );
    }
    Ok(rows)
}

/// The `export` mode.
pub async fn export(input: &Input, catalog: &Arc<Catalog>) -> Res<Value> {
    let sql = input.sql()?;
    let name = input
        .output
        .as_deref()
        .ok_or("output is required: the file name to write, such as result.csv")?;
    valid_name(name)?;
    let format = match (
        input.format,
        Path::new(name).extension().and_then(|e| e.to_str()),
    ) {
        (Some(f), _) => f,
        (None, Some(e)) if e.eq_ignore_ascii_case("csv") => ExportFormat::Csv,
        (None, Some(e)) if e.eq_ignore_ascii_case("parquet") => ExportFormat::Parquet,
        _ => return Err("format is required unless output ends in .csv or .parquet".into()),
    };
    let dir = Path::new(
        input
            .path
            .as_deref()
            .ok_or("export writes into the bound folder: give path as a folder")?,
    );
    if !dir.is_dir() {
        return Err(
            "export writes into the bound folder: path must be a folder, not a file".into(),
        );
    }
    let target = dir.join(name);
    let overwrite = input.overwrite == Some(true);
    let exists =
        || format!("{name} already exists; choose another output name or set overwrite to true");
    if target.exists() && !overwrite {
        return Err(exists());
    }
    let (tmp, mut file) = temp_file(dir)?;
    let rows = loop {
        let failed = match write_all(catalog, sql, format, file).await {
            Ok(rows) => break rows,
            // Our own temporary file: start it again.
            Err(Stop::Retry) => match File::create(&tmp) {
                Ok(f) => {
                    file = f;
                    continue;
                }
                Err(e) => io_error("cannot create the output file", &e),
            },
            Err(Stop::Fail(e)) => e,
        };
        let _ = std::fs::remove_file(&tmp);
        return Err(failed);
    };
    publish(&tmp, &target, overwrite).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            exists()
        } else {
            io_error("cannot finish the output file", &e)
        }
    })?;
    let bytes = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "written": name,
        "format": match format { ExportFormat::Csv => "csv", ExportFormat::Parquet => "parquet" },
        "rows": rows,
        "bytes": bytes,
        "warnings": catalog.warn.list(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_keep_null_and_empty_apart() {
        let mut out = Vec::new();
        for v in [
            None,
            Some(String::new()),
            Some("a,b".into()),
            Some("say \"hi\"".into()),
            Some("plain".into()),
            Some("l1\nl2".into()),
        ] {
            encode(v, &mut out);
            out.push(b'|');
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "|\"\"|\"a,b\"|\"say \"\"hi\"\"\"|plain|\"l1\nl2\"|"
        );
        // Every delimiter the reader may sniff, and edge spaces, force quotes.
        let mut out = Vec::new();
        for v in ["a;b", "a|b", "a\tb", " a", "a ", "a b"] {
            encode(Some(v.into()), &mut out);
            out.push(b'|');
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\"a;b\"|\"a|b\"|\"a\tb\"|\" a\"|\"a \"|a b|"
        );
    }

    #[test]
    fn output_names_are_plain_file_names() {
        for ok in ["a.csv", "out.parquet", "x", &"x".repeat(255)] {
            assert!(valid_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".hidden.csv",
            "../x.csv",
            "a/b.csv",
            "a\\b.csv",
            "x\0.csv",
            &"x".repeat(256),
        ] {
            assert!(valid_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn publishing_without_overwrite_never_replaces_a_file_that_appeared() {
        let d = crate::testkit::Dir::new();
        let (tmp, target) = (d.path().join(".t.part"), d.path().join("out.csv"));
        std::fs::write(&tmp, "new").unwrap();
        std::fs::write(&target, "theirs").unwrap();
        let e = publish(&tmp, &target, false).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "theirs");
        publish(&tmp, &target, true).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert!(!tmp.exists());
    }

    #[test]
    fn temporary_files_are_never_shared() {
        let d = crate::testkit::Dir::new();
        let (a, _) = temp_file(d.path()).unwrap();
        let (b, _) = temp_file(d.path()).unwrap();
        assert_ne!(a, b);
        std::fs::write(&a, "a leftover").unwrap();
        let (c, _) = temp_file(d.path()).unwrap();
        assert!(c != a && c != b);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a leftover");
    }

    #[test]
    fn header_lines_quote_what_needs_it() {
        assert_eq!(
            header_line(&["a".into(), "b,c".into(), String::new()]),
            b"a,\"b,c\",\"\"\n"
        );
    }
}
