use gents_lean_vocab::families::contracts_claude_platform_outcome as family;
use gents_lean_vocab::family::recode;
use gents_loop::backend_provider::BackendProviderKind;
use gents_loop::claude_platform::ClaudePlatform;

#[test]
fn generated_claude_platform_cases_match_the_lean_owner() {
    family::replay(|i| {
        let kind = BackendProviderKind::parse_optional(Some(i.kind.as_str())).unwrap();
        let settings: Option<ClaudePlatform> = recode(&i.claude_platform);
        let valid = ClaudePlatform::validate(kind, settings.as_ref()).is_ok();
        family::Output {
            valid,
            default_base_url: if valid {
                ClaudePlatform::default_endpoint(kind, settings.as_ref()).unwrap()
            } else {
                None
            },
        }
    });
}
