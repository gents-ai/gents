use std::sync::mpsc::{self, RecvTimeoutError};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use super::STALL_CEILING;

pub struct BlockingSteps<T> {
    events: UnboundedReceiver<(T, mpsc::Sender<()>)>,
    held: Option<mpsc::Sender<()>>,
}

impl<T> BlockingSteps<T>
where
    T: Send + 'static,
{
    pub fn channel() -> (Self, impl FnMut(T) + Send) {
        let (send, events) = unbounded_channel();
        let checkpoint = move |step| {
            let (resume, wait) = mpsc::channel();
            if send.send((step, resume)).is_ok() {
                assert_ne!(
                    wait.recv_timeout(STALL_CEILING),
                    Err(RecvTimeoutError::Timeout),
                    "blocking owner waited for release"
                );
            }
        };
        (Self { events, held: None }, checkpoint)
    }

    pub async fn next(&mut self) -> T {
        if let Some(held) = self.held.take() {
            held.send(()).unwrap();
        }
        let (step, release) = tokio::time::timeout(STALL_CEILING, self.events.recv())
            .await
            .expect("blocking owner stalled")
            .expect("blocking owner finished before checkpoint");
        self.held = Some(release);
        step
    }
}
