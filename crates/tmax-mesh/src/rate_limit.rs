use std::collections::HashMap;
use std::time::Instant;

/// Simple token-bucket rate limiter keyed by node_id.
pub struct RateLimiter {
    buckets: HashMap<String, TokenBucket>,
    capacity: u32,
    refill_per_sec: f64,
}

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// - `capacity`: max burst size per node
    /// - `refill_per_sec`: tokens added per second
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            capacity,
            refill_per_sec,
        }
    }

    /// Try to consume one token for `node_id`. Returns `true` if allowed.
    pub fn check(&mut self, node_id: &str) -> bool {
        let now = Instant::now();
        let cap = self.capacity as f64;
        let rate = self.refill_per_sec;

        let bucket = self
            .buckets
            .entry(node_id.to_string())
            .or_insert_with(|| TokenBucket {
                tokens: cap,
                last_refill: now,
            });

        // Refill
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(cap);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_within_capacity() {
        let mut rl = RateLimiter::new(3, 1.0);
        assert!(rl.check("a"));
        assert!(rl.check("a"));
        assert!(rl.check("a"));
    }

    #[test]
    fn rejects_over_capacity() {
        let mut rl = RateLimiter::new(2, 0.0); // No refill
        assert!(rl.check("a"));
        assert!(rl.check("a"));
        assert!(!rl.check("a"));
    }

    #[test]
    fn independent_per_node() {
        let mut rl = RateLimiter::new(1, 0.0);
        assert!(rl.check("a"));
        assert!(rl.check("b"));
        assert!(!rl.check("a"));
    }
}
