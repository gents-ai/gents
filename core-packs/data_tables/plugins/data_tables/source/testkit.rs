//! Helpers shared by the unit tests: scratch folders and one-call runners.
#![cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

#[path = "../tools/fixtures.rs"]
pub mod fixtures;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch folder that is removed when it goes out of scope.
pub struct Dir(PathBuf);

impl Dir {
    /// A new empty folder.
    pub fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "dt-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    /// The folder's path.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The folder's path as text.
    pub fn s(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }

    /// Writes `bytes` to `rel`, making folders on the way; returns the full path.
    pub fn put(&self, rel: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs one request through the whole plugin and returns its JSON result.
pub fn run(input: Value) -> Result<Value, String> {
    crate::run(&input.to_string()).map(|s| serde_json::from_str(&s).unwrap())
}

/// A query over `dir`.
pub fn query(dir: &Dir, sql: &str) -> Result<Value, String> {
    run(serde_json::json!({"path": dir.s(), "sql": sql}))
}

/// The rows of a result.
pub fn rows(v: &Value) -> Vec<Value> {
    v["rows"].as_array().cloned().unwrap_or_default()
}

use crate::render::{type_name, value_json};
use crate::table::{ScanError, TableSource};

/// Every row of a table (or of some of its columns) as JSON values.
pub fn collect(
    t: &dyn TableSource,
    projection: Option<&[usize]>,
) -> Result<Vec<Vec<Value>>, ScanError> {
    let mut out = Vec::new();
    for b in t.scan(projection) {
        let b = crate::engine::plain(&b?).map_err(|e| ScanError::Failed(e.to_string()))?;
        for i in 0..b.num_rows() {
            out.push(b.columns().iter().map(|c| value_json(c, i)).collect());
        }
    }
    Ok(out)
}

/// A table's column names and type names.
pub fn columns(t: &dyn TableSource) -> Vec<(String, String)> {
    t.schema()
        .fields()
        .iter()
        .map(|f| (f.name().clone(), type_name(f.data_type())))
        .collect()
}

/// `(name, type)` pairs written compactly.
pub fn cols(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// The rows of [`fixtures::sample_batch`] as this plugin reports them.
pub fn sample_rows() -> Vec<Vec<Value>> {
    use serde_json::json;
    vec![
        vec![
            json!(1),
            json!("Ana"),
            json!(1.5),
            json!(true),
            json!("2024-02-29"),
            json!("2024-02-29T12:30:45.500"),
            json!("123.45"),
            json!("18446744073709551615"),
            json!({"x": 1, "y": "a"}),
            json!([1, 2]),
        ],
        vec![
            json!(2),
            json!(""),
            Value::Null,
            json!(false),
            Value::Null,
            Value::Null,
            Value::Null,
            json!(1),
            json!({"x": null, "y": "b"}),
            json!([]),
        ],
        vec![
            json!(3),
            Value::Null,
            json!("NaN"),
            Value::Null,
            json!("1970-01-01"),
            json!("1970-01-01T00:00:00"),
            json!("-0.05"),
            Value::Null,
            json!({"x": 3, "y": null}),
            Value::Null,
        ],
    ]
}

/// The columns of [`fixtures::sample_batch`] as this plugin reports them.
pub fn sample_columns() -> Vec<(String, String)> {
    cols(&[
        ("id", "int64"),
        ("name", "text"),
        ("score", "float64"),
        ("active", "bool"),
        ("born", "date"),
        ("seen", "timestamp"),
        ("price", "decimal(10,2)"),
        ("big", "uint64"),
        ("nested", "struct"),
        ("tags", "list<int32>"),
    ])
}
