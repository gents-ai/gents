//! Pure tool output truncation lives in `gents-loop` and is re-exported
//! for native callers.
pub use gents_loop::truncation::{
    tool_result_truncation_mode, truncate, truncate_text, TextTruncation, TruncationLimits,
    TruncationMode, TruncationTrigger, LIVE_STREAM_CAPACITY_BYTES,
};
