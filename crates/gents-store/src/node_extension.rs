use std::any::{Any, TypeId};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use defra_node::Node as EmbeddedNode;
use rapidhash::{HashMapExt, RapidHashMap};

/// Values a composition root attaches to one embedded node, at most one per
/// type, for that node's lifetime. DefraDB's `Node` has no extension field,
/// so the slots live in a process table keyed by the node's address.
///
/// Each entry holds a `Weak` to its node. A live `Weak` keeps the node's
/// allocation, so no other node can be allocated at that address while the
/// entry exists; once the node's last strong reference drops, its entry is
/// dead and is removed, with its values, on the next attachment or lookup of
/// that address.
struct Slots {
    node: Weak<EmbeddedNode>,
    values: RapidHashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeExtensionError {
    #[error("this node already has a different {type_name} attached")]
    AlreadyAttached { type_name: &'static str },
}

fn table() -> &'static Mutex<RapidHashMap<usize, Slots>> {
    static TABLE: OnceLock<Mutex<RapidHashMap<usize, Slots>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(RapidHashMap::new()))
}

fn key(node: &EmbeddedNode) -> usize {
    node as *const EmbeddedNode as usize
}

/// Attaches `value` to `node` as its `T`. Attaching the value already
/// attached is accepted; a different one is refused, since a node's
/// composition is fixed once it is built.
pub fn attach<T>(node: &Arc<EmbeddedNode>, value: Arc<T>) -> Result<(), NodeExtensionError>
where
    T: Any + Send + Sync,
{
    let dead = {
        let mut table = table().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dead = take_dead(&mut table);
        let slots = table.entry(key(node)).or_insert_with(|| Slots {
            node: Arc::downgrade(node),
            values: RapidHashMap::new(),
        });
        match slots.values.get(&TypeId::of::<T>()) {
            Some(existing)
                if !std::ptr::addr_eq(Arc::as_ptr(existing), Arc::as_ptr(&value)) =>
            {
                return Err(NodeExtensionError::AlreadyAttached {
                    type_name: std::any::type_name::<T>(),
                });
            }
            Some(_) => {}
            None => {
                slots.values.insert(TypeId::of::<T>(), value);
            }
        }
        dead
    };
    drop(dead);
    Ok(())
}

/// The `T` attached to `node`, if any.
pub fn attached<T>(node: &EmbeddedNode) -> Option<Arc<T>>
where
    T: Any + Send + Sync,
{
    let table = table().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let slots = table.get(&key(node))?;
    // The caller holds `node`, so a live entry at its address is its own.
    if slots.node.strong_count() == 0 {
        return None;
    }
    let value = Arc::clone(slots.values.get(&TypeId::of::<T>())?);
    drop(table);
    value.downcast::<T>().ok()
}

/// Removes the entries whose node has dropped; the caller drops them after
/// releasing the table.
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
            Err(NodeExtensionError::AlreadyAttached { .. })
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
