use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::future::join_all;
use procurement_review::{
    core::ErrorCategory,
    runtime::{ReviewRun, Runtime, StageKind, StageRequest, StepError, grok::Model},
};
use serde::Deserialize;

struct RecordingModel {
    calls: Arc<Mutex<Vec<String>>>,
    response: Result<&'static str, ErrorCategory>,
    delay: Duration,
}

impl Model for RecordingModel {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        self.calls.lock().unwrap().push(prompt.to_owned());
        tokio::time::sleep(self.delay).await;
        self.response.map(str::to_owned)
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Output {
    value: u32,
}

fn runtime(
    response: Result<&'static str, ErrorCategory>,
    delay: Duration,
    timeout: Duration,
) -> (Runtime<RecordingModel>, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        Runtime::new(
            RecordingModel {
                calls: calls.clone(),
                response,
                delay,
            },
            timeout,
        ),
        calls,
    )
}

#[tokio::test]
async fn concurrent_steps_share_budget_but_runs_are_independent() {
    let (runtime, calls) = runtime(
        Ok(r#"{"value":42}"#),
        Duration::ZERO,
        Duration::from_secs(1),
    );
    let run = ReviewRun::new(1);
    let request = StageRequest::new(StageKind::Claims, "Return JSON");
    let results = join_all((0..16).map(|_| runtime.step::<Output>(&run, &request))).await;
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Ok(Output { value: 42 }))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(StepError::BudgetExhausted))
            .count(),
        15
    );
    assert_eq!(run.attempts(), 1);
    assert_eq!(calls.lock().unwrap().len(), 1);

    let empty = ReviewRun::new(0);
    assert_eq!(
        runtime.step::<Output>(&empty, &request).await,
        Err(StepError::BudgetExhausted)
    );
    assert_eq!(empty.attempts(), 0);
    let another = ReviewRun::new(1);
    assert_eq!(
        runtime.step::<Output>(&another, &request).await,
        Ok(Output { value: 42 })
    );
    assert_eq!(another.attempts(), 1);
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_single_attempt_budget_preserves_the_original_failure() {
    let cases = [
        (
            Err(ErrorCategory::RateLimited),
            Duration::ZERO,
            Duration::from_secs(1),
            ErrorCategory::RateLimited,
        ),
        (
            Err(ErrorCategory::Provider),
            Duration::ZERO,
            Duration::from_secs(1),
            ErrorCategory::Provider,
        ),
        (
            Ok("invalid json"),
            Duration::ZERO,
            Duration::from_secs(1),
            ErrorCategory::InvalidResponse,
        ),
        (
            Ok(r#"{"value":"wrong"}"#),
            Duration::ZERO,
            Duration::from_secs(1),
            ErrorCategory::InvalidResponse,
        ),
        (
            Ok(r#"{"value":42}"#),
            Duration::from_secs(1),
            Duration::from_millis(1),
            ErrorCategory::TimedOut,
        ),
    ];
    for (response, delay, timeout, expected) in cases {
        let (runtime, calls) = runtime(response, delay, timeout);
        let run = ReviewRun::new(1);
        let request = StageRequest::new(StageKind::Review, "Return JSON");
        assert_eq!(
            runtime.step::<Output>(&run, &request).await,
            Err(StepError::Model(expected))
        );
        assert_eq!(run.attempts(), 1);
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(
            runtime.step::<Output>(&run, &request).await,
            Err(StepError::BudgetExhausted)
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn retries_only_invalid_responses_up_to_three_times() {
    for (response, expected, calls_expected) in [
        (Ok("not json"), ErrorCategory::InvalidResponse, 4),
        (
            Err(ErrorCategory::RateLimited),
            ErrorCategory::RateLimited,
            1,
        ),
        (Err(ErrorCategory::Provider), ErrorCategory::Provider, 1),
        (Err(ErrorCategory::TimedOut), ErrorCategory::TimedOut, 1),
    ] {
        let (runtime, calls) = runtime(response, Duration::ZERO, Duration::from_secs(1));
        let run = ReviewRun::new(10);
        assert_eq!(
            runtime
                .step::<Output>(&run, &StageRequest::new(StageKind::Review, "Review"))
                .await,
            Err(StepError::Model(expected))
        );
        assert_eq!(run.attempts(), calls_expected);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), calls_expected);
        assert!(!calls[0].contains("retry_feedback"));
        assert!(calls[1..].iter().all(|p| p.contains("retry_feedback")));
    }
}
