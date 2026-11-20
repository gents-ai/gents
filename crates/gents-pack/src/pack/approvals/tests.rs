use serde::{Deserialize, Serialize};

use super::{Approvals, ApprovalsError, PackApproval, APPROVALS_FILE, MAX_APPROVALS_BYTES};
use crate::pack::wire_subject::WireSubject;
use crate::pack::{PackKind, PackPlugin};

fn subject(digest: &str, coordinate: &str) -> WireSubject {
    WireSubject {
        owner_pack_digest: format!("sha256:{digest}"),
        owner_pack_coordinate: coordinate.into(),
        namespace: "acme".into(),
        name: "tool".into(),
        version: "1.0.0".into(),
        digest: "sha256:artifact".into(),
        declaration: PackPlugin {
            name: "tool".into(),
            description: "Runs".into(),
            artifact: "plugins/tool.wasm".into(),
            language: "rust".into(),
            input_schema: serde_json::json!({"type": "object"}),
            ..PackPlugin::default()
        },
    }
}

fn approval(digest: &str, subjects: Vec<WireSubject>) -> PackApproval {
    PackApproval {
        digest: format!("sha256:{digest}"),
        coordinate: "acme/demo".into(),
        version: "1.0.0".into(),
        kind: PackKind::Plugins,
        approved_at: "2026-11-19T00:00:00Z".into(),
        subjects,
        yields: None,
    }
}

fn file(approvals: Vec<PackApproval>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "version": 3,
        "nodes": [{"node_did": "did:key:node", "approvals": approvals}],
    }))
    .unwrap()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Case {
    Absent,
    Oversized,
    Unparseable,
    Consistent,
    OtherDigest,
    OtherCoordinate,
    DuplicateDigest,
}

#[tokio::test]
async fn an_approvals_record_is_read_as_the_lean_model_reads_it() {
    let mut cases = Vec::new();
    for case in
        gents_lean_vocab::families::contracts_pack_installation_approvals_read::cases::<Case>()
    {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(APPROVALS_FILE);
        let written = match case.input.file {
            Case::Absent => None,
            Case::Oversized => Some(vec![b' '; MAX_APPROVALS_BYTES as usize + 1]),
            Case::Unparseable => Some(b"{\"version\": 1, \"nodes\": [".to_vec()),
            Case::Consistent => Some(file(vec![approval("p1", vec![subject("p1", "acme/demo")])])),
            Case::OtherDigest => Some(file(vec![approval("p1", vec![subject("p2", "acme/demo")])])),
            Case::OtherCoordinate => Some(file(vec![approval(
                "p1",
                vec![subject("p1", "acme/other")],
            )])),
            Case::DuplicateDigest => Some(file(vec![
                approval("p1", vec![subject("p1", "acme/demo")]),
                approval("p1", Vec::new()),
            ])),
        };
        if let Some(written) = written {
            std::fs::write(&path, written).unwrap();
        }
        let read = Approvals::under(root.path()).subjects("did:key:node").await;
        cases.push((case, read.ok().map(|subjects| subjects.len() as u64)));
    }
    for (case, read) in cases {
        case.check(read);
    }
}

#[tokio::test]
async fn an_approval_is_kept_per_node_and_replaced_by_its_digest() {
    let root = tempfile::tempdir().unwrap();
    let approvals = Approvals::under(root.path());
    assert!(approvals.subjects("did:key:node").await.unwrap().is_empty());
    approvals
        .record(
            "did:key:node",
            approval("p1", vec![subject("p1", "acme/demo")]),
        )
        .await
        .unwrap();
    approvals
        .record(
            "did:key:other",
            approval("p2", vec![subject("p2", "acme/demo")]),
        )
        .await
        .unwrap();
    approvals
        .record("did:key:node", approval("p1", Vec::new()))
        .await
        .unwrap();
    assert!(approvals.subjects("did:key:node").await.unwrap().is_empty());
    assert_eq!(
        approvals.subjects("did:key:other").await.unwrap(),
        vec![subject("p2", "acme/demo")]
    );
    let refused = approvals
        .record(
            "did:key:node",
            approval("p3", vec![subject("p1", "acme/demo")]),
        )
        .await;
    assert!(
        matches!(refused, Err(ApprovalsError::Inconsistent)),
        "{refused:?}"
    );
    assert!(approvals.subjects("did:key:node").await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unparseable_record_is_never_overwritten_by_an_approval() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(APPROVALS_FILE), b"not json").unwrap();
    let approvals = Approvals::under(root.path());
    let refused = approvals
        .record(
            "did:key:node",
            approval("p1", vec![subject("p1", "acme/demo")]),
        )
        .await;
    assert!(
        matches!(refused, Err(ApprovalsError::Unparseable(_))),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read(root.path().join(APPROVALS_FILE)).unwrap(),
        b"not json"
    );
}
