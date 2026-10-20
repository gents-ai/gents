//! Semantic retrieval over canonical transcripts: a derived, node-local
//! `TranscriptChunk` projection embedded by DefraDB and searched through its
//! DiskANN index (`Proofs/Retrieval`).

mod chunk;
pub mod embedding;
pub mod indexer;
pub mod plan;
pub(crate) mod search;

#[cfg(test)]
mod evals;
#[cfg(test)]
pub(crate) mod tests;
