//! Explicit compatibility for rows from the registered predecessor version only.
//! New-version rows never pass through this lens; absence is not runtime authority.

const FIELD: &str = "path_capability_digest";
const LEGACY_VALUE: &str = r#"05f25622d621f2cf7db7c72e2e687575f0bd8561153017dd03ff5bf84669c220"#;

include!("../../source_version_field.rs");
