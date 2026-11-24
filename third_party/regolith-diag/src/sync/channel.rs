//! kovan's lock-free MPMC channels, with only their non-blocking half.
//!
//! The wrappers expose `try_send`, `send_async`, `try_recv` and
//! `recv_async` and nothing else: kovan's blocking `recv`, its blocking
//! `send` on a full bounded channel, `recv_deadline`, `after` and `tick`
//! park or spawn threads, which nothing in this module may do.
//!
//! kovan's bounded channel has no `try_send`, so a bounded channel here is
//! kovan's unbounded channel with its capacity kept by a
//! [`Semaphore`](super::Semaphore): a send takes a slot permit and a
//! receive returns it. Senders waiting for room are woken oldest first,
//! with the semaphore's bounded bypass.

use core::fmt;

use super::raw_semaphore::{MAX, RawSemaphore};

/// An unbounded channel: sends never wait.
///
/// ```
/// use regolith::sync::unbounded;
///
/// let (tx, rx) = unbounded();
/// tx.try_send(7).expect("an unbounded channel always has room");
/// assert_eq!(rx.try_recv(), Some(7));
/// assert_eq!(rx.try_recv(), None);
/// ```
pub fn unbounded<T: 'static>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
    let (tx, rx) = kovan_channel::unbounded();
    (UnboundedSender(tx), UnboundedReceiver(rx))
}

/// A channel holding at most `cap` messages; a send waits for room.
///
/// A capacity of zero holds nothing: `try_send` always fails and
/// `send_async` never completes, as with kovan's own bounded channel. A
/// capacity above [`Semaphore::MAX_PERMITS`](super::Semaphore::MAX_PERMITS)
/// is held at that limit.
///
/// ```
/// use regolith::sync::bounded;
///
/// let (tx, rx) = bounded(1);
/// assert!(tx.try_send(1).is_ok());
/// assert_eq!(tx.try_send(2), Err(2));
/// assert_eq!(rx.try_recv(), Some(1));
/// assert!(tx.try_send(2).is_ok());
/// ```
pub fn bounded<T: 'static>(cap: usize) -> (BoundedSender<T>, BoundedReceiver<T>) {
    let (tx, rx) = kovan_channel::unbounded();
    // A slot frees only when the receiver gets to a message, which may be a
    // long wait: a sender queues at once.
    let slots = std::sync::Arc::new(RawSemaphore::without_spin(cap.min(MAX)));
    (
        BoundedSender {
            tx,
            slots: std::sync::Arc::clone(&slots),
        },
        BoundedReceiver { rx, slots },
    )
}

/// The sending half of an [`unbounded`] channel.
pub struct UnboundedSender<T: 'static>(kovan_channel::unbounded::Sender<T>);

impl<T: 'static> UnboundedSender<T> {
    /// Sends `value`. An unbounded channel always has room, so this never
    /// fails; the `Result` matches [`BoundedSender::try_send`].
    pub fn try_send(&self, value: T) -> Result<(), T> {
        self.0.send(value);
        Ok(())
    }

    /// Sends `value`; completes at once.
    pub async fn send_async(&self, value: T) {
        self.0.send(value);
    }
}

impl<T: 'static> Clone for UnboundedSender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: 'static> fmt::Debug for UnboundedSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnboundedSender").finish_non_exhaustive()
    }
}

/// The receiving half of an [`unbounded`] channel.
pub struct UnboundedReceiver<T: 'static>(kovan_channel::unbounded::Receiver<T>);

impl<T: 'static> UnboundedReceiver<T> {
    /// The next message, if one is waiting.
    pub fn try_recv(&self) -> Option<T> {
        self.0.try_recv()
    }

    /// Waits for the next message; `None` once every sender is gone and
    /// the channel is empty.
    pub async fn recv_async(&self) -> Option<T> {
        self.0.recv_async().await
    }
}

impl<T: 'static> Clone for UnboundedReceiver<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: 'static> fmt::Debug for UnboundedReceiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnboundedReceiver").finish_non_exhaustive()
    }
}

/// The sending half of a [`bounded`] channel.
pub struct BoundedSender<T: 'static> {
    tx: kovan_channel::unbounded::Sender<T>,
    slots: std::sync::Arc<RawSemaphore>,
}

impl<T: 'static> BoundedSender<T> {
    /// Sends `value` if the channel has room and no waiting sender is owed
    /// it; otherwise hands `value` back.
    pub fn try_send(&self, value: T) -> Result<(), T> {
        if !self.slots.try_acquire(1) {
            return Err(value);
        }
        self.tx.send(value);
        Ok(())
    }

    /// Waits for room, then sends `value`. Dropping the future before it
    /// completes drops `value` unsent.
    pub async fn send_async(&self, value: T) {
        self.slots.acquire(1).await.forget();
        self.tx.send(value);
    }
}

impl<T: 'static> Clone for BoundedSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            slots: std::sync::Arc::clone(&self.slots),
        }
    }
}

impl<T: 'static> fmt::Debug for BoundedSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedSender").finish_non_exhaustive()
    }
}

/// The receiving half of a [`bounded`] channel.
pub struct BoundedReceiver<T: 'static> {
    rx: kovan_channel::unbounded::Receiver<T>,
    slots: std::sync::Arc<RawSemaphore>,
}

impl<T: 'static> BoundedReceiver<T> {
    /// The next message, if one is waiting.
    pub fn try_recv(&self) -> Option<T> {
        let value = self.rx.try_recv()?;
        self.slots.release(1);
        Some(value)
    }

    /// Waits for the next message; `None` once every sender is gone and
    /// the channel is empty.
    pub async fn recv_async(&self) -> Option<T> {
        let value = self.rx.recv_async().await?;
        self.slots.release(1);
        Some(value)
    }
}

impl<T: 'static> Clone for BoundedReceiver<T> {
    fn clone(&self) -> Self {
        Self {
            rx: self.rx.clone(),
            slots: std::sync::Arc::clone(&self.slots),
        }
    }
}

impl<T: 'static> fmt::Debug for BoundedReceiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedReceiver").finish_non_exhaustive()
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::super::queue::Policy;
    use super::*;
    use crate::sync::test_support::{Polled, block_on};

    /// A sender waiting for a slot queues at once and never spins: a slot
    /// frees only when the receiver gets to a message, which may be long.
    /// The slots' spin budget stays at zero through a contended send, its
    /// wake and its retry.
    #[test]
    fn a_full_channel_never_spins_its_senders() {
        let (tx, rx) = bounded(1);
        assert_eq!(tx.slots.queue().spin.rounds(), 0);
        tx.try_send(1).expect("room for one");
        let mut waiting = Polled::new(tx.send_async(2));
        waiting.pending();
        assert_eq!(rx.try_recv(), Some(1));
        waiting.ready();
        assert_eq!(tx.slots.queue().spin.rounds(), 0, "the slots never spin");
    }

    #[test]
    fn a_full_channel_queues_senders_until_a_receive_frees_a_slot() {
        let (tx, rx) = bounded(1);
        tx.try_send(1).expect("room for one");
        let mut waiting = Polled::new(tx.send_async(2));
        waiting.pending();
        assert_eq!(tx.try_send(3), Err(3), "the channel is full");
        assert_eq!(rx.try_recv(), Some(1));
        assert_eq!(waiting.wakes(), 1);
        waiting.ready();
        assert_eq!(block_on(rx.recv_async()), Some(2));
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn a_receiver_learns_every_sender_left() {
        let (tx, rx) = unbounded();
        let thread = std::thread::spawn(move || {
            block_on(tx.send_async(5));
        });
        assert_eq!(block_on(rx.recv_async()), Some(5));
        thread.join().expect("sender");
        assert_eq!(block_on(rx.recv_async()), None);
    }
}
