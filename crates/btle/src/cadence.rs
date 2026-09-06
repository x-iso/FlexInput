//! How evenly a controller's reports actually arrive.
//!
//! ⭐ **Shared by both transports on purpose.** Two of them stream input off
//! one radio, and "is this rate the device's choice or our loss?" is the same
//! question in both — asked with the same numbers, or the answers cannot be
//! compared.

use std::time::{Duration, Instant};

/// Inter-report timing for one link, summarised every couple of seconds.
///
/// ⭐ **Because the gyro on this stream is a DERIVATIVE.** The per-side report
/// carries a fused angle and no angular rate, so every rate we publish is a
/// difference divided by the interval between two reports. That makes interval
/// jitter a direct multiplier on gyro noise — a stream whose spacing wobbles
/// produces a jumpy rate from a perfectly still controller, and no amount of
/// downstream smoothing recovers what the division already amplified.
///
/// It is measured per link, alongside how many links were streaming at the
/// time, because the reported symptom is that one Joy-Con turns jagged when the
/// OTHER one connects — which is a radio-scheduling story, and this is the
/// measurement that either tells it or kills it.
#[derive(Debug)]
pub struct Cadence {
    last: Option<Instant>,
    window_began: Instant,
    count: u32,
    sum_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    /// Sum of |dt - previous dt|: the jitter differentiation amplifies. Mean
    /// interval alone cannot see it — a stream alternating 5 ms and 25 ms has
    /// the same mean as a steady 15 ms one and behaves nothing like it.
    sum_abs_step_ms: f64,
    prev_ms: Option<f64>,
}

impl Cadence {
    pub fn new() -> Self {
        Self {
            last: None,
            window_began: Instant::now(),
            count: 0,
            sum_ms: 0.0,
            min_ms: f64::MAX,
            max_ms: 0.0,
            sum_abs_step_ms: 0.0,
            prev_ms: None,
        }
    }

    /// Record one report. Returns a summary once per `window`, then resets.
    pub fn tick(&mut self, now: Instant, window: Duration) -> Option<CadenceSummary> {
        if let Some(prev) = self.last {
            let ms = now.duration_since(prev).as_secs_f64() * 1000.0;
            self.count += 1;
            self.sum_ms += ms;
            self.min_ms = self.min_ms.min(ms);
            self.max_ms = self.max_ms.max(ms);
            if let Some(p) = self.prev_ms {
                self.sum_abs_step_ms += (ms - p).abs();
            }
            self.prev_ms = Some(ms);
        }
        self.last = Some(now);
        if self.window_began.elapsed() < window || self.count < 2 {
            return None;
        }
        let n = self.count as f64;
        let summary = CadenceSummary {
            hz: 1000.0 / (self.sum_ms / n),
            mean_ms: self.sum_ms / n,
            min_ms: self.min_ms,
            max_ms: self.max_ms,
            jitter_ms: self.sum_abs_step_ms / (n - 1.0).max(1.0),
            samples: self.count,
        };
        let last = self.last;
        *self = Cadence::new();
        self.last = last;
        Some(summary)
    }
}

pub struct CadenceSummary {
    pub hz: f64,
    pub mean_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub jitter_ms: f64,
    pub samples: u32,
}

#[cfg(test)]
mod cadence_tests {
    use super::Cadence;
    use std::time::{Duration, Instant};

    /// Feed `steps` millisecond gaps and take the summary at the end.
    fn summarise(steps: &[u64]) -> super::CadenceSummary {
        let base = Instant::now();
        let mut c = Cadence::new();
        let mut t = base;
        c.tick(t, Duration::from_secs(3600));
        let mut out = None;
        for (i, ms) in steps.iter().enumerate() {
            t += Duration::from_millis(*ms);
            // Only the final tick is allowed to close the window, so the
            // summary covers every step rather than an arbitrary prefix.
            let window = if i + 1 == steps.len() {
                Duration::ZERO
            } else {
                Duration::from_secs(3600)
            };
            if let Some(sum) = c.tick(t, window) {
                out = Some(sum);
            }
        }
        out.expect("a summary once the window closes")
    }

    #[test]
    fn a_steady_stream_has_no_jitter() {
        let c = summarise(&[10, 10, 10, 10]);
        assert_eq!(c.samples, 4);
        assert!((c.mean_ms - 10.0).abs() < 0.01, "mean {}", c.mean_ms);
        assert!((c.hz - 100.0).abs() < 0.1, "hz {}", c.hz);
        assert!(c.jitter_ms < 0.01, "jitter {}", c.jitter_ms);
    }

    #[test]
    fn jitter_is_what_separates_two_streams_with_the_same_mean() {
        // ⭐ The whole reason the metric exists. Both average 15 ms; one is a
        // usable derivative source and the other is not, and a mean-only
        // measurement calls them identical.
        let steady = summarise(&[15, 15, 15, 15, 15, 15]);
        let ragged = summarise(&[5, 25, 5, 25, 5, 25]);
        assert!((steady.mean_ms - ragged.mean_ms).abs() < 0.01);
        assert!(steady.jitter_ms < 0.01, "steady jitter {}", steady.jitter_ms);
        assert!(ragged.jitter_ms > 15.0, "ragged jitter {}", ragged.jitter_ms);
    }

    #[test]
    fn the_extremes_are_kept_not_averaged_away() {
        let c = summarise(&[10, 10, 40, 10]);
        assert!((c.min_ms - 10.0).abs() < 0.01, "min {}", c.min_ms);
        assert!((c.max_ms - 40.0).abs() < 0.01, "max {}", c.max_ms);
    }

    #[test]
    fn a_window_that_has_not_elapsed_reports_nothing() {
        let base = Instant::now();
        let mut c = Cadence::new();
        assert!(c.tick(base, Duration::from_secs(3600)).is_none());
        assert!(c
            .tick(base + Duration::from_millis(10), Duration::from_secs(3600))
            .is_none());
    }

    #[test]
    fn a_single_report_cannot_produce_an_interval() {
        // ❗ One report gives no gap, and a summary built from zero intervals
        // would divide by zero and publish a nonsense rate.
        let mut c = Cadence::new();
        assert!(c.tick(Instant::now(), Duration::ZERO).is_none());
    }
}
