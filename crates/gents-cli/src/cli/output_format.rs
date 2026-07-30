use anyhow::Result;
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Table,
    Json,
    Tree,
}

impl OutputFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Table => "table",
            Self::Json => "json",
            Self::Tree => "tree",
        }
    }

    pub(crate) fn ensure_supported(self, command: &str, supported: &[Self]) -> Result<Self> {
        if supported.contains(&self) {
            return Ok(self);
        }
        let supported = supported
            .iter()
            .map(|format| format.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "unsupported --output {} for {command}; supported values: {supported}",
            self.as_str()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_enum_spellings_are_lowercase() {
        let names: Vec<String> = OutputFormat::value_variants()
            .iter()
            .map(|v| v.to_possible_value().unwrap().get_name().to_string())
            .collect();
        assert_eq!(names, vec!["text", "table", "json", "tree"]);
    }

    #[test]
    fn ensure_supported_accepts_only_declared_subset() {
        assert_eq!(
            OutputFormat::Json
                .ensure_supported("demo", &[OutputFormat::Text, OutputFormat::Json])
                .unwrap(),
            OutputFormat::Json
        );
        let err = OutputFormat::Tree
            .ensure_supported("demo", &[OutputFormat::Text, OutputFormat::Json])
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported --output tree for demo"));
        assert!(err.contains("text, json"));
    }
}
