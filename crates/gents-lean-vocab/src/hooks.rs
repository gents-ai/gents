use crate::generated::*;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookChoice<V> {
    pub value: V,
    pub effects: Vec<LeanLifecycleHookEffect>,
}

/// A scripted plugin decision; `by_shown` picks `matching` when the plugin was
/// shown exactly `count` rows across its declared reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookDecision<V> {
    Refuse {
        reason: String,
    },
    Proceed {
        value: V,
        effects: Vec<LeanLifecycleHookEffect>,
    },
    ByShown {
        count: usize,
        matching: LeanLifecycleHookChoice<V>,
        otherwise: LeanLifecycleHookChoice<V>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookScript<V> {
    pub name: String,
    pub owns: Vec<String>,
    pub bound: usize,
    pub reads: Vec<String>,
    pub decision: LeanLifecycleHookDecision<V>,
}
