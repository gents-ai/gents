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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Dir;

    fn node(v: Value) -> Value {
        serde_json::from_str(&run(&v.to_string()).expect("a graph request").expect("a graph answer")).unwrap()
    }

    fn folder() -> Dir {
        let d = Dir::new();
        d.put("t.csv", "g,n\nx,1\nx,2\ny,4\n");
        d
    }

    #[test]
    fn only_a_request_with_a_run_id_is_a_graph_request() {
        assert!(run("{\"sql\":\"select 1\"}").is_none());
        assert!(run("not json run_id").is_none());
        assert!(run("[\"run_id\"]").is_none());
        assert!(run("{\"run_id\":\"r\"}").is_some());
        assert!(run("{\"note\":\"run_id\"}").is_none());
    }

    #[test]
    fn a_query_job_answers_with_one_result_and_a_record_per_row() {
        let d = folder();
        let out = node(json!({"run_id": "r1", "path": d.s(), "sql": "SELECT g, sum(n) AS s FROM t GROUP BY g", "files": [], "cursor": "", "table": null}));
        assert_eq!(out["result"]["mode"], "query");
        assert_eq!(out["result"]["complete"], true);
        assert_eq!(out["result"]["row_count"], 2);
        assert_eq!(out["result"]["offset"], 0);
        assert_eq!(out["result"]["order"], "columns");
        assert_eq!(out["result"]["columns"], "[{\"name\":\"g\",\"type\":\"text\"},{\"name\":\"s\",\"type\":\"int64\"}]");
        assert_eq!(out["result"]["warnings"], json!([]));
        assert!(out["result"].get("cursor").is_none() && out["result"].get("error").is_none());
        assert!(out["result"]["markdown"].as_str().unwrap().starts_with("| g | s |"));
        assert_eq!(out["rows"], json!([{"row": 0, "values": "[\"x\",3]"}, {"row": 1, "values": "[\"y\",4]"}]));
        assert!(out.get("tables").is_none());
    }

    #[test]
    fn a_long_result_carries_a_cursor_the_next_job_continues_from() {
        let d = folder();
        let first = node(json!({"run_id": "r1", "path": d.s(), "sql": "SELECT n FROM t", "max_rows": 2}));
        assert_eq!(first["result"]["complete"], false);
        let cursor = first["result"]["cursor"].as_str().unwrap().to_string();
        assert_eq!(first["rows"].as_array().unwrap().len(), 2);
        let second = node(json!({"run_id": "r1", "path": d.s(), "sql": "SELECT n FROM t", "max_rows": 2, "cursor": cursor}));
        assert_eq!(second["result"]["complete"], true);
        assert_eq!(second["result"]["offset"], 2);
        assert_eq!(second["rows"], json!([{"row": 2, "values": "[4]"}]));
    }

    #[test]
    fn a_job_may_ask_for_more_rows_than_one_node_answers_with_and_is_capped() {
        let d = Dir::new();
        let mut text = String::from("n\n");
        for i in 0..6000 {
            text.push_str(&format!("{i}\n"));
        }
        d.put("t.csv", text);
        let out = node(json!({"run_id": "r", "path": d.s(), "sql": "SELECT n FROM t", "max_rows": 100_000}));
        assert_eq!(out["rows"].as_array().unwrap().len(), 5000);
        assert_eq!(out["result"]["complete"], false);
    }

    #[test]
    fn a_tables_job_answers_a_record_per_table() {
        let d = folder();
        d.put("u.json", "[{\"a\":1}]");
        let out = node(json!({"run_id": "r", "path": d.s()}));
        assert_eq!(out["result"]["mode"], "tables");
        let tables = out["tables"].as_array().unwrap();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0], json!({"table": "t", "source": "t.csv", "format": "csv", "row_count": 3, "columns": "[{\"name\":\"g\",\"type\":\"text\"},{\"name\":\"n\",\"type\":\"int64\"}]"}));
        assert_eq!(tables[1]["table"], "u");
        assert!(out.get("rows").is_none());
    }

    #[test]
    fn a_describe_job_adds_the_profile_of_each_table() {
        let d = folder();
        let out = node(json!({"run_id": "r", "path": d.s(), "mode": "describe"}));
        let profile: Value = serde_json::from_str(out["tables"][0]["profile"].as_str().unwrap()).unwrap();
        assert_eq!(profile["rows_scanned"], 3);
        assert_eq!(profile["columns"][1]["max"], 4);
    }

    #[test]
    fn inline_tables_arrive_as_json_text_and_a_job_can_chain_on_a_previous_result() {
        let first = node(json!({"run_id": "r", "tables": "{\"t\": {\"rows\": [[1,\"a\"],[2,\"b\"]]}}", "sql": "SELECT * FROM t"}));
        let rows: Vec<Value> = first["rows"].as_array().unwrap().iter().map(|r| serde_json::from_str(r["values"].as_str().unwrap()).unwrap()).collect();
        let columns: Value = serde_json::from_str(first["result"]["columns"].as_str().unwrap()).unwrap();
        let chained = node(json!({"run_id": "r2", "tables": json!({"prev": {"columns": columns, "rows": rows}}).to_string(), "sql": "SELECT sum(column_1) AS s FROM prev"}));
        assert_eq!(chained["rows"], json!([{"row": 0, "values": "[3]"}]));
    }

    #[test]
    fn an_export_job_reports_the_file_it_wrote() {
        let d = folder();
        let out = node(json!({"run_id": "r", "path": d.s(), "mode": "export", "sql": "SELECT n FROM t", "output": "o.csv"}));
        assert_eq!(out["result"]["mode"], "export");
        assert_eq!((out["result"]["file"].clone(), out["result"]["rows_written"].clone(), out["result"]["complete"].clone()), (json!("o.csv"), json!(3), json!(true)));
        assert_eq!(std::fs::read_to_string(d.path().join("o.csv")).unwrap(), "n\n1\n2\n4\n");
    }

    #[test]
    fn a_failure_is_a_result_with_an_error_so_the_run_goes_on() {
        let d = folder();
        for (job, want) in [
            (json!({"run_id": "r", "path": d.s(), "sql": "DROP TABLE t"}), "only SELECT queries are accepted"),
            (json!({"run_id": "r", "path": "/no/such/folder", "sql": "SELECT 1"}), "cannot read /no/such/folder"),
            (json!({"run_id": "r", "tables": "{broken", "sql": "SELECT 1"}), "tables is not valid JSON"),
            (json!({"run_id": "r", "path": d.s(), "sql": "SELECT 1", "bogus": 1}), "invalid input"),
        ] {
            let out = node(job.clone());
            assert_eq!(out["result"]["complete"], false, "{job}");
            assert!(out["result"]["error"].as_str().unwrap().contains(want), "{job}: {out}");
            assert!(out.get("rows").is_none() && out.get("tables").is_none());
        }
    }
}
