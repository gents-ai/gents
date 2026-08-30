//! Shared fail-closed validation for signed semantic fields.

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, SecondsFormat};

pub fn require_identifier(name: &str, value: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "{name} must not be empty");
    anyhow::ensure!(
        value == value.trim(),
        "{name} must be canonical (no surrounding whitespace)"
    );
    anyhow::ensure!(
        !value.chars().any(char::is_control),
        "{name} contains control characters"
    );
    Ok(())
}

pub fn require_optional_identifier(name: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        require_identifier(name, value)?;
    }
    Ok(())
}

pub fn require_enum(name: &str, value: &str, allowed: &[&str]) -> Result<()> {
    require_identifier(name, value)?;
    anyhow::ensure!(allowed.contains(&value), "unsupported {name}: {value}");
    Ok(())
}

pub fn parse_utc_seconds(name: &str, value: &str) -> Result<DateTime<FixedOffset>> {
    require_identifier(name, value)?;
    anyhow::ensure!(value.ends_with('Z'), "{name} must use canonical UTC form");
    let parsed = DateTime::parse_from_rfc3339(value).with_context(|| format!("parsing {name}"))?;
    anyhow::ensure!(
        parsed.to_rfc3339_opts(SecondsFormat::Secs, true) == value,
        "{name} must use canonical UTC seconds form"
    );
    Ok(parsed)
}
