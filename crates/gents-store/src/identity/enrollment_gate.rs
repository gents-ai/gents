use std::sync::Arc;

use defra_node::Node;
use tokio::sync::Mutex;

#[derive(Default)]
struct EnrollmentDecisionGate(Arc<Mutex<()>>);

/// Serializes enrollment decisions and permission admission on the same embedded node.
pub fn enrollment_decision_gate(node: &Arc<Node>) -> Arc<Mutex<()>> {
    Arc::clone(&crate::node_extension::attached_or_default::<EnrollmentDecisionGate>(node).0)
}
