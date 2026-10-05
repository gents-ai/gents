//! Graph mode: the plugin as a node. A request that carries the `run_id` of
//! a graph run is a `ChartRequest` record; the answer is one `ChartResult`
//! record of plain fields. A chart that cannot be drawn is a result with its
//! reason in `error`, so the run records what happened instead of stalling.

use serde_json::{Map, Value, json};

use crate::err::{ChartError, Res};
use crate::output::{Shape, deliver_as};
use crate::pipeline;
use crate::spec::Request;

/// True when `raw` is a graph node's request.
pub fn is_node(raw: &str) -> bool {
    raw.contains("\"run_id\"")
        && matches!(serde_json::from_str::<Value>(raw), Ok(Value::Object(m)) if m.get("run_id").is_some_and(|v| !v.is_null()))
}

fn clean(mut fields: Map<String, Value>) -> Map<String, Value> {
    fields.retain(|_, v| {
        !(v.is_null() || v.as_str() == Some("") || v.as_array().is_some_and(Vec::is_empty))
    });
    fields
}

fn produce(fields: Map<String, Value>) -> Res<String> {
    let text = Value::Object(fields).to_string();
    let req: Request =
        serde_json::from_str(&text).map_err(|e| ChartError(crate::request_error(&e)))?;
    let spec_output = req.resolve()?.output;
    let (rendered, files) = pipeline::render(&req)?;
    deliver_as(rendered, spec_output, &files, Shape::Record)
}

/// Runs a node's request and returns the `ChartResult` record as JSON text.
pub fn run(raw: &str) -> String {
    let parsed: Result<Map<String, Value>, _> = serde_json::from_str(raw);
    let Ok(fields) = parsed else {
        return json!({"chart": "", "error": "the chart request is not a JSON object"}).to_string();
    };
    let fields = clean(fields);
    let chart = fields
        .get("chart")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    match produce(fields) {
        Ok(record) => record,
        Err(e) => json!({"chart": chart, "error": e.0, "warnings": []}).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(raw: &str) -> Value {
        serde_json::from_str(&run(raw)).unwrap()
    }

    const BAR: &str = r#"{"run_id":"r1","chart":"bar","title":"T","x":"k","y":["v"],"data":{"columns":["k","v"],"rows":[["a",1],["b",2]]},"series":null,"subtitle":"","line":[]}"#;

    #[test]
    fn a_node_is_recognised_by_its_run_id() {
        assert!(is_node(BAR));
        assert!(!is_node(r#"{"chart":"bar"}"#));
        assert!(!is_node(r#"{"run_id":null,"chart":"bar"}"#));
        assert!(!is_node("not json"));
        assert!(!is_node(r#"{"title":"mentions \"run_id\" in text"}"#));
    }

    #[test]
    fn a_request_record_becomes_a_result_record_with_the_image_inline() {
        let v = record(BAR);
        assert_eq!(v["chart"], "bar");
        assert_eq!(
            (v["width"].as_u64(), v["height"].as_u64()),
            (Some(800), Some(480))
        );
        assert!(v["svg"].as_str().unwrap().starts_with("<svg"));
        assert!(
            v["alt"]
                .as_str()
                .unwrap()
                .starts_with("Bar chart titled \"T\".")
        );
        assert!(v["png_base64"].as_str().unwrap().starts_with("iVBOR"));
        assert_eq!(v["png_width"], 800);
        assert_eq!(v["warnings"], json!([]));
        assert!(v.get("error").is_none() && v.get("run_id").is_none());
        let series: Value = serde_json::from_str(v["series_json"].as_str().unwrap()).unwrap();
        assert_eq!(series[0]["name"], "v");
        assert_eq!(series[0]["points"], 2);
    }

    #[test]
    fn unset_fields_are_ignored_not_rejected() {
        let v = record(
            r#"{"run_id":"r","chart":"line","y":[],"x":"","title":null,"width":null,"data":"a,b\n1,2\n2,3\n"}"#,
        );
        assert!(v.get("error").is_none(), "{v}");
        assert_eq!(v["chart"], "line");
    }

    #[test]
    fn data_may_be_json_text_or_csv_text_in_a_string() {
        let json_text = record(
            r#"{"run_id":"r","chart":"bar","data":"{\"columns\":[\"k\",\"v\"],\"rows\":[[\"a\",1]]}"}"#,
        );
        assert!(json_text.get("error").is_none(), "{json_text}");
        let csv_text = record(r#"{"run_id":"r","chart":"bar","data":"k,v\na,1\nb,2\n"}"#);
        assert!(csv_text.get("error").is_none(), "{csv_text}");
    }

    #[test]
    fn a_chart_that_cannot_be_drawn_is_a_record_with_the_reason() {
        let v = record(r#"{"run_id":"r","chart":"bar","y":["zzz"],"data":"k,v\na,1\n"}"#);
        assert_eq!(v["chart"], "bar");
        assert!(
            v["error"]
                .as_str()
                .unwrap()
                .contains("column \"zzz\" is not in the data"),
            "{v}"
        );
        assert!(v.get("svg").is_none() && v.get("png_base64").is_none());
    }

    #[test]
    fn an_unknown_field_is_a_record_with_the_reason_too() {
        let v = record(r#"{"run_id":"r","chart":"bar","titel":"x","data":"k,v\na,1\n"}"#);
        assert!(
            v["error"]
                .as_str()
                .unwrap()
                .contains("\"titel\" is not known"),
            "{v}"
        );
    }

    #[test]
    fn output_svg_leaves_out_the_png_and_output_png_leaves_out_the_svg() {
        let svg_only =
            record(&BAR.replace("\"chart\":\"bar\"", "\"chart\":\"bar\",\"output\":\"svg\""));
        assert!(svg_only.get("svg").is_some() && svg_only.get("png_base64").is_none());
        let png_only =
            record(&BAR.replace("\"chart\":\"bar\"", "\"chart\":\"bar\",\"output\":\"png\""));
        assert!(png_only.get("svg").is_none() && png_only.get("png_base64").is_some());
    }

    #[test]
    fn a_save_request_lists_the_files_it_wrote() {
        let d = crate::testutil::TempDir::new();
        let raw = BAR.replace(
            "\"run_id\":\"r1\"",
            &format!(
                "\"run_id\":\"r1\",\"path\":{},\"save\":\"out/chart\"",
                json!(d.path().to_str().unwrap())
            ),
        );
        let v = record(&raw);
        assert!(v.get("error").is_none(), "{v}");
        assert_eq!(v["svg_file"], "out/chart.svg");
        assert_eq!(v["png_file"], "out/chart.png");
        assert!(
            d.path().join("out/chart.svg").is_file() && d.path().join("out/chart.png").is_file()
        );
    }

    #[test]
    fn a_non_object_request_is_an_error_record() {
        let v: Value = serde_json::from_str(&run("[1]")).unwrap();
        assert_eq!(v["error"], "the chart request is not a JSON object");
    }
}
