use anyhow::{bail, Result};

pub(super) fn trim_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn require_trimmed(name: &str, value: impl AsRef<str>) -> Result<String> {
    let value = value.as_ref().trim().to_string();
    if value.is_empty() {
        bail!("{name} is required");
    }
    Ok(value)
}

/// Normalize an id list from the frontend: trim each entry, drop blanks, and
/// de-duplicate while preserving order. Used for `skill_refs`/`skill_excludes`
/// so an empty or whitespace-only selection lands as an empty `Vec` (rendered
/// as `null`, never `[]`, by the writer).
pub(super) fn sanitize_id_list(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}
