//! What `pack install --preview` reports so a person can review an install
//! before it writes: the pack's identity, the documents it would write, the
//! collections its schemas declare, what its plugins ask of the host and the
//! inference its slots need. Each is read from its owner; nothing here decides
//! whether the install may go on.

use anyhow::{Context, Result};
use gents::pack::{PackInferenceBindingPreview, PackInferenceBindings, PackManifest};
use serde_json::{json, Value};

/// One pack's inference as its install would bind it, and the required slots
/// that binding still leaves out.
pub(super) struct PackInference {
    coordinate: String,
    preview: PackInferenceBindingPreview,
    unbound: Vec<String>,
}

impl PackInference {
    pub(super) fn bound(manifest: &PackManifest, preview: PackInferenceBindingPreview) -> Self {
        Self {
            coordinate: coordinate(manifest),
            preview,
            unbound: Vec::new(),
        }
    }

    pub(super) fn is_bound(&self) -> bool {
        self.unbound.is_empty()
    }
}

fn coordinate(manifest: &PackManifest) -> String {
    format!("{}/{}", manifest.metadata.namespace, manifest.name)
}

/// `manifest`'s inference without refusing what is still unbound. When the
/// install's own owner binds it, that binding (with any it chose itself) is
/// the answer. When it refuses because required slots are missing, the
/// non-enforcing owner reports the slots and profiles, and those slots are
/// unbound. Any other refusal is the install's own: `None`.
pub(super) async fn pack_inference(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    manifest: &PackManifest,
    requested: &PackInferenceBindings,
) -> Option<PackInference> {
    // A binding names a slot of the pack or of any of its dependencies.
    let requested: PackInferenceBindings = requested
        .iter()
        .filter(|(slot, _)| {
            manifest
                .metadata
                .inference_slots
                .iter()
                .any(|declared| &declared.name == *slot)
        })
        .map(|(slot, profile)| (slot.clone(), profile.clone()))
        .collect();
    if let Ok(preview) =
        gents::pack::preview_pack_inference_bindings(access, manifest, owner, &requested).await
    {
        return Some(PackInference::bound(manifest, preview));
    }
    let preview = gents::pack::inspect_pack_inference_bindings(access, manifest, owner, &requested)
        .await
        .ok()?;
    let unbound: Vec<String> = preview
        .slots
        .iter()
        .filter(|slot| !slot.optional && !requested.contains_key(&slot.name))
        .map(|slot| slot.name.clone())
        .collect();
    (!unbound.is_empty()).then(|| PackInference {
        coordinate: coordinate(manifest),
        preview,
        unbound,
    })
}

/// The inference the pack and its dependencies need, by slot name, since one
/// binding serves every pack that declares the slot: each slot with the
/// profile bound to it (and whether the install chose it itself), the node's
/// profiles, and the required slots still unbound. A slot one pack binds by
/// itself and another leaves unbound is unbound.
pub(super) fn inference_needs(packs: &[PackInference]) -> Value {
    struct Slot<'a> {
        name: &'a str,
        description: &'a str,
        optional: bool,
        packs: Vec<&'a str>,
        profile: Option<&'a str>,
        automatic: bool,
        unbound: bool,
    }
    let mut slots: Vec<Slot> = Vec::new();
    for pack in packs {
        for declared in &pack.preview.slots {
            let profile = pack
                .preview
                .bindings
                .get(&declared.name)
                .map(String::as_str);
            let automatic = pack.preview.automatic && profile.is_some();
            let unbound = pack.unbound.contains(&declared.name);
            match slots.iter_mut().find(|slot| slot.name == declared.name) {
                Some(slot) => {
                    slot.optional &= declared.optional;
                    slot.automatic &= automatic;
                    slot.unbound |= unbound;
                    slot.packs.push(&pack.coordinate);
                }
                None => slots.push(Slot {
                    name: &declared.name,
                    description: &declared.description,
                    optional: declared.optional,
                    packs: vec![&pack.coordinate],
                    profile,
                    automatic,
                    unbound,
                }),
            }
        }
    }
    json!({
        "slots": slots.iter().map(|slot| {
            let profile = slot.profile.filter(|_| !slot.unbound);
            json!({
                "name": slot.name,
                "description": slot.description,
                "optional": slot.optional,
                "packs": slot.packs,
                "profile": profile,
                "automatic": slot.automatic && profile.is_some(),
            })
        }).collect::<Vec<_>>(),
        "profiles": packs.first().map_or(&[][..], |pack| &pack.preview.profiles),
        "unbound": slots.iter().filter(|slot| slot.unbound).map(|slot| slot.name).collect::<Vec<_>>(),
    })
}

/// The pack's coordinate, version and description, and what each of its
/// plugins asks for beyond a sealed sandbox, worded by the authority owner.
/// An install that would newly grant any of it is refused without
/// `--grant-authority`; one already granted that much installs without it.
pub(super) fn identity_and_authority(manifest: &PackManifest) -> Result<Value> {
    let mut authority = Vec::new();
    for plugin in &manifest.metadata.plugins {
        let declared = gents::plugin::authority::declared_manifold(plugin)?;
        if let Some(asks) = gents::plugin::authority::describe_plugin_authority(plugin, &declared) {
            authority.push(json!({ "plugin": plugin.name, "asks": asks }));
        }
    }
    Ok(json!({
        "coordinate": format!("{}/{}", manifest.metadata.namespace, manifest.name),
        "version": manifest.version,
        "description": manifest.description,
        "authority": authority,
    }))
}

/// Each document the prepared configuration would write, by collection, with
/// its logical id, the name it shows and its content (the canonical document
/// the install writes, without the fields it leaves unset), and for a data
/// tool surface the tools it declares. The node document is the install's
/// owner, not something the pack adds.
pub(super) fn documents(config: &gents::documents::PackConfig) -> Result<Vec<Value>> {
    let plan = gents::store::config_client::DesiredStateApplyPlan::from_pack_config(config)?;
    plan.documents()
        .iter()
        .filter(|document| document.collection != gents::Collection::Node)
        .map(|document| {
            let field = |key: &str| document.add.get(key).and_then(Value::as_str);
            let id = field(document.collection.unique_field());
            let mut item = json!({
                "collection": document.collection.graphql_type(),
                "id": id,
                "name": field("display_name").or_else(|| field("name")),
                "content": content(document.collection, &document.add)?,
            });
            if document.collection == gents::Collection::DatastoreToolSurface {
                let tools: Vec<&str> = config
                    .datastore_tool_surfaces
                    .iter()
                    .filter(|surface| Some(surface.surface_id.as_str()) == id)
                    .flat_map(|surface| surface.entries.iter().flatten())
                    .map(|entry| entry.tool_name())
                    .collect();
                item["tools"] = json!(tools);
            }
            Ok(item)
        })
        .collect()
}

/// A document's set fields, with a backend's stored secret replaced: a pack
/// may carry the secret its backend authenticates with, which the review
/// shows only by kind.
fn content(collection: gents::Collection, document: &Value) -> Result<Value> {
    let mut content = document.clone();
    let fields = content
        .as_object_mut()
        .context("a configuration document is an object")?;
    fields.retain(|_, value| !value.is_null());
    if collection == gents::Collection::InferenceBackend {
        if let Some(auth) = fields.get_mut("auth") {
            let stored: gents::documents::BackendAuth = serde_json::from_value(auth.clone())
                .context("decode the backend's authentication")?;
            if stored.is_stored_secret() {
                if let Some(auth) = auth.as_object_mut() {
                    for (key, value) in auth.iter_mut() {
                        if key != "kind" {
                            *value = json!("[redacted]");
                        }
                    }
                }
            }
        }
    }
    Ok(content)
}

/// The collections the pack's schemas declare, as DefraDB's SDL parser reads
/// them, or `None` when the pack carries schema patches: a patch changes
/// collections it does not declare, so the declared ones would not be the
/// whole of what it touches.
pub(super) fn collections(
    manifest: &PackManifest,
    asset: impl Fn(&str) -> Result<Vec<u8>>,
) -> Result<Option<Vec<String>>> {
    let Ok(paths) = gents::pack::document_pack_schema_paths(manifest) else {
        return Ok(None);
    };
    let mut names = Vec::new();
    for path in paths {
        let bytes = asset(path)?;
        let sdl = std::str::from_utf8(&bytes).with_context(|| format!("schema {path:?}"))?;
        for collection in query::parse_sdl(sdl).with_context(|| format!("schema {path:?}"))? {
            names.push(collection.name);
        }
    }
    names.sort();
    names.dedup();
    Ok(Some(names))
}

/// The collections the pack's schemas declare that the node already has with
/// the same declaration, which the install shares rather than creates, as the
/// schema owner's read-only preview finds them. `None` when that can't be
/// known before the install: the pack carries schema patches, or the owner
/// refuses a schema, which the install then reports.
pub(super) async fn shared_collections(
    access: &gents::store::config_client::ConfigAccess,
    manifest: &PackManifest,
    asset: impl Fn(&str) -> Result<Vec<u8>>,
) -> Option<Vec<String>> {
    let mut shared = Vec::new();
    for path in gents::pack::document_pack_schema_paths(manifest).ok()? {
        let bytes = asset(path).ok()?;
        let sdl = std::str::from_utf8(&bytes).ok()?;
        let plan = gents::store::config_client::preview_schema_install(access, sdl)
            .await
            .ok()?;
        shared.extend(plan.field_deltas.into_keys());
    }
    shared.sort();
    shared.dedup();
    Some(shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::pack::archive::PackArchive;

    fn fixture(name: &str) -> PackArchive {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gents/tests/fixtures/packs")
            .join(name);
        let (bytes, _) = gents::pack::archive::pack_dir(&root).unwrap();
        PackArchive::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn a_documents_pack_reports_what_it_would_write() {
        let archive = fixture("documents_fixture");
        let manifest = archive.manifest();
        let identity = identity_and_authority(manifest).unwrap();
        assert_eq!(identity["coordinate"], "fixture/documents_fixture");
        assert_eq!(identity["version"], "1.0.0");
        assert_eq!(identity["authority"], json!([]));
        let asset = |path: &str| Ok(archive.asset(path)?.to_vec());
        assert_eq!(
            collections(manifest, asset).unwrap(),
            Some(vec!["FixtureJob".to_owned()])
        );
        let config = gents::pack::load_pack_config(
            manifest,
            &gents::pack::PackInstallOptions {
                node_did: "did:key:zReviewOwner".into(),
            },
            &|path| Ok(archive.asset(path)?.to_vec()),
            &|_| None,
        )
        .unwrap();
        let documents = documents(&config).unwrap();
        let named: Vec<Value> = documents.iter().map(without_content).collect();
        assert!(named.contains(
            &json!({"collection": "Agent", "id": "fixture-worker", "name": "Fixture worker"})
        ));
        assert!(named.contains(&json!({
            "collection": "Task", "id": "fixture-worker-task", "name": "fixture-worker-task"
        })));
        assert!(documents
            .iter()
            .all(|document| document["collection"] != "Node"));
        let agent = documents
            .iter()
            .find(|document| document["id"] == "fixture-worker")
            .unwrap();
        assert_eq!(agent["content"]["agent_id"], "fixture-worker");
        assert_eq!(agent["content"]["display_name"], "Fixture worker");
        assert!(agent["content"]
            .as_object()
            .unwrap()
            .values()
            .all(|value| !value.is_null()));
    }

    fn without_content(document: &Value) -> Value {
        let mut document = document.clone();
        document.as_object_mut().unwrap().remove("content");
        document
    }

    #[test]
    fn a_backend_shows_its_stored_secret_only_by_kind() {
        let backend = |auth: Value| {
            json!({
                "node_did": "did:key:zReviewOwner", "backend_id": "hosted", "name": "Hosted",
                "provider_kind": "openai", "endpoint": "https://api.example.com/v1",
                "auth": auth,
            })
        };
        let redacted = content(
            gents::Collection::InferenceBackend,
            &backend(json!({"kind": "api_key", "key": "sk-live-secret"})),
        )
        .unwrap();
        assert_eq!(
            redacted["auth"],
            json!({"kind": "api_key", "key": "[redacted]"})
        );
        assert_eq!(redacted["endpoint"], "https://api.example.com/v1");
        let aws = content(
            gents::Collection::InferenceBackend,
            &backend(json!({
                "kind": "aws_credentials", "access_key_id": "AKIA", "secret_access_key": "s",
            })),
        )
        .unwrap();
        assert!(!aws.to_string().contains("AKIA"));
        let named = backend(json!({"kind": "environment", "variable": "HOSTED_KEY"}));
        assert_eq!(
            content(gents::Collection::InferenceBackend, &named).unwrap()["auth"],
            named["auth"]
        );
    }

    #[test]
    fn a_data_tool_surface_reports_its_tools() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/ux-plugins/board_docs");
        let (bytes, _) = gents::pack::archive::pack_dir(&root).unwrap();
        let archive = PackArchive::from_bytes(&bytes).unwrap();
        let config = gents::pack::load_pack_config(
            archive.manifest(),
            &gents::pack::PackInstallOptions {
                node_did: "did:key:zReviewOwner".into(),
            },
            &|path| Ok(archive.asset(path)?.to_vec()),
            &|_| None,
        )
        .unwrap();
        let documents = documents(&config).unwrap();
        assert_eq!(
            documents.iter().map(without_content).collect::<Vec<_>>(),
            vec![json!({
                "collection": "DatastoreToolSurface", "id": "board", "name": "Board",
                "tools": ["list_tasks", "list_moves", "add_task", "move_task"],
            })]
        );
        assert_eq!(documents[0]["content"]["surface_id"], "board");
    }

    #[test]
    fn a_plugin_reports_the_authority_it_asks_for() {
        let mut manifest = fixture("documents_fixture").manifest().clone();
        manifest.metadata.plugins = vec![serde_json::from_value(json!({
            "name": "fetcher",
            "description": "Fetches",
            "artifact": "plugins/fetcher.afb",
            "language": "rust",
            "input_schema": {"type": "object"},
            "manifold": {
                "fs": "None", "net": {"OutboundHttp": ["example.com"]}, "env": "None",
                "crypto": false, "child_process": false
            },
        }))
        .unwrap()];
        let identity = identity_and_authority(&manifest).unwrap();
        assert_eq!(
            identity["authority"],
            json!([{"plugin": "fetcher", "asks": "HTTP to example.com"}])
        );
    }

    #[test]
    fn schema_patches_leave_the_collections_unknown() {
        let mut manifest = fixture("documents_fixture").manifest().clone();
        manifest
            .metadata
            .assets
            .push("schemas/fixture_job.patch.json".into());
        assert_eq!(collections(&manifest, |_| Ok(Vec::new())).unwrap(), None);
    }
}
