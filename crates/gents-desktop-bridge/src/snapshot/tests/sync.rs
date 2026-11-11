use serde_json::json;

use crate::types::ClientUpdateEvent;

#[test]
fn client_update_event_serializes_store_reason_for_projection_wakes() {
    let event = ClientUpdateEvent::coarse("store");
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({ "reason": "store" })
    );
}
