use crate::native_service::ServiceExit;

pub const CRASH_LOOP_RESTARTS: u64 = 2;

pub fn job_sample(ready: bool, job_loaded: bool, last_exit: Option<&ServiceExit>) -> JobSample<'_> {
    match last_exit {
        _ if ready => JobSample::Ready,
        _ if !job_loaded => JobSample::Unloaded,
        Some(exit) => JobSample::Exited(exit),
        None => JobSample::Running,
    }
}

pub fn restarts_since(baseline: &mut Option<u64>, counter: u64) -> u64 {
    let first = baseline
        .filter(|first| *first <= counter)
        .unwrap_or(counter);
    *baseline = Some(first);
    counter - first
}

#[derive(Debug, Default)]
pub struct CrashLoopWatch {
    pub baseline: Option<u64>,
    looping: Option<(ServiceExit, u64)>,
}

pub enum JobSample<'a> {
    Unloaded,
    Ready,
    Running,
    Exited(&'a ServiceExit),
}

impl CrashLoopWatch {
    pub fn observe(&mut self, sample: JobSample<'_>) -> Option<(ServiceExit, u64)> {
        match sample {
            JobSample::Unloaded | JobSample::Ready => *self = Self::default(),
            JobSample::Exited(exit) if exit.clean => *self = Self::default(),
            JobSample::Exited(exit) => {
                let restarts = restarts_since(&mut self.baseline, exit.restarts);
                self.looping = (restarts >= CRASH_LOOP_RESTARTS).then(|| (exit.clone(), restarts));
            }
            JobSample::Running => {}
        }
        self.looping.clone()
    }
}
