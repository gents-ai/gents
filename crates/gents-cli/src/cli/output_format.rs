use clap::ValueEnum;

#[derive(Debug, thiserror::Error)]
pub(crate) enum OutputFormatError {
    #[error("unsupported --output {requested} for {command}; supported values: {supported}")]
    Unsupported {
        requested: &'static str,
        command: String,
        supported: String,
    },
}

type Result<T, E = OutputFormatError> = std::result::Result<T, E>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Table,
    Json,
    Tree,
    Ndjson,
}

impl OutputFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Table => "table",
            Self::Json => "json",
            Self::Tree => "tree",
            Self::Ndjson => "ndjson",
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
        Err(OutputFormatError::Unsupported {
            requested: self.as_str(),
            command: command.to_owned(),
            supported,
        })
    }
}
