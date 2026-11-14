use std::{sync::Once, time::Instant};

use tracing_subscriber::fmt::writer::{BoxMakeWriter, MakeWriterExt};
use tracing_subscriber::prelude::*;

use super::{StageKind, StepError};
use crate::core::ErrorCategory;

static INIT: Once = Once::new();

pub(crate) fn init() {
    init_logging(None);
}

/// Initialize filtered JSON events on stderr and in a file before starting a run.
/// Returns false if logging was already initialized or a subscriber is installed.
pub fn init_with_file(file: std::fs::File) -> bool {
    init_logging(Some(file))
}

fn init_logging(file: Option<std::fs::File>) -> bool {
    let mut initialized = false;
    INIT.call_once(|| {
        let writer = match file {
            Some(file) => BoxMakeWriter::new(std::io::stderr.and(file)),
            None => BoxMakeWriter::new(std::io::stderr),
        };
        initialized = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(writer)
                    .with_filter(filter()),
            )
            .try_init()
            .is_ok();
    });
    initialized
}

fn filter() -> tracing_subscriber::filter::Targets {
    tracing_subscriber::filter::Targets::new()
        .with_target("procurement_review", tracing::Level::INFO)
}

pub(super) struct TypedStage<'a> {
    run_id: &'a str,
    stage: &'static str,
    model_role: &'static str,
    started: Instant,
}

impl<'a> TypedStage<'a> {
    pub(super) fn start(run_id: &'a str, kind: StageKind) -> Self {
        init();
        let (stage, model_role) = match kind {
            StageKind::Claims => ("claims", "worker"),
            StageKind::Search => ("search", "worker"),
            StageKind::Review => ("review", "reviewer"),
            StageKind::Control => ("control", "reviewer"),
            StageKind::Comparison => ("comparison", "reviewer"),
        };
        tracing::info!(run_id, stage, model_role, event = "start");
        Self {
            run_id,
            stage,
            model_role,
            started: Instant::now(),
        }
    }

    pub(super) fn dispatch(&self, attempt: usize) {
        tracing::info!(
            run_id = self.run_id,
            stage = self.stage,
            model_role = self.model_role,
            event = "dispatch",
            attempt
        );
    }

    pub(super) fn finish(&self, result: Result<(), StepError>, attempt: Option<usize>) {
        let status = match result {
            Ok(()) => "decoded",
            Err(StepError::Preparation) => "preparation",
            Err(StepError::RestrictedContext) => "restricted_context",
            Err(StepError::BudgetExhausted) => "budget_exhausted",
            Err(StepError::Model(ErrorCategory::RateLimited)) => "rate_limited",
            Err(StepError::Model(ErrorCategory::TimedOut)) => "timed_out",
            Err(StepError::Model(ErrorCategory::InvalidResponse)) => "invalid_response",
            Err(StepError::Model(ErrorCategory::Provider)) => "provider_error",
            Err(StepError::Model(ErrorCategory::RestrictedContext)) => "model_restricted_context",
        };
        let duration_ms = self.started.elapsed().as_millis() as u64;
        tracing::info!(
            run_id = self.run_id,
            stage = self.stage,
            model_role = self.model_role,
            event = "finish",
            attempt,
            status,
            duration_ms
        );
    }
}

pub(crate) struct Stage {
    name: &'static str,
    id: String,
    model: String,
    started: Instant,
}

pub(crate) fn stage(name: &'static str, id: &str) -> Stage {
    model_stage(name, id, None)
}

pub(crate) fn model_stage(name: &'static str, id: &str, model: Option<&str>) -> Stage {
    init();
    let model = model.unwrap_or("");
    tracing::info!(stage = name, id, model, event = "start");
    Stage {
        name,
        id: id.to_owned(),
        model: model.to_owned(),
        started: Instant::now(),
    }
}

impl Stage {
    pub(crate) fn finish(self, status: &str) {
        tracing::info!(
            stage = self.name,
            id = self.id,
            model = self.model,
            event = "finish",
            status,
            duration_ms = self.started.elapsed().as_millis() as u64
        );
    }

    pub(crate) fn finish_result<T>(self, result: &Result<T, ErrorCategory>) {
        let status = match result {
            Ok(_) => "success".to_owned(),
            Err(error) => format!("{error:?}"),
        };
        self.finish(&status);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
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

    #[test]
    fn stages_emit_safe_structured_events_in_run_span() {
        const CHILD: &str = "PROCUREMENT_LOGGING_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::logging::tests::stages_emit_safe_structured_events_in_run_span",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let output = Buffer::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(move || writer.clone())
                .with_filter(super::filter()),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "provider", secret = "provider-secret");
            let span = tracing::info_span!("run", run_id = "run-123");
            let _entered = span.enter();
            super::model_stage("check", "check-1", Some("grok-test")).finish("success");
            super::model_stage("critic", "", Some("grok-critic")).finish("rate_limit");
        });
        let output = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(!output.contains("provider-secret"));
        let records: Vec<serde_json::Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 4);
        for record in &records {
            assert_eq!(record["span"]["run_id"], "run-123");
        }
        assert_eq!(records[0]["fields"]["stage"], "check");
        assert_eq!(records[0]["fields"]["model"], "grok-test");
        assert_eq!(records[1]["fields"]["model"], "grok-test");
        assert_eq!(records[2]["fields"]["stage"], "critic");
        assert_eq!(records[2]["fields"]["model"], "grok-critic");
        assert_eq!(records[3]["fields"]["model"], "grok-critic");
        assert_eq!(records[0]["fields"]["id"], "check-1");
        assert_eq!(records[1]["fields"]["status"], "success");
        assert!(records[1]["fields"]["duration_ms"].is_number());
        assert_eq!(records[2]["fields"]["id"], "");
        assert_eq!(records[3]["fields"]["status"], "rate_limit");
    }
    #[test]
    fn typed_finish_logs_the_actual_attempt_on_success_and_failure() {
        use crate::core::ErrorCategory;
        use crate::runtime::{ReviewRun, Runtime, StageKind, StageRequest, grok::Model};
        struct Mock(bool);
        impl Model for Mock {
            async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
                if self.0 {
                    Ok("[]".into())
                } else {
                    Err(ErrorCategory::Provider)
                }
            }
        }
        let output = Buffer::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(move || writer.clone())
                .with_filter(super::filter()),
        );
        tracing::subscriber::with_default(subscriber, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                for successful in [true, false] {
                    let model = Runtime::new(Mock(successful), std::time::Duration::from_secs(1));
                    let request = StageRequest::new(StageKind::Claims, "instruction");
                    let _: Result<Vec<()>, _> = model.step(&ReviewRun::new(1), &request).await;
                }
            });
        });
        let output = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        let records: Vec<serde_json::Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let finishes: Vec<_> = records
            .iter()
            .filter(|record| record["fields"]["event"] == "finish")
            .collect();
        assert_eq!(finishes.len(), 2);
        assert_eq!(finishes[0]["fields"]["status"], "decoded");
        assert_eq!(finishes[1]["fields"]["status"], "provider_error");
        assert!(
            finishes
                .iter()
                .all(|record| record["fields"]["attempt"] == 1)
        );
    }
}
