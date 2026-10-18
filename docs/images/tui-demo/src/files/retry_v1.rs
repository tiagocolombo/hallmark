//! Retries failed webhook deliveries with exponential backoff and jitter.

use std::time::Duration;

use rand::Rng;

use crate::webhooks::{Delivery, Outcome};

/// Attempts after the first, before a delivery is parked for review.
pub const MAX_ATTEMPTS: u32 = 6;

/// The delay before `attempt` (1-based): 2^attempt seconds, capped at
/// 10 minutes, with up to 20% jitter so retries don't arrive in waves.
pub fn backoff(attempt: u32) -> Duration {
    let base = 2u64.saturating_pow(attempt).min(600);
    let jitter = rand::thread_rng().gen_range(0..=base / 5);
    Duration::from_secs(base + jitter)
}

/// Decides what happens to a delivery after it failed.
pub fn on_failure(delivery: &mut Delivery, outcome: Outcome) -> Next {
    if !outcome.is_retryable() {
        return Next::Park(outcome.reason());
    }
    delivery.attempts += 1;
    if delivery.attempts > MAX_ATTEMPTS {
        return Next::Park("gave up after 6 retries".into());
    }
    Next::RetryIn(backoff(delivery.attempts))
}

pub enum Next {
    RetryIn(Duration),
    Park(String),
}
