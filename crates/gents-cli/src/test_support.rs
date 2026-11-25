use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::Instant;

use crate::commands::cloud::{DevicePolls, PollStep};
use crate::commands::eval::init::turn::Turn;

pub(crate) struct ScriptedPolls {
    steps: Mutex<VecDeque<PollStep>>,
    asked_at: Mutex<Vec<Duration>>,
    started: Instant,
}

impl ScriptedPolls {
    pub(crate) fn new(steps: impl IntoIterator<Item = PollStep>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            asked_at: Mutex::new(Vec::new()),
            started: Instant::now(),
        }
    }

    pub(crate) fn asked_at_secs(&self) -> Vec<u64> {
        self.asked_at
            .lock()
            .expect("poll times")
            .iter()
            .map(Duration::as_secs)
            .collect()
    }
}

impl DevicePolls for ScriptedPolls {
    async fn poll_once(&self, _device_code: &str) -> Result<PollStep> {
        self.asked_at
            .lock()
            .expect("poll times")
            .push(self.started.elapsed());
        Ok(self
            .steps
            .lock()
            .expect("script")
            .pop_front()
            .unwrap_or(PollStep::Pending))
    }
}

pub(crate) struct ScriptedTurn {
    replies: VecDeque<String>,
    pub(crate) sent: Vec<String>,
}

impl ScriptedTurn {
    pub(crate) fn new<S>(replies: impl IntoIterator<Item = S>) -> Self
    where
        S: Into<String>,
    {
        Self {
            replies: replies.into_iter().map(Into::into).collect(),
            sent: Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl Turn for ScriptedTurn {
    async fn send(&mut self, content: &str) -> Result<String> {
        self.sent.push(content.to_owned());
        self.replies
            .pop_front()
            .context("the scripted author has no reply left")
    }

    fn session_id(&self) -> &str {
        "scripted-session"
    }

    fn shows_replies(&self) -> bool {
        false
    }
}
