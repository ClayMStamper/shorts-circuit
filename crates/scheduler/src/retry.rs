//! Retry timing: exponential backoff with jitter, and rate-limit delays that are never shorter
//! than the provider asked for.

use std::time::Duration;

use rand::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Delay before the first retry (before jitter).
    pub base: Duration,
    /// Upper bound for any backoff delay.
    pub max: Duration,
    /// Transient failures allowed before the job fails.
    pub max_failures: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(30),
            max: Duration::from_secs(60 * 60),
            max_failures: 6,
        }
    }
}

impl RetryPolicy {
    /// Backoff after the `failures`-th consecutive failure (1-based): `base * 2^(failures-1)`,
    /// capped at `max`, then "equal jitter" — a uniform pick in `[d/2, d]` — so retries from
    /// many jobs don't line up.
    pub fn backoff(&self, failures: u32, rng: &mut impl Rng) -> Duration {
        let exp = failures.saturating_sub(1).min(30);
        let raw = self.base.saturating_mul(1u32 << exp).min(self.max);
        let half = raw / 2;
        let jitter_ms = u64::try_from(half.as_millis()).unwrap_or(u64::MAX);
        half + Duration::from_millis(rng.random_range(0..=jitter_ms))
    }

    /// Delay for a rate-limit response: at least `retry_after`, plus up to 10% (max 30 s)
    /// jitter.
    pub fn rate_limit_delay(&self, retry_after: Duration, rng: &mut impl Rng) -> Duration {
        let spread = (retry_after / 10).min(Duration::from_secs(30));
        let spread_ms = u64::try_from(spread.as_millis()).unwrap_or(u64::MAX);
        retry_after + Duration::from_millis(rng.random_range(0..=spread_ms))
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    #[test]
    fn backoff_grows_exponentially_within_jitter_bounds() {
        let policy = RetryPolicy {
            base: Duration::from_secs(10),
            max: Duration::from_secs(300),
            max_failures: 10,
        };
        let mut rng = StdRng::seed_from_u64(7);
        for (failures, nominal) in [
            (1, 10),
            (2, 20),
            (3, 40),
            (4, 80),
            (5, 160),
            (6, 300),
            (20, 300),
        ] {
            for _ in 0..50 {
                let d = policy.backoff(failures, &mut rng);
                let nominal = Duration::from_secs(nominal);
                assert!(d >= nominal / 2 && d <= nominal, "{failures}: {d:?}");
            }
        }
    }

    #[test]
    fn backoff_is_jittered() {
        let policy = RetryPolicy::default();
        let mut rng = StdRng::seed_from_u64(1);
        let samples: std::collections::HashSet<_> =
            (0..20).map(|_| policy.backoff(3, &mut rng)).collect();
        assert!(samples.len() > 10);
    }

    #[test]
    fn rate_limit_delay_never_undercuts_retry_after() {
        let policy = RetryPolicy::default();
        let mut rng = StdRng::seed_from_u64(3);
        for secs in [1, 60, 3600] {
            let retry_after = Duration::from_secs(secs);
            for _ in 0..50 {
                let d = policy.rate_limit_delay(retry_after, &mut rng);
                assert!(d >= retry_after);
                assert!(d <= retry_after + Duration::from_secs(30));
            }
        }
    }
}
