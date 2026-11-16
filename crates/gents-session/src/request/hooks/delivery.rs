/// Lean `Hooks.Delivery.State`: the terminal outcome and the keys of the
/// deliveries it owes, published by one transaction and settled one delivery
/// at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRecord<O, K> {
    pub terminal: Option<O>,
    pub owed: Vec<K>,
    pub delivered: Vec<K>,
}

impl<O, K> Default for DeliveryRecord<O, K> {
    fn default() -> Self {
        Self {
            terminal: None,
            owed: Vec::new(),
            delivered: Vec::new(),
        }
    }
}

impl<O, K> DeliveryRecord<O, K>
where
    K: PartialEq + Clone,
{
    /// Lean `Hooks.Delivery.publish`. A rolled-back transaction publishes
    /// nothing and a terminal is published once.
    pub fn publish<I>(&mut self, outcome: O, keys: I, committed: bool)
    where
        I: IntoIterator<Item = K>,
    {
        if !committed || self.terminal.is_some() {
            return;
        }
        self.terminal = Some(outcome);
        let mut owed: Vec<K> = Vec::new();
        for key in self.owed.drain(..).chain(keys) {
            // Lean `List.dedup` keeps the last occurrence of each key.
            owed.retain(|kept| kept != &key);
            owed.push(key);
        }
        self.owed = owed;
    }

    /// Lean `Hooks.Delivery.deliver`: `applied` is whether the delivery's own
    /// transaction committed. A redelivered key is inert.
    pub fn deliver(&mut self, key: &K, applied: bool) {
        if applied && self.owed.contains(key) && !self.delivered.contains(key) {
            self.delivered.insert(0, key.clone());
        }
    }

    /// Lean `Hooks.Delivery.pending`.
    pub fn pending(&self) -> Vec<K> {
        self.owed
            .iter()
            .filter(|key| !self.delivered.contains(key))
            .cloned()
            .collect()
    }
}
