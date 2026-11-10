//! The bounded keep-alive pool: at most `max_per_host` idle connections per
//! origin, each dropped after `idle_timeout`. Connections only enter after a
//! response body ended cleanly, so nothing here is ever half used.
//!
//! Lock-free throughout: a `HopscotchMap` of origin to a shared `ArrayQueue`,
//! so checkout and checkin are a lookup plus one queue operation and never
//! wait on another request. Expired sockets are discarded at checkout.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kovan_map::HopscotchMap;
use kovan_queue::array_queue::ArrayQueue;
use rapidhash::fast::RandomState;

use super::conn::Conn;

/// The origin an idle connection belongs to.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls: bool,
}

struct IdleConn {
    conn: Conn,
    deadline: Instant,
}

/// Idle connections by origin.
pub(crate) struct IdlePool {
    map: HopscotchMap<Key, Arc<ArrayQueue<IdleConn>>, RandomState>,
    max_per_host: usize,
    idle_timeout: Duration,
}

impl IdlePool {
    pub(crate) fn new(max_per_host: usize, idle_timeout: Duration) -> Self {
        Self {
            map: HopscotchMap::with_hasher(RandomState::new()),
            max_per_host,
            idle_timeout,
        }
    }

    /// Takes the oldest queued connection for `key` that has not expired.
    pub(crate) fn take(&self, key: &Key) -> Option<Conn> {
        let queue = self.map.get(key)?;
        let now = Instant::now();
        while let Some(idle) = queue.pop() {
            if idle.deadline > now {
                return Some(idle.conn);
            }
        }
        None
    }

    /// Parks a clean connection; a full queue or a zero limit drops it.
    pub(crate) fn put(&self, key: Key, conn: Conn) {
        if self.max_per_host == 0 {
            return;
        }
        let queue = match self.map.get(&key) {
            Some(q) => q,
            None => self
                .map
                .get_or_insert(key, Arc::new(ArrayQueue::new(self.max_per_host))),
        };
        let idle = IdleConn {
            conn,
            deadline: Instant::now() + self.idle_timeout,
        };
        drop(queue.push(idle));
    }
}
