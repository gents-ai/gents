use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::dossier::Dossier;
use super::draft::Draft;

pub const OWNER: &str = "did:key:owner";
pub const SLOT: &str = "primary";

pub fn dossier() -> Dossier {
    Dossier {
        pack_name: "eval_canary".into(),
        pack_version: "1.0.0".into(),
        pack_digest: "sha256:canary".into(),
        agent_slot: "canary".into(),
        slot: SLOT.into(),
        collections: BTreeMap::from([(
            "CanaryItem".to_owned(),
            BTreeSet::from(["item_id".to_owned(), "label".to_owned()]),
        )]),
        text: String::new(),
    }
}

pub fn case(case_id: &str, split: &str) -> Value {
    json!({
        "case_id": case_id,
        "split": split,
        "stages": [{
            "stage_id": "answer",
            "prompt": "Record one item.",
            "capture": [{
                "kind": "documents",
                "name": "items",
                "collection": "CanaryItem",
                "filter": {},
                "fields": ["item_id", "label"],
            }],
            "checks": [{
                "check": "captured_rows_count",
                "params": {"name": "items", "min": 1},
                "tier": "acceptance",
            }],
        }],
    })
}

pub fn good() -> Draft {
    Draft {
        definition_id: Some("canary-quality".into()),
        title: Some("Canary quality".into()),
        cases: vec![
            case("train-a", "train"),
            case("val-a", "validation"),
            case("ho-a", "held_out"),
        ],
    }
}
