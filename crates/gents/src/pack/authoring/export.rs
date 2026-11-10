//! Turns the configuration a node owns into a manifest root on disk
//! (`gents config export`).

mod root;
mod snapshot;

pub use root::{document_handle, write_manifest_root};
pub use snapshot::{read_owned_config_in_txn, sort_document_rows};
