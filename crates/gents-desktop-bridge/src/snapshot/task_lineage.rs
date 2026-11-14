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
    scoped_cache: BTreeMap<Scope, Option<ScopedTask>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ScopedTask {
    pub node: String,
    pub task: String,
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
            scoped_cache: BTreeMap::new(),
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

    pub fn originating_scoped_task(&mut self, scope: &Scope) -> Option<ScopedTask> {
        let mut current = scope.clone();
        let mut visited = BTreeSet::new();
        let mut path = Vec::new();
        let result = loop {
            if !visited.insert(current.clone()) {
                break None;
            }
            if let Some(cached) = self.scoped_cache.get(&current) {
                break cached.clone();
            }
            path.push(current.clone());
            let Some(Some(index)) = self.by_scope.get(&current) else {
                break None;
            };
            let row = &self.rows[*index];
            if let Some(task) = &row.task {
                break Some(ScopedTask {
                    node: current.node.clone(),
                    task: task.clone(),
                });
            }
            let Some(parent) = &row.starter else {
                break None;
            };
            current = parent.clone();
        };
        for scope in path {
            self.scoped_cache.insert(scope, result.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(value: &gents_lean_vocab::families::task_lineage_originating::Scope) -> Scope {
        Scope {
            node: value.node.to_string(),
            session: value.session.to_string(),
            requester: value.requester.map(|value| value.to_string()),
        }
    }

    fn scoped_scope(value: &gents_lean_vocab::families::task_lineage_scoped_task::Scope) -> Scope {
        Scope {
            node: value.node.to_string(),
            session: value.session.to_string(),
            requester: value.requester.map(|value| value.to_string()),
        }
    }

    #[test]
    fn generated_task_lineage_cases_match_projection() {
        gents_lean_vocab::families::task_lineage_originating::replay(|i| {
            let rows = i
                .rows
                .iter()
                .map(|row| Row {
                    scope: scope(&row.scope),
                    task: row.task.map(|value| value.to_string()),
                    starter: row.starter.as_ref().map(scope),
                })
                .collect();
            let owner = i.owner.to_string();
            let mut index = LineageIndex::new(rows);
            let task = index.originating_task(&owner, &scope(&i.scope));
            assert_eq!(index.originating_task(&owner, &scope(&i.scope)), task);
            assert_eq!(index.originating_task("other-node", &scope(&i.scope)), None);
            gents_lean_vocab::families::task_lineage_originating::LineageOutput {
                task: task.map(|task| task.parse().unwrap()),
            }
        });
    }

    #[test]
    fn generated_scoped_lineage_cases_preserve_the_task_owner() {
        gents_lean_vocab::families::task_lineage_scoped_task::replay(|i| {
            let rows = i
                .rows
                .iter()
                .map(|row| Row {
                    scope: scoped_scope(&row.scope),
                    task: row.task.map(|value| value.to_string()),
                    starter: row.starter.as_ref().map(scoped_scope),
                })
                .collect();
            let mut index = LineageIndex::new(rows);
            let task = index.originating_scoped_task(&scoped_scope(&i.scope));
            assert_eq!(index.originating_scoped_task(&scoped_scope(&i.scope)), task);
            gents_lean_vocab::families::task_lineage_scoped_task::ScopedOutput {
                task_node: task.as_ref().map(|task| task.node.parse().unwrap()),
                task: task.as_ref().map(|task| task.task.parse().unwrap()),
            }
        });
    }
}
