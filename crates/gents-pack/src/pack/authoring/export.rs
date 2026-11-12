//! Turns configuration that is live on a node into files: the whole owned
//! configuration as a manifest root (`gents config export`), or chosen
//! documents and what they reference as a pack (`pack export`).

mod pack;
mod root;
mod schema;
mod select;
mod snapshot;

pub use pack::{export_pack, ExportReport, PulledByReference};
pub use root::{document_handle, write_manifest_root};
pub use schema::collection_sdl;
pub use select::{selectable_kind, ExportSelection, SELECTABLE_KINDS};
pub use snapshot::{read_owned_config_in_txn, sort_document_rows};

#[cfg(test)]
mod pushdown_tests;
