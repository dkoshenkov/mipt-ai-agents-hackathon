use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_RUN: AtomicUsize = AtomicUsize::new(0);

/// Shared attempt budget for all stages participating in one review.
pub struct ReviewRun {
    pub(super) id: String,
    limit: usize,
    attempts: AtomicUsize,
}

impl ReviewRun {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub fn new(limit: usize) -> Self {
        Self {
            id: format!(
                "typed-{}-{}",
                std::process::id(),
                NEXT_RUN.fetch_add(1, Ordering::Relaxed)
            ),
            limit,
            attempts: AtomicUsize::new(0),
        }
    }

    /// Number of attempts dispatched, including requests that failed or timed out.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::Relaxed)
    }

    pub fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.attempts())
    }

    /// A retry is another dispatch on the same budget. Keep the last rejection
    /// if the run cannot afford another attempt (including any reserved review).
    pub(crate) async fn retry_invalid<T, U, E, F: Future<Output = T> + Send>(
        &self,
        reserved: usize,
        max_retries_for: impl Fn(Option<&'static str>) -> usize,
        mut send: impl FnMut(Option<&'static str>) -> F,
        mut check: impl FnMut(T) -> Result<U, (E, Option<&'static str>)>,
    ) -> Result<U, E> {
        let mut feedback = None;
        let mut retries = 0;
        let mut previous_reason = None;
        let mut reason_retries = 0;
        loop {
            let (error, reason) = match check(send(feedback).await) {
                Ok(result) => return Ok(result),
                Err(rejection) => rejection,
            };
            feedback = reason;
            if feedback != previous_reason {
                reason_retries = 0;
            }
            if retries == 3
                || feedback.is_none()
                || self.remaining() <= reserved
                || reason_retries >= max_retries_for(feedback)
            {
                return Err(error);
            }
            retries += 1;
            reason_retries += 1;
            previous_reason = feedback;
            tracing::info!(
                run_id = self.id(),
                event = "retry_invalid",
                retry = retries,
                remaining = self.remaining(),
                reason = feedback
            );
        }
    }

    pub(super) async fn retry_transient<
        T,
        F: Future<Output = Result<T, super::grok::RetryError>>,
    >(
        &self,
        reserved: usize,
        first_is_retry: bool,
        max_delay: std::time::Duration,
        model: Option<&str>,
        mut send: impl FnMut(usize) -> F,
    ) -> Result<T, super::grok::RetryError> {
        let mut last_error = None;
        for retry in 0..=2 {
            let attempt = if retry == 0 && !first_is_retry {
                self.reserve()
            } else {
                self.reserve_with_reserve(reserved)
            };
            let Some(attempt) = attempt else {
                return Err(last_error.unwrap_or_else(|| {
                    crate::core::RequestError::new(
                        crate::core::ErrorCategory::Provider,
                        "budget",
                        "Attempt budget exhausted",
                    )
                    .into()
                }));
            };
            use tracing::Instrument;
            let error = match send(attempt)
                .instrument(tracing::info_span!(
                    "model_request",
                    run_id = self.id(),
                    model = model.unwrap_or(""),
                    attempt
                ))
                .await
            {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            let retryable = matches!(error.error.http_status, Some(429 | 500 | 502 | 503 | 504));
            let delay = retry_delay(error.retry_after, retry);
            tracing::info!(
                run_id = self.id(),
                model = model.unwrap_or(""),
                event = "request_failure",
                status = error.error.http_status,
                retryable,
                remaining = self.remaining(),
                retry
            );
            // Do not retry earlier than a server's requested delay or wait unboundedly.
            if !retryable || retry == 2 || delay >= max_delay || self.remaining() <= reserved {
                return Err(error);
            }
            tracing::info!(
                run_id = self.id(),
                model = model.unwrap_or(""),
                event = "retry_transient",
                status = error.error.http_status,
                remaining = self.remaining(),
                delay_ms = delay.as_millis() as u64
            );
            last_error = Some(error);
            tokio::time::sleep(delay).await;
        }
        unreachable!()
    }

    pub(super) fn reserve(&self) -> Option<usize> {
        self.reserve_with_reserve(0)
    }

    pub(super) fn reserve_with_reserve(&self, reserved: usize) -> Option<usize> {
        let mut used = self.attempts();
        while used < self.limit.saturating_sub(reserved) {
            match self.attempts.compare_exchange_weak(
                used,
                used + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(used + 1),
                Err(actual) => used = actual,
            }
        }
        None
    }
}

fn retry_delay(retry_after: Option<std::time::Duration>, retry: u32) -> std::time::Duration {
    retry_after
        .unwrap_or_default()
        .max(std::time::Duration::from_millis(1000 * (1 << retry)))
}

#[cfg(test)]
mod tests {
    use super::ReviewRun;
    use std::sync::{Arc, Barrier};

    #[test]
    fn budget_is_atomic_and_independent_for_each_run() {
        let run = Arc::new(ReviewRun::new(1));
        let barrier = Arc::new(Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let run = Arc::clone(&run);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    run.reserve()
                })
            })
            .collect();
        let reservations: Vec<_> = threads
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .collect();
        assert_eq!(reservations, [1]);
        assert_eq!(run.attempts(), 1);
        assert_eq!(run.reserve(), None);

        let empty = ReviewRun::new(0);
        assert_eq!(empty.reserve(), None);
        assert_eq!(empty.attempts(), 0);

        let another = ReviewRun::new(1);
        assert_ne!(run.id, another.id);
        assert_eq!(another.reserve(), Some(1));
        assert_eq!(another.attempts(), 1);
    }
    #[tokio::test]
    async fn transient_retries_obey_status_budget_and_reserve() {
        use crate::core::{ErrorCategory, RequestError};
        for (status, budget, reserved, expected) in [
            (503, 4, 1, 3),
            (429, 2, 1, 1),
            (400, 4, 0, 1),
            (501, 4, 0, 1),
        ] {
            let run = ReviewRun::new(budget);
            let result: Result<(), _> = run
                .retry_transient(
                    reserved,
                    false,
                    std::time::Duration::from_secs(180),
                    None,
                    |_| async move {
                        let mut error =
                            RequestError::new(ErrorCategory::Provider, "provider", "failure");
                        error.http_status = Some(status);
                        Err(error.into())
                    },
                )
                .await;
            assert_eq!(result.unwrap_err().error.http_status, Some(status));
            assert_eq!(run.attempts(), expected);
        }
        assert_eq!(
            super::retry_delay(Some(std::time::Duration::from_secs(30)), 0),
            std::time::Duration::from_secs(30)
        );
        assert!(
            super::retry_delay(Some(std::time::Duration::MAX), 0)
                >= std::time::Duration::from_secs(180)
        );
        assert_eq!(
            super::retry_delay(None, 0),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            super::retry_delay(None, 1),
            std::time::Duration::from_secs(2)
        );
        let run = ReviewRun::new(3);
        let result: Result<(), _> = run
            .retry_transient(
                0,
                false,
                std::time::Duration::from_secs(180),
                None,
                |_| async {
                    let mut error = RequestError::new(
                        ErrorCategory::RateLimited,
                        "provider",
                        "Provider request failed",
                    );
                    error.http_status = Some(429);
                    Err(super::super::grok::RetryError {
                        error,
                        retry_after: Some(std::time::Duration::MAX),
                    })
                },
            )
            .await;
        assert!(result.is_err());
        assert_eq!(run.attempts(), 1);
    }

    #[tokio::test]
    async fn caller_can_limit_retries_for_one_reason() {
        let run = ReviewRun::new(9);
        let reason = "caller-defined correction";
        let result: Result<(), ()> = run
            .retry_invalid(
                0,
                |_| 1,
                |_| async {
                    run.reserve();
                },
                |_| Err(((), Some(reason))),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(run.attempts(), 2);
    }
}
