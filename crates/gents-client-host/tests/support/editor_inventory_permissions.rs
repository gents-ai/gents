use super::{editor_review, Attachment};
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::file_image_hydration::FileImageDemand;
use gents_protocol::file_inventory::{
    FileInventoryImage, FileInventoryImageReference, FileInventoryObservation,
    FileInventorySnapshotView, FileInventoryViewCursor,
};
use serde_json::{json, Value};
use std::time::Duration;

const REFRESH: &str = "447d05fb-0fad-4e68-a548-0f186c956948";
const BLOCKED: &str = "d441c48e-67db-421d-968b-37cddbc16116";
const RESTORED: &str = "d0dbe170-284b-409c-be7c-234cc74f3931";

pub struct InventoryPermissions {
    args: Value,
    snapshot: FileInventorySnapshotView,
    image: FileInventoryImage,
    demand: FileImageDemand,
}

impl InventoryPermissions {
    pub async fn capture(
        client: &mut Attachment,
        id: &mut u32,
        demand: &FileImageDemand,
    ) -> Result<Self> {
        let args = json!({"nodeDid":demand.admission.node_did,
            "sessionId":demand.admission.session_id});
        let mut read = args.clone();
        read["action"] = json!({"kind":"read"});
        let observation: FileInventoryObservation = serde_json::from_value(
            client
                .try_call(*id, "desktop_session_file_inventory", read)
                .await?,
        )?;
        *id += 1;
        let snapshot = observation
            .current
            .iter()
            .chain(observation.previous.iter())
            .find(|snapshot| snapshot.complete && snapshot.known_inventory)
            .context("review did not retain complete metadata")?
            .clone();
        ensure!(
            snapshot.entries.len() == 1
                && snapshot.entries[0].tool_call_doc_id == demand.tool_call_doc_id,
            "permission probe selected a different physical tool"
        );
        let reference = FileInventoryImageReference {
            cursor: FileInventoryViewCursor {
                attempt_id: snapshot.attempt_id.clone(),
                snapshot_digest: snapshot.binding.digest.clone(),
                offset: snapshot.offset,
            },
            entry_index: 0,
        };
        let mut selected = args.clone();
        selected["reference"] = serde_json::to_value(reference)?;
        let image: FileInventoryImage = serde_json::from_value(
            client
                .try_call(*id, "desktop_inventory_file_change", selected)
                .await?,
        )?;
        *id += 1;
        ensure!(
            image.entry == snapshot.entries[0],
            "selected image changed its metadata"
        );
        Ok(Self {
            args,
            snapshot,
            image,
            demand: demand.clone(),
        })
    }

    async fn observe(
        &self,
        client: &mut Attachment,
        id: &mut u32,
        action: Value,
    ) -> Result<FileInventoryObservation> {
        let mut args = self.args.clone();
        args["action"] = action;
        let value = client
            .try_call(*id, "desktop_session_file_inventory", args)
            .await;
        *id += 1;
        Ok(serde_json::from_value(value?)?)
    }

    fn image_args(&self, retry: Option<&str>) -> Result<Value> {
        let mut args = self.args.clone();
        args["reference"] = serde_json::to_value(&self.image.reference)?;
        if let Some(retry) = retry {
            args["retryId"] = json!(retry);
        }
        Ok(args)
    }

    pub async fn revoked(&self, client: &mut Attachment, id: &mut u32) -> Result<Value> {
        let retained = self.observe(client, id, json!({"kind":"read"})).await?;
        ensure!(
            retained.current.as_ref() == Some(&self.snapshot)
                || retained.previous.as_ref() == Some(&self.snapshot),
            "revocation hid retained inventory metadata"
        );
        let cached: FileInventoryImage = serde_json::from_value(
            client
                .try_call(*id, "desktop_inventory_file_change", self.image_args(None)?)
                .await?,
        )?;
        *id += 1;
        ensure!(
            cached == self.image,
            "revocation changed the retained selected image"
        );
        let pending = self
            .observe(client, id, json!({"kind":"refresh", "attemptId":REFRESH}))
            .await?;
        ensure!(
            pending.attempt_id == REFRESH && pending.previous.as_ref() == Some(&self.snapshot),
            "new metadata attempt lost its retained complete snapshot"
        );
        let mut advance = self.args.clone();
        advance["action"] = json!({"kind":"advance"});
        let started = tokio::time::Instant::now();
        let metadata = client
            .try_call_with_timeout(
                *id,
                "desktop_session_file_inventory",
                advance,
                Duration::from_secs(45),
            )
            .await;
        *id += 1;
        let metadata_error = metadata.expect_err("revoked enrollment served new metadata");
        ensure!(
            started.elapsed() < Duration::from_secs(5),
            "revoked metadata read waited for delivery"
        );
        let started = tokio::time::Instant::now();
        let image = client
            .try_call_with_timeout(
                *id,
                "desktop_inventory_file_change",
                self.image_args(Some(BLOCKED))?,
                Duration::from_secs(45),
            )
            .await;
        *id += 1;
        let image_error = image.expect_err("revoked enrollment served a fresh selected image");
        ensure!(
            started.elapsed() < Duration::from_secs(5),
            "revoked selected image waited for delivery"
        );
        for error in [&metadata_error, &image_error] {
            ensure!(
                error.message.contains("Reconnect") || error.message.contains("rejected"),
                "revoked inventory failed for an unrelated reason: {error}"
            );
        }
        let held = self.observe(client, id, json!({"kind":"read"})).await?;
        ensure!(
            held.attempt_id == REFRESH && held.previous.as_ref() == Some(&self.snapshot),
            "failed metadata delivery replaced its retry identity or retained prefix"
        );
        Ok(
            json!({"retainedMetadataReadable":true, "cachedSelectedImageReadable":true,
            "freshMetadataRefused":metadata_error.message, "freshImageRefused":image_error.message,
            "attemptPreserved":true}),
        )
    }

    pub async fn restored(
        &self,
        client: &mut Attachment,
        id: &mut u32,
        access: &ConfigAccess,
        verifier: &dyn gents::NodeIdentity,
    ) -> Result<Value> {
        let before = editor_review::review_receipts(access, &self.demand.tool_call_doc_id).await?;
        let resumed = self.observe(client, id, json!({"kind":"advance"})).await?;
        let snapshot = resumed.current.context("resumed inventory snapshot")?;
        ensure!(
            resumed.attempt_id == REFRESH
                && snapshot.complete
                && snapshot.known_inventory
                && snapshot.entries == self.snapshot.entries,
            "reenrollment did not resume the exact pending metadata attempt"
        );
        let metadata_only =
            editor_review::review_receipts(access, &self.demand.tool_call_doc_id).await?;
        ensure!(
            metadata_only == before,
            "metadata recovery created an image demand"
        );
        let args = self.image_args(Some(RESTORED))?;
        for _ in 0..2 {
            let image: FileInventoryImage = serde_json::from_value(
                client
                    .try_call(*id, "desktop_inventory_file_change", args.clone())
                    .await?,
            )?;
            *id += 1;
            ensure!(
                image == self.image,
                "selected retry changed the recorded image or cursor"
            );
        }
        let receipts =
            editor_review::review_receipts(access, &self.demand.tool_call_doc_id).await?;
        let mut demand = self.demand.clone();
        demand.admission.request_key = demand.derive_retry_request_key(RESTORED);
        let matching: Vec<_> = receipts
            .iter()
            .filter(|row| row["request_key"] == demand.admission.request_key)
            .collect();
        ensure!(
            matching.len() == 1 && receipts.len() == before.len() + 1,
            "selected retry did not reuse exactly one image demand"
        );
        editor_review::verify_model_receipt(verifier, matching[0], &demand).await?;
        ensure!(
            matching[0]["status"] == "served"
                && matching[0]["observation_doc_id"] == self.image.entry.observation_doc_id,
            "selected retry served a different observation"
        );
        ensure!(
            before.iter().all(|old| receipts.contains(old)),
            "inventory recovery changed an earlier signed image outcome"
        );
        Ok(
            json!({"sameMetadataAttemptResumed":true, "metadataCreatedNoImageDemand":true,
            "selectedRetryServedOnce":true, "signedImageReceiptVerified":true,
            "earlierReceiptsPreserved":true}),
        )
    }
}
