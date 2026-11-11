//! Operations callers ask core to perform, each declared once: its typed
//! input, receipt and error, the recovery each failure names, the permission
//! it needs and its help. Front ends parse into these types and render them.

pub mod agent;
pub mod document;
pub mod inference;
pub mod mcp_service;
pub mod request;
pub mod session;
pub mod skill;
pub mod tools;

/// The call that lets a caller correct a refused operation. Each front end
/// renders it in its own syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextCall {
    VerbHelp {
        resource: &'static str,
        verb: &'static str,
    },
    ResourceHelp {
        resource: &'static str,
    },
    /// Read the caller's own document of `resource`, which shows what it holds.
    Inspect {
        resource: &'static str,
    },
    /// List `resource`, which shows the documents the caller may name.
    List {
        resource: &'static str,
    },
    /// Run `verb` of `resource`, which takes no further input.
    Verb {
        resource: &'static str,
        verb: &'static str,
    },
}

/// Tri-state for an optional text field. A present JSON null is `Clear`;
/// the property must be absent to stay `Omitted`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StringUpdate {
    #[default]
    Omitted,
    Clear,
    Set(String),
}

impl<'de> serde::Deserialize<'de> for StringUpdate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        })
    }
}

impl StringUpdate {
    pub fn is_present(&self) -> bool {
        !matches!(self, Self::Omitted)
    }

    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Set(value) => Some(value),
            Self::Omitted | Self::Clear => None,
        }
    }

    pub fn owned_value(&self) -> Option<String> {
        self.value().map(ToOwned::to_owned)
    }
}
