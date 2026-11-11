use anyhow::{bail, Result};

pub(super) fn require_trimmed(name: &str, value: impl AsRef<str>) -> Result<String> {
    let value = value.as_ref().trim().to_string();
    if value.is_empty() {
        bail!("{name} is required");
    }
    Ok(value)
}
