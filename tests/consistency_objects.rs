use std::time::Duration;

use procurement_review::{
    consistency::{
        ReviewOptions,
        claims::{
            ClaimBatch, ClaimProposal, ConstraintProposal, ObjectProposal, Quote, validate_batch,
        },
        markdown::MarkdownIndex,
        review_markdown,
    },
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model},
};
use serde_json::{Value, json};

struct Replay;
impl Model for Replay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let prompt: Value = serde_json::from_str(prompt).unwrap();
        let data = prompt["data"].as_array().unwrap();
        let Some(packet) = data
            .iter()
            .find(|f| f["name"] == "packet")
            .map(|f| &f["value"])
        else {
            return Err(ErrorCategory::InvalidResponse);
        };
        let mut claims = Vec::new();
        for block in packet["blocks"].as_array().unwrap() {
            if !packet["subjects"]
                .as_array()
                .unwrap()
                .contains(&block["id"])
            {
                continue;
            }
            let text = block["text"][0].as_str().unwrap();
            let q = |id: Value, text: &str| json!({"block_id":id,"quote":text});
            let id = block["id"].clone();
            let (key, evidence, alternatives, value) = if text.starts_with("Лифт № 1 имеет")
            {
                (
                    "Новый лифт № 1",
                    vec![q(json!(0), "Новый лифт № 1"), q(id.clone(), "Лифт № 1")],
                    vec![],
                    "300",
                )
            } else if text.starts_with("Он имеет") {
                (
                    "Новый лифт № 1",
                    vec![q(json!(0), "Новый лифт № 1"), q(id.clone(), "Он")],
                    vec![],
                    "300",
                )
            } else if text.starts_with("Старый лифт") {
                (
                    "Старый лифт № 2",
                    vec![q(id.clone(), "Старый лифт № 2")],
                    vec![],
                    "500",
                )
            } else if text.starts_with("Твердомер имеет") {
                (
                    "",
                    vec![q(id.clone(), "Твердомер")],
                    vec![
                        q(json!(0), "Стационарный твердомер № 1"),
                        q(json!(1), "Портативный твердомер № 2"),
                    ],
                    "500",
                )
            } else {
                continue;
            };
            claims.push(json!({"statement":q(id.clone(),text),"object":{"key":key,"evidence":evidence,"alternatives":alternatives},"property":q(id.clone(),"массу"),"constraint":{"kind":"numeric","operator":q(id.clone(),"ровно"),"value":q(id.clone(),value),"unit":q(id.clone(),"кг")},"stage":null,"conditions":[],"mandatory":null}));
        }
        Ok(
            json!({"claims":claims,"priority_rules":[],"standard_mentions":[],"refinements":[]})
                .to_string(),
        )
    }
}

async fn report(markdown: &str) -> procurement_review::consistency::ConsistencyReport {
    let document = Document {
        markdown: markdown.into(),
        visibility: Visibility::Public,
    };
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    review_markdown(
        &document,
        ReviewOptions::default(),
        &Runtime::new(Replay, Duration::from_secs(1)),
        &catalog,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn numbered_and_pronoun_references_keep_new_and_old_equipment_separate() {
    for reference in [
        "Лифт № 1 имеет массу ровно 300 кг.",
        "Он имеет массу ровно 300 кг.",
    ] {
        let markdown =
            format!("Новый лифт № 1.\n\n{reference}\n\nСтарый лифт № 2 имеет массу ровно 500 кг.");
        let report = report(&markdown).await;
        assert_eq!(report.claims.len(), 2);
        assert!(report.claims.iter().all(|c| c.complete));
        assert_ne!(report.claims[0].object.key, report.claims[1].object.key);
        assert!(report.investigations.is_empty());
        assert!(report.object_questions.is_empty());
        for claim in &report.claims {
            for source in &claim.object.evidence {
                assert_eq!(&markdown[source.evidence.range.clone()], source.text);
            }
        }
    }
}

#[tokio::test]
async fn ambiguous_hardness_tester_is_asked_with_both_sources() {
    let markdown = "Стационарный твердомер № 1.\n\nПортативный твердомер № 2.\n\nТвердомер имеет массу ровно 500 кг.";
    let report = report(markdown).await;
    assert_eq!(report.object_questions.len(), 1);
    let question = &report.object_questions[0];
    assert_eq!(question.alternatives.len(), 2);
    for item in &question.alternatives {
        assert_eq!(&markdown[item.evidence.range.clone()], item.text);
    }
    assert!(report.investigations.is_empty());
}

#[test]
fn unrelated_local_quote_cannot_validate_a_remote_definition() {
    let index =
        MarkdownIndex::new("Новый лифт № 1.\n\nСтарый лифт № 2 имеет массу ровно 500 кг.").unwrap();
    let q = |block_id, quote: &str| Quote {
        block_id,
        quote: quote.into(),
    };
    let claim = ClaimProposal {
        statement: q(1, "Старый лифт № 2 имеет массу ровно 500 кг."),
        object: ObjectProposal {
            key: "Новый лифт № 1".into(),
            evidence: vec![q(0, "Новый лифт № 1"), q(1, "Старый лифт № 2")],
            alternatives: vec![],
        },
        property: q(1, "массу"),
        constraint: ConstraintProposal::Numeric {
            operator: q(1, "ровно"),
            value: q(1, "500"),
            unit: Some(q(1, "кг")),
        },
        stage: None,
        conditions: vec![],
        mandatory: None,
    };
    let outcome = validate_batch(
        &index,
        &[0, 1],
        ClaimBatch {
            claims: vec![claim],
            priority_rules: vec![],
        },
    );
    assert_eq!(outcome.claims[0].object.key, None);
    assert!(!outcome.claims[0].complete);
}
