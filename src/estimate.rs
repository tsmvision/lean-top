//! Lake-reported job progress, prior successful totals, and bounded time forecasts.
use crate::job_history::History;
use crate::monitor::LogTail;
use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub struct Estimate {
    pub total: Option<u64>,
    pub completed: Option<u64>,
    pub remaining_seconds: Option<f64>,
    pub total_seconds: Option<f64>,
    pub error: Option<String>,
}

pub struct Estimator {
    history: History,
    generation: Option<u64>,
    samples: VecDeque<(Instant, u64)>,
    attempted_success: Option<(u64, u64)>,
    save_error: Option<String>,
}

impl Estimator {
    pub fn new(project: &Path, build_key: &str) -> Self {
        Self::from_history(History::new(project, build_key))
    }

    fn from_history(history: History) -> Self {
        Self {
            history,
            generation: None,
            samples: VecDeque::new(),
            attempted_success: None,
            save_error: None,
        }
    }

    pub fn refresh(&mut self, log: &LogTail) -> Estimate {
        self.refresh_at(log, Instant::now())
    }

    fn refresh_at(&mut self, log: &LogTail, now: Instant) -> Estimate {
        if self
            .generation
            .replace(log.generation)
            .is_some_and(|old| old != log.generation)
        {
            self.samples.clear();
            self.attempted_success = None;
        }
        let success = log.successful_total();
        if let Some(total) = success {
            let attempt = (log.generation, total);
            if self.attempted_success != Some(attempt) {
                self.attempted_success = Some(attempt);
                self.save_error = self
                    .history
                    .store(total)
                    .err()
                    .map(|error| format!("Cannot save job history: {error}"));
            }
        }
        let pair = log.current_progress();
        let completed = success.or_else(|| pair.map(|(done, _)| done));
        let total = if let Some(total) = success {
            Some(total)
        } else {
            match (
                self.history.total(),
                pair.map(|(_, total)| total).filter(|total| *total > 0),
            ) {
                (Some(previous), Some(current)) => Some(previous.max(current)),
                (previous, current) => previous.or(current),
            }
        };
        if let Some(done) = completed {
            if self.samples.back().is_some_and(|(_, old)| done < *old) {
                self.samples.clear();
            }
            if self.samples.back().map(|(_, value)| *value) != Some(done) {
                if self.samples.len() == 32 {
                    self.samples.pop_front();
                }
                self.samples.push_back((now, done));
            }
        }
        let rate = self.samples.front().and_then(|&(start, first)| {
            let elapsed = now.saturating_duration_since(start).as_secs_f64();
            let done = completed?;
            (elapsed >= 1.0 && done > first).then(|| (done - first) as f64 / elapsed)
        });
        Estimate {
            total,
            completed,
            remaining_seconds: if success.is_some() {
                Some(0.0)
            } else {
                total.zip(completed).and_then(|(total, done)| {
                    rate.map(|rate| total.saturating_sub(done) as f64 / rate)
                })
            },
            total_seconds: total.zip(rate).map(|(total, rate)| total as f64 / rate),
            error: self.save_error.clone().or_else(|| {
                self.history
                    .error()
                    .map(|error| format!("Job history: {error}"))
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "lean-jobs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn estimator(&self) -> Estimator {
            Estimator::from_history(History::isolated(
                &self.0,
                "Main-cold",
                &self.0.join("cache"),
            ))
        }
        fn log(&self, text: &str) -> LogTail {
            let path = self.0.join("build.log");
            fs::write(&path, text).unwrap();
            let mut log = LogTail::new(Some(path));
            log.refresh();
            log
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn no_history_uses_lake_counts_immediately_without_sources() {
        let fixture = Fixture::new();
        let mut estimator = fixture.estimator();
        let log = fixture.log("⣾ [16000/20000] Running Main\r");
        let estimate = estimator.refresh(&log);
        assert_eq!(
            (estimate.completed, estimate.total),
            (Some(16000), Some(20000))
        );
        assert!(estimate.error.is_none(), "{:?}", estimate.error);
    }

    #[test]
    fn prior_success_seeds_total_current_growth_wins_and_final_success_is_exact() {
        let fixture = Fixture::new();
        let mut estimator = fixture.estimator();
        let log = fixture.log("Build completed successfully (30000 jobs).\n");
        let complete = estimator.refresh(&log);
        assert_eq!(
            (complete.completed, complete.total),
            (Some(30000), Some(30000))
        );
        let mut restarted = fixture.estimator();
        let mut log = fixture.log("⣾ [16000/20000] Running Main\r");
        let estimate = restarted.refresh(&log);
        assert_eq!(
            (estimate.completed, estimate.total),
            (Some(16000), Some(30000))
        );
        log.progress = Some((17000, 40000));
        assert_eq!(restarted.refresh(&log).total, Some(40000));
        // Discovery is still incomplete and is not persisted as a success.
        assert_eq!(fixture.estimator().history.total(), Some(30000));
        let log = fixture.log("Build completed successfully (25000 jobs).\n");
        let final_value = restarted.refresh(&log);
        assert_eq!(
            (final_value.completed, final_value.total),
            (Some(25000), Some(25000))
        );
        assert_eq!(fixture.estimator().history.total(), Some(25000));
    }

    #[test]
    fn history_can_supply_total_before_any_live_pair_without_inventing_completed_count() {
        let fixture = Fixture::new();
        let mut estimator = fixture.estimator();
        estimator.history.store(90000).unwrap();
        let value = estimator.refresh(&LogTail::default());
        assert_eq!((value.completed, value.total), (None, Some(90000)));
    }

    #[test]
    fn time_uses_job_deltas_and_estimated_total_and_resets_on_new_run() {
        let fixture = Fixture::new();
        let mut estimator = fixture.estimator();
        estimator.history.store(30000).unwrap();
        let mut log = LogTail::default();
        log.progress = Some((16000, 20000));
        let now = Instant::now();
        estimator.refresh_at(&log, now);
        log.progress = Some((16010, 20000));
        let value = estimator.refresh_at(&log, now + Duration::from_secs(2));
        assert_eq!(value.total_seconds, Some(6000.0));
        assert_eq!(value.remaining_seconds, Some(2798.0));
        log.progress = Some((1, 3));
        log.generation += 1;
        assert_eq!(
            estimator
                .refresh_at(&log, now + Duration::from_secs(3))
                .remaining_seconds,
            None
        );
        assert_eq!(estimator.samples.len(), 1);
    }

    #[test]
    fn unknown_zero_and_unrelated_fraction_are_not_fabricated_counts() {
        let fixture = Fixture::new();
        let mut estimator = fixture.estimator();
        let log = fixture.log("warning: ratio [123/456]\n");
        let value = estimator.refresh(&log);
        assert_eq!((value.completed, value.total), (None, None));
        let log = fixture.log("⣾ [0/0] Running job computation\r");
        let value = estimator.refresh(&log);
        assert_eq!((value.completed, value.total), (Some(0), None));
        let log = fixture.log("Nothing to build.\n");
        let value = estimator.refresh(&log);
        assert_eq!((value.completed, value.total), (Some(0), Some(0)));
        assert_eq!(value.remaining_seconds, Some(0.0));
    }
}
