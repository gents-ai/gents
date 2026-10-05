//! The `query` mode: run a SELECT and return one page of it.
//!
//! A page is rows `offset..offset+n` of the result in its documented order
//! (see the engine module). The result is streamed: rows before the offset are
//! counted and dropped, rows after the page are never built, and the page
//! itself is bounded by rows and by bytes. A cursor repeats nothing and skips
//! nothing because the query is run again, in the same order, from the top.
use std::sync::Arc;

use datafusion::common::DataFusionError;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::Res;
use crate::catalog::Catalog;
use crate::cursor;
use crate::engine::{Engine, Order, Prepared, explain, plain, scan_error};
use crate::input::Input;
use crate::render::{markdown, type_name, value_json};
use crate::table::{Fnv, ScanError};

/// Bytes the SQL engine's operators may hold: well under the plugin's memory limit, which
/// also covers the decoded rows in flight, the plan and the wasm runtime itself.
pub const POOL_BYTES: usize = 1024 * 1024 * 1024;
/// A text value longer than this is cut in a result.
pub const MAX_CELL_BYTES: usize = 64 * 1024;

/// One page of a result.
pub struct Page {
    /// Column names and type names.
    pub columns: Vec<(String, String)>,
    /// The rows of the page.
    pub rows: Vec<Vec<Value>>,
    /// Whether rows remain after the page.
    pub more: bool,
    /// Rows that came before the page.
    pub offset: u64,
    /// How the rows are ordered.
    pub order: Order,
    /// Facts about the values in the page, for the warnings.
    pub notes: Vec<String>,
}

fn order_name(o: Order) -> &'static str {
    match o {
        Order::Query => "query",
        Order::File => "file",
        Order::Columns => "columns",
        Order::None => "none",
    }
}

/// A fingerprint of the request and of the data it read, for cursors.
pub fn fingerprint(kind: &str, sql: &str, catalog: &Catalog, order: Order) -> u64 {
    let mut h = Fnv::default();
    h.text(kind);
    h.text(sql);
    h.text(order_name(order));
    let o = catalog.options();
    h.num(u64::from(o.delimiter.unwrap_or(0)));
    h.num(match o.header {
        None => 2,
        Some(b) => u64::from(b),
    });
    h.num(catalog.opened_fingerprint());
    h.0
}

async fn read_page(
    engine: &Engine,
    prepared: Prepared,
    offset: u64,
    max_rows: usize,
    max_bytes: usize,
) -> Result<Page, DataFusionError> {
    let order = prepared.order;
    let mut stream = engine.stream(prepared).await?;
    let schema = stream.schema();
    let columns: Vec<(String, String)> = schema
        .fields()
        .iter()
        .map(|f| (f.name().clone(), type_name(f.data_type())))
        .collect();
    let (mut skip, mut bytes) = (offset, 0usize);
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let (mut cut, mut non_finite, mut big_ints) = (0u64, 0u64, 0u64);
    let mut more = false;
    'outer: while let Some(batch) = stream.next().await {
        let batch = plain(&batch?)?;
        let n = batch.num_rows();
        let mut start = 0;
        if skip > 0 {
            let s = skip.min(n as u64) as usize;
            skip -= s as u64;
            start = s;
        }
        for i in start..n {
            if rows.len() == max_rows || (!rows.is_empty() && bytes >= max_bytes) {
                more = true;
                break 'outer;
            }
            let mut row: Vec<Value> = batch.columns().iter().map(|c| value_json(c, i)).collect();
            for (v, (_, ty)) in row.iter_mut().zip(&columns) {
                match v {
                    Value::String(s) if s.len() > MAX_CELL_BYTES => {
                        let mut at = MAX_CELL_BYTES;
                        while !s.is_char_boundary(at) {
                            at -= 1;
                        }
                        s.truncate(at);
                        cut += 1;
                    }
                    Value::String(s)
                        if ty.starts_with("float")
                            && matches!(s.as_str(), "NaN" | "Infinity" | "-Infinity") =>
                    {
                        non_finite += 1
                    }
                    Value::String(_) if ty == "int64" || ty == "uint64" => big_ints += 1,
                    _ => {}
                }
            }
            bytes += serde_json::to_string(&row).map_or(0, |s| s.len() + 1);
            rows.push(row);
        }
    }
    let mut notes = Vec::new();
    if cut > 0 {
        notes.push(format!(
            "{cut} text values were cut at {} KiB; select SUBSTR ranges to read the rest",
            MAX_CELL_BYTES / 1024
        ));
    }
    if non_finite > 0 {
        notes.push(format!("{non_finite} float values are NaN or infinite and appear as the strings \"NaN\", \"Infinity\" and \"-Infinity\""));
    }
    if big_ints > 0 {
        notes.push(format!(
            "{big_ints} integers beyond 2^53 appear as strings so no digit is lost"
        ));
    }
    Ok(Page {
        columns,
        rows,
        more,
        offset,
        order,
        notes,
    })
}

/// Plans and reads one page of `sql`, planning again with wider type inference when a
/// table's types turn out to be wrong for rows past its sample.
pub async fn run_page(
    catalog: &Arc<Catalog>,
    kind: &str,
    sql: &str,
    cursor_in: Option<&str>,
    max_rows: usize,
    max_bytes: usize,
) -> Res<(Page, u64)> {
    loop {
        let engine = Engine::new(Arc::clone(catalog), POOL_BYTES)?;
        let prepared = engine.prepare(sql).await?;
        let fp = fingerprint(kind, sql, catalog, prepared.order);
        let offset = match cursor_in {
            Some(c) => cursor::decode(c, fp)?,
            None => 0,
        };
        match read_page(&engine, prepared, offset, max_rows, max_bytes).await {
            Ok(page) => return Ok((page, fp)),
            Err(e) => match scan_error(&e) {
                Some(ScanError::Conflict {
                    table,
                    column,
                    line,
                }) => {
                    if !catalog.widen(table) {
                        return Err(format!(
                            "column {column} of {table} holds values of mixed types around line {line} that cannot be read as one type"
                        ));
                    }
                }
                _ => return Err(explain(&e)),
            },
        }
    }
}

/// The `query` mode.
pub async fn query(input: &Input, catalog: &Arc<Catalog>) -> Res<Value> {
    let sql = input.sql()?;
    let (page, fp) = run_page(
        catalog,
        "query",
        sql,
        input.cursor.as_deref(),
        input.max_rows()?,
        input.max_bytes()?,
    )
    .await?;
    Ok(page_json(&page, fp, input.markdown_rows()?, catalog))
}

/// The result object of a page.
pub fn page_json(page: &Page, fp: u64, markdown_rows: usize, catalog: &Catalog) -> Value {
    let columns: Vec<Value> = page
        .columns
        .iter()
        .map(|(n, t)| json!({"name": n, "type": t}))
        .collect();
    let (mut md, cut) = markdown(&page.columns, &page.rows, markdown_rows);
    if cut {
        md.push_str(&format!(
            "\n\n(the first {markdown_rows} of {} rows of this page; all are in rows)",
            page.rows.len()
        ));
    }
    let mut warnings = catalog.warn.list();
    warnings.extend(page.notes.iter().cloned());
    if page.order == Order::None {
        warnings.push(
            "the result has no sortable column, so its row order is not guaranteed between calls"
                .into(),
        );
    }
    let mut out = json!({
        "columns": columns,
        "rows": page.rows,
        "row_count": page.rows.len(),
        "offset": page.offset,
        "order": order_name(page.order),
        "markdown": md,
        "warnings": warnings,
    });
    if page.more {
        out["next"] = json!({"cursor": cursor::encode(fp, page.offset + page.rows.len() as u64)});
    }
    out
}
