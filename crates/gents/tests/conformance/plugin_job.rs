//! Generated plugin-job admission, adoption, control, key, authority, wire and
//! lineage rows checked against the native job owners. Every `expected` value comes from the Lean
//! owners (`admitStep`, `settleStep`, `adopt`, `adoptTerminal`, `hostPin`,
//! `uniqueHandle`, `splitAddress`, `Control.kill`, `resumeValid`, `resumePin`,
//! `echo`, `resultReadAuthorized`, `resultOffered`, `onInvocationFailure`, the
//! `SpawnKey` encoders and `decodeRetryKey?`, `originOf`, `provenanceOf`,
//! `recordAfterInstall` and `openingRequest`); the rows' inputs are translated
//! to the owners' native types and nothing is recomputed here.
//!
//! Wherever `gents_protocol::job` has the type, the translation builds that
//! type. Lean models only the presence or count of a rider, so a protocol field
//! the model does not carry (a prompt, a title, a write's body) is left empty.
//! Composed wire traces carry the actual resolution table and raw ceilings.
use gents_lean_vocab::{
    lean_plugin_job_authority_cases, lean_plugin_job_wire_cases, LeanPluginJobAuthorityCase,
    LeanPluginJobRecord, LeanPluginJobWireCase, LeanPluginJobWireKind,
};
use gents_pack::pack::home_install::{
    installer_provenance, record_after_install_via, recorded_provenance, InstalledVia,
};
use gents_pack::pack::job_admission::job_block_admissible;
use gents_pack::pack::PackKind;
use gents_protocol::job::{
    ChildOutputRequest, ChildOutputResponse, ChildOutputSlice, DataCall, DataQuery,
    DataQueryResponse, JobCall, JobControlKind, JobControlOutcome, JobControlRequest, JobOrigin,
    JobOutcome, JobPolicy, JobReadError, JobResultRequest, JobResultResponse, JobState,
    JobStepInput, JobStepOutput, JobView, PluginDataDeclaration, PluginJobDeclaration,
    PluginJobSettings, RoundResult, StampedField,
};
use gents_protocol::job_settings::{policy_valid, wire_targets_from};
use gents_toolcalls::background::job::origin::call_origin;
use gents_toolcalls::test_support::plugin_job::{job_policy, spelled, target_ids};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Lean `Record` as the stored `installed_via`: no record, a record without
/// the field (written before the field existed), or a stored value.
fn home_record(r: &LeanPluginJobRecord) -> Option<Option<InstalledVia>> {
    match (r.kind.as_str(), r.via.as_deref()) {
        ("absent", None) => None,
        ("legacy", None) => Some(None),
        ("written", Some(via)) => Some(Some(spelled(via))),
        other => panic!("unknown Lean install record {other:?}"),
    }
}

/// Lean `callOrigin`, `provenanceOf`, `installerProvenance`, `recordAfterInstall`,
/// `packJobBlockAdmissible`, `Policy.valid` and `wireTargetsFrom` through the
/// pure cores `origin::call_origin`,
/// `home_install::{recorded_provenance, installer_provenance, record_after_install_via}` and
/// `pack::job_admission::job_block_admissible` and `job_settings::{policy_valid, wire_targets_from}`.
#[test]
fn generated_plugin_job_authority_cases_drive_origin_and_provenance() {
    let cases = lean_plugin_job_authority_cases();
    let mut names = BTreeSet::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for case in cases {
        assert!(names.insert(case.name()), "duplicate {}", case.name());
        match case {
            LeanPluginJobAuthorityCase::Provenance {
                name,
                pack_kind,
                record,
                expected,
            } => {
                *kinds.entry("provenance").or_default() += 1;
                assert_eq!(
                    recorded_provenance(spelled::<PackKind>(pack_kind), home_record(record)),
                    spelled::<InstalledVia>(&expected.provenance),
                    "{name}: provenance"
                );
            }
            LeanPluginJobAuthorityCase::Install {
                name,
                prior,
                pack_kind,
                explicit,
                via,
                expected,
            } => {
                *kinds.entry("install").or_default() += 1;
                let kind: PackKind = spelled(pack_kind);
                let stored = record_after_install_via(
                    home_record(prior),
                    kind.clone(),
                    *explicit,
                    spelled(via),
                );
                let record = Some(Some(stored));
                assert_eq!(record, home_record(&expected.record), "{name}: record");
                assert_eq!(
                    recorded_provenance(kind, record),
                    spelled::<InstalledVia>(&expected.provenance),
                    "{name}: provenance after install"
                );
            }
            LeanPluginJobAuthorityCase::Installer {
                name,
                via,
                pack_authority,
                expected,
            } => {
                *kinds.entry("installer").or_default() += 1;
                assert_eq!(
                    installer_provenance(spelled(via), *pack_authority),
                    spelled::<InstalledVia>(&expected.provenance),
                    "{name}: installer provenance"
                );
            }
            LeanPluginJobAuthorityCase::Origin {
                name,
                call,
                expected,
            } => {
                *kinds.entry("origin").or_default() += 1;
                assert_eq!(
                    call_origin(call.script.as_deref()),
                    spelled::<JobOrigin>(&expected.origin),
                    "{name}: origin"
                );
            }
            LeanPluginJobAuthorityCase::PackJobBlock {
                name,
                via,
                closure,
                block,
                expected,
            } => {
                *kinds.entry("pack_job_block").or_default() += 1;
                assert_eq!(
                    job_block_admissible(
                        spelled(via),
                        closure,
                        &block.targets_from,
                        &block.trusted,
                        spelled(&block.dynamic),
                    ),
                    expected.admissible,
                    "{name}: admissible"
                );
            }
            LeanPluginJobAuthorityCase::PolicyValid {
                name,
                agent_targets,
                policy,
                expected,
            } => {
                *kinds.entry("policy_valid").or_default() += 1;
                assert_eq!(
                    policy_valid(agent_targets, &job_policy(policy)),
                    expected.valid,
                    "{name}: valid"
                );
            }
            LeanPluginJobAuthorityCase::ConsentedDynamic {
                name,
                consent,
                mode,
                expected,
            } => {
                *kinds.entry("consented_dynamic").or_default() += 1;
                assert_eq!(
                    gents_protocol::job_settings::consented_dynamic(*consent, spelled(mode)),
                    spelled(&expected.mode),
                    "{name}: mode"
                );
            }
            LeanPluginJobAuthorityCase::PublishedRead {
                name,
                publication_in_progress,
                expected,
            } => {
                *kinds.entry("published_read").or_default() += 1;
                let home = tempfile::tempdir().expect("plugin home");
                let _stage = publication_in_progress.then(|| {
                    gents_pack::plugin::store::try_lock_store(home.path()).expect("stage lock")
                });
                let result = gents_pack::plugin::store::read_published_record(
                    home.path(),
                    "fixture",
                    "absent",
                );
                assert_eq!(
                    matches!(
                        result,
                        Err(gents_pack::plugin::store::PublishedPluginReadError::Busy)
                    ),
                    expected.deferred,
                    "{name}: defer staged read"
                );
            }
            LeanPluginJobAuthorityCase::WireTargetsFrom {
                name,
                pack,
                pack_targets,
                policy,
                job_targets,
                caller_targets,
                expected,
            } => {
                *kinds.entry("wire_targets_from").or_default() += 1;
                assert_eq!(
                    wire_targets_from(
                        pack,
                        &target_ids(pack_targets),
                        job_policy(policy),
                        &target_ids(job_targets),
                        &target_ids(caller_targets),
                    ),
                    (
                        job_policy(&expected.policy),
                        target_ids(&expected.job_targets),
                        target_ids(&expected.caller_targets),
                    ),
                    "{name}: wired policy and targets"
                );
            }
        }
    }
    assert_eq!(
        kinds,
        BTreeMap::from([
            ("consented_dynamic", 4),
            ("install", 6),
            ("installer", 4),
            ("origin", 4),
            ("pack_job_block", 5),
            ("policy_valid", 4),
            ("provenance", 12),
            ("published_read", 2),
            ("wire_targets_from", 2),
        ]),
        "Lean pins these authority rows in `authority_regressions`"
    );
}

/// Decodes one wire sample strictly into its protocol type and returns the
/// re-encoded JSON, which must equal the Lean rendering.
fn round_trip<T>(case: &LeanPluginJobWireCase) -> serde_json::Value
where
    T: DeserializeOwned + Serialize,
{
    let decoded: T = serde_json::from_value(case.value.clone())
        .unwrap_or_else(|error| panic!("{}: strict decode: {error}", case.name));
    let encoded = serde_json::to_value(&decoded)
        .unwrap_or_else(|error| panic!("{}: encode: {error}", case.name));
    assert_eq!(encoded, case.value, "{}: re-encoded JSON", case.name);
    encoded
}

fn round_trip_case(case: &LeanPluginJobWireCase) -> serde_json::Value {
    match case.kind {
        LeanPluginJobWireKind::JobStepInput => round_trip::<JobStepInput>(case),
        LeanPluginJobWireKind::JobStepOutput => round_trip::<JobStepOutput>(case),
        LeanPluginJobWireKind::JobView => round_trip::<JobView>(case),
        LeanPluginJobWireKind::JobOutcome => {
            let encoded = round_trip::<JobOutcome>(case);
            assert_eq!(
                encoded["code"], case.name,
                "{}: a host code names its row",
                case.name
            );
            encoded
        }
    }
}

/// Every wire sample the Lean types render decodes strictly into its
/// `gents_protocol::job` type and re-encodes to the same canonical JSON.
#[test]
fn generated_plugin_job_wire_cases_round_trip_protocol_types() {
    let cases = lean_plugin_job_wire_cases();
    let mut names = BTreeSet::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for case in cases {
        assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
        round_trip_case(case);
        *kinds
            .entry(match case.kind {
                LeanPluginJobWireKind::JobStepInput => "job_step_input",
                LeanPluginJobWireKind::JobStepOutput => "job_step_output",
                LeanPluginJobWireKind::JobView => "job_view",
                LeanPluginJobWireKind::JobOutcome => "job_outcome",
            })
            .or_default() += 1;
    }
    assert_eq!(
        kinds,
        BTreeMap::from([
            ("job_outcome", 9),
            ("job_step_input", 3),
            ("job_step_output", 9),
            ("job_view", 1),
        ]),
        "Lean pins these wire samples in `wire_regressions`"
    );
}

/// `strip_secret_fields` drops a field whose upper-cased name contains one of
/// these (`documents::callback::is_secret_env_name`), so no job field may.
const SECRET_SUBSTRINGS: [&str; 4] = ["KEY", "SECRET", "TOKEN", "PASSWORD"];

fn collect_field_names(value: &serde_json::Value, names: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, nested) in map {
                names.insert(name.clone());
                collect_field_names(nested, names);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_field_names(item, names);
            }
        }
        _ => {}
    }
}

/// Every optional field is set so `skip_serializing_if` hides none.
fn data_query() -> DataQuery {
    DataQuery {
        collection: "c".to_owned(),
        filter: serde_json::Map::new(),
        order: Some(serde_json::Map::new()),
        limit: 1,
        offset: Some(0),
        fields: vec!["f".to_owned()],
    }
}

#[test]
fn plugin_job_protocol_field_names_are_secret_free() {
    let mut names = BTreeSet::new();
    for case in lean_plugin_job_wire_cases() {
        collect_field_names(&round_trip_case(case), &mut names);
    }
    let settings: PluginJobSettings =
        serde_json::from_value(serde_json::json!({})).expect("settings defaults");
    for constructed in [
        serde_json::to_value(JobPolicy::default()),
        serde_json::to_value(settings),
        serde_json::to_value(JobControlRequest {
            row_doc_id: "row".to_owned(),
            kind: JobControlKind::Stop,
        }),
        serde_json::to_value(JobResultRequest {
            row_doc_id: "row".to_owned(),
            offset: 0,
            limit: 1,
        }),
        serde_json::to_value(JobResultResponse {
            row_doc_id: "row".to_owned(),
            state: JobState::Running,
            result: "text".to_owned(),
            offset: 0,
            total_bytes: 4,
            truncated: true,
        }),
        serde_json::to_value(ChildOutputRequest {
            slots: vec!["1:a".to_owned()],
            offset: 0,
            limit: 1,
        }),
        serde_json::to_value(ChildOutputResponse {
            slots: BTreeMap::from([(
                "1:a".to_owned(),
                ChildOutputSlice {
                    text: "text".to_owned(),
                    truncated: true,
                },
            )]),
        }),
        serde_json::to_value(DataCall {
            id: "d1".to_owned(),
            query: data_query(),
        }),
        serde_json::to_value(DataQueryResponse {
            rows: vec![serde_json::Map::new()],
        }),
        serde_json::to_value(JobCall::ChildOutput {
            id: "c1".to_owned(),
            child_output: ChildOutputRequest {
                slots: Vec::new(),
                offset: 0,
                limit: 1,
            },
        }),
        serde_json::to_value(JobCall::Result {
            id: "c2".to_owned(),
            result: JobResultRequest {
                row_doc_id: "row".to_owned(),
                offset: 0,
                limit: 1,
            },
        }),
        serde_json::to_value(RoundResult::Result { result: 1 }),
        serde_json::to_value(RoundResult::<u32>::Error {
            error: "e".to_owned(),
        }),
        serde_json::to_value(JobControlOutcome::Applied {
            state: JobState::Running,
            killed_children: 1,
        }),
        serde_json::to_value(JobControlOutcome::AlreadyTerminal {
            state: JobState::Running,
        }),
        serde_json::to_value(JobReadError::NotAJob),
        serde_json::to_value(JobReadError::Unavailable {
            message: "m".to_owned(),
        }),
        serde_json::to_value(JobReadError::Malformed {
            message: "m".to_owned(),
        }),
        serde_json::to_value(PluginJobDeclaration(true)),
        serde_json::to_value(PluginDataDeclaration {
            collections: vec!["c".to_owned()],
            ops: vec!["query".to_owned()],
            stamped: BTreeMap::from([("f".to_owned(), StampedField::CallerDid)]),
        }),
    ] {
        collect_field_names(&constructed.expect("constructed value encodes"), &mut names);
    }
    assert!(
        names.contains("row_doc_id") && names.contains("trusted_packs"),
        "the walk reaches nested and defaulted fields: {names:?}"
    );
    for name in &names {
        let upper = name.to_ascii_uppercase();
        for needle in SECRET_SUBSTRINGS {
            assert!(
                !upper.contains(needle),
                "job field {name:?} contains {needle}"
            );
        }
    }
}
