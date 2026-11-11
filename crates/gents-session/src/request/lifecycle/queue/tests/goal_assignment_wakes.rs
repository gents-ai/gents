use super::*;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::{json, Value};

