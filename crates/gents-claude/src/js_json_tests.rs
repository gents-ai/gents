use proptest::prelude::*;
use serde_json::Value;

use super::*;

fn restringify(text: &str) -> String {
    js_restringify(text).unwrap().get().to_owned()
}

#[test]
fn numbers_print_as_ecmascript_number_to_string() {
    // Outputs of `JSON.stringify(JSON.parse(input))` in V8.
    for (input, want) in [
        ("0", "0"),
        ("-0", "0"),
        ("-0.0", "0"),
        ("0e5", "0"),
        ("1", "1"),
        ("-1", "-1"),
        ("1.0", "1"),
        ("1.50", "1.5"),
        ("100", "100"),
        ("1e2", "100"),
        ("1E2", "100"),
        ("0.1", "0.1"),
        ("0.000001", "0.000001"),
        ("0.0000001", "1e-7"),
        ("1e-7", "1e-7"),
        ("1.5e-7", "1.5e-7"),
        ("123456789012345680000", "123456789012345680000"),
        ("1e21", "1e+21"),
        ("1e20", "100000000000000000000"),
        ("1.5e21", "1.5e+21"),
        ("123e-8", "0.00000123"),
        ("123e-20", "1.23e-18"),
        ("9007199254740993", "9007199254740992"),
        ("9007199254740992", "9007199254740992"),
        ("0.30000000000000004", "0.30000000000000004"),
        ("5e-324", "5e-324"),
        ("1.7976931348623157e308", "1.7976931348623157e+308"),
        ("1e999", "null"),
        ("-1e999", "null"),
        ("3.14159", "3.14159"),
        ("-12.5e1", "-125"),
    ] {
        assert_eq!(restringify(input), want, "input {input}");
    }
}

#[test]
fn keys_keep_first_occurrence_order_and_the_last_duplicate_value() {
    assert_eq!(
        restringify(r#"{ "b": 1, "a": 2, "b": 3, "c": {"z":1,"y":2} }"#),
        r#"{"b":3,"a":2,"c":{"z":1,"y":2}}"#
    );
    assert_eq!(restringify("{}"), "{}");
    assert_eq!(restringify(" [ ] "), "[]");
    assert_eq!(
        restringify(r#"{"a":[1, 2 ,{"k" : null}], "t":true,"f":false}"#),
        r#"{"a":[1,2,{"k":null}],"t":true,"f":false}"#
    );
}

#[test]
fn escapes_follow_json_stringify() {
    assert_eq!(restringify(r#""a\/b""#), r#""a/b""#);
    assert_eq!(
        restringify(r#""\u0041\u00e9\u4e2d""#),
        "\"A\u{e9}\u{4e2d}\""
    );
    assert_eq!(
        restringify(r#""\u001f\u0000\u007f""#),
        "\"\\u001f\\u0000\u{7f}\""
    );
    assert_eq!(restringify(r#""\b\f\n\r\t\"\\""#), r#""\b\f\n\r\t\"\\""#);
    assert_eq!(
        restringify("\"\u{2028}\u{1f600}\""),
        "\"\u{2028}\u{1f600}\""
    );
    // A surrogate pair joins; a lone one stays a lowercase escape.
    assert_eq!(restringify(r#""\ud83d\ude00""#), "\"\u{1f600}\"");
    assert_eq!(restringify(r#""\uD800""#), r#""\ud800""#);
    assert_eq!(restringify(r#""\uDC00x\uD800""#), r#""\udc00x\ud800""#);
    assert_eq!(restringify(r#"{"\uD800":1,"\ud800":2}"#), r#"{"\ud800":2}"#);
}

#[test]
fn invalid_json_is_refused() {
    for bad in [
        "",
        "{",
        "[1,]",
        "{\"a\":1,}",
        "01",
        "1.",
        ".5",
        "+1",
        "1e",
        "'a'",
        "\"a",
        "\"\u{1}\"",
        "\"\\x\"",
        "\"\\u12\"",
        "nul",
        "[1] 2",
        "{a:1}",
        "-",
        "--1",
    ] {
        assert!(js_restringify(bad).is_err(), "accepted {bad:?}");
    }
    let deep = "[".repeat(MAX_DEPTH + 2);
    assert!(js_restringify(&deep).is_err());
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-(1i64 << 53)..(1i64 << 53)).prop_map(Value::from),
        (-1.0e6f64..1.0e6).prop_map(Value::from),
        ".{0,12}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec(("[a-z]{1,4}", inner), 0..6)
                .prop_map(|entries| Value::Object(entries.into_iter().collect())),
        ]
    })
}

proptest! {
    #[test]
    fn restringify_is_idempotent_and_value_preserving(value in json_value()) {
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let once = restringify(&pretty);
        prop_assert_eq!(restringify(&once), once.clone());
        let parsed: Value = serde_json::from_str(&once).unwrap();
        prop_assert_eq!(parsed, value);
    }

    #[test]
    fn key_order_is_preserved(keys in prop::collection::vec("[a-z]{1,6}", 1..8)) {
        let mut unique: Vec<String> = Vec::new();
        for key in &keys {
            if !unique.contains(key) {
                unique.push(key.clone());
            }
        }
        let text = format!(
            "{{{}}}",
            keys.iter().enumerate().map(|(i, k)| format!("\"{k}\": {i}")).collect::<Vec<_>>().join(", ")
        );
        let out = restringify(&text);
        let at = |key: &String| out.find(&format!("\"{key}\":")).unwrap();
        let positions: Vec<usize> = unique.iter().map(at).collect();
        prop_assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn number_text_matches_a_round_trip(n in any::<f64>().prop_filter("finite", |n| n.is_finite())) {
        let mut out = String::new();
        push_number(&mut out, n);
        let back: f64 = out.parse().unwrap();
        prop_assert_eq!(back, if n == 0.0 { 0.0 } else { n });
    }
}
