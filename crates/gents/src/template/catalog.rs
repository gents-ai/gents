//! Runtime-owned variables available to task templates.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    Task,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    variables: &'static [&'static str],
}

impl Catalog {
    pub fn is_available_at(&self, var: &str, site: Site) -> bool {
        match site {
            Site::Task => self.variables.contains(&var),
        }
    }
}

pub fn default_catalog() -> Catalog {
    Catalog {
        variables: &["node.node_did", "node.behavior_id", "ctx.now"],
    }
}
