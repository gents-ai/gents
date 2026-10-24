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
            order: match value["order"]["kind"].as_str().unwrap() {
                "unresolved" => None,
                "ordered" => Some(gents_protocol::file_inventory::FileInventoryOrdinal {
                    index: value["order"]["index"].as_u64().unwrap(),
                    entries: value["order"]["entries"].as_u64().unwrap(),
                }),
                other => panic!("unknown modeled order {other}"),
            },
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
                FILE_INVENTORY_RECEIPT_VERSION
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
        "missing" => FileImageMetadata::Missing {},
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
async fn absent_order_preserves_signed_receipts_and_cannot_gain_an_ordinal() {
    let (_directory, names) = tokio::task::spawn_blocking(|| {
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
    let case = &contracts["file_inventory_demand_cases"][0];
    let demand = names.demand(&case["demand"]);
    let mut receipt = FileInventoryPageReceipt {
        version: FILE_INVENTORY_RECEIPT_VERSION,
        request_key: demand.derive_request_key().unwrap(),
        signer_did: names.source.did().to_owned(),
        page: names.page(&case["initial_page"]),
        signature: Vec::new(),
    };
    let payload = receipt.signing_payload().unwrap();
    receipt.signature = names.source.sign(&payload).await.unwrap();
    let stored = serde_json::to_value(&receipt).unwrap();
    assert!(stored["page"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry.get("order").is_none()));
    let restored: FileInventoryPageReceipt = serde_json::from_value(stored).unwrap();
    assert_eq!(restored.signing_payload().unwrap(), payload);
    verify_reply(
        &demand,
        &gents_protocol::file_inventory::FileInventoryReply::Page(restored),
        &names.source,
    )
    .await
    .unwrap();
    receipt.page.entries[0].order = Some(gents_protocol::file_inventory::FileInventoryOrdinal {
        index: 0,
        entries: 1,
    });
    assert!(verify_reply(
        &demand,
        &gents_protocol::file_inventory::FileInventoryReply::Page(receipt),
        &names.source
    )
    .await
    .is_err());
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

#[tokio::test]
async fn generated_completion_requires_confirmed_authorized_delivery() {
    use crate::agent::p2p_reconcile::session_hydration::{
        HydrationDeliveryResult, HydrationTerminalWriteResult,
    };
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
    let demand = names.demand(&contracts["file_inventory_demand_cases"][0]["demand"]);
    let cases = contracts["file_inventory_completion_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let candidate = names.reply(&case["candidate"], &demand).await;
        let actual = settle_reply(
            &names.source,
            &demand,
            &candidate,
            InventoryDeliveryObservation {
                before: case["before"].as_bool().unwrap(),
                after: case["after"].as_bool().unwrap(),
                delivery: if case["confirmed"] == true {
                    HydrationDeliveryResult::Confirmed
                } else {
                    HydrationDeliveryResult::Indeterminate
                },
                write: if case["committed"] == true {
                    HydrationTerminalWriteResult::Committed
                } else {
                    HydrationTerminalWriteResult::Failed
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(actual.is_some(), !case["expected"].is_null(), "{case}");
        if let Some(actual) = actual {
            verify_reply(&demand, &actual, &names.source).await.unwrap();
            assert_eq!(
                actual,
                names.reply(&case["expected"], &demand).await,
                "{case}"
            );
        }
    }
    drop(names);
    tokio::fs::remove_dir_all(directory.path()).await.unwrap();
}

#[tokio::test]
async fn generated_page_grants_bind_only_the_exact_metadata_document() {
    use gents_protocol::file_inventory::{
        FileInventoryPageGrant, FileInventoryResponse, FILE_INVENTORY_PAGE_GRANT_VERSION,
    };
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
    let cases = contracts["file_inventory_page_grant_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let expected = names.demand(&case["expected_demand"]);
        let mut grant = FileInventoryPageGrant {
            version: if case["version"] == true {
                FILE_INVENTORY_PAGE_GRANT_VERSION
            } else {
                0
            },
            demand: names.demand(&case["actual_demand"]),
            document_id: text(&case["document"]),
            signer_did: names.actor(&case["signer"]),
            signature: vec![],
        };
        let signer = if case["signer"] == "node-1" {
            &names.source
        } else {
            &names.other
        };
        grant.signature = signer
            .sign(&grant.signing_payload().unwrap())
            .await
            .unwrap();
        if case["verified"] == false {
            grant.signature[0] ^= 1;
        }
        let serialized = serde_json::to_vec(&FileInventoryResponse::Page(grant.clone())).unwrap();
        let restored = serde_json::from_slice(&serialized).unwrap();
        let result = VerifiedInventoryResponse::verify(restored, &expected, &names.source).await;
        assert_eq!(
            result.is_ok(),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if result.is_ok() {
            grant.document_id.push_str("-other");
            assert!(VerifiedInventoryResponse::verify(
                FileInventoryResponse::Page(grant),
                &expected,
                &names.source
            )
            .await
            .is_err());
        }
    }
    drop(names);
    tokio::fs::remove_dir_all(directory.path()).await.unwrap();
}

#[tokio::test]
async fn native_inventory_admission_and_response_commit_preserve_attempts_and_rollback() {
    use crate::config_client::ConfigAccess;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmissionOptions,
    };
    use gents_protocol::file_inventory::{FileInventoryReply, FileInventoryResponse};
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-transport-storage".into(),
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
    let mut demand = names.demand(&contracts["file_inventory_demand_cases"][0]["demand"]);
    demand.admission.request.requester_did = names.source.did().into();
    let document = admit_demand(&fixture.node, &demand).await.unwrap();
    assert_eq!(
        admit_demand(&fixture.node, &demand).await.unwrap(),
        document
    );
    let mut fresh = demand.clone();
    fresh.attempt_id.push_str("-new");
    assert_ne!(admit_demand(&fixture.node, &fresh).await.unwrap(), document);
    let mut page = names.page(&contracts["file_inventory_demand_cases"][0]["initial_page"]);
    page.binding.admission = demand.admission.clone();
    page.next.as_mut().unwrap().binding.admission = demand.admission.clone();
    let receipt = sign_current_page(
        &names.source,
        &demand.derive_request_key().unwrap(),
        page,
        || std::future::ready(Ok(true)),
    )
    .await
    .unwrap();
    let prepared = delivery::prepare_page(&fixture.node, &demand, &receipt, &names.source)
        .await
        .unwrap();
    let mut changed = receipt.clone();
    changed.page.binding.digest.push_str("-new");
    let retained = delivery::prepare_page(&fixture.node, &demand, &changed, &names.source)
        .await
        .unwrap();
    assert_eq!(retained.document_id, prepared.document_id);
    assert_eq!(retained.receipt, receipt);
    for commit in [false, true] {
        let result = ConfigAccess::transact_local_write(
            &fixture.node,
            None,
            "test.inventory.atomic_response",
            |txn| {
                let (demand, prepared, verifier) = (&demand, &prepared, &names.source);
                Box::pin(async move {
                    let row = requests::load_request_in_txn(txn, demand).await?.unwrap();
                    let selected = journal::record_in_txn(
                        txn,
                        demand,
                        &FileInventoryReply::Page(prepared.receipt.clone()),
                        verifier,
                    )
                    .await?;
                    assert_eq!(selected, FileInventoryReply::Page(prepared.receipt.clone()));
                    let response = FileInventoryResponse::Page(
                        delivery::sign_page_grant(verifier, demand, prepared).await?,
                    );
                    requests::publish_response_in_txn(txn, &row, &response).await?;
                    anyhow::ensure!(commit, "rollback after journal and response writes");
                    Ok(response)
                })
            },
        )
        .await;
        assert_eq!(result.is_ok(), commit);
        let recorded = load_recorded_reply(&fixture.node, &demand, &names.source)
            .await
            .unwrap();
        let row = ConfigAccess::transact_local_readonly(
            &fixture.node,
            None,
            "test.inventory.reload_request",
            |txn| Box::pin(requests::load_request_in_txn(txn, &demand)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(recorded.is_some(), commit);
        assert_eq!(row.status, if commit { "served" } else { "pending" });
        if let Ok(response) = result {
            assert_eq!(
                serde_json::to_value(row.response.unwrap()).unwrap(),
                serde_json::to_value(&response).unwrap()
            );
            let verified = VerifiedInventoryResponse::verify(response, &demand, &names.source)
                .await
                .unwrap();
            let delivered = verified
                .read_page(fixture.node.clone(), &names.source)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(delivered, receipt);
            assert!(load_recorded_reply(&fixture.node, &fresh, &names.source)
                .await
                .unwrap()
                .is_none());
        }
    }
    fixture.node.shutdown().await;
    drop(names);
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}

#[tokio::test]
async fn generated_preparation_retries_keep_the_original_snapshot() {
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
    let cases = contracts["file_inventory_preparation_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-preparation-retries".into(),
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
    for case in cases {
        let mut demand = names.demand(&case["demand"]);
        demand.attempt_id.push_str(case["name"].as_str().unwrap());
        let sign = |page: &Value| serde_json::json!({"kind":"page", "page":page});
        let mut prior_id = None;
        if !case["prior"].is_null() {
            let FileInventoryReply::Page(receipt) =
                names.reply(&sign(&case["prior"]), &demand).await
            else {
                unreachable!()
            };
            prior_id = Some(
                delivery::prepare_page(&fixture.node, &demand, &receipt, &names.source)
                    .await
                    .unwrap()
                    .document_id,
            );
        }
        let FileInventoryReply::Page(candidate) =
            names.reply(&sign(&case["candidate"]), &demand).await
        else {
            unreachable!()
        };
        let result = ConfigAccess::transact_local_write(
            &fixture.node,
            None,
            "test.inventory.prepare_commit",
            |txn| {
                let (demand, candidate, verifier) = (&demand, &candidate, &names.source);
                Box::pin(async move {
                    let prepared =
                        delivery::prepare_page_in_txn(txn, demand, candidate, verifier).await?;
                    anyhow::ensure!(case["committed"] == true, "rollback after preparation");
                    Ok(prepared)
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
        let retained = delivery::load_prepared_page(&fixture.node, &demand, &names.source)
            .await
            .unwrap();
        if case["expected"].is_null() {
            assert!(retained.is_none(), "{}", case["name"]);
        } else {
            let retained = retained.unwrap();
            assert_eq!(
                retained.receipt.page,
                names.page(&case["expected"]),
                "{}",
                case["name"]
            );
            if let Some(id) = prior_id {
                assert_eq!(retained.document_id, id);
            }
        }
        assert!(load_recorded_reply(&fixture.node, &demand, &names.source)
            .await
            .unwrap()
            .is_none());
    }
    fixture.node.shutdown().await;
    drop(names);
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}

#[tokio::test]
async fn generated_client_scope_requires_a_unique_retained_admission() {
    use crate::config_client::ConfigAccess;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmissionOptions,
    };
    use serde_json::json;
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_client_scope_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let (fixture, _) = published_admission_with_owner(PublishedAdmissionOptions {
            name: format!("inventory-scope-{}", text(&case["name"])),
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
        for row in case["rows"].as_array().unwrap() {
            begin_client_progress(
                &fixture.node,
                &names.admission(row),
                "retained-attempt",
                &names.source,
            )
            .await
            .unwrap();
        }
        let peer = text(&case["peer"]);
        let node = names.actor(&case["node"]);
        let session = text(&case["session"]);
        let actual =
            load_client_progress_for_scope(&fixture.node, &peer, &node, &session, &names.source)
                .await;
        match text(&case["expected"]["kind"]).as_str() {
            "error" => {
                let expected = match text(&case["expected"]["reason"]).as_str() {
                    "invalid" => FileInventoryScopeError::Invalid,
                    "ambiguous" => FileInventoryScopeError::Ambiguous,
                    other => panic!("unknown retained-scope error: {other}"),
                };
                assert_eq!(
                    actual
                        .unwrap_err()
                        .downcast_ref::<FileInventoryScopeError>(),
                    Some(&expected),
                    "{case}"
                );
            }
            "missing" => assert!(actual.unwrap().is_none(), "{case}"),
            "selected" => assert_eq!(
                actual.unwrap().unwrap().admission(),
                &names.admission(&case["expected"]["admission"]),
                "{case}"
            ),
            other => panic!("unknown retained-scope result: {other}"),
        }
        if case["name"] == "exact" {
            let rows = ConfigAccess::local(fixture.node.clone()).execute(
                "{ FileInventoryProgress { _docID progress_key client_did scope_key progress } }",
            ).await.unwrap();
            let mut row = rows["data"]["FileInventoryProgress"][0].clone();
            let id = row.as_object_mut().unwrap().remove("_docID").unwrap();
            row["progress_key"] = json!("forged-index-key");
            ConfigAccess::transact_local_write(&fixture.node, None, "test.inventory.scope_index_corruption", |txn| {
                let (row, id) = (&row, &id);
                Box::pin(async move {
                    txn.execute(&format!(r#"mutation {{ delete_FileInventoryProgress(docID: "{}") {{ _docID }} }}"#,
                        crate::graphql::escape_graphql_string(id.as_str().unwrap()))).await?;
                    txn.execute_with_variables(
                        "mutation($input: FileInventoryProgressMutationInputArg!) { create_FileInventoryProgress(input: $input) { _docID } }",
                        &json!({"input":row}),
                    ).await
                })
            }).await.unwrap();
            assert!(load_client_progress_for_scope(
                &fixture.node,
                &peer,
                &node,
                &session,
                &names.source
            )
            .await
            .is_err());
        }
        let path = fixture.path.clone();
        fixture.node.shutdown().await;
        drop(fixture);
        tokio::fs::remove_dir_all(path).await.unwrap();
    }
}

#[tokio::test]
async fn generated_client_progress_survives_reload_and_rollback() {
    use crate::config_client::ConfigAccess;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmissionOptions,
    };
    use gents_protocol::file_inventory::{FileInventoryPageDemand, FileInventoryReply};
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_client_progress_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let (fixture, _) = published_admission_with_owner(PublishedAdmissionOptions {
            name: format!("inventory-client-{}", text(&case["name"])),
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
        let admission = names.admission(&case["admission"]);
        let mut state = resume_client_progress(
            &fixture.node,
            &admission,
            text(&case["attempt"]).as_str(),
            &names.source,
        )
        .await
        .unwrap();
        let initial_demand = state
            .active()
            .pending_demand(&names.source)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            load_client_progress(&fixture.node, &admission, &names.source)
                .await
                .unwrap()
                .unwrap(),
            state
        );
        assert_eq!(
            initial_demand,
            state
                .active()
                .pending_demand(&names.source)
                .await
                .unwrap()
                .unwrap()
        );
        let mut seen: Vec<(FileInventoryPageDemand, FileInventoryReply)> = Vec::new();
        for step in case["steps"].as_array().unwrap() {
            let action = &step["action"];
            let attempt = text(&action["attempt"]);
            let mut candidate = None;
            let mut delivered = None;
            let expected_accept = step["accepted"] == true;
            let committed = step["committed"] == true;
            if action["kind"] == "resume" {
                candidate = Some(Ok(state.clone()));
            } else if action["kind"] == "restart" {
                candidate = Some(state.restart(&attempt, &names.source).await);
            } else {
                let mut demand = state
                    .active()
                    .pending_demand(&names.source)
                    .await
                    .unwrap()
                    .unwrap_or_else(|| FileInventoryPageDemand {
                        admission: admission.clone(),
                        attempt_id: attempt.clone(),
                        cursor: state
                            .active()
                            .receiver()
                            .pages
                            .last()
                            .and_then(|page| page.receipt.page.next.clone()),
                    });
                demand.attempt_id = attempt.clone();
                let reply = if action["kind"] == "page" {
                    let page = names.page(&action["page"]);
                    demand.cursor = (page.offset != 0).then(|| FileInventoryCursor {
                        binding: page.binding.clone(),
                        offset: page.offset,
                    });
                    let mut receipt = sign_current_page(
                        &names.source,
                        &demand.derive_request_key().unwrap(),
                        page,
                        || std::future::ready(Ok(true)),
                    )
                    .await
                    .unwrap();
                    if action["nonce_matches"] == false {
                        receipt.request_key.push_str("-wrong");
                        receipt.signature = names
                            .source
                            .sign(&receipt.signing_payload().unwrap())
                            .await
                            .unwrap();
                    }
                    FileInventoryReply::Page(receipt)
                } else {
                    let mut receipt = sign_refusal(
                        &names.source,
                        &demand,
                        serde_json::from_value(action["reason"].clone()).unwrap(),
                    )
                    .await
                    .unwrap();
                    if action["verified"] == false {
                        receipt.signature[0] ^= 1;
                    }
                    FileInventoryReply::Refused(receipt)
                };
                let reply = seen
                    .iter()
                    .find(|(prior_demand, prior_reply)| {
                        prior_demand == &demand
                            && match (prior_reply, &reply) {
                                (
                                    FileInventoryReply::Page(left),
                                    FileInventoryReply::Page(right),
                                ) => {
                                    left.page == right.page && left.request_key == right.request_key
                                }
                                (
                                    FileInventoryReply::Refused(left),
                                    FileInventoryReply::Refused(right),
                                ) => left == right,
                                _ => false,
                            }
                    })
                    .map(|(_, prior)| prior.clone())
                    .unwrap_or(reply);
                seen.push((demand.clone(), reply.clone()));
                delivered = Some((demand, reply));
            }
            let result = match &delivered {
                Some((demand, reply)) => state.receive(demand, reply, &names.source).await,
                None => candidate.unwrap(),
            };
            assert_eq!(
                result.is_ok(),
                expected_accept,
                "{}: {action}; {result:?}",
                case["name"]
            );
            match &delivered {
                Some((demand, reply)) => {
                    let stored = ConfigAccess::transact_local_write(
                        &fixture.node,
                        None,
                        "test.inventory.client_commit",
                        |txn| {
                            let (node, verifier) = (&fixture.node, &names.source);
                            Box::pin(async move {
                                let next =
                                    client::receive_in_txn(txn, node, demand, reply, verifier)
                                        .await?;
                                anyhow::ensure!(committed, "rollback after retaining client page");
                                Ok(next)
                            })
                        },
                    )
                    .await;
                    assert_eq!(
                        stored.is_ok(),
                        expected_accept && committed,
                        "{}: {action}; {stored:?}",
                        case["name"]
                    );
                }
                None if action["kind"] == "resume" && expected_accept && committed => {
                    resume_client_progress(&fixture.node, &admission, &attempt, &names.source)
                        .await
                        .unwrap();
                }
                None if expected_accept && committed => {
                    begin_client_progress(&fixture.node, &admission, &attempt, &names.source)
                        .await
                        .unwrap();
                }
                _ => {}
            }
            if expected_accept && committed {
                state = result.unwrap();
            }
            let restored = load_client_progress(&fixture.node, &admission, &names.source)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(restored, state);
            let serialized = serde_json::to_vec(&restored).unwrap();
            let decoded: ClientInventoryProgress = serde_json::from_slice(&serialized).unwrap();
            let prefix = decoded.validated(&names.source).await.unwrap();
            let pending = decoded
                .active()
                .pending_demand(&names.source)
                .await
                .unwrap();
            let view = decoded.observation(None, &names.source).await.unwrap();
            let last_page_entries = if let Some(page) = decoded.active().receiver().pages.last() {
                let cursor = gents_protocol::file_inventory::FileInventoryViewCursor {
                    attempt_id: decoded.active().attempt_id().into(),
                    snapshot_digest: page.receipt.page.binding.digest.clone(),
                    offset: page.receipt.page.offset,
                };
                let selected = decoded
                    .observation(Some(&cursor), &names.source)
                    .await
                    .unwrap();
                let mut wrong = cursor.clone();
                wrong.snapshot_digest.push_str("-different");
                assert!(decoded
                    .observation(Some(&wrong), &names.source)
                    .await
                    .is_err());
                Some(selected.current.unwrap().entries.len())
            } else {
                None
            };
            let summary = serde_json::json!({
                "attempt": decoded.active().attempt_id(),
                "retired": serde_json::to_value(&decoded).unwrap()["retired_attempts"],
                "pages": decoded.active().receiver().pages.len(),
                "visible_entries": view.current.as_ref().map(|view| view.entries.len()),
                "next_received_offset": view.current.as_ref().and_then(|view| view.next.as_ref().map(|next| next.offset)),
                "last_page_entries": last_page_entries,
                "complete": prefix.as_ref().is_some_and(FileInventoryPrefix::complete),
                "known": prefix.as_ref().is_some_and(FileInventoryPrefix::known_inventory),
                "refusal": decoded.active().refusal().map(|receipt| receipt.reason),
                "pending_offset": pending.map(|demand| demand.cursor.map_or(0, |cursor| cursor.offset)),
                "previous_pages": decoded.previous().map(|prior| prior.receiver().pages.len()),
            });
            assert_eq!(summary, step["expected"], "{}: {action}", case["name"]);
            let mut tampered = serde_json::to_value(&decoded).unwrap();
            if let Some(signature) = tampered["active"]["receiver"]["pages"]
                .as_array_mut()
                .and_then(|pages| pages.first_mut())
                .and_then(|page| page["receipt"]["signature"].as_array_mut())
            {
                signature[0] = serde_json::json!(signature[0].as_u64().unwrap() ^ 1);
                let tampered: ClientInventoryProgress = serde_json::from_value(tampered).unwrap();
                assert!(tampered.validated(&names.source).await.is_err());
            }
        }
        let path = fixture.path.clone();
        fixture.node.shutdown().await;
        drop(fixture);
        let reopened = std::sync::Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .data_path(&path)
                .with_node_identity_did(names.source.did())
                .build()
                .await
                .unwrap(),
        );
        let persisted = load_client_progress(&reopened, &admission, &names.source)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted, state,
            "{} after actual database reopen",
            case["name"]
        );
        let mut corrupt = serde_json::to_value(&persisted).unwrap();
        let signature = if corrupt["active"]["receiver"]["pages"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            &mut corrupt["active"]["refusal"]["signature"]
        } else {
            &mut corrupt["active"]["receiver"]["pages"][0]["receipt"]["signature"]
        };
        signature[0] = serde_json::json!(signature[0].as_u64().unwrap() ^ 1);
        ConfigAccess::transact_local_write(&reopened, None, "test.inventory.corrupt_retained_page", |txn| {
            let corrupt = &corrupt;
            Box::pin(async move {
                txn.execute_with_variables("mutation($input: FileInventoryProgressMutationInputArg!) { update_FileInventoryProgress(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"progress": corrupt}})).await
            })
        }).await.unwrap();
        assert!(load_client_progress(&reopened, &admission, &names.source)
            .await
            .is_err());
        reopened.shutdown().await;
        drop(reopened);
        drop(names);
        tokio::fs::remove_dir_all(path).await.unwrap();
    }
}

#[tokio::test]
async fn generated_operator_inventory_requires_the_native_owner() {
    use crate::config_client::ConfigAccess;
    use crate::tool_call_lifecycle::admission_fixture::{
        ensure_fixture_session, published_admission_with_owner, PublishedAdmissionOptions,
    };
    use gents_protocol::file_inventory::{
        FileInventoryCoverage, FileInventoryPageDemand, FileInventoryReply,
    };
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_operator_owner_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    let (fixture, _) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-operator-owner".into(),
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
    let session = "operator-empty-inventory";
    ensure_fixture_session(
        &fixture.node,
        session,
        names.source.did(),
        Some(names.source.did()),
        "2026-10-24T00:00:00Z",
    )
    .await;
    let session_row = ConfigAccess::transact_local_readonly(
        &fixture.node,
        None,
        "test.inventory.operator_session",
        |txn| {
            let source = names.source.did();
            Box::pin(crate::session::load_agent_session_row_in_txn(
                txn,
                source,
                session,
                Some(source),
            ))
        },
    )
    .await
    .unwrap()
    .unwrap();
    for case in cases {
        assert_eq!(names.actor(&case["owner"]), fixture.node_did);
        assert!(case["source"]["catalog"]["applied_pairing_routes"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(case["source"]["catalog"]["verified_active_memberships"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(case["source"]["tools"].as_array().unwrap().is_empty());
        assert!(case["source"]["projections"].as_array().unwrap().is_empty());
        let demand = FileInventoryPageDemand {
            admission: FileInventoryAdmission {
                request: HydrationRequest {
                    request_key: format!("operator:{}", case["name"]),
                    peer_id: "operator-client".into(),
                    requester_did: names.source.did().into(),
                    node_did: names.actor(&case["admission"]["node"]),
                    session_id: session.into(),
                },
                session_doc_id: session_row.doc_id.clone(),
            },
            attempt_id: text(&case["name"]),
            cursor: None,
        };
        let signer: &dyn NodeIdentity = if names.actor(&case["signer"]) == names.source.did() {
            &names.source
        } else {
            &names.other
        };
        let actor = names.actor(&case["actor"]);
        let result = read_operator_inventory(&fixture.node, signer, &actor, &demand).await;
        assert_eq!(
            result.is_ok(),
            case["allowed"] == true,
            "{}: {result:?}",
            case["name"]
        );
        if case["allowed"] == false {
            assert!(result.unwrap_err().is::<OperatorInventoryDenied>());
            continue;
        }
        let reply = result.unwrap();
        verify_reply(&demand, &reply, &names.source).await.unwrap();
        let FileInventoryReply::Page(page) = &reply else {
            panic!("operator read did not return an empty page");
        };
        assert_eq!(
            page.page.entries.len(),
            case["page"]["entries"].as_array().unwrap().len()
        );
        let prefix = FileInventoryReceiver {
            admission: demand.admission.clone(),
            pages: vec![],
        }
        .receive_for_demand(&demand, page.clone(), &names.source)
        .await
        .unwrap()
        .validated(&names.source)
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            prefix.known_inventory(),
            case["expected"]["known_empty"].as_bool().unwrap()
        );
        assert_eq!(
            page.page.binding.coverage,
            serde_json::from_value::<FileInventoryCoverage>(
                case["page"]["binding"]["coverage"].clone()
            )
            .unwrap()
        );
        assert_eq!(
            read_operator_inventory(&fixture.node, signer, &actor, &demand)
                .await
                .unwrap(),
            reply
        );
        assert!(
            load_recorded_reply(&fixture.node, &demand, &names.source)
                .await
                .unwrap()
                .is_none(),
            "HTTP preparation must not claim an explicit-transfer acknowledgment"
        );
        let mut changed = demand.clone();
        changed.attempt_id.push_str("-wrong-physical-session");
        changed.admission.session_doc_id = document(&serde_json::json!(999));
        let refused = read_operator_inventory(&fixture.node, signer, &actor, &changed)
            .await
            .unwrap();
        assert!(
            matches!(&refused, FileInventoryReply::Refused(receipt) if receipt.reason == serde_json::from_value(case["wrong_physical_refusal"].clone()).unwrap())
        );
        verify_reply(&changed, &refused, &names.source)
            .await
            .unwrap();
        assert_eq!(
            load_recorded_reply(&fixture.node, &changed, &names.source)
                .await
                .unwrap(),
            Some(refused)
        );
    }
    fixture.node.shutdown().await;
    drop(names);
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}
