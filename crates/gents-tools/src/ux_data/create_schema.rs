use std::path::Path;

use gents_pack::plugin::ux::UxListing;

#[derive(Debug, thiserror::Error)]
pub enum UxCreateSchemaError {
    #[error("page {page:?} cannot sign writes to {collection:?}: {reason}")]
    Refused {
        page: String,
        collection: String,
        reason: UxCreateSchemaRefusal,
    },
    #[error("page {page:?} cannot sign writes to {collection:?}: reading its trusted pack failed: {source}")]
    Read {
        page: String,
        collection: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("page {page:?} cannot sign writes to {collection:?}: reading its trusted pack did not finish: {source}")]
    Task {
        page: String,
        collection: String,
        #[source]
        source: tokio::task::JoinError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum UxCreateSchemaRefusal {
    #[error("installed pack content does not match its recorded digest for {pack:?}; reinstall that pack")]
    DigestMismatch { pack: String },
    #[error("installed UX record does not match its trusted pack {pack:?}; reinstall that pack")]
    RecordMismatch { pack: String },
    #[error(
        "installed UX declaration does not match its trusted pack {pack:?}; reinstall that pack"
    )]
    DeclarationMismatch { pack: String },
    #[error("the page has no owning pack; reinstall it from its owning pack")]
    NoPack,
    #[error("its pack {pack:?} is not installed; reinstall that pack")]
    PackNotInstalled { pack: String },
    #[error("the trusted archive for its pack {pack:?} is missing; reinstall that pack")]
    ArchiveMissing { pack: String },
    #[error("its pack {pack:?} declares no schema for this collection; declare it in that pack with one collection per schema file, then rebuild and reinstall the pack")]
    CollectionNotDeclared { pack: String },
    #[error("schema file {path:?} in its pack {pack:?} declares {count} collections; signing needs one collection per schema file, so split the file, then rebuild and reinstall the pack")]
    MultipleCollections {
        pack: String,
        path: String,
        count: usize,
    },
    #[error("its pack {pack:?} declares this collection in both {first:?} and {second:?}; keep one schema declaration, then rebuild and reinstall the pack")]
    DuplicateCollection {
        pack: String,
        first: String,
        second: String,
    },
}

pub(super) async fn trusted_ux_create_schema(
    home: &Path,
    listing: &UxListing,
    collection: &str,
) -> Result<String, UxCreateSchemaError> {
    let home = home.to_owned();
    let task_listing = listing.clone();
    let task_collection = collection.to_owned();
    tokio::task::spawn_blocking(move || resolve_schema(&home, &task_listing, &task_collection))
        .await
        .map_err(|source| UxCreateSchemaError::Task {
            page: listing.id.clone(),
            collection: collection.to_owned(),
            source,
        })?
}

/// A packaged page signs only against its own verified archive. A collection
/// present on the node or in a different pack does not supply that authority.
/// Archive integrity and the installed UX declaration are checked before its
/// schema is selected; no refusal falls back to an unsigned mutation.
fn resolve_schema(
    home: &Path,
    listing: &UxListing,
    collection: &str,
) -> Result<String, UxCreateSchemaError> {
    let refused = |reason| UxCreateSchemaError::Refused {
        page: listing.id.clone(),
        collection: collection.to_owned(),
        reason,
    };
    let read = |source| UxCreateSchemaError::Read {
        page: listing.id.clone(),
        collection: collection.to_owned(),
        source,
    };
    let coordinate = listing
        .pack
        .as_deref()
        .ok_or_else(|| refused(UxCreateSchemaRefusal::NoPack))?;
    let record = gents_pack::pack::read_home_install(home, coordinate)
        .map_err(|error| read(error.into()))?
        .ok_or_else(|| {
            refused(UxCreateSchemaRefusal::PackNotInstalled {
                pack: coordinate.to_owned(),
            })
        })?;
    let store = gents_pack::pack::store::PackStore::new(home);
    if !store
        .path(&record.digest)
        .map_err(|error| read(error.into()))?
        .is_file()
    {
        return Err(refused(UxCreateSchemaRefusal::ArchiveMissing {
            pack: coordinate.to_owned(),
        }));
    }
    let archive = store
        .open(&record.digest)
        .map_err(|error| read(error.into()))?;
    let manifest_bytes = archive
        .asset("manifest.json")
        .map_err(|error| read(error.into()))?
        .to_vec();
    let manifest = gents_pack::pack::PackManifest::from_slice(&manifest_bytes)
        .map_err(|error| read(error.into()))?;
    let mut schemas = std::collections::BTreeMap::new();
    for path in gents_pack::pack::document_pack_schema_paths(&manifest)
        .map_err(|error| read(error.into()))?
    {
        schemas.insert(
            path.to_owned(),
            archive
                .asset(path)
                .map_err(|error| read(error.into()))?
                .to_vec(),
        );
    }
    let digest = gents_pack::pack::digest_declared_assets(&manifest, |path| {
        if path == "manifest.json" {
            Ok(manifest_bytes.as_slice())
        } else if let Some(bytes) = schemas.get(path) {
            Ok(bytes.as_slice())
        } else {
            archive.asset(path)
        }
    })
    .map_err(|error| read(error.into()))?;
    if digest != record.digest {
        return Err(refused(UxCreateSchemaRefusal::DigestMismatch {
            pack: coordinate.to_owned(),
        }));
    }
    if format!("{}/{}", manifest.metadata.namespace, manifest.name) != coordinate
        || manifest.version != record.version
        || manifest.metadata.kind != record.kind
    {
        return Err(refused(UxCreateSchemaRefusal::RecordMismatch {
            pack: coordinate.to_owned(),
        }));
    }
    if !manifest.metadata.ux.iter().any(|entry| {
        format!("{coordinate}/{}", entry.name) == listing.id && entry.data == listing.data
    }) {
        return Err(refused(UxCreateSchemaRefusal::DeclarationMismatch {
            pack: coordinate.to_owned(),
        }));
    }
    let mut found: Option<(String, String)> = None;
    for (path, bytes) in &schemas {
        let sdl = std::str::from_utf8(bytes).map_err(|error| read(error.into()))?;
        let parsed = query::parse_sdl(sdl).map_err(|error| read(error.into()))?;
        if parsed.iter().any(|schema| schema.name == collection) {
            if parsed.len() != 1 {
                return Err(refused(UxCreateSchemaRefusal::MultipleCollections {
                    pack: coordinate.to_owned(),
                    path: path.clone(),
                    count: parsed.len(),
                }));
            }
            if let Some((first, _)) = found {
                return Err(refused(UxCreateSchemaRefusal::DuplicateCollection {
                    pack: coordinate.to_owned(),
                    first,
                    second: path.clone(),
                }));
            }
            found = Some((path.clone(), sdl.to_owned()));
        }
    }
    found.map(|(_, sdl)| sdl).ok_or_else(|| {
        refused(UxCreateSchemaRefusal::CollectionNotDeclared {
            pack: coordinate.to_owned(),
        })
    })
}
