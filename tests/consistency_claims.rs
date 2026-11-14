use std::time::Duration;

use procurement_review::{
    consistency::{
        ReviewOptions,
        claims::{Constraint, Operator},
        review_markdown,
    },
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model},
};
use serde_json::{Value, json};

struct RangeReplay;
impl Model for RangeReplay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let prompt: Value = serde_json::from_str(prompt).unwrap();
        let field = |name: &str| {
            prompt["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"] == name)
                .map(|entry| &entry["value"])
        };
        if let Some(packet) = field("packet") {
            let mut claims = vec![];
            for block in packet["blocks"].as_array().unwrap() {
                if !packet["subjects"]
                    .as_array()
                    .unwrap()
                    .contains(&block["id"])
                {
                    continue;
                }
                let text = block["text"][0].as_str().unwrap();
                let quote = |text: &str| json!({"block_id":block["id"], "quote":text});
                let pairs: &[(&str, &str)] = if text.contains("от 300 до 500") {
                    &[("от", "300"), ("до", "500")]
                } else if text.contains("не ранее") {
                    &[("не ранее", "100")]
                } else {
                    &[("не позднее", "600")]
                };
                for &(operator, value) in pairs {
                    claims.push(json!({"statement":quote(text),"object":{"key":"изделие","evidence":[quote("изделие")],"alternatives":[]},"property":quote("срок"),"constraint":{"kind":"numeric","operator":quote(operator),"value":quote(value),"unit":quote("дней")},"stage":null,"conditions":[],"mandatory":null}));
                }
            }
            return Ok(json!({"claims":claims,"priority_rules":[],"standard_mentions":[],"refinements":[]}).to_string());
        }
        Err(ErrorCategory::InvalidResponse)
    }
}

#[tokio::test]
async fn explicit_ranges_and_deadline_bounds_survive_the_public_pipeline() {
    let document = Document { markdown: "изделие срок от 300 до 500 дней\n\nизделие срок не ранее 100 дней\n\nизделие срок не позднее 600 дней".into(), visibility: Visibility::Public };
    let runtime = Runtime::new(RangeReplay, Duration::from_secs(1));
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    let report = review_markdown(&document, ReviewOptions::default(), &runtime, &catalog)
        .await
        .unwrap();
    let operators: Vec<_> = report
        .claims
        .iter()
        .map(|c| match &c.constraint {
            Constraint::Numeric {
                operator, value, ..
            } => (value.text.as_str(), *operator),
            _ => panic!("expected numeric claim"),
        })
        .collect();
    assert_eq!(
        operators,
        vec![
            ("300", Operator::AtLeast),
            ("500", Operator::AtMost),
            ("100", Operator::AtLeast),
            ("600", Operator::AtMost)
        ]
    );
    assert!(report.packets.iter().all(|p| p.binding_issues.is_empty()));
    assert!(report.investigations.is_empty());
}
