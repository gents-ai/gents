use super::*;

impl LeanR4cBackgroundWorkCase {
    pub fn witness(&self) -> &'static str {
        match self {
            Self::ReadToolOutputCanonicalSourceReconstruction { .. } => {
                "r4c.read_tool_output.canonical_source_reconstruction"
            }
        }
    }
}

