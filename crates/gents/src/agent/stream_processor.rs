pub use gents_loop::stream_processor::*;

// Glue for the test suite below, which reaches these bare through
// `use super::*` the way it did before the move.
#[cfg(test)]
use crate::agent::loop_stream::LoopStreamItem;
#[cfg(test)]
use crate::hook::DefraSessionHook;

#[cfg(test)]
#[path = "stream_processor_tests.rs"]
mod tests;
