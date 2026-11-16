use std::any::{Any, TypeId};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use defra_node::Node as EmbeddedNode;
use rapidhash::{HashMapExt, RapidHashMap};

#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("a different {type_name} is already attached")]
    AlreadyAttached { type_name: &'static str },
}

#[derive(Clone)]
struct Slot {
    value: Arc<dyn Any + Send + Sync>,
    type_name: &'static str,
}

/// At most one value per type, fixed once attached.
#[derive(Clone, Default)]
pub struct Extensions(RapidHashMap<TypeId, Slot>);

impl Extensions {
    /// Nothing composed: every lifecycle point reached through it fails
    /// closed.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn of<T>(value: Arc<T>) -> Self
    where
        T: Any + Send + Sync,
    {
        let mut extensions = Self::default();
        extensions.0.insert(
            TypeId::of::<T>(),
            Slot {
                value,
                type_name: std::any::type_name::<T>(),
            },
        );
        extensions
    }

    pub fn attach<T>(&mut self, value: Arc<T>) -> Result<(), ExtensionError>
    where
        T: Any + Send + Sync,
    {
        self.insert(
            TypeId::of::<T>(),
            Slot {
                value,
                type_name: std::any::type_name::<T>(),
            },
        )
    }

    pub fn attach_all(&mut self, other: &Extensions) -> Result<(), ExtensionError> {
        other
            .0
            .iter()
            .try_for_each(|(key, slot)| self.insert(*key, slot.clone()))
    }

    fn insert(&mut self, key: TypeId, slot: Slot) -> Result<(), ExtensionError> {
        match self.0.get(&key) {
            Some(existing)
                if !std::ptr::addr_eq(Arc::as_ptr(&existing.value), Arc::as_ptr(&slot.value)) =>
            {
                Err(ExtensionError::AlreadyAttached {
                    type_name: slot.type_name,
                })
            }
            Some(_) => Ok(()),
            None => {
                self.0.insert(key, slot);
                Ok(())
            }
        }
    }

    pub fn get<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        Arc::clone(&self.0.get(&TypeId::of::<T>())?.value)
            .downcast::<T>()
            .ok()
    }
}

impl std::fmt::Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Extensions").field(&self.0.len()).finish()
    }
}

impl PartialEq for Extensions {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().all(|(key, value)| {
                other.0.get(key).is_some_and(|theirs| {
                    std::ptr::addr_eq(Arc::as_ptr(&value.value), Arc::as_ptr(&theirs.value))
                })
            })
    }
}

impl Eq for Extensions {}

/// DefraDB's `Node` has no extension field, so a node's extensions live in a
/// process table keyed by its address. Each entry holds a `Weak` to its node:
/// a live `Weak` keeps the node's allocation, so no other node can be
/// allocated at that address while the entry exists. Once the node's last
/// strong reference drops, its entry is removed, with its values, on the next
/// attachment.
struct Slots {
    node: Weak<EmbeddedNode>,
    values: Extensions,
}

fn table() -> &'static Mutex<RapidHashMap<usize, Slots>> {
    static TABLE: OnceLock<Mutex<RapidHashMap<usize, Slots>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(RapidHashMap::new()))
}

fn key(node: &EmbeddedNode) -> usize {
    node as *const EmbeddedNode as usize
}

#[cfg(test)]
fn attach<T>(node: &Arc<EmbeddedNode>, value: Arc<T>) -> Result<(), ExtensionError>
where
    T: Any + Send + Sync,
{
    attach_all(node, &Extensions::of(value))
}

pub(crate) fn attach_all(
    node: &Arc<EmbeddedNode>,
    extensions: &Extensions,
) -> Result<(), ExtensionError> {
    let (dead, attached) = {
        let mut table = table()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dead = take_dead(&mut table);
        let slots = table.entry(key(node)).or_insert_with(|| Slots {
            node: Arc::downgrade(node),
            values: Extensions::default(),
        });
        (dead, slots.values.attach_all(extensions))
    };
    drop(dead);
    attached
}

pub fn attached<T>(node: &EmbeddedNode) -> Option<Arc<T>>
where
    T: Any + Send + Sync,
{
    let table = table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let slots = table.get(&key(node))?;
    if slots.node.strong_count() == 0 {
        return None;
    }
    slots.values.get::<T>()
}

fn take_dead(table: &mut RapidHashMap<usize, Slots>) -> Vec<Slots> {
    let dead_keys = table
        .iter()
        .filter(|(_, slots)| slots.node.strong_count() == 0)
        .map(|(key, _)| *key)
        .collect::<Vec<_>>();
    dead_keys
        .into_iter()
        .filter_map(|key| table.remove(&key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Marker(&'static str);

    async fn node() -> Arc<EmbeddedNode> {
        Arc::new(EmbeddedNode::builder().build().await.unwrap())
    }

    #[tokio::test]
    async fn two_nodes_in_one_process_hold_their_own_values() {
        let first = node().await;
        let second = node().await;
        let unattached = node().await;
        attach(&first, Arc::new(Marker("first"))).unwrap();
        attach(&second, Arc::new(Marker("second"))).unwrap();

        assert_eq!(attached::<Marker>(&first).unwrap().0, "first");
        assert_eq!(attached::<Marker>(&second).unwrap().0, "second");
        assert!(attached::<Marker>(&unattached).is_none());
    }

    #[tokio::test]
    async fn a_node_keeps_the_value_it_was_built_with() {
        let node = node().await;
        let value = Arc::new(Marker("composed"));
        attach(&node, Arc::clone(&value)).unwrap();
        attach(&node, Arc::clone(&value)).unwrap();
        assert!(matches!(
            attach(&node, Arc::new(Marker("other"))),
            Err(ExtensionError::AlreadyAttached { .. })
        ));
        assert_eq!(attached::<Marker>(&node).unwrap().0, "composed");
    }

    #[tokio::test]
    async fn a_dropped_nodes_values_are_released() {
        let node = node().await;
        let value = Arc::new(Marker("released"));
        attach(&node, Arc::clone(&value)).unwrap();
        drop(node);
        let next = self::node().await;
        attach(&next, Arc::new(Marker("next"))).unwrap();
        assert_eq!(Arc::strong_count(&value), 1);
    }
}
