use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Scope {
    pub node: String,
    pub session: String,
    pub requester: Option<String>,
}

pub(super) struct Row {
    pub scope: Scope,
    pub task: Option<String>,
    pub starter: Option<Scope>,
}

pub(super) struct LineageIndex {
    rows: Vec<Row>,
    by_scope: BTreeMap<Scope, Option<usize>>,
    cache: BTreeMap<(String, Scope), Option<String>>,
}

impl LineageIndex {
    pub fn new(rows: Vec<Row>) -> Self {
        let mut by_scope = BTreeMap::new();
        for (index, row) in rows.iter().enumerate() {
            by_scope
                .entry(row.scope.clone())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(index));
        }
        Self {
            rows,
            by_scope,
            cache: BTreeMap::new(),
        }
    }

    pub fn originating_task(&mut self, owner: &str, scope: &Scope) -> Option<String> {
        let mut current = scope.clone();
        let mut visited = BTreeSet::new();
        let mut path = Vec::new();
        let task = loop {
            if current.node != owner || !visited.insert(current.clone()) {
                break None;
            }
            if let Some(cached) = self.cache.get(&(owner.to_owned(), current.clone())) {
                break cached.clone();
            }
            path.push(current.clone());
            let Some(Some(index)) = self.by_scope.get(&current) else {
                break None;
            };
            let row = &self.rows[*index];
            if let Some(task) = &row.task {
                break Some(task.clone());
            }
            let Some(parent) = &row.starter else {
                break None;
            };
            current = parent.clone();
        };
        for scope in path {
            self.cache.insert((owner.to_owned(), scope), task.clone());
        }
        task
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn scope(value: &Value) -> Scope {
        Scope {
            node: value["node"].as_u64().unwrap().to_string(),
            session: value["session"].as_u64().unwrap().to_string(),
            requester: value["requester"].as_u64().map(|value| value.to_string()),
        }
    }

    #[test]
    fn generated_task_lineage_cases_match_projection() {
        let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases: Vec<_> = contract["session_document_cases"]["projection"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["operation"] == "task_lineage")
            .collect();
        assert!(!cases.is_empty());
        for case in cases {
            let rows = case["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| Row {
                    scope: scope(&row["scope"]),
                    task: row["task"].as_u64().map(|value| value.to_string()),
                    starter: (!row["starter"].is_null()).then(|| scope(&row["starter"])),
                })
                .collect();
            let owner = case["owner"].as_u64().unwrap().to_string();
            let expected = case["task"].as_u64().map(|value| value.to_string());
            let mut index = LineageIndex::new(rows);
            assert_eq!(
                index.originating_task(&owner, &scope(&case["scope"])),
                expected,
                "{}",
                case["name"]
            );
            assert_eq!(
                index.originating_task(&owner, &scope(&case["scope"])),
                expected
            );
            assert_eq!(
                index.originating_task("other-node", &scope(&case["scope"])),
                None
            );
        }
    }
}
