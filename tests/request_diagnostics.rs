use procurement_review::{
    core::{Document, ErrorCategory, Visibility},
    runtime::{Check, Runtime, grok::Model},
};
use std::time::Duration;

struct Reply(&'static str);
impl Model for Reply {
    async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
        Ok(self.0.into())
    }
}

#[tokio::test]
async fn distinguishes_json_syntax_schema_and_timeout() {
    for (text, kind) in [("[?]", "Syntax"), ("{}", "Data")] {
        let runtime = Runtime::new(Reply(text), Duration::from_secs(1));
        let error = runtime
            .request_json_detailed::<Vec<u32>>("unused")
            .await
            .unwrap_err();
        assert_eq!(error.stage, "json_decode");
        assert!(error.reason.contains(kind));
        assert!(error.reason.contains("line 1"));
        assert!(!error.reason.contains(text));
    }
    struct Slow;
    impl Model for Slow {
        async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok("[]".into())
        }
    }
    let error = Runtime::new(Slow, Duration::from_millis(1))
        .request_json_detailed::<Vec<u32>>("")
        .await
        .unwrap_err();
    assert_eq!(error.category, ErrorCategory::TimedOut);
    assert_eq!(error.stage, "timeout");
}

#[tokio::test]
async fn report_keeps_diagnostic_and_old_reports_still_decode() {
    let outcomes = Runtime::new(Reply("{}"), Duration::from_secs(1))
        .run_parallel(
            &Document {
                markdown: "document".into(),
                visibility: Visibility::Public,
            },
            &[Check {
                id: "A01".into(),
                prompt: "check".into(),
                materials: vec![],
            }],
        )
        .await
        .unwrap();
    let json = serde_json::to_value(&outcomes[0]).unwrap();
    assert_eq!(json["diagnostic"]["stage"], "json_decode");
    assert_eq!(json["result"]["Err"], "InvalidResponse");
    let old: procurement_review::core::CheckOutcome =
        serde_json::from_str(r#"{"check_id":"A01","result":{"Err":"InvalidResponse"}}"#).unwrap();
    assert!(old.diagnostic.is_none());
}

#[tokio::test]
async fn accepts_complete_fenced_json_but_not_prose_or_partial_fences() {
    for text in ["```json\n[1]\n```", "```\n[1]\n```", " [1] "] {
        assert_eq!(
            Runtime::new(Reply(text), Duration::from_secs(1))
                .request_json_detailed::<Vec<u32>>("")
                .await
                .unwrap(),
            vec![1]
        );
    }
    for text in [
        "Here is JSON: [1]",
        "```json\n[1]",
        "```json\n[1]\n```\nextra",
        "```json\n[1]```",
    ] {
        assert!(
            Runtime::new(Reply(text), Duration::from_secs(1))
                .request_json_detailed::<Vec<u32>>("")
                .await
                .is_err()
        );
    }
}
