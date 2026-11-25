use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use defra_node::Node as EmbeddedNode;
use rapidhash::RapidHashMap;

use super::controller::{AdmitError, CapacityPool, PoolPermit};

type PoolMap = RapidHashMap<(String, String), Arc<CapacityPool>>;
type StandaloneMap = RapidHashMap<(String, String, usize), Arc<CapacityPool>>;

/// The backend admission pools of one node in this process. The runtime's
/// `AdmissionRegistry` publishes each pool it admits here and withdraws it
/// when it retires it, so plugin model calls take their permits from the pool
/// agent model calls take theirs from, and a backend's server runs at most its
/// `max_concurrent` calls from this process.
#[derive(Default)]
pub struct BackendCallPools {
    runtime: AtomicBool,
    admitted: Mutex<PoolMap>,
    standalone: Mutex<StandaloneMap>,
}

fn locked<T>(map: &Mutex<T>) -> MutexGuard<'_, T> {
    map.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn key(node_did: &str, backend_id: &str) -> (String, String) {
    (node_did.to_owned(), backend_id.to_owned())
}

impl BackendCallPools {
    pub fn of(node: &Arc<EmbeddedNode>) -> Arc<Self> {
        gents_store::node_extension::attached_or_default::<Self>(node)
    }

    pub(super) fn serve_runtime(&self) {
        self.runtime.store(true, Ordering::SeqCst);
    }

    pub(super) fn admit(&self, node_did: &str, backend_id: &str, pool: &Arc<CapacityPool>) {
        locked(&self.admitted).insert(key(node_did, backend_id), Arc::clone(pool));
    }

    pub(super) fn retire(&self, node_did: &str, backend_id: &str, pool: &Arc<CapacityPool>) {
        let mut admitted = locked(&self.admitted);
        if admitted
            .get(&key(node_did, backend_id))
            .is_some_and(|current| Arc::ptr_eq(current, pool))
        {
            admitted.remove(&key(node_did, backend_id));
        }
    }

    /// A plugin call's admission to the backend. With a runtime in this
    /// process it is the pool the runtime admits, and none while the runtime
    /// admits no pool for the backend; without one, a pool of the backend's
    /// `max_concurrent` this process's plugin calls to it share.
    pub fn for_backend(
        &self,
        node_did: &str,
        backend_id: &str,
        max_concurrent: usize,
        max_queue_depth: usize,
    ) -> BackendCallAdmission {
        let pool = if self.runtime.load(Ordering::SeqCst) {
            locked(&self.admitted)
                .get(&key(node_did, backend_id))
                .cloned()
        } else {
            Some(Arc::clone(
                locked(&self.standalone)
                    .entry((node_did.to_owned(), backend_id.to_owned(), max_concurrent))
                    .or_insert_with(|| CapacityPool::open(max_concurrent)),
            ))
        };
        BackendCallAdmission {
            pool,
            max_queue_depth,
        }
    }
}

/// A backend's admission pool as a call no `InferenceCall` row records (a
/// plugin model call) reaches it: it waits in the backend's queue, bounded by
/// `max_queue_depth`, like an agent's call.
#[derive(Clone)]
pub struct BackendCallAdmission {
    pool: Option<Arc<CapacityPool>>,
    max_queue_depth: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendCallRefused {
    #[error("the backend's admission queue is full")]
    QueueFull,
    #[error("the backend's admission is closed")]
    Closed,
}

/// Held for as long as the call runs at the server.
pub struct BackendCallPermit {
    _permit: PoolPermit,
}

impl BackendCallAdmission {
    pub async fn admit(&self) -> Result<BackendCallPermit, BackendCallRefused> {
        let Some(pool) = &self.pool else {
            return Err(BackendCallRefused::Closed);
        };
        let (immediate, _) = pool.enter(self.max_queue_depth);
        let admitted = match immediate {
            Some(outcome) => outcome,
            None => {
                let _queued = pool.queued();
                pool.admit_waiting().await.ok_or(AdmitError::Closed)
            }
        };
        match admitted {
            Ok(permit) => Ok(BackendCallPermit { _permit: permit }),
            Err(AdmitError::NoPermits) => Err(BackendCallRefused::QueueFull),
            Err(AdmitError::Closed) => Err(BackendCallRefused::Closed),
        }
    }
}
