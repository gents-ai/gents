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
/// as a user, since what it declares is unknown. `ignoring_pack` leaves out
/// one pack's own declaration and pages: the pack whose removal is asking.
pub(crate) async fn application_collection_users(
    access: &ConfigAccess,
    home: Option<&Path>,
    node_did: &str,
    name: &str,
    ignoring_pack: Option<&str>,
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
            .filter(|pack| Some(pack.coordinate.as_str()) != ignoring_pack)
            .map(|pack| (pack.coordinate, pack.digest))
            .collect();
    let mut users = Vec::new();
    if let Some(home) = home {
        let home = home.to_owned();
        let granted: Vec<String> = surfaces.keys().cloned().collect();
        let declared_in = installed.clone();
        let name = name.to_owned();
        let ignored = ignoring_pack.map(str::to_owned);
        let (pages, packs) = tokio::task::spawn_blocking(move || {
            let mut pages = Vec::new();
            let mut declared_in = declared_in;
            for record in crate::pack::list_readable_home_installs(&home)? {
                if Some(&record.coordinate) == ignored.as_ref() {
                    continue;
                }
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
                release_with: match coordinate
                    .strip_prefix(crate::documents::PLUGIN_SOURCE_NAMESPACE)
                    .and_then(|name| name.strip_prefix('/'))
                {
                    Some(source) => format!(
                        r#"config ["plugin","build","{source}"] with a set.source whose data.collections no longer names it: a plugin built on this node is rebuilt, not removed with its collections"#
                    ),
                    None => format!(
                        r#"config ["pack","remove","{coordinate}","--with-collections"]"#
                    ),
                },
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

/// The collections of one installed pack: the ones its SDL declares, read
/// from the pack store under the node home, and the ones its install created
/// there, read from the pack's home record. A collection that was already on
/// the node when the pack was installed is declared but not created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PackCollections {
    pub(crate) coordinate: String,
    pub(crate) declared: std::collections::BTreeSet<String>,
    pub(crate) created: std::collections::BTreeSet<String>,
}

pub(crate) async fn pack_collections(
    access: &ConfigAccess,
    home: &Path,
    node_did: &str,
    coordinate: &str,
) -> Result<PackCollections> {
    let digest = crate::pack::list_installed_packs(access, node_did)
        .await?
        .into_iter()
        .find(|pack| pack.coordinate == coordinate)
        .with_context(|| format!("{coordinate} is not installed; see pack list"))?
        .digest;
    let home = home.to_owned();
    let coordinate = coordinate.to_owned();
    tokio::task::spawn_blocking(move || {
        Ok(PackCollections {
            declared: crate::pack::declared_collections(&home, &digest)?,
            created: crate::pack::read_home_install(&home, &coordinate)?
                .map(|record| record.created_collections.into_iter().collect())
                .unwrap_or_default(),
            coordinate,
        })
    })
    .await
    .context("reading the pack's declared collections")?
}

/// What removing a pack with its collections does to one collection it
/// declares: removed with `documents` documents, or kept for `kept_for`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct CollectionCleanupPlan {
    pub(crate) collection: String,
    pub(crate) removable: bool,
    /// The documents the calling actor can read; removal takes every document.
    pub(crate) documents: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) kept_for: Vec<CollectionUser>,
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

/// The collection a relation field's kind points at, as DefraDB records it.
fn relation_target(field: &serde_json::Value) -> Option<String> {
    match serde_json::from_value::<schema::FieldKind>(field.get("Kind")?.clone()).ok()? {
        schema::FieldKind::Relation { collection_id, .. } => Some(collection_id),
        schema::FieldKind::Named { name, .. } => Some(name),
        _ => None,
    }
}

/// Every refusal condition for removing the existing application collections
/// `pack` declares, read before anything is changed: a collection the pack's
/// install did not create is kept, as is one something on node `node_did`
/// other than the pack still names, and one a collection that stays has a
/// field relating to (the condition DefraDB's own removal refuses on, see
/// [`ConfigAccess::remove_collections`]). Gents-managed collections are never
/// candidates. The pack's own documents are still on the node when a preview
/// asks, so a user among them is reported until the removal takes it.
pub(crate) async fn collection_cleanup_plan(
    access: &ConfigAccess,
    home: Option<&Path>,
    node_did: &str,
    pack: &PackCollections,
) -> Result<Vec<CollectionCleanupPlan>> {
    let existing = access.collection_names().await?;
    let mut plan = Vec::new();
    for collection in &pack.declared {
        if !existing.contains(collection)
            || crate::store::schema::migration::DEFAULT_REGISTRY
                .managed_names()
                .any(|managed| managed == collection)
        {
            continue;
        }
        let mut kept_for = Vec::new();
        if !pack.created.contains(collection) {
            kept_for.push(CollectionUser {
                user: format!(
                    "installing {} did not create it: it was already on the node, or the install left no record of creating it",
                    pack.coordinate
                ),
                release_with: format!(r#"schema ["collection","preview","remove","{collection}"]"#),
            });
        }
        kept_for.extend(
            application_collection_users(
                access,
                home,
                node_did,
                collection,
                Some(&pack.coordinate),
            )
            .await?,
        );
        let rows = access
            .execute(&format!("{{ {collection} {{ _docID }} }}"))
            .await?;
        let documents = rows["data"][collection]
            .as_array()
            .context("document count query did not return rows")?
            .len();
        plan.push(CollectionCleanupPlan {
            collection: collection.clone(),
            removable: kept_for.is_empty(),
            documents,
            kept_for,
        });
    }
    let versions = access.collection_versions().await?;
    let names_of = |target: &str| -> Vec<&str> {
        versions
            .iter()
            .filter(|version| {
                ["CollectionID", "VersionID", "Name"]
                    .iter()
                    .any(|key| version[*key].as_str() == Some(target))
            })
            .filter_map(|version| version["Name"].as_str())
            .collect()
    };
    let relations: Vec<(&str, &str, Vec<&str>)> = versions
        .iter()
        .flat_map(|version| {
            let holder = version["Name"].as_str().unwrap_or_default();
            version["Fields"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |field| {
                    Some((
                        holder,
                        field["Name"].as_str().unwrap_or_default(),
                        relation_target(field)?,
                    ))
                })
        })
        .map(|(holder, field, target)| (holder, field, names_of(&target)))
        .collect();
    loop {
        let staying = |name: &str| {
            !plan
                .iter()
                .any(|entry| entry.removable && entry.collection == name)
        };
        let held: Vec<(usize, CollectionUser)> = plan
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.removable)
            .filter_map(|(index, entry)| {
                let (holder, field, _) = relations.iter().find(|(holder, _, targets)| {
                    *holder != entry.collection
                        && staying(holder)
                        && targets.contains(&entry.collection.as_str())
                })?;
                Some((
                    index,
                    CollectionUser {
                        user: format!("collection {holder} field {field:?} relates to it"),
                        release_with: format!(
                            r#"schema ["collection","preview","remove","{holder}"]"#
                        ),
                    },
                ))
            })
            .collect();
        if held.is_empty() {
            break;
        }
        for (index, user) in held {
            plan[index].removable = false;
            plan[index].kept_for.push(user);
        }
    }
    Ok(plan)
}

/// Empties and removes each collection [`collection_cleanup_plan`] finds
/// removable, as the actor of `access`; the rest are kept and reported with
/// what keeps them. The plan is complete before the first document is
/// removed, so a refusal costs no data. Ownership of pack-declared
/// collections is discussed in gents-ai/gents#3572.
pub(crate) async fn remove_unused_collections(
    access: &ConfigAccess,
    home: Option<&Path>,
    node_did: &str,
    pack: &PackCollections,
) -> Result<Vec<CollectionCleanup>> {
    let plan = collection_cleanup_plan(access, home, node_did, pack).await?;
    let removable: Vec<String> = plan
        .iter()
        .filter(|entry| entry.removable)
        .map(|entry| entry.collection.clone())
        .collect();
    for collection in &removable {
        access.truncate_collection(collection).await?;
    }
    if !removable.is_empty() {
        access.remove_collections(&removable).await?;
    }
    Ok(plan
        .into_iter()
        .map(|entry| CollectionCleanup {
            collection: entry.collection,
            removed: entry.removable,
            documents_removed: entry.removable.then_some(entry.documents),
            kept_for: entry.kept_for,
        })
        .collect())
}
