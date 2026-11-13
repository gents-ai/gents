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
use crate::render::{cell_json, cut_at, markdown_capped, type_name};
use crate::table::{Fnv, ScanError};

/// Bytes the SQL engine's operators may hold: well under the plugin's memory limit, which
/// also covers the decoded rows in flight, the plan and the wasm runtime itself.
pub const POOL_BYTES: usize = 1024 * 1024 * 1024;
/// A value (a text, or the JSON of a list or record) longer than this is cut in a result.
pub const MAX_CELL_BYTES: usize = 64 * 1024;
/// A row longer than this has its long texts shortened.
const MAX_ROW_BYTES: usize = 1024 * 1024;
/// The most bytes the whole result of a call may take: the host's output limit is 4 MiB and
/// the rest is the Markdown table, the column list and the warnings.
pub const OUTPUT_BUDGET: usize = 3_500_000;
/// The most bytes the Markdown table of a page may take.
const MARKDOWN_BYTES: usize = 256 * 1024;

/// The bytes a row takes in the result, with its separator.
fn row_len(row: &[Value]) -> usize {
    serde_json::to_string(row).map_or(0, |s| s.len() + 1)
}

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
    // Types read in full differ from sampled ones, so the two are different results.
    for name in catalog.full_names() {
        h.text(&name);
    }
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
    let (mut cut, mut non_finite, mut big_ints, mut shortened) = (0u64, 0u64, 0u64, 0u64);
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
            let mut row: Vec<Value> = Vec::with_capacity(columns.len());
            for (c, (_, ty)) in batch.columns().iter().zip(&columns) {
                let (v, was_cut) = cell_json(c, i, MAX_CELL_BYTES);
                match &v {
                    _ if was_cut => cut += 1,
                    Value::String(s)
                        if ty.starts_with("float")
                            && matches!(s.as_str(), "NaN" | "Infinity" | "-Infinity") =>
                    {
                        non_finite += 1
                    }
                    Value::String(_) if ty == "int64" || ty == "uint64" => big_ints += 1,
                    _ => {}
                }
                row.push(v);
            }
            let mut len = row_len(&row);
            if len > MAX_ROW_BYTES {
                // Cells each under the cell cap can still add up past the page: shorten the
                // long texts evenly so one row always fits.
                let each = (MAX_ROW_BYTES / columns.len().max(1)).max(256);
                for v in &mut row {
                    if let Value::String(s) = v
                        && s.len() > each
                    {
                        s.truncate(cut_at(s, each).len());
                        shortened += 1;
                    }
                }
                len = row_len(&row);
            }
            bytes += len;
            rows.push(row);
        }
    }
    let mut notes = Vec::new();
    if cut > 0 {
        notes.push(format!(
            "{cut} values were cut at {} KiB; select a part of them (SUBSTR, a list slice or one field) to read the rest",
            MAX_CELL_BYTES / 1024
        ));
    }
    if shortened > 0 {
        notes.push(format!(
            "{shortened} long text values were shortened so that a row fits in {} KiB; select fewer columns to read them whole",
            MAX_ROW_BYTES / 1024
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
/// table's types turn out to be wrong for rows past its sample. With `settle`, a page that has
/// a next page first reads the sampled tables in full to settle their types, so that every page
/// of the chain has the same column types; a later page of that chain does the same on seeing
/// the first page's cursor.
pub async fn run_page(
    catalog: &Arc<Catalog>,
    kind: &str,
    sql: &str,
    cursor_in: Option<&str>,
    settle: bool,
    max_rows: usize,
    max_bytes: usize,
) -> Res<(Page, u64)> {
    loop {
        let engine = Engine::new(Arc::clone(catalog), POOL_BYTES)?;
        let prepared = engine.prepare(sql).await?;
        let fp = fingerprint(kind, sql, catalog, prepared.order);
        let offset = match cursor_in {
            Some(c) => {
                let (cursor_fp, offset) = cursor::parts(c)?;
                if cursor_fp != fp {
                    if settle && catalog.settle() {
                        continue;
                    }
                    return Err(cursor::mismatch());
                }
                offset
            }
            None => 0,
        };
        match read_page(&engine, prepared, offset, max_rows, max_bytes).await {
            Ok(page) => {
                if settle && page.more && catalog.settle() {
                    continue;
                }
                return Ok((page, fp));
            }
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
        true,
        input.max_rows()?,
        input.max_bytes()?,
    )
    .await?;
    fit(page, fp, input.markdown_rows()?, catalog)
}

/// The result object of `page`, shortened (and said so) until the whole of it, rows, Markdown,
/// columns and warnings, fits in [`OUTPUT_BUDGET`].
fn fit(mut page: Page, fp: u64, markdown_rows: usize, catalog: &Catalog) -> Res<Value> {
    let mut trimmed = false;
    loop {
        let out = page_json(&page, fp, markdown_rows, catalog);
        let size = serde_json::to_string(&out).map_or(usize::MAX, |s| s.len());
        if size <= OUTPUT_BUDGET {
            return Ok(out);
        }
        if page.rows.len() <= 1 {
            return Err(
                "the result is too large to return; select fewer columns, or cut long values with SUBSTR".into(),
            );
        }
        // Keep the share of the rows that fits, a tenth less to settle in one or two rounds.
        let keep = (page.rows.len() / 10 * 9)
            .min(page.rows.len() * OUTPUT_BUDGET / size)
            .clamp(1, page.rows.len() - 1);
        page.rows.truncate(keep);
        page.more = true;
        if trimmed {
            page.notes.pop();
        }
        trimmed = true;
        page.notes.push(format!(
            "the page was cut to {keep} rows to fit the output limit; read on with next.cursor"
        ));
    }
}

/// The result object of a page.
pub fn page_json(page: &Page, fp: u64, markdown_rows: usize, catalog: &Catalog) -> Value {
    let columns: Vec<Value> = page
        .columns
        .iter()
        .map(|(n, t)| json!({"name": n, "type": t}))
        .collect();
    let (mut md, shown) = markdown_capped(&page.columns, &page.rows, markdown_rows, MARKDOWN_BYTES);
    if shown < page.rows.len() {
        md.push_str(&format!(
            "\n\n(the first {shown} of {} rows of this page; all are in rows)",
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
