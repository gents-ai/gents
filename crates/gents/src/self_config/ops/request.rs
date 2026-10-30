use anyhow::{bail, Result};
use serde_json::{Map, Value};

use super::AgentAnchor;
use crate::config_client::patch::{SelfConfigPatch, SelfConfigTarget};
use crate::config_client::ConfigApplyTxn;

pub(crate) struct ApplyRequest<'a> {
    pub(crate) target: SelfConfigTarget,
    pub(crate) patch: SelfConfigPatch,
    pub(crate) allow_create: bool,
    pub(crate) require_create: bool,
    pub(crate) guard_selected_chain: bool,
    pub(crate) resolve_unique: ResolveUniqueFn<'a>,
    pub(crate) on_create: OnCreateFn<'a>,
    pub(crate) normalize: NormalizeFn<'a>,
    pub(crate) validate: ValidateFn<'a>,
    pub(crate) guard: GuardFn<'a>,
}

pub(crate) type ResolveUniqueFn<'a> =
    Box<dyn Fn(&AgentAnchor) -> Result<String> + Send + Sync + 'a>;
pub(crate) type OnCreateFn<'a> =
    Box<dyn Fn(&str, &mut Map<String, Value>) -> Result<()> + Send + Sync + 'a>;
pub(crate) type GuardFn<'a> = Box<
    dyn Fn(&AgentAnchor, &Map<String, Value>, &Map<String, Value>) -> Result<()> + Send + Sync + 'a,
>;

pub(crate) type ValidateFn<'a> = Box<
    dyn for<'b> Fn(
            &'b ConfigApplyTxn<'b>,
            &'b AgentAnchor,
            &'b Map<String, Value>,
            &'b Map<String, Value>,
        ) -> futures::future::BoxFuture<'b, Result<()>>
        + Send
        + Sync
        + 'a,
>;

pub(crate) type NormalizeFn<'a> = Box<
    dyn for<'b> Fn(
            &'b ConfigApplyTxn<'b>,
            &'b AgentAnchor,
            &'b Map<String, Value>,
            &'b mut Map<String, Value>,
        ) -> futures::future::BoxFuture<'b, Result<()>>
        + Send
        + Sync
        + 'a,
>;

impl<'a> ApplyRequest<'a> {
    pub(crate) fn new(target: SelfConfigTarget, patch: SelfConfigPatch) -> Self {
        Self {
            target,
            patch,
            allow_create: false,
            require_create: false,
            guard_selected_chain: true,
            resolve_unique: Box::new(|_| bail!("resolve_unique not set (internal bug)")),
            on_create: Box::new(|_, _| Ok(())),
            normalize: Box::new(|_, _, _, _| Box::pin(async { Ok(()) })),
            validate: Box::new(|_, _, _, _| Box::pin(async { Ok(()) })),
            guard: Box::new(|_, _, _| Ok(())),
        }
    }
}
