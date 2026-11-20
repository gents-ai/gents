use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use defra_node::Node;
use tokio::sync::Mutex;

/// Serializes enrollment decisions and permission admission on the same embedded node.
pub fn enrollment_decision_gate(node: &Node) -> Arc<Mutex<()>> {
    static GATES: OnceLock<StdMutex<BTreeMap<usize, Weak<Mutex<()>>>>> = OnceLock::new();
    let key = node as *const Node as usize;
    let mut gates = GATES
        .get_or_init(|| StdMutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return gate;
    }
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(Mutex::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    gate
}
