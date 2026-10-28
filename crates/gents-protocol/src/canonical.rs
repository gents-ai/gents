use chrono::{DateTime, FixedOffset, SecondsFormat};

#[derive(Debug, thiserror::Error)]
pub enum CanonicalError {
    #[error("{name} must not be empty")]
    Empty { name: String },
    #[error("{name} must be canonical (no surrounding whitespace)")]
    NotTrimmed { name: String },
    #[error("{name} contains control characters")]
    ControlCharacters { name: String },
    #[error("unsupported {name}: {value}")]
    UnsupportedValue { name: String, value: String },
    #[error("{name} must use canonical UTC form")]
    NotUtc { name: String },
    #[error("parsing {name}")]
    Timestamp {
        name: String,
        #[source]
        source: chrono::ParseError,
    },
    #[error("{name} must use canonical UTC seconds form")]
    NotUtcSeconds { name: String },
}

type Result<T, E = CanonicalError> = std::result::Result<T, E>;

pub fn require_identifier(name: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(CanonicalError::Empty {
            name: name.to_owned(),
        });
    }
    if value != value.trim() {
        return Err(CanonicalError::NotTrimmed {
            name: name.to_owned(),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(CanonicalError::ControlCharacters {
            name: name.to_owned(),
        });
    }
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
    if !allowed.contains(&value) {
        return Err(CanonicalError::UnsupportedValue {
            name: name.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(())
}

pub fn parse_utc_seconds(name: &str, value: &str) -> Result<DateTime<FixedOffset>> {
    require_identifier(name, value)?;
    if !value.ends_with('Z') {
        return Err(CanonicalError::NotUtc {
            name: name.to_owned(),
        });
    }
    let parsed =
        DateTime::parse_from_rfc3339(value).map_err(|source| CanonicalError::Timestamp {
            name: name.to_owned(),
            source,
        })?;
    if parsed.to_rfc3339_opts(SecondsFormat::Secs, true) != value {
        return Err(CanonicalError::NotUtcSeconds {
            name: name.to_owned(),
        });
    }
    Ok(parsed)
}
