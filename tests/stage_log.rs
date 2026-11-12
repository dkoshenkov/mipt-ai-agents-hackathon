use std::{
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use procurement_review::{
    core::{ErrorCategory, Visibility},
    runtime::{ReviewRun, Runtime, StageKind, StageRequest, StepError, grok::Model},
};
use serde::{Serialize, Serializer};
use serde_json::Value;
use tracing_subscriber::prelude::*;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn capture(future: impl std::future::Future<Output = ()>) -> (String, Vec<Value>) {
    let output = Buffer::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(move || writer.clone()),
    );
    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future);
    });
    let output = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    let records = output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["fields"].clone())
        .collect();
    (output, records)
}

struct Fake(Arc<AtomicUsize>);
impl Model for Fake {
    async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
        self.0.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        Ok("{}".into())
    }
}

struct Slow(Arc<AtomicUsize>);
impl Model for Slow {
    async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

struct SensitiveModel(Arc<Mutex<Vec<String>>>);
impl Model for SensitiveModel {
    fn name(&self) -> Option<&str> {
        Some("secret-model-name")
    }

    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        self.0.lock().unwrap().push(prompt.to_owned());
        Ok(r#"{"answer":"secret-response"}"#.into())
    }
}

struct FailingSerialize(AtomicUsize);
impl Serialize for FailingSerialize {
    fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(serde::ser::Error::custom("secret-serialization-error"))
    }
}

#[test]
fn typed_stage_logs_exclude_sensitive_values() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let failure = FailingSerialize(AtomicUsize::new(0));
    let (output, records) = capture(async {
        let model = SensitiveModel(prompts.clone());
        assert_eq!(model.name(), Some("secret-model-name"));
        let runtime = Runtime::new(model, Duration::from_secs(1));
        let run = ReviewRun::new(1);
        let request = StageRequest::new(StageKind::Claims, "secret-prompt-instruction")
            .field("document", &[Visibility::Public], &"secret-document")
            .field("external_id", &[Visibility::Public], &"secret-external-id");
        let answer = runtime.step::<Value>(&run, &request).await.unwrap();
        assert_eq!(answer["answer"], "secret-response");
        let invalid = StageRequest::new(StageKind::Review, "Review").field(
            "invalid",
            &[Visibility::Public],
            &failure,
        );
        assert_eq!(
            runtime.step::<Value>(&run, &invalid).await,
            Err(StepError::Preparation)
        );
    });
    assert_eq!(failure.0.load(Ordering::SeqCst), 1);
    let prompts = prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    for value in [
        "secret-prompt-instruction",
        "secret-document",
        "secret-external-id",
    ] {
        assert!(prompts[0].contains(value));
    }
    for (stage, event, status) in [
        ("claims", "start", None),
        ("claims", "dispatch", None),
        ("claims", "finish", Some("decoded")),
        ("review", "start", None),
        ("review", "finish", Some("preparation")),
    ] {
        assert!(records.iter().any(|r| r["stage"] == stage
            && r["event"] == event
            && status.is_none_or(|status| r["status"] == status)));
    }
    for secret in [
        "secret-model-name",
        "secret-prompt-instruction",
        "secret-document",
        "secret-external-id",
        "secret-response",
        "secret-serialization-error",
    ] {
        assert!(!output.contains(secret), "log contains {secret}");
    }
}

#[test]
fn timeout_is_logged_as_a_dispatched_failure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_, records) = capture(async {
        let runtime = Runtime::new(Slow(calls.clone()), Duration::from_millis(1));
        let run = ReviewRun::new(1);
        let request = StageRequest::new(StageKind::Review, "Review");
        assert_eq!(
            runtime.step::<Value>(&run, &request).await,
            Err(StepError::Model(ErrorCategory::TimedOut))
        );
        assert_eq!(
            runtime.step::<Value>(&run, &request).await,
            Err(StepError::BudgetExhausted)
        );
        assert_eq!(run.attempts(), 1);
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        records.iter().filter(|r| r["event"] == "dispatch").count(),
        1
    );
    let timed_out = records.iter().find(|r| r["status"] == "timed_out").unwrap();
    assert_eq!(timed_out["attempt"], 1);
    assert!(timed_out["duration_ms"].is_number());
    let refused = records
        .iter()
        .find(|r| r["status"] == "budget_exhausted")
        .unwrap();
    assert_eq!(timed_out["run_id"], refused["run_id"]);
    assert!(refused.get("attempt").is_none());
}

#[test]
fn concurrent_runs_log_dispatches_and_local_refusals() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_, records) = capture(async {
        let runtime = Runtime::new(Fake(calls.clone()), Duration::from_secs(1));
        let first = ReviewRun::new(1);
        let second = ReviewRun::new(1);
        let claims = StageRequest::new(StageKind::Claims, "Extract");
        let review = StageRequest::new(StageKind::Review, "Review");
        let (one, refused, two) = tokio::join!(
            runtime.step::<Value>(&first, &claims),
            runtime.step::<Value>(&first, &claims),
            runtime.step::<Value>(&second, &review),
        );
        assert!(
            (one.is_ok() && refused == Err(StepError::BudgetExhausted))
                || (refused.is_ok() && one == Err(StepError::BudgetExhausted))
        );
        assert!(two.is_ok());
        let restricted = StageRequest::new(StageKind::Review, "Review").field(
            "value",
            &[Visibility::Private],
            &"private",
        );
        assert_eq!(
            runtime.step::<Value>(&second, &restricted).await,
            Err(StepError::RestrictedContext)
        );
        assert_eq!(first.attempts(), 1);
        assert_eq!(second.attempts(), 1);
    });
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(records.len(), 10);
    let dispatches: Vec<_> = records
        .iter()
        .filter(|r| r["event"] == "dispatch")
        .collect();
    assert_eq!(dispatches.len(), calls.load(Ordering::SeqCst));
    assert_ne!(dispatches[0]["run_id"], dispatches[1]["run_id"]);
    for dispatch in dispatches {
        assert_eq!(dispatch["attempt"], 1);
        let run_id = &dispatch["run_id"];
        let stage = &dispatch["stage"];
        assert_eq!(
            records
                .iter()
                .filter(|r| &r["run_id"] == run_id
                    && &r["stage"] == stage
                    && r["event"] == "finish"
                    && r["status"] == "decoded"
                    && r["attempt"] == 1)
                .count(),
            1
        );
    }
    for status in ["budget_exhausted", "restricted_context"] {
        let refusal = records
            .iter()
            .find(|r| r["event"] == "finish" && r["status"] == status)
            .unwrap();
        assert!(refusal.get("attempt").is_none());
        assert!(refusal["duration_ms"].is_number());
    }
}
