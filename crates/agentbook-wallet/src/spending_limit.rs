use alloy::primitives::U256;
use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Tracks a single spend event: amount in smallest units and when it happened.
#[derive(Debug, Clone)]
struct SpendRecord {
    /// Amount in wei (ETH) or 6-decimal units (USDC).
    amount: U256,
    /// Unix timestamp in milliseconds.
    timestamp_ms: u64,
}

/// Per-asset spending limits: per-transaction max and rolling daily max.
#[derive(Debug, Clone)]
pub struct AssetLimits {
    /// Maximum amount per single transaction (in smallest units).
    pub max_per_tx: U256,
    /// Maximum cumulative amount over a rolling 24-hour window (in smallest units).
    pub max_daily: U256,
}

/// Configuration for yolo wallet spending limits.
#[derive(Debug, Clone)]
pub struct SpendingLimitConfig {
    pub eth: AssetLimits,
    pub usdc: AssetLimits,
}

impl Default for SpendingLimitConfig {
    fn default() -> Self {
        Self {
            eth: AssetLimits {
                // 0.01 ETH per tx = 10_000_000_000_000_000 wei
                max_per_tx: U256::from(10_000_000_000_000_000u64),
                // 0.1 ETH daily = 100_000_000_000_000_000 wei
                max_daily: U256::from(100_000_000_000_000_000u64),
            },
            usdc: AssetLimits {
                // 10 USDC per tx = 10_000_000 (6 decimals)
                max_per_tx: U256::from(10_000_000u64),
                // 100 USDC daily = 100_000_000 (6 decimals)
                max_daily: U256::from(100_000_000u64),
            },
        }
    }
}

/// Enforces per-transaction and daily spending limits for the yolo wallet.
///
/// Call `check_and_record` before executing a transaction. If it returns `Ok(())`,
/// the spend has been recorded and the transaction may proceed. If it returns
/// `Err(SpendingLimitError)`, the transaction must be rejected.
pub struct SpendingLimiter {
    config: SpendingLimitConfig,
    eth_history: VecDeque<SpendRecord>,
    usdc_history: VecDeque<SpendRecord>,
}

/// Which asset is being spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asset {
    Eth,
    Usdc,
}

impl std::fmt::Display for Asset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Asset::Eth => write!(f, "ETH"),
            Asset::Usdc => write!(f, "USDC"),
        }
    }
}

/// Error returned when a spending limit is exceeded.
#[derive(Debug, Clone)]
pub enum SpendingLimitError {
    /// Single transaction exceeds per-tx limit.
    PerTxExceeded {
        asset: Asset,
        requested: U256,
        limit: U256,
    },
    /// Transaction would push daily total over the daily limit.
    DailyExceeded {
        asset: Asset,
        requested: U256,
        spent_today: U256,
        limit: U256,
    },
}

impl std::fmt::Display for SpendingLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpendingLimitError::PerTxExceeded {
                asset,
                requested,
                limit,
            } => {
                write!(
                    f,
                    "yolo {asset} transaction exceeds per-tx limit: requested {requested}, max {limit}"
                )
            }
            SpendingLimitError::DailyExceeded {
                asset,
                requested,
                spent_today,
                limit,
            } => {
                write!(
                    f,
                    "yolo {asset} transaction exceeds daily limit: requested {requested}, \
                     already spent {spent_today} in the last 24h, daily max {limit}"
                )
            }
        }
    }
}

impl std::error::Error for SpendingLimitError {}

const ROLLING_WINDOW_MS: u64 = 24 * 60 * 60 * 1000; // 24 hours

impl SpendingLimiter {
    /// Create a new limiter with the given configuration.
    pub fn new(config: SpendingLimitConfig) -> Self {
        Self {
            config,
            eth_history: VecDeque::new(),
            usdc_history: VecDeque::new(),
        }
    }

    /// Check whether a spend is allowed and, if so, record it.
    ///
    /// Returns `Ok(())` if the transaction is within limits (and the spend is recorded),
    /// or `Err(SpendingLimitError)` if limits would be exceeded (nothing is recorded).
    pub fn check_and_record(
        &mut self,
        asset: Asset,
        amount: U256,
    ) -> Result<(), SpendingLimitError> {
        self.check_and_record_at(asset, amount, Self::now_ms())
    }

    /// Testable version that accepts an explicit timestamp.
    fn check_and_record_at(
        &mut self,
        asset: Asset,
        amount: U256,
        now_ms: u64,
    ) -> Result<(), SpendingLimitError> {
        let (limits, history) = match asset {
            Asset::Eth => (&self.config.eth, &mut self.eth_history),
            Asset::Usdc => (&self.config.usdc, &mut self.usdc_history),
        };

        // 1. Per-transaction check
        if amount > limits.max_per_tx {
            return Err(SpendingLimitError::PerTxExceeded {
                asset,
                requested: amount,
                limit: limits.max_per_tx,
            });
        }

        // 2. Prune expired records
        let cutoff = now_ms.saturating_sub(ROLLING_WINDOW_MS);
        while let Some(front) = history.front() {
            if front.timestamp_ms < cutoff {
                history.pop_front();
            } else {
                break;
            }
        }

        // 3. Daily cumulative check
        let spent_today: U256 = history
            .iter()
            .map(|r| r.amount)
            .fold(U256::ZERO, |a, b| a + b);
        if spent_today + amount > limits.max_daily {
            return Err(SpendingLimitError::DailyExceeded {
                asset,
                requested: amount,
                spent_today,
                limit: limits.max_daily,
            });
        }

        // 4. Record
        history.push_back(SpendRecord {
            amount,
            timestamp_ms: now_ms,
        });

        Ok(())
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SpendingLimitConfig {
        SpendingLimitConfig {
            eth: AssetLimits {
                max_per_tx: U256::from(100u64),
                max_daily: U256::from(500u64),
            },
            usdc: AssetLimits {
                max_per_tx: U256::from(1000u64),
                max_daily: U256::from(5000u64),
            },
        }
    }

    #[test]
    fn allows_spend_within_per_tx_limit() {
        let mut limiter = SpendingLimiter::new(test_config());
        assert!(
            limiter
                .check_and_record_at(Asset::Eth, U256::from(50u64), 1000)
                .is_ok()
        );
    }

    #[test]
    fn allows_spend_at_exact_per_tx_limit() {
        let mut limiter = SpendingLimiter::new(test_config());
        assert!(
            limiter
                .check_and_record_at(Asset::Eth, U256::from(100u64), 1000)
                .is_ok()
        );
    }

    #[test]
    fn rejects_spend_exceeding_per_tx_limit() {
        let mut limiter = SpendingLimiter::new(test_config());
        let result = limiter.check_and_record_at(Asset::Eth, U256::from(101u64), 1000);
        assert!(result.is_err());
        match result.unwrap_err() {
            SpendingLimitError::PerTxExceeded {
                asset,
                requested,
                limit,
            } => {
                assert_eq!(asset, Asset::Eth);
                assert_eq!(requested, U256::from(101u64));
                assert_eq!(limit, U256::from(100u64));
            }
            other => panic!("expected PerTxExceeded, got {other}"),
        }
    }

    #[test]
    fn rejects_when_daily_limit_exceeded() {
        let mut limiter = SpendingLimiter::new(test_config());
        let now = 1_000_000u64;
        // Spend 100 five times (total 500 = daily limit)
        for i in 0..5 {
            assert!(
                limiter
                    .check_and_record_at(Asset::Eth, U256::from(100u64), now + i)
                    .is_ok()
            );
        }
        // Next spend should fail
        let result = limiter.check_and_record_at(Asset::Eth, U256::from(1u64), now + 5);
        assert!(result.is_err());
        match result.unwrap_err() {
            SpendingLimitError::DailyExceeded {
                asset,
                spent_today,
                limit,
                ..
            } => {
                assert_eq!(asset, Asset::Eth);
                assert_eq!(spent_today, U256::from(500u64));
                assert_eq!(limit, U256::from(500u64));
            }
            other => panic!("expected DailyExceeded, got {other}"),
        }
    }

    #[test]
    fn rolling_window_expires_old_records() {
        let mut limiter = SpendingLimiter::new(test_config());
        let t0 = 1_000_000u64;
        // Spend 500 at t0 (fills daily limit)
        for i in 0..5 {
            assert!(
                limiter
                    .check_and_record_at(Asset::Eth, U256::from(100u64), t0 + i)
                    .is_ok()
            );
        }
        // 24h + 1ms later, old records should be expired
        let t1 = t0 + ROLLING_WINDOW_MS + 1;
        assert!(
            limiter
                .check_and_record_at(Asset::Eth, U256::from(100u64), t1)
                .is_ok()
        );
    }

    #[test]
    fn partial_window_expiry() {
        let mut limiter = SpendingLimiter::new(test_config());
        let t0 = 1_000_000u64;
        // Spend 200 at t0
        assert!(
            limiter
                .check_and_record_at(Asset::Eth, U256::from(100u64), t0)
                .is_ok()
        );
        assert!(
            limiter
                .check_and_record_at(Asset::Eth, U256::from(100u64), t0 + 1)
                .is_ok()
        );

        // Spend 300 at t0 + 12h (total 500 = daily limit)
        let t1 = t0 + ROLLING_WINDOW_MS / 2;
        for i in 0..3 {
            assert!(
                limiter
                    .check_and_record_at(Asset::Eth, U256::from(100u64), t1 + i)
                    .is_ok()
            );
        }

        // At t0 + 24h + 2ms, the first two records expire but the later three remain (300)
        let t2 = t0 + ROLLING_WINDOW_MS + 2;
        // 300 spent in window, 200 more would be 500 = limit, should be ok
        assert!(
            limiter
                .check_and_record_at(Asset::Eth, U256::from(100u64), t2)
                .is_ok()
        );
        // Now 400 in window, 101 more would be 501 > 500
        assert!(
            limiter
                .check_and_record_at(Asset::Eth, U256::from(100u64), t2 + 1)
                .is_ok()
        );
        // Now 500 exactly, 1 more should fail
        let result = limiter.check_and_record_at(Asset::Eth, U256::from(1u64), t2 + 2);
        assert!(result.is_err());
    }

    #[test]
    fn eth_and_usdc_tracked_independently() {
        let mut limiter = SpendingLimiter::new(test_config());
        let now = 1_000_000u64;
        // Fill ETH daily limit
        for i in 0..5 {
            assert!(
                limiter
                    .check_and_record_at(Asset::Eth, U256::from(100u64), now + i)
                    .is_ok()
            );
        }
        // USDC should still be allowed
        assert!(
            limiter
                .check_and_record_at(Asset::Usdc, U256::from(1000u64), now + 10)
                .is_ok()
        );
    }

    #[test]
    fn usdc_per_tx_limit_enforced() {
        let mut limiter = SpendingLimiter::new(test_config());
        let result = limiter.check_and_record_at(Asset::Usdc, U256::from(1001u64), 1000);
        assert!(result.is_err());
        match result.unwrap_err() {
            SpendingLimitError::PerTxExceeded { asset, .. } => {
                assert_eq!(asset, Asset::Usdc);
            }
            other => panic!("expected PerTxExceeded, got {other}"),
        }
    }

    #[test]
    fn error_messages_are_descriptive() {
        let err = SpendingLimitError::PerTxExceeded {
            asset: Asset::Eth,
            requested: U256::from(200u64),
            limit: U256::from(100u64),
        };
        let msg = err.to_string();
        assert!(msg.contains("per-tx limit"));
        assert!(msg.contains("ETH"));

        let err = SpendingLimitError::DailyExceeded {
            asset: Asset::Usdc,
            requested: U256::from(50u64),
            spent_today: U256::from(4990u64),
            limit: U256::from(5000u64),
        };
        let msg = err.to_string();
        assert!(msg.contains("daily limit"));
        assert!(msg.contains("USDC"));
    }

    #[test]
    fn zero_amount_always_allowed() {
        let mut limiter = SpendingLimiter::new(test_config());
        assert!(
            limiter
                .check_and_record_at(Asset::Eth, U256::ZERO, 1000)
                .is_ok()
        );
    }

    #[test]
    fn default_config_has_sensible_values() {
        let config = SpendingLimitConfig::default();
        // 0.01 ETH per tx
        assert_eq!(config.eth.max_per_tx, U256::from(10_000_000_000_000_000u64));
        // 0.1 ETH daily
        assert_eq!(config.eth.max_daily, U256::from(100_000_000_000_000_000u64));
        // 10 USDC per tx
        assert_eq!(config.usdc.max_per_tx, U256::from(10_000_000u64));
        // 100 USDC daily
        assert_eq!(config.usdc.max_daily, U256::from(100_000_000u64));
    }
}
