//! `OpenAiWireApi` moved to gents-loop (G-1): `provider_input`'s per-turn
//! projection switches on it with no native dependency. Re-exported here so
//! `crate::openai_wire` keeps every symbol this crate's callers already use.
pub use gents_loop::openai_wire::OpenAiWireApi;
