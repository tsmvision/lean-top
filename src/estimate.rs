//! Forecast time from Lake's reported job progress; never infer file counts.
use crate::monitor::LogTail;
use std::collections::VecDeque;
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub struct Estimate {
    pub remaining_seconds: Option<f64>,
    pub total_seconds: Option<f64>,
}

#[derive(Default)]
pub struct Estimator {
    generation: Option<u64>,
    samples: VecDeque<(Instant, u64)>,
}

impl Estimator {
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
        }
        let Some((reported, total)) = log.current_progress() else {
            return Estimate::default();
        };
        if self.samples.back().is_some_and(|(_, old)| reported < *old) {
            self.samples.clear();
        }
        if self.samples.back().map(|(_, value)| *value) != Some(reported) {
            if self.samples.len() == 32 {
                self.samples.pop_front();
            }
            self.samples.push_back((now, reported));
        }
        let &(start, first) = self.samples.front().unwrap();
        let elapsed = now.saturating_duration_since(start).as_secs_f64();
        let rate =
            (elapsed >= 1.0 && reported > first).then(|| (reported - first) as f64 / elapsed);
        Estimate {
            total_seconds: rate.map(|rate| total as f64 / rate),
            remaining_seconds: if reported == total {
                Some(0.0)
            } else {
                rate.map(|rate| (total - reported) as f64 / rate)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn time_uses_lake_counter_deltas_and_accepts_a_growing_total() {
        let now = Instant::now();
        let mut log = LogTail::default();
        let mut estimator = Estimator::default();
        log.progress = Some((16000, 20000));
        assert_eq!(estimator.refresh_at(&log, now).total_seconds, None);
        log.progress = Some((16010, 20000));
        let estimate = estimator.refresh_at(&log, now + Duration::from_secs(2));
        assert_eq!(estimate.total_seconds, Some(4000.0));
        assert_eq!(estimate.remaining_seconds, Some(798.0));
        log.progress = Some((16020, 30000));
        let estimate = estimator.refresh_at(&log, now + Duration::from_secs(4));
        assert_eq!(estimate.total_seconds, Some(6000.0));
        assert_eq!(estimate.remaining_seconds, Some(2796.0));
        assert!(estimator.samples.len() <= 32);
    }

    #[test]
    fn repeated_polls_do_not_invent_progress_and_restart_does_not_change_counts() {
        let now = Instant::now();
        let mut log = LogTail::default();
        log.progress = Some((16000, 20000));
        let mut estimator = Estimator::default();
        for tick in 0..100 {
            assert_eq!(
                estimator
                    .refresh_at(&log, now + Duration::from_secs(tick))
                    .total_seconds,
                None
            );
        }
        assert_eq!(estimator.samples.len(), 1);
        Estimator::default().refresh(&log);
        assert_eq!(log.current_progress(), Some((16000, 20000)));
    }

    #[test]
    fn generation_and_backwards_counter_reset_the_rate() {
        let now = Instant::now();
        let mut estimator = Estimator::default();
        let mut log = LogTail::default();
        log.progress = Some((10, 100));
        estimator.refresh_at(&log, now);
        log.progress = Some((20, 100));
        assert!(estimator
            .refresh_at(&log, now + Duration::from_secs(2))
            .total_seconds
            .is_some());
        log.generation += 1;
        assert_eq!(
            estimator
                .refresh_at(&log, now + Duration::from_secs(3))
                .total_seconds,
            None
        );
        log.progress = Some((1, 100));
        assert_eq!(
            estimator
                .refresh_at(&log, now + Duration::from_secs(4))
                .total_seconds,
            None
        );
        assert_eq!(estimator.samples.len(), 1);
    }

    #[test]
    fn absent_progress_is_unknown_and_completion_has_zero_remaining() {
        let now = Instant::now();
        let mut estimator = Estimator::default();
        let mut log = LogTail::default();
        assert_eq!(estimator.refresh_at(&log, now).remaining_seconds, None);
        log.progress = Some((42, 42));
        let estimate = estimator.refresh_at(&log, now);
        assert_eq!(estimate.remaining_seconds, Some(0.0));
        assert_eq!(estimate.total_seconds, None);
    }
}
