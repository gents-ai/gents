#[derive(Debug, thiserror::Error)]
pub(crate) enum PackCommandError {
    #[error("invalid inference slot binding {value:?}; expected NAME=PROFILE_ID")]
    InferenceSlotSyntax { value: String },
    #[error("invalid inference slot binding {value:?}; slot and profile ID must not be blank")]
    InferenceSlotBlank { value: String },
    #[error("inference slot {slot:?} was bound more than once")]
    InferenceSlotDuplicate { slot: String },
    #[error(
        "the pack's digest is {actual}, not the expected {expected}. Nothing was installed. \
         Next: gents pack show ./dir"
    )]
    DigestMismatch { actual: String, expected: String },
    #[error("--wire applies only to documents and plugins packs; {pack} is not one")]
    WireNeedsDocumentsOrPlugins { pack: String },
    #[error("--wire needs a pack that ships a plugin; {pack} ships none")]
    WireNeedsPlugin { pack: String },
    #[error("pack {pack} declares {count} inference slots; expected exactly one")]
    SlotCount { pack: String, count: usize },
    #[error("pack {pack} slot {slot} declares {count} agents; expected exactly one")]
    SlotAgentCount {
        pack: String,
        slot: String,
        count: usize,
    },
    #[error(
        "pack {pack} declares {count} agents in its inference slots; name one as <pack>:<agent>"
    )]
    DefaultAgentCount { pack: String, count: usize },
    #[error("{package} is not installed")]
    NotInstalled { package: String },
}
