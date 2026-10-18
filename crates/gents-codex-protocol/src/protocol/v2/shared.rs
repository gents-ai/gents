use serde::Deserialize;
use serde::Serialize;

macro_rules! wire_enum {
    (
        $(#[$enum_meta:meta])*
        pub enum $Name:ident {
            $( $(#[$variant_meta:meta])* $Variant:ident ),+ $(,)?
        }
    ) => {
        #[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
        $(#[$enum_meta])*
        #[serde(rename_all = "camelCase")]
        pub enum $Name {
            $( $(#[$variant_meta])* $Variant ),+
        }

    };
}

pub(super) use wire_enum;

pub(super) const fn default_enabled() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NonSteerableTurnKind {
    Review,
    Compact,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CodexErrorInfo {
    ContextWindowExceeded,
    UsageLimitExceeded,
    ServerOverloaded,
    CyberPolicy,
    HttpConnectionFailed {
        #[serde(rename = "httpStatusCode")]
        http_status_code: Option<u16>,
    },
    ResponseStreamConnectionFailed {
        #[serde(rename = "httpStatusCode")]
        http_status_code: Option<u16>,
    },
    InternalServerError,
    Unauthorized,
    BadRequest,
    ThreadRollbackFailed,
    SandboxError,
    ResponseStreamDisconnected {
        #[serde(rename = "httpStatusCode")]
        http_status_code: Option<u16>,
    },
    ResponseTooManyFailedAttempts {
        #[serde(rename = "httpStatusCode")]
        http_status_code: Option<u16>,
    },
    ActiveTurnNotSteerable {
        #[serde(rename = "turnKind")]
        turn_kind: NonSteerableTurnKind,
    },
    Other,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AskForApproval {
    #[serde(rename = "untrusted")]
    UnlessTrusted,
    OnFailure,
    OnRequest,
    Granular {
        sandbox_approval: bool,
        rules: bool,
        #[serde(default)]
        skill_approval: bool,
        #[serde(default)]
        request_permissions: bool,
        mcp_elicitations: bool,
    },
    Never,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalsReviewer {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "guardian_subagent", alias = "auto_review")]
    AutoReview,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}
