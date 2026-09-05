//! Explicit compatibility for rows from the registered predecessor version only.
//! New-version rows never pass through this lens; absence is not runtime authority.

const FIELD: &str = "path_capability";
const LEGACY_VALUE: &str = r#"{"mode":"unrestrictedCompatibility"}"#;

include!("../../source_version_field.rs");
