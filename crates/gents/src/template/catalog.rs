//! Runtime-owned variable catalog for prompt templating (#497).

use std::collections::BTreeMap;

/// How often a variable's value may change. Mirrors Lean `Volatility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Volatility {
    RunConstant,
    PerRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    System,
    RequestContext,
    Task,
}

#[derive(Debug, Clone)]
struct Entry {
    volatility: Volatility,
    availability: &'static [Site],
}

#[derive(Debug, Clone)]
pub struct Catalog {
    entries: BTreeMap<&'static str, Entry>,
}

impl Catalog {
    pub fn volatility(&self, var: &str) -> Option<Volatility> {
        self.entries.get(var).map(|e| e.volatility)
    }

    pub fn is_available_at(&self, var: &str, site: Site) -> bool {
        self.entries
            .get(var)
            .is_some_and(|e| e.availability.contains(&site))
    }
}

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

#[cfg(test)]
impl Catalog {
    /// Build a catalog from explicit entries. Test-only: lets the guard tests
    /// exercise combinations (e.g. a run-constant var NOT available in the
    /// system preamble) that the v1 `default_catalog` does not contain.
    pub(crate) fn from_entries(entries: &[(&'static str, Volatility, &'static [Site])]) -> Catalog {
        let mut map = BTreeMap::new();
        for (key, volatility, availability) in entries {
            map.insert(
                *key,
                Entry {
                    volatility: *volatility,
                    availability,
                },
            );
        }
        Catalog { entries: map }
    }
}
