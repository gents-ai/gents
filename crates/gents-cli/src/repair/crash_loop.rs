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

#[cfg(test)]
mod tests {
    use super::*;

    fn service_exit(restarts: u64) -> ServiceExit {
        ServiceExit {
            reason: "exited with code 78".to_string(),
            restarts,
            clean: false,
            code: Some(78),
        }
    }

    fn looping_restarts(watch: &mut CrashLoopWatch, sample: JobSample<'_>) -> Option<u64> {
        watch.observe(sample).map(|(_, restarts)| restarts)
    }

    #[test]
    fn status_reports_a_crash_loop_only_relative_to_its_first_observation() {
        let mut watch = CrashLoopWatch::default();
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(7))),
            None
        );
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(8))),
            None
        );
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(9))),
            Some(2)
        );
        assert_eq!(looping_restarts(&mut watch, JobSample::Unloaded), None);
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(9))),
            None,
            "an unloaded job starts a new baseline"
        );
    }

    #[test]
    fn a_job_that_dies_seconds_into_boot_is_a_crash_loop_across_running_samples() {
        let mut watch = CrashLoopWatch::default();
        let samples = [
            (JobSample::Running, None),
            (JobSample::Exited(&service_exit(3)), None),
            (JobSample::Running, None),
            (JobSample::Running, None),
            (JobSample::Exited(&service_exit(4)), None),
            (JobSample::Running, None),
            (JobSample::Exited(&service_exit(5)), Some(2)),
            (JobSample::Running, Some(2)),
            (JobSample::Running, Some(2)),
        ];
        for (index, (sample, expected)) in samples.into_iter().enumerate() {
            assert_eq!(
                looping_restarts(&mut watch, sample),
                expected,
                "sample {index}"
            );
        }
        let (exit, _) = watch.observe(JobSample::Running).expect("still looping");
        assert_eq!(exit.reason, "exited with code 78");

        assert_eq!(looping_restarts(&mut watch, JobSample::Ready), None);
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(6))),
            None,
            "readiness starts a new baseline"
        );
        assert_eq!(
            looping_restarts(
                &mut watch,
                JobSample::Exited(&ServiceExit {
                    reason: "exited normally".to_string(),
                    restarts: 9,
                    clean: true,
                    code: Some(0),
                })
            ),
            None
        );
    }
}
