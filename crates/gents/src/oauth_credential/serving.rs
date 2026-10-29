use serde::Serialize;

use super::*;

pub fn backend_account<'a>(
    backend: &crate::InferenceBackend,
    accounts: &'a [AccountSummary],
) -> Option<Option<&'a AccountSummary>> {
    use crate::backend_provider::BackendProviderOauthExt;
    let crate::document_config::BackendAuth::NodeOAuth { account_ref } = &backend.auth else {
        return None;
    };
    let provider = backend.provider_kind.oauth_provider()?;
    Some(
        accounts
            .iter()
            .find(|account| account.provider == provider && account.account_ref == *account_ref),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    Enabled,
    Disabled,
    Missing,
}

impl AccountState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Missing => "account not on this node",
        }
    }
}

impl Serialize for AccountState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServingAccount {
    pub label: String,
    pub state: AccountState,
    #[serde(skip)]
    pub provider: Option<&'static str>,
}

pub fn serving_account(
    backend: &crate::InferenceBackend,
    accounts: &[AccountSummary],
) -> ServingAccount {
    let state = |enabled| {
        if enabled {
            AccountState::Enabled
        } else {
            AccountState::Disabled
        }
    };
    use crate::backend_provider::BackendProviderOauthExt;
    let provider = matches!(
        backend.auth,
        crate::document_config::BackendAuth::NodeOAuth { .. }
    )
    .then(|| backend.provider_kind.oauth_provider())
    .flatten();
    let (label, state) = match backend_account(backend, accounts) {
        None => (backend.name.clone(), state(backend.enabled)),
        Some(None) => (backend.name.clone(), AccountState::Missing),
        Some(Some(account)) => (account.label.clone(), state(account.enabled)),
    };
    ServingAccount {
        label,
        state,
        provider,
    }
}
