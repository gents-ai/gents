//! Operations callers ask core to perform, each declared once: its typed
//! input, receipt and error, the recovery each failure names, the permission
//! it needs and its help. Front ends parse into these types and render them.

pub mod agent;
pub mod document;
pub mod schema;

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
