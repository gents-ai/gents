use super::*;
use crate::identity::KeyIdentity;
use gents_protocol::file_changes::{FileChangeImages, FileImage, FilePermissions, ImageFailure};
use gents_protocol::file_image_hydration::FileImageScope;
use gents_protocol::file_inventory::{
    FileChangeMetadata, FileImageMetadata, FileInventoryCursor, FileInventoryPage,
};
use gents_protocol::session_hydration::HydrationRequest;
use serde_json::Value;

struct Names {
    source: KeyIdentity,
    other: KeyIdentity,
}

impl Names {
    fn actor(&self, value: &Value) -> String {
        match value.as_str().unwrap() {
            "node-1" => self.source.did().to_owned(),
            "node-2" => self.other.did().to_owned(),
            name => name.to_owned(),
        }
    }

    fn admission(&self, value: &Value) -> FileInventoryAdmission {
        FileInventoryAdmission {
            request: HydrationRequest {
                request_key: text(&value["key"]),
                peer_id: text(&value["peer"]),
                requester_did: text(&value["requester"]),
                node_did: self.actor(&value["node"]),
                session_id: text(&value["session"]),
            },
            session_doc_id: document(&value["native_session"]),
        }
    }

    fn binding(&self, value: &Value) -> FileInventoryBinding {
        FileInventoryBinding {
            admission: self.admission(&value["admission"]),
            digest: text(&value["digest"]),
            coverage: serde_json::from_value(value["coverage"].clone()).unwrap(),
        }
    }

    fn entry(&self, value: &Value) -> FileInventoryEntry {
        FileInventoryEntry {
            tool_call_doc_id: document(&value["tool"]),
            request_doc_id: document(&value["request"]),
            scope: FileImageScope {
                requester_did: text(&value["scope"]["requester"]),
                node_did: self.actor(&value["scope"]["node"]),
                session_id: text(&value["scope"]["session"]),
            },
            projection_doc_id: document(&value["projection"]),
            observation_doc_id: document(&value["observation"]),
            change: FileChangeMetadata {
                root: text(&value["change"]["root"]),
                path: text(&value["change"]["path"]),
                before: metadata(&value["change"]["before"]),
                after: metadata(&value["change"]["after"]),
            },
        }
    }

    fn demand(&self, value: &Value) -> gents_protocol::file_inventory::FileInventoryPageDemand {
        gents_protocol::file_inventory::FileInventoryPageDemand {
            admission: self.admission(&value["admission"]),
            attempt_id: text(&value["attempt"]),
            cursor: (!value["cursor"].is_null()).then(|| FileInventoryCursor {
                binding: self.binding(&value["cursor"]["binding"]),
                offset: value["cursor"]["offset"].as_u64().unwrap(),
            }),
        }
    }

    fn page(&self, value: &Value) -> FileInventoryPage {
        FileInventoryPage {
            binding: self.binding(&value["binding"]),
            offset: value["offset"].as_u64().unwrap(),
            entries: value["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| self.entry(entry))
                .collect(),
            next: (!value["next"].is_null()).then(|| FileInventoryCursor {
                binding: self.binding(&value["next"]["binding"]),
                offset: value["next"]["offset"].as_u64().unwrap(),
            }),
        }
    }

    async fn reply(
        &self,
        value: &Value,
        demand: &gents_protocol::file_inventory::FileInventoryPageDemand,
    ) -> gents_protocol::file_inventory::FileInventoryReply {
        use gents_protocol::file_inventory::FileInventoryReply;
        if value["kind"] == "refused" {
            FileInventoryReply::Refused(
                sign_refusal(
                    &self.source,
                    demand,
                    serde_json::from_value(value["reason"].clone()).unwrap(),
                )
                .await
                .unwrap(),
            )
        } else {
            FileInventoryReply::Page(
                sign_current_page(
                    &self.source,
                    &demand.derive_request_key().unwrap(),
                    self.page(&value["page"]),
                    || std::future::ready(Ok(true)),
                )
                .await
                .unwrap(),
            )
        }
    }

    async fn receipt(&self, envelope: &Value) -> FileInventoryPageReceipt {
        let page = &envelope["page"];
        let mut receipt = FileInventoryPageReceipt {
            version: if envelope["purpose"] == "file_inventory_v2" {
                2
            } else {
                1
            },
            request_key: text(&envelope["request_key"]),
            signer_did: self.actor(&envelope["signer"]),
            page: FileInventoryPage {
                binding: self.binding(&page["binding"]),
                offset: page["offset"].as_u64().unwrap(),
                entries: page["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| self.entry(entry))
                    .collect(),
                next: (!page["next"].is_null()).then(|| FileInventoryCursor {
                    binding: self.binding(&page["next"]["binding"]),
                    offset: page["next"]["offset"].as_u64().unwrap(),
                }),
            },
            signature: Vec::new(),
        };
        let signer = if envelope["signer"] == "node-1" {
            &self.source
        } else {
            &self.other
        };
        receipt.signature = signer
            .sign(&receipt.signing_payload().unwrap())
            .await
            .unwrap();
        if envelope["signature_verified"] == false {
            receipt.signature[0] ^= 1;
        }
        receipt
    }

    async fn receiver(&self, value: &Value) -> FileInventoryReceiver {
        let mut pages = Vec::new();
        for page in value["pages"].as_array().unwrap() {
            pages.push(ReceivedInventoryPage {
                request_key: text(&page["request_key"]),
                receipt: self.receipt(&page["envelope"]).await,
            });
        }
        FileInventoryReceiver {
            admission: self.admission(&value["admission"]),
            pages,
        }
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap().to_owned()
}

// These labels exercise the metadata representation only. Native snapshot
// admission still binds database-created document IDs and current ACP owners.
fn document(value: &Value) -> String {
    format!("document-{:020}", value.as_u64().unwrap())
}

fn metadata(value: &Value) -> FileImageMetadata {
    match value["kind"].as_str().unwrap() {
        "missing" => FileImageMetadata::Missing,
        "file" => FileImageMetadata::File {
            content_hash: text(&value["content_hash"]),
        },
        "unavailable" => FileImageMetadata::Unavailable {
            reason: match value["reason"].as_str().unwrap() {
                "read_failed" => ImageFailure::ReadFailed,
                "changed_during_read" => ImageFailure::ChangedDuringRead,
                "too_large" => ImageFailure::TooLarge,
                "unsupported_entry" => ImageFailure::UnsupportedEntry,
                "mutation_incomplete" => ImageFailure::MutationIncomplete,
                reason => panic!("unrecognized modeled image failure: {reason}"),
            },
        },
        kind => panic!("unrecognized modeled image metadata: {kind}"),
    }
}

#[tokio::test]
async fn generated_receivers_follow_signed_inventory_prefixes() {
    let (directory, names) = tokio::task::spawn_blocking(|| {
        let directory = tempfile::tempdir().unwrap();
        let names = Names {
            source: KeyIdentity::load_or_create(directory.path().join("source.key"), None).unwrap(),
            other: KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap(),
        };
        (directory, names)
    })
    .await
    .unwrap();
    let snapshot: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = snapshot["file_inventory_receiver_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let receiver = names.receiver(&case["state"]).await;
        let receipt = names.receipt(&case["envelope"]).await;
        let result = receiver
            .receive(text(&case["request_key"]), receipt, &names.source)
            .await;
        let expected = &case["expected"];
        if expected.is_null() {
            assert!(
                result.is_err(),
                "{}: unexpectedly accepted an invalid prefix",
                case["name"]
            );
            continue;
        }
        let actual = result.unwrap_or_else(|error| panic!("{}: {error:#}", case["name"]));
        let prefix = actual.validated(&names.source).await.unwrap().unwrap();
        let validated = &expected["validated"];
        assert_eq!(
            prefix.binding,
            names.binding(&validated["binding"]),
            "{}",
            case["name"]
        );
        let entries: Vec<_> = validated["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| names.entry(entry))
            .collect();
        assert_eq!(prefix.entries, entries, "{}", case["name"]);
        assert_eq!(
            prefix.complete,
            expected["complete"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(
            prefix.known_inventory(),
            expected["known_inventory"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        let stored = serde_json::to_vec(&actual).unwrap();
        let restored: FileInventoryReceiver = serde_json::from_slice(&stored).unwrap();
        assert_eq!(
            restored.validated(&names.source).await.unwrap().unwrap(),
            prefix,
            "{}: reload",
            case["name"]
        );
    }
    tokio::task::spawn_blocking(move || drop(directory))
        .await
        .unwrap();
}

#[test]
fn metadata_wire_erases_and_rejects_image_bytes() {
    let image = FileImage::File {
        content_hash: "recorded-hash".into(),
        data: "recorded image bytes".into(),
        permissions: FilePermissions {
            readonly: false,
            unix_mode: Some(0o640),
        },
        links: Some(1),
    };
    let change = FileChangeImages {
        root: "workspace".into(),
        path: "src/main.rs".into(),
        before: image.clone(),
        after: FileImage::Missing,
    };
    let projection = FileChangeMetadata::from(&change);
    assert_eq!(
        serde_json::to_value(&projection).unwrap(),
        serde_json::json!({
            "root": "workspace", "path": "src/main.rs",
            "before": {"kind": "file", "content_hash": "recorded-hash"},
            "after": {"kind": "missing"},
        })
    );
    let mut different_bytes = change;
    if let FileImage::File { data, .. } = &mut different_bytes.before {
        *data = "different retained bytes".into();
    }
    assert_eq!(FileChangeMetadata::from(&different_bytes), projection);
    let image = serde_json::json!({"kind":"file", "content_hash":"hash", "data":"retained bytes"});
    assert!(serde_json::from_value::<FileImageMetadata>(image).is_err());
}

#[tokio::test]
async fn generated_issuance_withholds_revoked_authority() {
    let (directory, names) = tokio::task::spawn_blocking(|| {
        let directory = tempfile::tempdir().unwrap();
        let names = Names {
            source: KeyIdentity::load_or_create(directory.path().join("source.key"), None).unwrap(),
            other: KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap(),
        };
        (directory, names)
    })
    .await
    .unwrap();
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_issuance_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let envelope = names.receipt(&case["envelope"]).await;
        let signer = if case["envelope"]["signer"] == "node-1" {
            &names.source
        } else {
            &names.other
        };
        let observations = std::sync::atomic::AtomicUsize::new(0);
        let result = sign_current_page(signer, &envelope.request_key, envelope.page, || {
            let key = if observations.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                "before"
            } else {
                "after"
            };
            std::future::ready(Ok(case[key].as_bool().unwrap()))
        })
        .await;
        assert_eq!(
            result.is_ok(),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if let Ok(receipt) = result {
            let receiver = FileInventoryReceiver {
                admission: receipt.page.binding.admission.clone(),
                pages: vec![],
            };
            let received = receiver
                .receive(receipt.request_key.clone(), receipt, &names.source)
                .await
                .unwrap();
            assert!(received.validated(&names.source).await.unwrap().is_some());
        }
    }
    drop(names);
    tokio::fs::remove_dir_all(directory.path()).await.unwrap();
}

#[tokio::test]
async fn generated_demands_preserve_attempt_and_cursor_identity() {
    use gents_protocol::file_inventory::FileInventoryPageDemand;
    let (directory, names) = tokio::task::spawn_blocking(|| {
        let directory = tempfile::tempdir().unwrap();
        let names = Names {
            source: KeyIdentity::load_or_create(directory.path().join("source.key"), None).unwrap(),
            other: KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap(),
        };
        (directory, names)
    })
    .await
    .unwrap();
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_demand_cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    let decode = |value: &Value| FileInventoryPageDemand {
        admission: names.admission(&value["admission"]),
        attempt_id: text(&value["attempt"]),
        cursor: (!value["cursor"].is_null()).then(|| FileInventoryCursor {
            binding: names.binding(&value["cursor"]["binding"]),
            offset: value["cursor"]["offset"].as_u64().unwrap(),
        }),
    };
    let initial = decode(&cases[0]["demand"]);
    for case in cases {
        let demand = decode(&case["demand"]);
        assert_eq!(
            demand.validate_reference().is_ok(),
            case["well_formed"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if demand.validate_reference().is_ok() {
            let key = demand.derive_request_key().unwrap();
            let stored = serde_json::to_vec(&demand).unwrap();
            let restored: FileInventoryPageDemand = serde_json::from_slice(&stored).unwrap();
            assert_eq!(restored.derive_request_key().unwrap(), key);
            let value = &case["initial_page"];
            let page = FileInventoryPage {
                binding: names.binding(&value["binding"]),
                offset: value["offset"].as_u64().unwrap(),
                entries: value["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| names.entry(entry))
                    .collect(),
                next: (!value["next"].is_null()).then(|| FileInventoryCursor {
                    binding: names.binding(&value["next"]["binding"]),
                    offset: value["next"]["offset"].as_u64().unwrap(),
                }),
            };
            let mut receipt = FileInventoryPageReceipt {
                version: FILE_INVENTORY_RECEIPT_VERSION,
                request_key: key.clone(),
                signer_did: names.source.did().to_owned(),
                page,
                signature: Vec::new(),
            };
            assert_eq!(
                demand.validate_page_reference(&receipt).is_ok(),
                case["matches_initial_page"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
            if case["same_initial"] == true {
                receipt.signature = names
                    .source
                    .sign(&receipt.signing_payload().unwrap())
                    .await
                    .unwrap();
                let receiver = FileInventoryReceiver {
                    admission: demand.admission.clone(),
                    pages: vec![],
                };
                receiver
                    .receive_for_demand(&demand, receipt.clone(), &names.source)
                    .await
                    .unwrap();
                let mut another_attempt = demand.clone();
                another_attempt.attempt_id.push_str("-new");
                assert!(receiver
                    .receive_for_demand(&another_attempt, receipt, &names.source)
                    .await
                    .is_err());
            }

            assert_eq!(
                key == initial.derive_request_key().unwrap(),
                case["same_initial"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
        }
    }
    drop(names);
    tokio::fs::remove_dir_all(directory.path()).await.unwrap();
}

#[tokio::test]
async fn generated_refusals_verify_exact_demand() {
    use gents_protocol::file_inventory::{FileInventoryRefusalReason, FileInventoryReply};
    let (directory, names) = tokio::task::spawn_blocking(|| {
        let directory = tempfile::tempdir().unwrap();
        let names = Names {
            source: KeyIdentity::load_or_create(directory.path().join("source.key"), None).unwrap(),
            other: KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap(),
        };
        (directory, names)
    })
    .await
    .unwrap();
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_refusal_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let expected = names.demand(&case["expected_demand"]);
        let actual = names.demand(&case["actual_demand"]);
        for reason in [
            FileInventoryRefusalReason::Rejected,
            FileInventoryRefusalReason::Stale,
            FileInventoryRefusalReason::Unavailable,
        ] {
            let mut receipt = sign_refusal(&names.source, &actual, reason).await.unwrap();
            if case["version"] == false {
                receipt.version += 1;
            }
            if case["nonce_matches"] == false {
                receipt.request_key.push_str("-wrong");
            }
            receipt.signer_did = names.actor(&case["signer"]);
            let signer = if case["signer"] == "node-1" {
                &names.source
            } else {
                &names.other
            };
            receipt.signature = signer
                .sign(&receipt.signing_payload().unwrap())
                .await
                .unwrap();
            if case["verified"] == false {
                receipt.signature[0] ^= 1;
            }
            let reply = FileInventoryReply::Refused(receipt);
            let restored: FileInventoryReply =
                serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap();
            assert_eq!(
                verify_reply(&expected, &restored, &names.source)
                    .await
                    .is_ok(),
                case["expected"].as_bool().unwrap(),
                "{} {reason:?}",
                case["name"]
            );
            if case["expected"] == true {
                let FileInventoryReply::Refused(mut tampered) = restored else {
                    unreachable!()
                };
                tampered.reason = if reason == FileInventoryRefusalReason::Stale {
                    FileInventoryRefusalReason::Rejected
                } else {
                    FileInventoryRefusalReason::Stale
                };
                assert!(verify_reply(
                    &expected,
                    &FileInventoryReply::Refused(tampered),
                    &names.source
                )
                .await
                .is_err());
            }
        }
    }
    drop(names);
    tokio::fs::remove_dir_all(directory.path()).await.unwrap();
}

#[tokio::test]
async fn generated_reply_journals_preserve_immutable_outcomes() {
    use crate::config_client::ConfigAccess;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmissionOptions,
    };
    use gents_protocol::file_inventory::FileInventoryReply;
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_reply_journal_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-reply-journal".into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let path = fixture.path.clone();
    let names = tokio::task::spawn_blocking(move || Names {
        source: KeyIdentity::load_or_create(path.join("test-agent.key"), None).unwrap(),
        other: KeyIdentity::load_or_create(path.join("other.key"), None).unwrap(),
    })
    .await
    .unwrap();
    assert_eq!(names.source.did(), fixture.node_did);
    for case in cases {
        let mut demand = names.demand(&case["demand"]);
        demand.attempt_id = text(&case["name"]);
        let prior = if case["prior"].is_null() {
            None
        } else {
            Some(names.reply(&case["prior"], &demand).await)
        };
        if let Some(prior) = &prior {
            assert_eq!(
                &record_reply(&fixture.node, &demand, prior, &names.source)
                    .await
                    .unwrap(),
                prior
            );
        }
        let candidate = names.reply(&case["candidate"], &demand).await;
        let result = ConfigAccess::transact_local_write(
            &fixture.node,
            None,
            "test.inventory.reply_commit",
            |txn| {
                let (demand, candidate, verifier) = (&demand, &candidate, &names.source);
                Box::pin(async move {
                    let reply = journal::record_in_txn(txn, demand, candidate, verifier).await?;
                    anyhow::ensure!(
                        case["committed"] == true,
                        "fixture rollback after writing reply"
                    );
                    Ok(reply)
                })
            },
        )
        .await;
        assert_eq!(
            result.is_ok(),
            case["committed"] == true,
            "{}",
            case["name"]
        );
        let retained = load_recorded_reply(&fixture.node, &demand, &names.source)
            .await
            .unwrap();
        if case["expected"].is_null() {
            assert!(retained.is_none(), "{}", case["name"]);
        } else {
            let expected = prior.clone().unwrap_or_else(|| candidate.clone());
            assert_eq!(retained, Some(expected.clone()), "{}", case["name"]);
            match (&expected, &case["expected"]) {
                (FileInventoryReply::Page(receipt), value) => {
                    assert_eq!(value["kind"], "page");
                    assert_eq!(receipt.page, names.page(&value["page"]));
                }
                (FileInventoryReply::Refused(receipt), value) => {
                    assert_eq!(value["kind"], "refused");
                    assert_eq!(
                        serde_json::to_value(receipt.reason).unwrap(),
                        value["reason"]
                    );
                }
            }
            assert_eq!(
                record_reply(&fixture.node, &demand, &candidate, &names.source)
                    .await
                    .unwrap(),
                expected
            );
        }
        let mut fresh = demand.clone();
        fresh.attempt_id.push_str("-new");
        assert!(load_recorded_reply(&fixture.node, &fresh, &names.source)
            .await
            .unwrap()
            .is_none());
    }
    for invalid in ["foreign-owner", "corrupt-signature", "wrong-demand"] {
        let mut demand = names.demand(&cases[0]["demand"]);
        demand.attempt_id = invalid.into();
        let reply = names.reply(&cases[1]["candidate"], &demand).await;
        let mut stored_reply = serde_json::to_value(&reply).unwrap();
        let mut stored_demand = demand.clone();
        if invalid == "corrupt-signature" {
            stored_reply["receipt"]["signature"][0] =
                serde_json::json!(stored_reply["receipt"]["signature"][0].as_u64().unwrap() ^ 1);
        }
        if invalid == "wrong-demand" {
            stored_demand.attempt_id.push_str("-other");
        }
        let input = serde_json::json!({
            "request_key": demand.derive_request_key().unwrap(),
            "node_did": names.source.did(), "demand": stored_demand, "reply": stored_reply,
        });
        let actor = (invalid == "foreign-owner").then(|| names.other.did().parse().unwrap());
        let id = ConfigAccess::transact_local_write(&fixture.node, actor, "test.inventory.invalid_reply", |txn| {
            let input = &input;
            Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: FileInventoryReplyMutationInputArg!) { create_FileInventoryReply(input: $input) { _docID } }",
                    &serde_json::json!({"input": input})).await?;
                crate::graphql::created_doc_id(&response, "FileInventoryReply")
            })
        }).await.unwrap();
        if invalid == "foreign-owner" {
            fixture
                .node
                .add_dac_actor_relationship(
                    names.other.did(),
                    "FileInventoryReply",
                    &id,
                    "reader",
                    names.source.did(),
                )
                .await
                .unwrap();
        }
        assert!(
            load_recorded_reply(&fixture.node, &demand, &names.source)
                .await
                .is_err(),
            "{invalid}"
        );
        assert!(
            record_reply(&fixture.node, &demand, &reply, &names.source)
                .await
                .is_err(),
            "{invalid}"
        );
    }
    fixture.node.shutdown().await;
    drop(names);
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}
