//! Runtime-owned variable catalog for prompt templating (#497).
//!
//! The catalog is the single source of truth for variable volatility and render
//! site availability. Behavior documents can reference variables, but cannot
//! declare volatility; the runtime owns that classification.

use std::collections::BTreeMap;

/// How often a variable's value may change. Mirrors Lean `Volatility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Volatility {
    /// Filled once at runtime start, then frozen. Cache-safe in system prompts.
    RunConstant,
    /// Varies per request. Forbidden in system prompts.
    PerRequest,
}

/// Where a variable is supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// The frozen system preamble.
    System,
    /// The per-request context message.
    RequestContext,
    /// Task `prompt_template` render.
    Task,
}

#[derive(Debug, Clone)]
struct Entry {
    volatility: Volatility,
    availability: &'static [Site],
}

/// The runtime catalog: full dotted ref -> entry.
#[derive(Debug, Clone)]
pub struct Catalog {
    entries: BTreeMap<&'static str, Entry>,
}

impl Catalog {
    /// Volatility of a full ref, or `None` if the ref is unknown.
    pub fn volatility(&self, var: &str) -> Option<Volatility> {
        self.entries.get(var).map(|e| e.volatility)
    }

    /// Whether `var` is a known catalog ref available at `site`.
    pub fn is_available_at(&self, var: &str, site: Site) -> bool {
        self.entries
            .get(var)
            .is_some_and(|e| e.availability.contains(&site))
    }
}

/// The v1 catalog.
pub fn default_catalog() -> Catalog {
    use Site::*;
    use Volatility::*;

    let mut entries = BTreeMap::new();
    let mut add = |key: &'static str, volatility: Volatility, availability: &'static [Site]| {
        entries.insert(
            key,
            Entry {
                volatility,
                availability,
            },
        );
    };

    add(
        "node.node_did",
        RunConstant,
        &[System, RequestContext, Task],
    );
    add(
        "node.behavior_id",
        RunConstant,
        &[System, RequestContext, Task],
    );

    add("ctx.now", PerRequest, &[RequestContext, Task]);
    add("ctx.collection_summary", PerRequest, &[RequestContext]);

    Catalog { entries }
}
