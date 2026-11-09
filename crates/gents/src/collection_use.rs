//! What on a node still names an application collection, for the tools that
//! empty or remove one. It joins configuration references, installed pack
//! declarations and P2P pairings, so it sits outside those layers.

use std::path::Path;

use anyhow::{Context, Result};

use crate::store::config_client::ConfigAccess;

use crate::documents::ConfigReferences;

/// One thing that names an application collection, and the call that releases it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct CollectionUser {
    pub(crate) user: String,
    pub(crate) release_with: String,
}

const PAIRINGS: [&str; 3] = [
    "PeerPairingDesired",
    "DataPlanePairingDesired",
    "PeerPairingApplied",
];

/// Everything `access` can see on node `node_did` that names application
/// collection `name`: datastore surface entries, the pages granted those
/// surfaces, event sources, installed packs whose SDL declares it, and P2P
/// pairings that replicate it. Empty means nothing does.
///
/// Reads run as the actor of `access`, so a document its policy hides from
/// that actor is not reported. Pack declarations are read from the pack store
/// under `home`; an installed pack whose archive cannot be read is reported
/// as a user, since what it declares is unknown.
pub(crate) async fn application_collection_users(
    access: &ConfigAccess,
    home: Option<&Path>,
    node_did: &str,
    name: &str,
) -> Result<Vec<CollectionUser>> {
    let (surfaces, sources, pairings) = access
        .transact_readonly("collection_use.read", |txn| {
            Box::pin(async move {
                let (surfaces, sources) = ConfigReferences::load_in_txn(txn, node_did)
                    .await?
                    .application_collection_users(name)?;
                let mut pairings = Vec::new();
                for pairing in PAIRINGS {
                    let response = txn
                        .execute(&format!("{{ {pairing} {{ peer_id collections }} }}"))
                        .await?;
                    for row in response["data"][pairing]
                        .as_array()
                        .with_context(|| format!("{pairing} query did not return rows"))?
                    {
                        let replicated = row["collections"]
                            .as_array()
                            .is_some_and(|names| names.iter().any(|value| value == name));
                        if replicated {
                            pairings.push(row["peer_id"].as_str().unwrap_or_default().to_owned());
                        }
                    }
                }
                pairings.sort();
                pairings.dedup();
                anyhow::Ok((surfaces, sources, pairings))
            })
        })
        .await?;

    let installed: std::collections::BTreeMap<String, String> =
        crate::pack::list_installed_packs(access, node_did)
            .await?
            .into_iter()
            .map(|pack| (pack.coordinate, pack.digest))
            .collect();
    let mut users = Vec::new();
    if let Some(home) = home {
        let home = home.to_owned();
        let granted: Vec<String> = surfaces.keys().cloned().collect();
        let declared_in = installed.clone();
        let name = name.to_owned();
        let (pages, packs) = tokio::task::spawn_blocking(move || {
            let mut pages = Vec::new();
            let mut declared_in = declared_in;
            for record in crate::pack::list_readable_home_installs(&home)? {
                for page in &record.ux {
                    for grant in &page.data {
                        if granted.contains(&grant.surface) {
                            pages.push((
                                format!("{}/{}", record.coordinate, page.name),
                                grant.surface.clone(),
                                record.coordinate.clone(),
                            ));
                        }
                    }
                }
                declared_in
                    .entry(record.coordinate)
                    .or_insert(record.digest);
            }
            let mut packs = Vec::new();
            for (coordinate, digest) in &declared_in {
                match crate::pack::declared_collections(&home, digest) {
                    Ok(declared) if declared.contains(&name) => {
                        packs.push((coordinate.clone(), None));
                    }
                    Ok(_) => {}
                    Err(error) => packs.push((coordinate.clone(), Some(format!("{error:#}")))),
                }
            }
            anyhow::Ok((pages, packs))
        })
        .await
        .context("reading installed pack declarations")??;
        for (page, surface, coordinate) in pages {
            users.push(CollectionUser {
                user: format!("page {page} data grant on datastore surface {surface:?}"),
                release_with: format!(r#"config ["pack","remove","{coordinate}"]"#),
            });
        }
        for (coordinate, unreadable) in packs {
            users.push(CollectionUser {
                user: match unreadable {
                    None => format!("installed pack {coordinate} declares it"),
                    Some(reason) => format!(
                        "installed pack {coordinate}: its declared collections cannot be read ({reason})"
                    ),
                },
                release_with: format!(
                    r#"config ["pack","remove","{coordinate}","--with-collections"]"#
                ),
            });
        }
    } else {
        for coordinate in installed.keys() {
            users.push(CollectionUser {
                user: format!(
                    "installed pack {coordinate}: its declared collections cannot be read without the node home"
                ),
                release_with: format!(r#"config ["pack","remove","{coordinate}"]"#),
            });
        }
    }
    for (surface, tools) in surfaces {
        users.push(CollectionUser {
            user: format!("datastore surface {surface:?} entries {tools:?}"),
            release_with: format!(
                r#"config ["cleanup","preview","--target","datastore={surface}"], or update the surface to drop those entries"#
            ),
        });
    }
    for source in sources {
        users.push(CollectionUser {
            user: format!("event source {source:?}"),
            release_with: format!(
                r#"config ["cleanup","preview","--target","event-source={source}"]"#
            ),
        });
    }
    for peer in pairings {
        users.push(CollectionUser {
            user: format!("P2P pairing with peer {peer:?} replicates it"),
            release_with: r#"p2p ["help"]: end the pairing or stop replicating the collection"#
                .to_owned(),
        });
    }
    Ok(users)
}

/// The collections the SDL of installed pack `coordinate` declares, read from
/// the pack store under `home`.
pub(crate) async fn pack_declared_collections(
    access: &ConfigAccess,
    home: &Path,
    node_did: &str,
    coordinate: &str,
) -> Result<std::collections::BTreeSet<String>> {
    let digest = crate::pack::list_installed_packs(access, node_did)
        .await?
        .into_iter()
        .find(|pack| pack.coordinate == coordinate)
        .with_context(|| format!("{coordinate} is not installed; see pack list"))?
        .digest;
    let home = home.to_owned();
    tokio::task::spawn_blocking(move || crate::pack::declared_collections(&home, &digest))
        .await
        .context("reading the pack's declared collections")?
}

/// What became of one collection a removed pack declared.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct CollectionCleanup {
    pub(crate) collection: String,
    pub(crate) removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) documents_removed: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) kept_for: Vec<CollectionUser>,
}

/// Empties and removes each existing application collection in `declared`
/// that nothing on node `node_did` still names, as the actor of `access`;
/// the rest are kept and reported with their users. Gents-managed
/// collections are never candidates. Ownership of pack-declared collections
/// is discussed in gents-ai/gents#3572.
pub(crate) async fn remove_unused_collections(
    access: &ConfigAccess,
    home: Option<&Path>,
    node_did: &str,
    declared: impl IntoIterator<Item = String>,
) -> Result<Vec<CollectionCleanup>> {
    let existing = access.collection_names().await?;
    let mut report = Vec::new();
    let mut removable = Vec::new();
    for collection in declared {
        if !existing.contains(&collection)
            || crate::store::schema::migration::DEFAULT_REGISTRY
                .managed_names()
                .any(|managed| managed == collection)
        {
            continue;
        }
        let kept_for = application_collection_users(access, home, node_did, &collection).await?;
        if kept_for.is_empty() {
            let rows = access
                .execute(&format!("{{ {collection} {{ _docID }} }}"))
                .await?;
            let documents = rows["data"][&collection]
                .as_array()
                .context("document count query did not return rows")?
                .len();
            access.truncate_collection(&collection).await?;
            removable.push(collection.clone());
            report.push(CollectionCleanup {
                collection,
                removed: true,
                documents_removed: Some(documents),
                kept_for,
            });
        } else {
            report.push(CollectionCleanup {
                collection,
                removed: false,
                documents_removed: None,
                kept_for,
            });
        }
    }
    if !removable.is_empty() {
        access.remove_collections(&removable).await?;
    }
    Ok(report)
}
