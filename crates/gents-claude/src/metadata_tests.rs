use serde_json::json;

use super::*;

fn identity() -> UserIdentity {
    UserIdentity {
        device_id: "a".repeat(64),
        account_uuid: "acct".into(),
        session_id: "sess".into(),
        parent_session_id: None,
    }
}

fn extra(pairs: &[(&str, Value)]) -> Vec<(String, Value)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn plain() -> String {
    format!(
        "{{\"device_id\":\"{}\",\"account_uuid\":\"acct\",\"session_id\":\"sess\"}}",
        "a".repeat(64)
    )
}

#[test]
fn no_extras() {
    assert_eq!(user_id(&identity(), &[]).unwrap(), plain());
}

#[test]
fn parent_session_is_last() {
    let mut who = identity();
    who.parent_session_id = Some("parent".into());
    assert_eq!(
        user_id(&who, &[]).unwrap(),
        format!(
            "{},\"parent_session_id\":\"parent\"}}",
            plain().trim_end_matches('}')
        )
    );
}

#[test]
fn extras_filter_reserved_keys_and_non_scalars() {
    let rendered = user_id(
        &identity(),
        &extra(&[
            ("team", json!("x")),
            ("ti", json!("t")),
            ("os", json!("linux")),
            ("tk", json!("secret")),
            ("nested", json!({"a": 1})),
            ("list", json!([1])),
            ("nothing", Value::Null),
            ("n", json!(2)),
            ("f", json!(true)),
        ]),
    )
    .unwrap();
    assert_eq!(
        rendered,
        format!(
            "{{\"team\":\"x\",\"n\":2,\"f\":true,{}",
            plain().trim_start_matches('{')
        )
    );
}

#[test]
fn colliding_key_keeps_position_and_takes_identity_value() {
    let rendered = user_id(
        &identity(),
        &extra(&[
            ("a", json!(1)),
            ("session_id", json!("mine")),
            ("b", json!(2)),
        ]),
    )
    .unwrap();
    assert_eq!(
        rendered,
        format!(
            "{{\"a\":1,\"session_id\":\"sess\",\"b\":2,\"device_id\":\"{}\",\"account_uuid\":\"acct\"}}",
            "a".repeat(64)
        )
    );
}

#[test]
fn unset_parent_keeps_the_extra_value() {
    let rendered = user_id(&identity(), &extra(&[("parent_session_id", json!("x"))])).unwrap();
    assert!(rendered.starts_with("{\"parent_session_id\":\"x\",\"device_id\""));
}

#[test]
fn integer_like_keys_enumerate_first() {
    let rendered = user_id(
        &identity(),
        &extra(&[
            ("b", json!(1)),
            ("10", json!(2)),
            ("2", json!(3)),
            ("02", json!(4)),
        ]),
    )
    .unwrap();
    assert!(rendered.starts_with("{\"2\":3,\"10\":2,\"b\":1,\"02\":4,"));
}

#[test]
fn the_512_unit_boundary_keeps_then_drops_extras() {
    let overhead = user_id(&identity(), &extra(&[("k", json!(""))]))
        .unwrap()
        .len();
    let fill = 512 - overhead;
    let keep = user_id(&identity(), &extra(&[("k", json!("x".repeat(fill)))])).unwrap();
    assert_eq!(keep.len(), 512);
    assert!(keep.starts_with("{\"k\":"));
    let drop = user_id(&identity(), &extra(&[("k", json!("x".repeat(fill + 1)))])).unwrap();
    assert_eq!(drop, plain());
}

#[test]
fn the_boundary_counts_utf16_units() {
    let overhead = user_id(&identity(), &extra(&[("k", json!(""))]))
        .unwrap()
        .len();
    let fill = 512 - overhead;
    // U+00E9 is 2 UTF-8 bytes but one unit: still kept at exactly 512 units.
    let kept = user_id(&identity(), &extra(&[("k", json!("\u{e9}".repeat(fill)))])).unwrap();
    assert!(kept.starts_with("{\"k\":"));
    assert_eq!(kept.encode_utf16().count(), 512);
    // U+1F600 is 2 units: fill/2 emoji + one ASCII is 512 units, one more unit drops.
    let at = format!("{}{}", "\u{1F600}".repeat(fill / 2), "x".repeat(fill % 2));
    let kept = user_id(&identity(), &extra(&[("k", json!(at.clone()))])).unwrap();
    assert_eq!(kept.encode_utf16().count(), 512);
    assert!(kept.starts_with("{\"k\":"));
    let over = user_id(&identity(), &extra(&[("k", json!(format!("{at}x")))])).unwrap();
    assert_eq!(over, plain());
}

#[test]
fn string_escaping_matches_json_stringify() {
    let rendered = user_id(
        &identity(),
        &extra(&[("k", json!("a\"b\\c\n\u{1}\u{7f}\u{2028}"))]),
    )
    .unwrap();
    assert!(rendered.starts_with("{\"k\":\"a\\\"b\\\\c\\n\\u0001\u{7f}\u{2028}\","));
}

#[test]
fn numbers_print_like_javascript() {
    for (value, expected) in [
        (1.0, "1"),
        (-0.0, "0"),
        (1.5, "1.5"),
        (1e21, "1e+21"),
        (1.5e300, "1.5e+300"),
        (123456789012345680000.0, "123456789012345680000"),
        (1e-6, "0.000001"),
        (1e-7, "1e-7"),
        (-2.5e-7, "-2.5e-7"),
        (0.1, "0.1"),
    ] {
        assert_eq!(js_number(value), expected);
    }
}

#[test]
fn rejects_a_malformed_device_id() {
    for bad in [
        "",
        "A".repeat(64).as_str(),
        "a".repeat(63).as_str(),
        "a".repeat(65).as_str(),
    ] {
        let mut who = identity();
        who.device_id = bad.into();
        assert_eq!(user_id(&who, &[]), Err(MetadataError::InvalidDeviceId));
    }
}

#[test]
fn device_id_is_a_stable_hex_digest() {
    assert_eq!(
        device_id(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_ne!(device_id(b"node-a"), device_id(b"node-b"));
    assert_eq!(device_id(b"node-a"), device_id(b"node-a"));
}
