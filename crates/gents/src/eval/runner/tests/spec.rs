use super::*;
use crate::eval::runner::spec::{fixtures, stage_specs};

#[test]
fn fixtures_merge_the_case_after_the_definition_and_read_assets_from_the_pack() {
    let pack = tempfile::tempdir().unwrap();
    std::fs::write(pack.path().join("seed.json"), b"{}").unwrap();
    let shared = EvalFixtures {
        assets: vec!["seed.json".into()],
        documents: Vec::new(),
        files: Vec::new(),
        schemas: vec!["type A {}".into()],
    };
    let per_case = EvalFixtures {
        assets: Vec::new(),
        documents: vec![EvalFixtureDocument {
            collection: "A".into(),
            document: json!({"id": 1}),
        }],
        files: vec![EvalFixtureFile {
            path: "inventory/edge-07.json".into(),
            contents: "{\"disk\": \"84%\"}\n".into(),
        }],
        schemas: vec!["type B {}".into()],
    };

    let merged = fixtures(pack.path(), Some(&shared), Some(&per_case)).unwrap();
    assert_eq!(merged.schemas, ["type A {}", "type B {}"]);
    assert_eq!(merged.documents.len(), 1);
    assert_eq!(
        merged.files,
        [
            FixtureFile {
                path: "seed.json".into(),
                contents: b"{}".to_vec(),
            },
            FixtureFile {
                path: "inventory/edge-07.json".into(),
                contents: b"{\"disk\": \"84%\"}\n".to_vec(),
            },
        ]
    );

    let escaping = EvalFixtures {
        assets: vec!["../outside.json".into()],
        ..EvalFixtures::default()
    };
    let error = fixtures(pack.path(), Some(&escaping), None).unwrap_err();
    assert!(
        format!("{error:#}").contains("inside the pack"),
        "{error:#}"
    );

    let absent = EvalFixtures {
        assets: vec!["gone.json".into()],
        ..EvalFixtures::default()
    };
    assert!(
        fixtures(pack.path(), Some(&absent), None).is_err(),
        "a missing asset is the runner's failure, not a trial outcome"
    );
}

#[test]
fn a_directory_asset_materializes_every_file_under_it() {
    let pack = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(pack.path().join("brief/notes")).unwrap();
    std::fs::write(pack.path().join("brief/kickoff.md"), b"k").unwrap();
    std::fs::write(pack.path().join("brief/notes/spec.json"), b"{}").unwrap();
    std::fs::write(pack.path().join("other.md"), b"o").unwrap();
    let declared = EvalFixtures {
        assets: vec!["brief".into()],
        ..EvalFixtures::default()
    };
    let merged = fixtures(pack.path(), Some(&declared), None).unwrap();
    assert_eq!(
        merged.files,
        [
            FixtureFile {
                path: "brief/kickoff.md".into(),
                contents: b"k".to_vec(),
            },
            FixtureFile {
                path: "brief/notes/spec.json".into(),
                contents: b"{}".to_vec(),
            },
        ]
    );
}

#[test]
fn a_stage_captures_what_it_declares_and_otherwise_what_the_run_requested() {
    let case: EvalCase = serde_json::from_value(json!({
        "case_id": "k",
        "split": "train",
        "stages": [
            {
                "stage_id": "declared",
                "prompt": "p",
                "deadline_secs": 60,
                "capture": [
                    {
                        "kind": "documents",
                        "name": "items",
                        "collection": "MailboxItem",
                        "filter": {"requester_did": {"_eq": "$trial"}},
                        "fields": ["title", "status"]
                    },
                    {"kind": "file", "name": "notes", "glob": "**/*.txt"}
                ]
            },
            {"stage_id": "bare", "prompt": "p"},
            {
                "stage_id": "seeded",
                "seed": {"collection": "Event", "document": {"kind": "signup"}},
                "deadline_secs": 60
            }
        ]
    }))
    .unwrap();
    let fallback = vec![Capture::Documents {
        name: "rows".into(),
        collection: "Row".into(),
        filter: json!({}),
        fields: Vec::new(),
    }];

    let stages = stage_specs(&case, &fallback);

    assert_eq!(
        (stages[0].stage_id.as_str(), stages[0].deadline_secs),
        ("declared", Some(60))
    );
    assert_eq!(stages[1].deadline_secs, None);
    assert_eq!(
        stages[0].captures,
        vec![
            Capture::Documents {
                name: "items".into(),
                collection: "MailboxItem".into(),
                filter: json!({"requester_did": {"_eq": "$trial"}}),
                fields: vec!["title".into(), "status".into()],
            },
            Capture::File {
                name: "notes".into(),
                glob: "**/*.txt".into(),
            },
        ]
    );
    assert_eq!(stages[1].captures, fallback);
    assert_eq!(stages[0].seed, None);
    assert_eq!(
        stages[2].seed,
        Some(FixtureDocument {
            collection: "Event".into(),
            document: json!({"kind": "signup"}),
        })
    );
}
