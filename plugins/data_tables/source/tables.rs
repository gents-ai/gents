//! The `tables` and `describe` modes: what the data holds, without a query.
//!
//! `tables` lists each table with its columns and, when it is known without a
//! scan, its row count (Parquet and inline tables always; text and sheets when
//! the whole file fit the type-inference sample; otherwise `null`, never a
//! guess). `describe` adds per-column statistics over the first `sample_rows`
//! rows and a few sample rows, and says when it only saw a sample.
use std::sync::Arc;

use arrow::datatypes::DataType;
use serde_json::{Value, json};

use crate::Res;
use crate::catalog::{Catalog, Spec};
use crate::cursor;
use crate::input::{Input, MAX_DESCRIBE_COLUMNS};
use crate::query::run_page;
use crate::render::{markdown, type_name};
use crate::table::{Fnv, TableSource};

const LIST_PAGE: usize = 50;
const DESCRIBE_PAGE: usize = 10;
const MAX_COLUMNS_LISTED: usize = 1000;
const SAMPLE_ROWS_SHOWN: usize = 5;

fn selected<'a>(input: &Input, catalog: &'a Catalog) -> Res<Vec<&'a Spec>> {
    match input.table.as_deref() {
        None => Ok(catalog.specs().iter().collect()),
        Some(name) => match catalog.spec(name) {
            Some(s) => Ok(vec![s]),
            None => {
                let names: Vec<&str> = catalog
                    .specs()
                    .iter()
                    .take(20)
                    .map(|s| s.name.as_str())
                    .collect();
                Err(format!(
                    "there is no table named {name}; the tables are {}",
                    names.join(", ")
                ))
            }
        },
    }
}

fn names_fingerprint(kind: &str, specs: &[&Spec], extra: &[&str]) -> u64 {
    let mut h = Fnv::default();
    h.text(kind);
    for s in specs {
        h.text(&s.name);
        h.text(&s.source);
        h.text(s.sheet.as_deref().unwrap_or(""));
    }
    for e in extra {
        h.text(e);
    }
    h.0
}

fn page_of<'a>(
    input: &Input,
    specs: &'a [&'a Spec],
    fp: u64,
    size: usize,
) -> Res<(&'a [&'a Spec], u64)> {
    let offset = match &input.cursor {
        Some(c) => cursor::decode(c, fp)?,
        None => 0,
    };
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(specs.len());
    let end = (start + size.min(input.max_rows()?)).min(specs.len());
    Ok((&specs[start..end], end as u64))
}

fn column_list(t: &dyn TableSource) -> (Vec<Value>, usize) {
    let schema = t.schema();
    let all = schema.fields().len();
    let cols = schema
        .fields()
        .iter()
        .take(MAX_COLUMNS_LISTED)
        .map(|f| json!({"name": f.name(), "type": type_name(f.data_type())}))
        .collect();
    (cols, all)
}

fn header(spec: &Spec) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("name".into(), json!(spec.name));
    m.insert("source".into(), json!(spec.source));
    m.insert("format".into(), json!(spec.format));
    if let Some(s) = &spec.sheet {
        m.insert("sheet".into(), json!(s));
    }
    m
}

/// The `tables` mode.
pub fn list(input: &Input, catalog: &Arc<Catalog>) -> Res<Value> {
    let specs = selected(input, catalog)?;
    let fp = names_fingerprint("tables", &specs, &[]);
    let (page, end) = page_of(input, &specs, fp, LIST_PAGE)?;
    let mut tables = Vec::new();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for spec in page {
        let mut entry = header(spec);
        let mut line = vec![
            json!(spec.name),
            json!(match &spec.sheet {
                Some(s) => format!("{}#{s}", spec.source),
                None => spec.source.clone(),
            }),
        ];
        match catalog.open(&spec.name) {
            Ok(t) => {
                let (cols, all) = column_list(t.as_ref());
                entry.insert(
                    "row_count".into(),
                    t.row_count().map_or(Value::Null, |n| json!(n)),
                );
                entry.insert("column_count".into(), json!(all));
                if all > cols.len() {
                    entry.insert("columns_listed".into(), json!(cols.len()));
                    catalog.warn.once(
                        &format!("cols-{}", spec.name),
                        format!(
                            "table {} lists only its first {MAX_COLUMNS_LISTED} of {all} columns",
                            spec.name
                        ),
                    );
                }
                if let Some(d) = t.details() {
                    entry.insert("details".into(), d);
                }
                line.push(t.row_count().map_or(json!("unknown"), |n| json!(n)));
                let names: Vec<String> = cols
                    .iter()
                    .take(8)
                    .filter_map(|c| c["name"].as_str().map(str::to_string))
                    .collect();
                line.push(json!(format!(
                    "{}{}",
                    names.join(", "),
                    if all > 8 { ", ..." } else { "" }
                )));
                entry.insert("columns".into(), Value::Array(cols));
            }
            Err(e) => {
                entry.insert("error".into(), json!(e));
                line.push(json!("unknown"));
                line.push(json!(format!("error: {e}")));
            }
        }
        rows.push(line);
        tables.push(Value::Object(entry));
    }
    let cols: Vec<(String, String)> = ["table", "source", "rows", "columns"]
        .iter()
        .map(|n| ((*n).to_string(), "text".to_string()))
        .collect();
    let (md, _) = markdown(&cols, &rows, usize::MAX);
    let mut out = json!({"tables": tables, "markdown": md, "warnings": catalog.warn.list()});
    if (end as usize) < specs.len() {
        out["next"] = json!({"cursor": cursor::encode(fp, end)});
    }
    Ok(out)
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn numeric(t: &DataType) -> bool {
    use DataType::*;
    matches!(
        t,
        Int8 | Int16
            | Int32
            | Int64
            | UInt8
            | UInt16
            | UInt32
            | UInt64
            | Float32
            | Float64
            | Decimal128(..)
            | Decimal256(..)
    )
}

fn ordered(t: &DataType) -> bool {
    use DataType::*;
    numeric(t)
        || matches!(
            t,
            Utf8 | LargeUtf8 | Utf8View | Date32 | Date64 | Time32(_) | Time64(_) | Timestamp(..)
        )
}

fn countable(t: &DataType) -> bool {
    ordered(t) || matches!(t, DataType::Boolean)
}

/// The `describe` mode.
pub async fn describe(input: &Input, catalog: &Arc<Catalog>) -> Res<Value> {
    let specs = selected(input, catalog)?;
    let sample = input.sample_rows()?;
    let extra = [
        format!("{sample}"),
        input
            .columns
            .as_ref()
            .map(|c| c.join(","))
            .unwrap_or_default(),
    ];
    let fp = names_fingerprint("describe", &specs, &[&extra[0], &extra[1]]);
    let (page, end) = page_of(input, &specs, fp, DESCRIBE_PAGE)?;
    let (mut tables, mut markdown_parts) = (Vec::new(), Vec::new());
    for spec in page {
        let mut entry = header(spec);
        match describe_one(spec, input, catalog, sample).await {
            Ok((fields, md)) => {
                entry.extend(fields);
                markdown_parts.push(md);
            }
            Err(e) => {
                entry.insert("error".into(), json!(e));
                markdown_parts.push(format!("### {}\n\nerror: {e}", spec.name));
            }
        }
        tables.push(Value::Object(entry));
    }
    let mut out = json!({"tables": tables, "markdown": markdown_parts.join("\n\n"), "warnings": catalog.warn.list()});
    if (end as usize) < specs.len() {
        out["next"] = json!({"cursor": cursor::encode(fp, end)});
    }
    Ok(out)
}

async fn describe_one(
    spec: &Spec,
    input: &Input,
    catalog: &Arc<Catalog>,
    sample: u64,
) -> Res<(serde_json::Map<String, Value>, String)> {
    let table = catalog.open(&spec.name)?;
    let schema = table.schema();
    let chosen: Vec<usize> = match &input.columns {
        Some(names) => names
            .iter()
            .map(|n| {
                schema
                    .index_of(n)
                    .map_err(|_| format!("table {} has no column {n}", spec.name))
            })
            .collect::<Res<_>>()?,
        None => (0..schema.fields().len().min(MAX_DESCRIBE_COLUMNS)).collect(),
    };
    if chosen.len() > MAX_DESCRIBE_COLUMNS {
        return Err(format!(
            "describe reports at most {MAX_DESCRIBE_COLUMNS} columns a call; name fewer in columns"
        ));
    }
    let omitted = schema.fields().len().saturating_sub(chosen.len());
    if omitted > 0 && input.columns.is_none() {
        catalog.warn.once(&format!("describe-cols-{}", spec.name), format!("table {} is described by its first {MAX_DESCRIBE_COLUMNS} of {} columns; name others in columns", spec.name, schema.fields().len()));
    }
    let mut select = vec!["count(*) AS rows_scanned".to_string()];
    for (k, &c) in chosen.iter().enumerate() {
        let (q, t) = (quote(schema.field(c).name()), schema.field(c).data_type());
        select.push(format!("count({q}) AS nn{k}"));
        if countable(t) {
            select.push(format!("count(DISTINCT {q}) AS dc{k}"));
        }
        if ordered(t) {
            select.push(format!("min({q}) AS mn{k}"));
            select.push(format!("max({q}) AS mx{k}"));
        }
        if numeric(t) {
            select.push(format!("avg({q}) AS av{k}"));
        }
    }
    let tname = quote(&spec.name);
    let stats_sql = format!(
        "SELECT {} FROM (SELECT * FROM {tname} LIMIT {sample})",
        select.join(", ")
    );
    let (stats, _) = run_page(catalog, "describe", &stats_sql, None, 1, 1_000_000).await?;
    let row = stats.rows.first().ok_or("the table could not be read")?;
    let at = |name: &str| {
        stats
            .columns
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| row[i].clone())
    };
    let scanned = at("rows_scanned").and_then(|v| v.as_u64()).unwrap_or(0);
    let total = table.row_count();
    let sampled = total.map_or(scanned >= sample, |n| n > scanned);
    let mut columns = Vec::new();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for (k, &c) in chosen.iter().enumerate() {
        let f = schema.field(c);
        let non_null = at(&format!("nn{k}")).and_then(|v| v.as_u64()).unwrap_or(0);
        let mut col = json!({"name": f.name(), "type": type_name(f.data_type()), "non_null": non_null, "nulls": scanned.saturating_sub(non_null)});
        for (key, alias) in [
            ("distinct", "dc"),
            ("min", "mn"),
            ("max", "mx"),
            ("mean", "av"),
        ] {
            if let Some(v) = at(&format!("{alias}{k}")) {
                col[key] = v;
            }
        }
        let show = |key: &str| col.get(key).map_or_else(|| json!(""), Clone::clone);
        rows.push(vec![
            col["name"].clone(),
            col["type"].clone(),
            json!(non_null),
            show("distinct"),
            show("min"),
            show("max"),
            show("mean"),
        ]);
        columns.push(col);
    }
    let names: Vec<String> = chosen
        .iter()
        .map(|&c| quote(schema.field(c).name()))
        .collect();
    let sample_sql = format!(
        "SELECT {} FROM {tname} LIMIT {SAMPLE_ROWS_SHOWN}",
        names.join(", ")
    );
    let (sample_page, _) = run_page(
        catalog,
        "describe",
        &sample_sql,
        None,
        SAMPLE_ROWS_SHOWN,
        1_000_000,
    )
    .await?;
    let mut fields = serde_json::Map::new();
    fields.insert("row_count".into(), total.map_or(Value::Null, |n| json!(n)));
    fields.insert("rows_scanned".into(), json!(scanned));
    fields.insert("sampled".into(), json!(sampled));
    fields.insert("columns".into(), Value::Array(columns));
    if omitted > 0 {
        fields.insert("columns_omitted".into(), json!(omitted));
    }
    fields.insert(
        "sample".into(),
        json!({"columns": sample_page.columns.iter().map(|(n, _)| n).collect::<Vec<_>>(), "rows": sample_page.rows}),
    );
    let cols: Vec<(String, String)> = [
        "column", "type", "non_null", "distinct", "min", "max", "mean",
    ]
    .iter()
    .map(|n| ((*n).to_string(), "text".to_string()))
    .collect();
    let (table_md, _) = markdown(&cols, &rows, usize::MAX);
    let size = total.map_or_else(|| "rows not counted".to_string(), |n| format!("{n} rows"));
    let scope = if sampled {
        format!(", statistics over the first {scanned} rows")
    } else {
        String::new()
    };
    Ok((
        fields,
        format!("### {} ({size}{scope})\n\n{table_md}", spec.name),
    ))
}
