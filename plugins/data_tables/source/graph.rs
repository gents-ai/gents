//! Graph mode: the plugin as the `data-run` node of a graph. A graph node's
//! request carries the `run_id` of its run, which no other caller sends, so
//! its presence selects this mode. A `DataJob` document is run and answered
//! with records the next node reads: one `DataResult`, a `DataRow` per result
//! row (query mode) and a `DataTable` per table (tables and describe modes).
//! The fields are the ordinary input's, so the two modes cannot drift apart;
//! the records are documented in TOOL.md. A failure is a `DataResult` with an
//! `error`, so the run goes on and the next node can read why.
use serde_json::{Map, Value, json};

/// Rows one node call returns; a result past this carries a cursor.
const GRAPH_MAX_ROWS: u64 = 5000;
/// Bytes of row data one node call returns.
const GRAPH_MAX_BYTES: u64 = 1_500_000;

/// Runs a graph node's request, or returns `None` when `raw` is not one.
pub fn run(raw: &str) -> Option<crate::Res<String>> {
    if !raw.contains("\"run_id\"") {
        return None;
    }
    let Ok(Value::Object(mut fields)) = serde_json::from_str(raw) else {
        return None;
    };
    fields.remove("run_id")?;
    // A document's unset fields come back as null, an empty string or an empty list.
    fields.retain(|_, v| {
        !(v.is_null() || v.as_str() == Some("") || v.as_array().is_some_and(Vec::is_empty))
    });
    Some(Ok(answer(fields).to_string()))
}

/// The inline tables of a job, which a document holds as JSON text.
fn unwrap_tables(fields: &mut Map<String, Value>) -> Result<(), String> {
    if let Some(Value::String(text)) = fields.get("tables") {
        let parsed: Value =
            serde_json::from_str(text).map_err(|e| format!("tables is not valid JSON: {e}"))?;
        fields.insert("tables".into(), parsed);
    }
    Ok(())
}

fn answer(mut fields: Map<String, Value>) -> Value {
    let mode = fields
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or(if fields.contains_key("sql") {
            "query"
        } else {
            "tables"
        })
        .to_string();
    let capped = |fields: &mut Map<String, Value>, key: &str, cap: u64| {
        let v = fields
            .get(key)
            .and_then(Value::as_u64)
            .map_or(cap, |n| n.min(cap));
        fields.insert(key.into(), json!(v));
    };
    capped(&mut fields, "max_rows", GRAPH_MAX_ROWS);
    capped(&mut fields, "max_bytes", GRAPH_MAX_BYTES);
    let outcome = unwrap_tables(&mut fields)
        .and_then(|()| {
            serde_json::from_value::<crate::input::Input>(Value::Object(fields))
                .map_err(|e| format!("invalid input: {e}"))
        })
        .and_then(|input| crate::call(&input));
    match outcome {
        Err(e) => json!({"result": {"mode": mode, "complete": false, "error": e, "warnings": []}}),
        Ok(out) => records(&mode, &out),
    }
}

fn text(v: &Value) -> Value {
    json!(v.to_string())
}

fn records(mode: &str, out: &Value) -> Value {
    let next = out["next"]["cursor"].as_str();
    let mut result = json!({
        "mode": mode,
        "complete": next.is_none(),
        "markdown": out.get("markdown").cloned().unwrap_or(json!("")),
        "warnings": out.get("warnings").cloned().unwrap_or(json!([])),
    });
    if let Some(c) = next {
        result["cursor"] = json!(c);
    }
    match mode {
        "query" => {
            let offset = out["offset"].as_u64().unwrap_or(0);
            result["columns"] = text(&out["columns"]);
            result["row_count"] = out["row_count"].clone();
            result["offset"] = json!(offset);
            result["order"] = out["order"].clone();
            let rows: Vec<Value> = out["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, r)| json!({"row": offset + i as u64, "values": text(r)}))
                .collect();
            json!({"result": result, "rows": rows})
        }
        "export" => {
            result["rows_written"] = out["rows"].clone();
            result["file"] = out["written"].clone();
            json!({"result": result})
        }
        _ => {
            let tables: Vec<Value> = out["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| {
                    let mut rec = json!({
                        "table": t["name"],
                        "source": t["source"],
                        "format": t["format"],
                        "columns": text(&t["columns"]),
                    });
                    if let Some(s) = t["sheet"].as_str() {
                        rec["sheet"] = json!(s);
                    }
                    if let Some(n) = t["row_count"].as_u64() {
                        rec["row_count"] = json!(n);
                    }
                    if let Some(e) = t["error"].as_str() {
                        rec["error"] = json!(e);
                    }
                    if mode == "describe" {
                        rec["profile"] = text(t);
                    }
                    rec
                })
                .collect();
            json!({"result": result, "tables": tables})
        }
    }
}
