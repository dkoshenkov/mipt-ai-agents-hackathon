use super::*;
use crate::core::{Citation, ErrorCategory, Finding, Visibility};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Barrier;

struct Fake {
    calls: Arc<AtomicUsize>,
    barrier: Arc<Barrier>,
}
impl grok::Model for Fake {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.barrier.wait().await;
        if prompt.contains("rate-limit") {
            return Err(ErrorCategory::RateLimited);
        }
        if prompt.contains("timeout") {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(serde_json::to_string(&vec![Finding {
            description: "found".into(),
            citations: vec![Citation {
                quote: "document".into(),
                links: vec![],
            }],
        }])
        .unwrap())
    }
}
fn doc(visibility: Visibility) -> Document {
    Document {
        markdown: "document".into(),
        visibility,
    }
}
fn check(id: &str) -> Check {
    Check {
        id: id.into(),
        prompt: id.into(),
        materials: vec![],
    }
}

#[tokio::test]
async fn validates_and_runs_dynamic_checks_concurrently_with_independent_timeouts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::new(
        Fake {
            calls: calls.clone(),
            barrier: Arc::new(Barrier::new(2)),
        },
        Duration::from_millis(100),
    );
    assert_eq!(
        runtime
            .run_parallel(
                &Document {
                    markdown: " ".into(),
                    visibility: Visibility::Public
                },
                &[check("a")]
            )
            .await,
        Err(InputError::EmptyDocument)
    );
    assert_eq!(
        runtime.run_parallel(&doc(Visibility::Public), &[]).await,
        Err(InputError::EmptyChecks)
    );
    for visibility in [Visibility::Private, Visibility::Unknown] {
        assert_eq!(
            runtime.run_parallel(&doc(visibility), &[check("a")]).await,
            Err(InputError::RestrictedContext)
        );
        let mut c = check("a");
        c.materials.push(doc(visibility));
        assert_eq!(
            runtime.run_parallel(&doc(Visibility::Public), &[c]).await,
            Err(InputError::RestrictedContext)
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = runtime
        .run_parallel(
            &doc(Visibility::Public),
            &[check("slow"), check("rate-limit")],
        )
        .await
        .unwrap();
    assert_eq!(
        result
            .iter()
            .map(|o| o.check_id.as_str())
            .collect::<Vec<_>>(),
        ["slow", "rate-limit"]
    );
    assert_eq!(result[0].result.as_ref().unwrap()[0].description, "found");
    assert_eq!(result[1].result, Err(ErrorCategory::RateLimited));
    let result = runtime
        .run_parallel(&doc(Visibility::Public), &[check("timeout"), check("new")])
        .await
        .unwrap();
    assert_eq!(result[0].result, Err(ErrorCategory::TimedOut));
    assert!(result[1].result.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[test]
fn check_prompts_share_the_document_prefix() {
    let document = doc(Visibility::Public);
    let a = CheckInput {
        document: &document,
        check: &check("first"),
    }
    .prompt();
    let b = CheckInput {
        document: &document,
        check: &check("second"),
    }
    .prompt();
    let prefix = a.strip_suffix(r#""first"}"#).unwrap();
    assert!(prefix.contains(r#""document":"document""#));
    assert_eq!(b.strip_suffix(r#""second"}"#), Some(prefix));
}
