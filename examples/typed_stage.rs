use std::time::Duration;

use procurement_review::{
    core::{ErrorCategory, Visibility},
    runtime::{ReviewRun, Runtime, StageKind, StageRequest, grok::Model},
};
use serde::{Deserialize, Serialize};

struct LocalModel;

impl Model for LocalModel {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        if prompt.contains("Review the evidence") {
            Ok(r#"{"evidence_id":"unknown","conclusion":"Проверено"}"#.into())
        } else {
            Ok(r#"{"id":"evidence-1","quote":"грузовой лифт"}"#.into())
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Evidence {
    id: String,
    quote: String,
}

#[derive(Debug, Deserialize)]
struct Review {
    evidence_id: String,
    conclusion: String,
}

#[tokio::main]
async fn main() {
    let runtime = Runtime::new(LocalModel, Duration::from_secs(1));
    let run = ReviewRun::new(2);
    let document = "Описание: грузовой лифт";
    let first = StageRequest::new(StageKind::Claims, "Extract evidence as JSON.").field(
        "document",
        &[Visibility::Public],
        &document,
    );
    let evidence: Evidence = runtime.step(&run, &first).await.expect("first step");

    let second = StageRequest::new(StageKind::Review, "Review the evidence as JSON.").field(
        "evidence",
        &[Visibility::Public],
        &evidence,
    );
    let review: Review = runtime.step(&run, &second).await.expect("second step");

    // JSON has the expected shape, but only the application can validate the ID.
    if review.evidence_id != evidence.id {
        println!("Отклонён неизвестный ID: {}", review.evidence_id);
    } else {
        println!("Заключение: {}", review.conclusion);
    }
    assert_ne!(review.evidence_id, evidence.id);
    assert_eq!(run.attempts(), 2);
    println!("Сохранённое доказательство: {evidence:?}");
    println!("Попыток: {}", run.attempts());
}
