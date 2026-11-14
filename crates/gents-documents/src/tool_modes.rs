use anyhow::{bail, Result};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum FileToolMode {
    #[default]
    Off,
    ReadOnly,
    ReadWrite,
}

impl FileToolMode {
    pub const ALL: [Self; 3] = [Self::Off, Self::ReadOnly, Self::ReadWrite];

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "Off" => Ok(Self::Off),
            "ReadOnly" => Ok(Self::ReadOnly),
            "ReadWrite" => Ok(Self::ReadWrite),
            other => bail!("unknown file tool mode {other}"),
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::ReadOnly => 1,
            Self::ReadWrite => 2,
        }
    }

    pub fn meet(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum BashMode {
    #[default]
    Off,
    ReadOnly,
    Unrestricted,
}

impl BashMode {
    pub const ALL: [Self; 3] = [Self::Off, Self::ReadOnly, Self::Unrestricted];

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "Off" => Ok(Self::Off),
            "ReadOnly" => Ok(Self::ReadOnly),
            "Unrestricted" => Ok(Self::Unrestricted),
            other => bail!("unknown bash mode {other}"),
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::ReadOnly => 1,
            Self::Unrestricted => 2,
        }
    }

    pub fn meet(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}
