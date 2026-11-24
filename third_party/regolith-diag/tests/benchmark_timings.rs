//! The run-file adapters preserve measured data and leave gaps explicit.

#[path = "../benches/common/json.rs"]
mod json;
#[path = "../benches/common/records.rs"]
mod records;
#[path = "../benches/common/run_meta.rs"]
mod run_meta;
#[path = "../benches/common/timings.rs"]
mod timings;

use json::{Json, parse};

fn normalized(name: &str, data: &str) -> Json {
    timings::normalize(name, Some(parse(data).unwrap()), "clean").unwrap()
}

fn rows(family: &Json) -> &[Json] {
    family.get("rows").unwrap().as_array().unwrap()
}

#[test]
fn a_transaction_retains_its_ranges_attempts_and_workload() {
    let family = normalized(
        "transaction",
        r#"{"quick":false,"matrix":[{"flavor":"optimistic","isolation":"defralevel",
        "throughput":{"contended_threads":8,"hot_keys":8,
        "uncontended_commits_per_s":{"median":100,"min":90,"max":110},
        "contended_commits_per_s":{"median":50,"min":40,"max":60},
        "contended_attempts":{"commits":100,"conflicts":20}}}]}"#,
    );
    assert_eq!(rows(&family).len(), 2);
    assert_eq!(rows(&family)[0].get("min").unwrap().as_f64(), Some(90.0));
    assert_eq!(rows(&family)[1].get("max").unwrap().as_f64(), Some(60.0));
    assert_eq!(
        rows(&family)[1]
            .get("context")
            .unwrap()
            .get("threads")
            .unwrap()
            .as_f64(),
        Some(8.0)
    );
    let source = family.get("source").unwrap();
    let cell = &source.get("matrix").unwrap().as_array().unwrap()[0];
    assert!(
        cell.get("throughput")
            .unwrap()
            .get("contended_attempts")
            .is_some()
    );
}

#[test]
fn a_scan_records_each_size_level_and_metric_without_inventing_ranges() {
    let family = normalized(
        "scan_commit",
        r#"[{"keys":32,"isolation":"serializable",
        "commit_us":5,"scan_and_commit_us":10}]"#,
    );
    assert_eq!(rows(&family).len(), 2);
    assert_eq!(rows(&family)[0].get("median").unwrap().as_f64(), Some(5.0));
    assert!(matches!(rows(&family)[0].get("max"), Some(Json::Null)));
}

#[test]
fn abandoned_operations_do_not_look_like_a_faster_completed_workload() {
    let family = normalized(
        "transaction",
        r#"{"matrix":[{"throughput":{
        "contended_attempts":{"abandoned":1}}}]}"#,
    );
    assert!(rows(&family)[0].get("invalid_reason").is_none());
    assert!(rows(&family)[1].get("invalid_reason").is_some());
    let family = normalized(
        "transaction",
        r#"{"matrix":[{
        "correctness":{"survival_pct_min":99},"throughput":{}}]}"#,
    );
    assert!(
        rows(&family)
            .iter()
            .all(|row| row.get("invalid_reason").is_some())
    );
}

#[test]
fn append_rates_and_allocation_latency_have_their_own_directions() {
    let family = normalized(
        "append",
        r#"{"append":[{"writers":4,"commits_per_s":{
        "put":{"median":100,"min":90,"max":110},
        "append":{"median":80,"min":70,"max":90},
        "append_once":{"median":60,"min":50,"max":70}}}],
        "allocate":{"latency_ns_p50":{"median":10,"min":9,"max":11}}}"#,
    );
    assert_eq!(rows(&family).len(), 6);
    assert!(matches!(
        rows(&family)[0].get("higher_is_better"),
        Some(Json::Bool(true))
    ));
    assert!(matches!(
        rows(&family)[3].get("higher_is_better"),
        Some(Json::Bool(false))
    ));
}

#[test]
fn read_and_callback_rows_keep_their_units_and_measured_bounds() {
    {
        let name = "read_rules";
        let family = normalized(
            name,
            r#"{"quick":true,"reps":3,"rows":[
            {"name":"read","value":5,"unit":"ns/read","min":4,"max":6}]}"#,
        );
        assert_eq!(
            rows(&family)[0].get("unit").unwrap().as_str(),
            Some("ns/read")
        );
        assert_eq!(rows(&family)[0].get("max").unwrap().as_f64(), Some(6.0));
        assert_eq!(
            family.get("context").unwrap().get("reps").unwrap().as_f64(),
            Some(3.0)
        );
    }
    let family = normalized("txn_callbacks", r#"[{"case":"commit","us":2,"min_us":1}]"#);
    assert_eq!(rows(&family)[0].get("min").unwrap().as_f64(), Some(1.0));
    assert!(matches!(rows(&family)[0].get("max"), Some(Json::Null)));
    let family = normalized(
        "sync",
        r#"{"benchmarks":[
        {"benchmark":"sync_uncontended/mutex","median_ns":5,"mean_ns":6}]}"#,
    );
    assert_eq!(rows(&family)[0].get("median").unwrap().as_f64(), Some(5.0));
    assert!(matches!(rows(&family)[0].get("max"), Some(Json::Null)));
}

#[test]
fn timing_trust_is_downgraded_without_changing_the_source() {
    let source = parse(r#"{"trust":"clean","matrix":[{"throughput":{}}]}"#).unwrap();
    let family = timings::normalize("transaction", Some(source), "contaminated").unwrap();
    assert_eq!(family.get("trust").unwrap().as_str(), Some("contaminated"));
    assert_eq!(
        family.get("source").unwrap().get("trust").unwrap().as_str(),
        Some("clean")
    );
    let source = parse(r#"{"timing_trust":"contaminated","matrix":[{"throughput":{}}]}"#).unwrap();
    let family = timings::normalize("transaction", Some(source), "clean").unwrap();
    assert_eq!(family.get("trust").unwrap().as_str(), Some("contaminated"));
    let source =
        parse(r#"{"measurement":{"guard_passed":true},"matrix":[{"throughput":{}}]}"#).unwrap();
    let family = timings::normalize("transaction", Some(source), "contaminated").unwrap();
    assert_eq!(family.get("trust").unwrap().as_str(), Some("clean"));
}

#[test]
fn every_uncollected_family_is_absent() {
    for name in timings::FAMILIES {
        let family = timings::normalize(name, None, "clean").unwrap();
        assert_eq!(family.get("trust").unwrap().as_str(), Some("absent"));
        assert!(rows(&family).is_empty());
    }
}

#[test]
fn an_unknown_trust_or_family_is_refused() {
    assert!(timings::normalize("unknown", Some(Json::Arr(Vec::new())), "clean").is_err());
    let source = parse(r#"{"trust":"maybe"}"#).unwrap();
    assert!(timings::normalize("transaction", Some(source), "clean").is_err());
    assert!(timings::normalize("transaction", Some(Json::Num(1.0)), "clean").is_err());
    let source = parse(r#"{"trust":true}"#).unwrap();
    assert!(timings::normalize("transaction", Some(source), "clean").is_err());
}
