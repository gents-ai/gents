//! `gents eval init`: an interview with a model that drafts an eval
//! definition pack for one behavior of a subject pack.
//!
//! The pure core lives here: the subject dossier the author reads, the draft
//! it answers with, the validation that draft must pass, and the writer that
//! turns a validated draft into a definition pack on disk. Nothing reaches
//! `--out` before validation, including the loader round trip, passes.

// The interview command wires these; until then only their tests use them.
#[cfg_attr(not(test), allow(dead_code))]
mod dossier;
#[cfg_attr(not(test), allow(dead_code))]
mod draft;
#[cfg_attr(not(test), allow(dead_code))]
mod validate;
#[cfg_attr(not(test), allow(dead_code))]
mod write;
