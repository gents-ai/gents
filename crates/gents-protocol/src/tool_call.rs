#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum AwaitMode {
    #[default]
    Foreground,
    Background,
}

impl AwaitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AwaitMode::Foreground => "foreground",
            AwaitMode::Background => "background",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "foreground" => Some(AwaitMode::Foreground),
            "background" => Some(AwaitMode::Background),
            _ => None,
        }
    }

    pub const ALL: &'static [AwaitMode] = &[AwaitMode::Foreground, AwaitMode::Background];
}
