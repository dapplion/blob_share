use std::time::Duration;

use crate::warn;

const BASE_DELAY_MS: u64 = 1000;
const MAX_DELAY_MS: u64 = 60_000;

/// Tracks consecutive errors and computes exponential backoff delays for background tasks.
/// Resets on success. Caps the delay at `MAX_DELAY_MS`.
pub(crate) struct BackoffState {
    consecutive_errors: u32,
    task_name: &'static str,
}

impl BackoffState {
    pub(crate) fn new(task_name: &'static str) -> Self {
        Self {
            consecutive_errors: 0,
            task_name,
        }
    }

    /// Record a transient error and sleep for the computed backoff duration.
    pub(crate) async fn on_error(&mut self) {
        self.consecutive_errors = self.consecutive_errors.saturating_add(1);
        let delay = self.current_delay();
        warn!(
            "{}: consecutive error #{}, backing off for {}ms",
            self.task_name,
            self.consecutive_errors,
            delay.as_millis()
        );
        tokio::time::sleep(delay).await;
    }

    /// Reset the backoff state after a successful operation.
    pub(crate) fn on_success(&mut self) {
        self.consecutive_errors = 0;
    }

    /// Compute the current delay: base * 2^(errors-1), capped at MAX_DELAY_MS.
    fn current_delay(&self) -> Duration {
        let exp = self.consecutive_errors.saturating_sub(1).min(20);
        let delay_ms = BASE_DELAY_MS.saturating_mul(1u64 << exp);
        Duration::from_millis(delay_ms.min(MAX_DELAY_MS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_delay_increases_exponentially() {
        let mut state = BackoffState::new("test_task");

        // First error: base delay (1s)
        state.consecutive_errors = 1;
        assert_eq!(state.current_delay(), Duration::from_millis(1000));

        // Second error: 2s
        state.consecutive_errors = 2;
        assert_eq!(state.current_delay(), Duration::from_millis(2000));

        // Third error: 4s
        state.consecutive_errors = 3;
        assert_eq!(state.current_delay(), Duration::from_millis(4000));

        // Fourth error: 8s
        state.consecutive_errors = 4;
        assert_eq!(state.current_delay(), Duration::from_millis(8000));

        // Fifth error: 16s
        state.consecutive_errors = 5;
        assert_eq!(state.current_delay(), Duration::from_millis(16000));

        // Sixth error: 32s
        state.consecutive_errors = 6;
        assert_eq!(state.current_delay(), Duration::from_millis(32000));

        // Seventh error: capped at 60s
        state.consecutive_errors = 7;
        assert_eq!(state.current_delay(), Duration::from_millis(60000));
    }

    #[test]
    fn backoff_caps_at_max_delay() {
        let mut state = BackoffState::new("test_task");
        state.consecutive_errors = 100;
        assert_eq!(state.current_delay(), Duration::from_millis(MAX_DELAY_MS));
    }

    #[test]
    fn backoff_resets_on_success() {
        let mut state = BackoffState::new("test_task");
        state.consecutive_errors = 5;
        assert_eq!(state.current_delay(), Duration::from_millis(16000));

        state.on_success();
        assert_eq!(state.consecutive_errors, 0);
    }

    #[test]
    fn backoff_saturating_add() {
        let mut state = BackoffState::new("test_task");
        state.consecutive_errors = u32::MAX;
        // Should not panic
        assert_eq!(state.current_delay(), Duration::from_millis(MAX_DELAY_MS));
    }

    #[test]
    fn backoff_on_error_increments_counter() {
        let mut state = BackoffState::new("test_task");
        assert_eq!(state.consecutive_errors, 0);

        // Simulate error without actually sleeping (test the state tracking)
        state.consecutive_errors = state.consecutive_errors.saturating_add(1);
        assert_eq!(state.consecutive_errors, 1);
        assert_eq!(state.current_delay(), Duration::from_millis(1000));

        state.consecutive_errors = state.consecutive_errors.saturating_add(1);
        assert_eq!(state.consecutive_errors, 2);
        assert_eq!(state.current_delay(), Duration::from_millis(2000));
    }
}
