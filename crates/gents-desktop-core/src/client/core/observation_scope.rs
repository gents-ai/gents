use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::ClientCoreError;

type Result<T, E = ClientCoreError> = std::result::Result<T, E>;

#[derive(Default)]
struct Focus {
    requested: Option<String>,
    attachments: usize,
}

struct Inner {
    focus: Mutex<Focus>,
    effective: watch::Sender<Option<String>>,
}

pub(super) struct ObservationScope(Arc<Inner>);

/// Keeps observation open for a frontend whose selection is not part of the
/// embedding shell's focus. Dropping the attachment restores the latest intent.
pub struct ClientObservationAttachment(Arc<Inner>);

impl ObservationScope {
    pub fn new() -> Self {
        let (effective, _) = watch::channel(None);
        Self(Arc::new(Inner {
            focus: Mutex::new(Focus::default()),
            effective,
        }))
    }

    pub fn select(&self, requested: Option<String>) {
        let mut focus = self
            .0
            .focus
            .lock()
            .expect("client observation scope lock poisoned");
        focus.requested = requested;
        self.0.publish(&focus);
    }

    pub fn current(&self) -> Option<String> {
        self.0.effective.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<String>> {
        self.0.effective.subscribe()
    }

    pub fn attach(&self) -> Result<ClientObservationAttachment> {
        let mut focus = self
            .0
            .focus
            .lock()
            .expect("client observation scope lock poisoned");
        focus.attachments = focus
            .attachments
            .checked_add(1)
            .ok_or(ClientCoreError::TooManyObservationAttachments)?;
        self.0.publish(&focus);
        Ok(ClientObservationAttachment(self.0.clone()))
    }
}

impl Inner {
    fn publish(&self, focus: &Focus) {
        let effective = if focus.attachments == 0 {
            focus.requested.clone()
        } else {
            None
        };
        self.effective.send_if_modified(|current| {
            if *current == effective {
                false
            } else {
                *current = effective;
                true
            }
        });
    }
}

impl Drop for ClientObservationAttachment {
    fn drop(&mut self) {
        let mut focus = self
            .0
            .focus
            .lock()
            .expect("client observation scope lock poisoned");
        focus.attachments -= 1;
        self.0.publish(&focus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn node(value: &Value) -> Option<String> {
        value.as_u64().map(|id| format!("did:scope:{id}"))
    }

    #[test]
    fn generated_attachment_traces_keep_every_frontend_observable() {
        let snapshot: Value =
            gents_lean_contract::load_contract_snapshot().expect("executable Lean client contract");
        let cases = snapshot["client_observation_scope_cases"]
            .as_array()
            .expect("observation scope cases");
        assert!(!cases.is_empty());
        for case in cases {
            let owner = ObservationScope::new();
            owner.select(node(&case["initial"]["requested"]));
            let mut attachments = (0..case["initial"]["attachments"].as_u64().unwrap())
                .map(|_| owner.attach().unwrap())
                .collect::<Vec<_>>();
            let mut observations = owner.subscribe();
            for (index, state) in case["expected"].as_array().unwrap().iter().enumerate() {
                if index > 0 {
                    let action = &case["actions"][index - 1];
                    match action["kind"].as_str().unwrap() {
                        "select" => owner.select(node(&action["node"])),
                        "attach" => attachments.push(owner.attach().unwrap()),
                        "detach" => {
                            attachments.pop();
                        }
                        _ => panic!("unknown generated observation action"),
                    }
                }
                assert_eq!(owner.current(), node(&state["effective"]), "{case}");
                assert_eq!(
                    *observations.borrow_and_update(),
                    node(&state["effective"]),
                    "{case}"
                );
                assert_eq!(
                    attachments.len() as u64,
                    state["attachments"].as_u64().unwrap(),
                    "{case}"
                );
                assert_eq!(
                    owner.0.focus.lock().unwrap().requested,
                    node(&state["requested"]),
                    "{case}"
                );
            }
        }
    }
}
