use super::*;
use serde_json::json;

fn key() -> Value {
    json!({"kind": "environment", "variable": "PLATFORM_TEST_KEY"})
}

fn aws() -> Value {
    json!({
        "kind": "aws_credentials",
        "access_key_id": "AKIDEXAMPLE",
        "secret_access_key": "secret-access-key-value",
        "session_token": "session-token-value",
    })
}

fn google() -> Value {
    json!({"kind": "google_credentials", "credentials_json": "{\"private_key\":\"google-secret\"}"})
}

#[test]
fn the_self_configuration_guard_keeps_cloud_credentials_operator_managed() {
    let doc = |auth: Value| match json!({ "auth": auth }) {
        Value::Object(map) => map,
        _ => unreachable!(),
    };
    for credentials in [aws(), google()] {
        let kept = guard_backend_auth(&doc(credentials.clone()), &doc(credentials.clone()));
        assert!(kept.is_ok(), "{kept:?}");
        let introduced = guard_backend_auth(&doc(key()), &doc(credentials));
        let message = introduced.unwrap_err().to_string();
        assert!(message.contains("operator-managed"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }
    let mut changed = aws();
    changed["secret_access_key"] = "another-secret".into();
    assert!(guard_backend_auth(&doc(aws()), &doc(changed)).is_err());
}
