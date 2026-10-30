use gents_loop::claude_platform::ClaudePlatform;

use crate::backend_provider::BackendProviderKind;

#[test]
fn generated_claude_platform_cases_match_the_lean_owner() {
    let cases = crate::lean_vocab_test::lean_claude_platform_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let kind = BackendProviderKind::parse_optional(case["kind"].as_str()).unwrap();
        let settings: Option<ClaudePlatform> =
            serde_json::from_value(case["claude_platform"].clone()).unwrap();
        let valid = case["valid"].as_bool().unwrap();
        assert_eq!(
            ClaudePlatform::validate(kind, settings.as_ref()).is_ok(),
            valid,
            "{name}"
        );
        if valid {
            assert_eq!(
                ClaudePlatform::default_endpoint(kind, settings.as_ref())
                    .unwrap()
                    .as_deref(),
                case["default_base_url"].as_str(),
                "{name}"
            );
        } else {
            assert!(case["default_base_url"].is_null(), "{name}");
        }
    }
}
