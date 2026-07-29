use super::support::*;

const NORMAL_LEAN: &str = r#"
inductive Sample where
  | one
  | two

namespace Sample

def toDefraDB : Sample -> String
  | .one => "one"
  | .two => "two"

def fromDefraDB? : String -> Option Sample
  | "one" => some .one
  | "two" => some .two
  | _ => none

end Sample
"#;

#[test]
fn parses_normal_to_defradb_values() {
    assert_eq!(
        parse_lean_to_defradb_values(NORMAL_LEAN, "Sample").unwrap(),
        vec!["one", "two"]
    );
}

#[test]
fn reports_missing_namespace() {
    let error = parse_lean_to_defradb_values(NORMAL_LEAN, "Missing").unwrap_err();
    assert_eq!(error, LeanVocabularyParseError::MissingNamespace);
}

#[test]
fn reports_missing_to_defradb() {
    let lean = r#"
namespace Sample

def unrelated : String := "value"

end Sample
"#;

    let error = parse_lean_to_defradb_values(lean, "Sample").unwrap_err();
    assert_eq!(error, LeanVocabularyParseError::MissingToDefraDB);
}

#[test]
fn reports_malformed_arm() {
    let lean = r#"
namespace Sample

def toDefraDB : Sample -> String
  | .one "one"

end Sample
"#;

    let error = parse_lean_to_defradb_values(lean, "Sample").unwrap_err();
    assert!(matches!(
        error,
        LeanVocabularyParseError::MalformedArm {
            reason: "missing `=>`",
            ..
        }
    ));
}

#[test]
fn parses_target_from_multiple_namespaces() {
    let lean = r#"
namespace Other

def toDefraDB : Other -> String
  | .one => "other"

end Other

namespace Sample

def toDefraDB : Sample -> String
  | .one => "sample"

end Sample
"#;

    assert_eq!(
        parse_lean_to_defradb_values(lean, "Sample").unwrap(),
        vec!["sample"]
    );
}

#[test]
fn extracts_contract_json_between_sentinels() {
    let stdout = format!(
        "debug {{noise}}\n{}\n{{\"ok\":true}}\n{}\nmore {{noise}}\n",
        gents_lean_contract::CONTRACT_JSON_BEGIN,
        gents_lean_contract::CONTRACT_JSON_END
    );

    assert_eq!(
        gents_lean_contract::extract_contract_json(&stdout).unwrap(),
        "{\"ok\":true}"
    );
}

#[test]
fn assertion_message_identifies_sources_and_differences() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(|| {
        assert_lean_to_defradb_vocabulary_matches(LeanVocabulary {
            lean_file: "sample.lean",
            model: NORMAL_LEAN,
            namespace: "Sample",
            rust_source: "Sample::ALL",
            rust_values: &["one", "three"],
        });
    });
    std::panic::set_hook(previous_hook);

    let panic = result.expect_err("assertion should fail");
    let message = panic_message(panic.as_ref());
    assert!(message.contains("Lean file: sample.lean"));
    assert!(message.contains("namespace: Sample"));
    assert!(message.contains("Rust vocabulary source: Sample::ALL"));
    assert!(message.contains("missing Lean values (present in Rust): [\"three\"]"));
    assert!(message.contains("extra Lean values (absent from Rust): [\"two\"]"));
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|message| message.to_string())
        })
        .unwrap_or_else(|| "non-string panic".to_string())
}
