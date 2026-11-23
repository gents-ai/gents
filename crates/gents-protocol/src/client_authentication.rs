use serde::{Deserialize, Serialize};

pub const STATUS_FIELD: &str = "client_authentication";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientAuthentication {
    Local,
    Vera {
        worker: String,
        deployment: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        shared_household: bool,
    },
    Unavailable,
}
