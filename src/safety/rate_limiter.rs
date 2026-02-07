use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
};
use nonzero_ext::nonzero;
use std::time::Duration;

type DirectLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>;

pub struct MessageRateLimiter {
    per_second: DirectLimiter,
    per_minute: DirectLimiter,
    per_hour: DirectLimiter,
    per_day: DirectLimiter,
}

impl MessageRateLimiter {
    pub fn new() -> Self {
        Self {
            per_second: RateLimiter::direct(Quota::per_second(nonzero!(4u32))),
            per_minute: RateLimiter::direct(Quota::per_minute(nonzero!(20u32))),
            per_hour: RateLimiter::direct(
                Quota::with_period(Duration::from_secs(3600 / 300))
                    .unwrap()
                    .allow_burst(nonzero!(300u32)),
            ),
            per_day: RateLimiter::direct(
                Quota::with_period(Duration::from_secs(120))
                    .unwrap()
                    .allow_burst(nonzero!(720u32)),
            ),
        }
    }

    pub fn check(&self) -> Result<(), crate::error::KleviathanError> {
        if self.per_second.check().is_err() {
            return Err(crate::error::KleviathanError::RateLimit(
                "Per-second rate limit exceeded (4/s)".into(),
            ));
        }
        if self.per_minute.check().is_err() {
            return Err(crate::error::KleviathanError::RateLimit(
                "Per-minute rate limit exceeded (20/min)".into(),
            ));
        }
        if self.per_hour.check().is_err() {
            return Err(crate::error::KleviathanError::RateLimit(
                "Per-hour rate limit exceeded (300/hr)".into(),
            ));
        }
        if self.per_day.check().is_err() {
            return Err(crate::error::KleviathanError::RateLimit(
                "Per-day rate limit exceeded (720/day)".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_four_rapid_requests() {
        let limiter = MessageRateLimiter::new();
        for _ in 0..4 {
            assert!(limiter.check().is_ok());
        }
    }

    #[test]
    fn rejects_fifth_rapid_request() {
        let limiter = MessageRateLimiter::new();
        for _ in 0..4 {
            let _ = limiter.check();
        }
        let result = limiter.check();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::error::KleviathanError::RateLimit(msg) if msg.contains("4/s"))
        );
    }

    #[test]
    fn per_minute_limit_enforced() {
        let limiter = MessageRateLimiter::new();
        let mut accepted = 0;
        for _ in 0..25 {
            if limiter.check().is_ok() {
                accepted += 1;
            }
        }
        assert!(accepted <= 20);
    }

    #[test]
    fn all_tiers_active_simultaneously() {
        let limiter = MessageRateLimiter::new();
        assert!(limiter.check().is_ok());

        let per_second_only = MessageRateLimiter::new();
        for _ in 0..4 {
            let _ = per_second_only.check();
        }
        let err = per_second_only.check().unwrap_err();
        match err {
            crate::error::KleviathanError::RateLimit(msg) => {
                assert!(msg.contains("/s") || msg.contains("/min") || msg.contains("/hr") || msg.contains("/day"));
            }
            _ => panic!("Expected RateLimit error"),
        }
    }
}
