use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use procurement_review::{
    core::{BranchReport, Completeness, Document, ErrorCategory, Visibility},
    runtime::{Check, Runtime, grok::Model, scenario::review_223},
};
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

struct Fake {
    api_key: &'static str,
}

impl Model for Fake {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        assert!(!prompt.contains(self.api_key));
        if prompt.contains("fail-marker") {
            Err(ErrorCategory::RateLimited)
        } else if prompt.contains("Review the evidence") {
            Ok(r#"{"conclusion":"secret-verdict"}"#.into())
        } else {
            Ok(
                r#"[{"description":"secret-answer","citations":[{"quote":"secret-document"}]}]"#
                    .into(),
            )
        }
    }
}

#[test]
fn review_runs_have_distinct_ids_and_safe_stage_logs() {
    let output = Buffer::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(move || writer.clone()),
    );
    tracing::subscriber::with_default(subscriber, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let agents = Runtime::new(
                Fake {
                    api_key: "secret-api-key",
                },
                Duration::from_secs(1),
            );
            let critic = Runtime::new(
                Fake {
                    api_key: "secret-api-key",
                },
                Duration::from_secs(1),
            );
            let document = Document {
                markdown: "secret-document".into(),
                visibility: Visibility::Public,
            };
            for (prompt, expected) in [
                ("secret-prompt", Completeness::Complete),
                ("fail-marker secret-prompt", Completeness::Partial),
            ] {
                let checks = [
                    Check {
                        id: "first".into(),
                        prompt: "secret-prompt".into(),
                        materials: vec![Document {
                            markdown: "secret-material".into(),
                            visibility: Visibility::Public,
                        }],
                    },
                    Check {
                        id: "second".into(),
                        prompt: prompt.into(),
                        materials: vec![],
                    },
                ];
                let BranchReport::Reviewed { evidence, .. } =
                    review_223(&agents, &critic, &document, &checks)
                        .await
                        .unwrap()
                else {
                    panic!("expected reviewed report");
                };
                assert_eq!(evidence.completeness(), expected);
            }
        });
    });

    let output = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    for secret in [
        "secret-document",
        "secret-prompt",
        "secret-material",
        "secret-answer",
        "secret-verdict",
        "secret-api-key",
        "fail-marker",
    ] {
        assert!(!output.contains(secret), "log contains {secret}");
    }
    let records: Vec<serde_json::Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut runs = Vec::new();
    let mut current = Vec::new();
    for record in &records {
        current.push(record);
        if record["fields"]["stage"] == "branch" && record["fields"]["event"] == "finish" {
            runs.push(std::mem::take(&mut current));
        }
    }
    assert!(current.is_empty());
    assert_eq!(runs.len(), 2);
    let run_ids: Vec<_> = runs
        .iter()
        .map(|run| {
            let id = run[0]["span"]["run_id"].as_str().unwrap();
            for record in run {
                assert_eq!(record["span"]["run_id"], id);
            }
            for (stage, stage_id) in [
                ("branch", "223"),
                ("check", "first"),
                ("check", "second"),
                ("aggregation", ""),
                ("critic", ""),
            ] {
                for event in ["start", "finish"] {
                    assert_eq!(
                        run.iter()
                            .filter(|record| record["fields"]["stage"] == stage
                                && record["fields"]["id"] == stage_id
                                && record["fields"]["event"] == event)
                            .count(),
                        1
                    );
                }
            }
            id
        })
        .collect();
    assert_ne!(run_ids[0], run_ids[1]);
    assert!(
        runs[1]
            .iter()
            .any(|record| record["fields"]["stage"] == "check"
                && record["fields"]["id"] == "second"
                && record["fields"]["event"] == "finish"
                && record["fields"]["status"] == "RateLimited")
    );
}
