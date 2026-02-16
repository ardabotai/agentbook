use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Ban escalation schedule: 1min -> 10min -> 1hr -> 1day -> 1week -> 1month -> 1year.
const BAN_DURATIONS: [Duration; 7] = [
    Duration::from_secs(60),         // 1 minute
    Duration::from_secs(600),        // 10 minutes
    Duration::from_secs(3_600),      // 1 hour
    Duration::from_secs(86_400),     // 1 day
    Duration::from_secs(604_800),    // 1 week
    Duration::from_secs(2_592_000),  // 30 days
    Duration::from_secs(31_536_000), // 1 year
];

/// Per-key token bucket rate limiter with automatic banning.
///
/// Normal flow: token bucket allows burst up to `capacity`, refills at `per_second`.
/// Abuse flow: after `threshold` consecutive violations, the key is banned.
/// Ban duration escalates: 1min -> 10min -> 1hr -> 1day -> 1week -> 1month -> 1year.
pub struct RateLimiter {
    buckets: HashMap<String, Bucket>,
    bans: HashMap<String, BanEntry>,
    capacity: u32,
    refill_rate: f64,
    ban_threshold: u32,
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
    /// Consecutive violation count (resets on successful check).
    violations: u32,
    /// How many times this key has been banned (persists across ban cycles).
    times_banned: u32,
}

struct BanEntry {
    banned_at: Instant,
    duration: Duration,
    times_banned: u32,
}

/// Look up ban duration from the escalation schedule.
fn ban_duration_for(times_banned: u32) -> Duration {
    let idx = (times_banned as usize).min(BAN_DURATIONS.len() - 1);
    BAN_DURATIONS[idx]
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// - `capacity`: max burst size (tokens)
    /// - `per_second`: sustained rate (tokens/sec)
    ///
    /// Default ban policy: 10 violations triggers a ban.
    /// Ban escalation: 1min -> 10min -> 1hr -> 1day -> 1week -> 1month -> 1year.
    pub fn new(capacity: u32, per_second: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            bans: HashMap::new(),
            capacity,
            refill_rate: per_second,
            ban_threshold: 10,
        }
    }

    /// Create with a custom violation threshold (for testing).
    #[cfg(test)]
    fn with_threshold(capacity: u32, per_second: f64, threshold: u32) -> Self {
        Self {
            buckets: HashMap::new(),
            bans: HashMap::new(),
            capacity,
            refill_rate: per_second,
            ban_threshold: threshold,
        }
    }

    /// Check if the key is allowed to proceed.
    pub fn check(&mut self, key: &str) -> CheckResult {
        let now = Instant::now();

        // Fast path: check if banned
        if let Some(ban) = self.bans.get(key) {
            let elapsed = now.duration_since(ban.banned_at);
            if elapsed < ban.duration {
                return CheckResult::Banned {
                    remaining: ban.duration - elapsed,
                };
            }
        }

        // Remove expired ban and carry over ban count to bucket
        if let Some(ban) = self.bans.remove(key) {
            let bucket = self.buckets.entry(key.to_string()).or_insert(Bucket {
                tokens: self.capacity as f64,
                last_refill: now,
                violations: 0,
                times_banned: 0,
            });
            bucket.times_banned = ban.times_banned;
        }

        // Token bucket check
        let cap = self.capacity as f64;
        let rate = self.refill_rate;

        let bucket = self.buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: cap,
            last_refill: now,
            violations: 0,
            times_banned: 0,
        });

        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(cap);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            bucket.violations = 0;
            CheckResult::Allowed
        } else {
            bucket.violations += 1;

            if bucket.violations >= self.ban_threshold {
                bucket.violations = 0;
                let ban_index = bucket.times_banned;
                bucket.times_banned += 1;
                let duration = ban_duration_for(ban_index);

                tracing::warn!(
                    key = %key,
                    ban_secs = duration.as_secs(),
                    times_banned = bucket.times_banned,
                    "auto-banned after repeated rate limit violations"
                );

                self.bans.insert(
                    key.to_string(),
                    BanEntry {
                        banned_at: now,
                        duration,
                        times_banned: bucket.times_banned,
                    },
                );
                CheckResult::Banned {
                    remaining: duration,
                }
            } else {
                CheckResult::RateLimited
            }
        }
    }

    /// Remove stale entries that have been idle for a long time.
    pub fn cleanup(&mut self, max_idle_secs: f64) {
        let now = Instant::now();
        self.buckets.retain(|_, bucket| {
            now.duration_since(bucket.last_refill).as_secs_f64() < max_idle_secs
        });
        self.bans
            .retain(|_, ban| now.duration_since(ban.banned_at) < ban.duration);
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    #[allow(dead_code)]
    pub fn banned_count(&self) -> usize {
        self.bans.len()
    }
}

#[derive(Debug, PartialEq)]
pub enum CheckResult {
    Allowed,
    RateLimited,
    Banned { remaining: Duration },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn allows_within_capacity() {
        let mut rl = RateLimiter::new(5, 10.0);
        for _ in 0..5 {
            assert_eq!(rl.check("a"), CheckResult::Allowed);
        }
    }

    #[test]
    fn rejects_over_capacity() {
        let mut rl = RateLimiter::new(3, 1.0);
        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::RateLimited);
    }

    #[test]
    fn refills_over_time() {
        let mut rl = RateLimiter::new(2, 100.0);
        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::RateLimited);

        sleep(Duration::from_millis(50));
        assert_eq!(rl.check("a"), CheckResult::Allowed);
    }

    #[test]
    fn independent_keys() {
        let mut rl = RateLimiter::new(1, 0.1);
        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::RateLimited);
        assert_eq!(rl.check("b"), CheckResult::Allowed);
    }

    #[test]
    fn ban_after_repeated_violations() {
        let mut rl = RateLimiter::with_threshold(1, 0.001, 5);

        assert_eq!(rl.check("a"), CheckResult::Allowed);
        for _ in 0..4 {
            assert_eq!(rl.check("a"), CheckResult::RateLimited);
        }
        match rl.check("a") {
            CheckResult::Banned { remaining } => {
                assert_eq!(remaining.as_secs(), 60);
            }
            other => panic!("expected Banned, got {other:?}"),
        }

        assert!(matches!(rl.check("a"), CheckResult::Banned { .. }));
    }

    #[test]
    fn ban_escalation_schedule() {
        assert_eq!(ban_duration_for(0).as_secs(), 60);
        assert_eq!(ban_duration_for(1).as_secs(), 600);
        assert_eq!(ban_duration_for(2).as_secs(), 3_600);
        assert_eq!(ban_duration_for(3).as_secs(), 86_400);
        assert_eq!(ban_duration_for(4).as_secs(), 604_800);
        assert_eq!(ban_duration_for(5).as_secs(), 2_592_000);
        assert_eq!(ban_duration_for(6).as_secs(), 31_536_000);
        assert_eq!(ban_duration_for(7).as_secs(), 31_536_000);
        assert_eq!(ban_duration_for(100).as_secs(), 31_536_000);
    }

    #[test]
    fn ban_escalates_on_repeat_offense() {
        let mut rl = RateLimiter::with_threshold(1, 0.001, 2);

        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::RateLimited);
        match rl.check("a") {
            CheckResult::Banned { remaining } => {
                assert_eq!(remaining.as_secs(), 60, "first ban should be 1 minute");
            }
            other => panic!("expected Banned, got {other:?}"),
        }

        rl.bans.remove("a");

        assert_eq!(rl.check("a"), CheckResult::RateLimited);
        match rl.check("a") {
            CheckResult::Banned { remaining } => {
                assert_eq!(remaining.as_secs(), 600, "second ban should be 10 minutes");
            }
            other => panic!("expected Banned, got {other:?}"),
        }

        rl.bans.remove("a");

        assert_eq!(rl.check("a"), CheckResult::RateLimited);
        match rl.check("a") {
            CheckResult::Banned { remaining } => {
                assert_eq!(remaining.as_secs(), 3_600, "third ban should be 1 hour");
            }
            other => panic!("expected Banned, got {other:?}"),
        }
    }

    #[test]
    fn cleanup_removes_expired_bans() {
        let mut rl = RateLimiter::with_threshold(1, 0.001, 2);

        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::RateLimited);
        assert!(matches!(rl.check("a"), CheckResult::Banned { .. }));
        assert_eq!(rl.banned_count(), 1);

        rl.bans.get_mut("a").unwrap().banned_at = Instant::now() - Duration::from_secs(120);
        rl.cleanup(600.0);
        assert_eq!(rl.banned_count(), 0);
    }

    #[test]
    fn other_keys_unaffected_by_ban() {
        let mut rl = RateLimiter::with_threshold(1, 0.001, 2);

        assert_eq!(rl.check("a"), CheckResult::Allowed);
        assert_eq!(rl.check("a"), CheckResult::RateLimited);
        assert!(matches!(rl.check("a"), CheckResult::Banned { .. }));

        assert_eq!(rl.check("b"), CheckResult::Allowed);
    }
}
