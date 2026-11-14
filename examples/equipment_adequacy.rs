//! Offline replay by default; --live-public explicitly marks supplied files as public.
use procurement_review::{
    core::{ErrorCategory, Visibility},
    qualification::{
        sources::{PublicHttp, SnapshotHttp},
        *,
    },
    runtime::{
        Runtime,
        grok::{Grok, Model},
        search::{SearchModel, SearchResult},
    },
};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Deserialize)]
struct Exchange {
    operation: String,
    response: serde_json::Value,
}
#[derive(Clone)]
struct RecordedModel {
    responses: Arc<Mutex<VecDeque<Exchange>>>,
    searches: Arc<Mutex<VecDeque<SearchResult>>>,
}
impl RecordedModel {
    fn new(responses: &str, searches: &str) -> Self {
        Self {
            responses: Arc::new(Mutex::new(serde_json::from_str(responses).unwrap())),
            searches: Arc::new(Mutex::new(serde_json::from_str(searches).unwrap())),
        }
    }
}
impl Model for RecordedModel {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let data: serde_json::Value =
            serde_json::from_str(prompt).map_err(|_| ErrorCategory::InvalidResponse)?;
        let instruction = data["instruction"]
            .as_str()
            .ok_or(ErrorCategory::InvalidResponse)?;
        let operation = if instruction.starts_with("Extract") {
            "extract"
        } else if instruction.starts_with("Verify") {
            "verify"
        } else if instruction.contains("ONLY final review") {
            "review"
        } else {
            "assess"
        };
        let exchange = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(ErrorCategory::InvalidResponse)?;
        if exchange.operation != operation {
            return Err(ErrorCategory::InvalidResponse);
        }
        Ok(exchange.response.to_string())
    }
}
impl SearchModel for RecordedModel {
    async fn search(&self, _: &str, _: usize) -> Result<SearchResult, ErrorCategory> {
        self.searches
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(ErrorCategory::InvalidResponse)
    }
}

async fn replay(text: &str, worker_responses: &str, reviewer_responses: &str) -> serde_json::Value {
    let input = QualificationInput {
        document: Material::markdown("tz", text, Visibility::Public),
        attachments: vec![],
    };
    let worker_records = RecordedModel::new(
        worker_responses,
        include_str!("fixtures/qualification/search.json"),
    );
    let reviewer_records = RecordedModel::new(reviewer_responses, "[]");
    let worker = Runtime::new(worker_records.clone(), Duration::from_secs(1));
    let reviewer = Runtime::new(reviewer_records.clone(), Duration::from_secs(1));
    let mut http = SnapshotHttp::default();
    http.insert(
        "https://manufacturer.example/hardness.html",
        "text/html",
        include_bytes!("fixtures/qualification/hardness.html").to_vec(),
    );
    let options = QualificationOptions::default();
    let before = review_equipment_adequacy(&input, &options, &worker, &reviewer, &http)
        .await
        .expect("valid fixture input");
    assert!(before.coverage.complete(), "{:?}", before.coverage.gaps);
    let (requirement, question) = before
        .questions()
        .find(|(r, _)| r.property == "division")
        .expect("division question");
    let answer = Clarification {
        question_id: question.id.clone(),
        requirement_id: requirement.id.clone(),
        document_version: before.materials[0].version.clone(),
        text: "При рабочей нагрузке нужно различать изменения массы 0,02 г.".into(),
        visibility: Visibility::Public,
    };
    let restored = serde_json::from_slice(&serde_json::to_vec(&before).unwrap()).unwrap();
    let ResumeOutcome::Updated { report: after } =
        resume_equipment_adequacy(&input, restored, answer, &options, &worker, &http).await
    else {
        panic!("fixture resume rejected")
    };
    assert!(worker_records.responses.lock().unwrap().is_empty());
    assert!(worker_records.searches.lock().unwrap().is_empty());
    assert!(reviewer_records.responses.lock().unwrap().is_empty());
    serde_json::json!({"before":before,"after":after})
}

fn material(path: &str) -> Result<Material, Box<dyn std::error::Error>> {
    let format = match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => MaterialFormat::Markdown,
        "html" | "htm" => MaterialFormat::Html,
        "pdf" => MaterialFormat::Pdf,
        _ => return Err(format!("Unsupported input: {path}").into()),
    };
    Ok(Material {
        id: path.into(),
        visibility: Visibility::Public,
        format,
        bytes: std::fs::read(path)?,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--live-public") {
        if args.len() < 2 {
            return Err(
                "Usage: equipment_adequacy --live-public specification.md [passport.pdf ...]"
                    .into(),
            );
        }
        let input = QualificationInput {
            document: material(&args[1])?,
            attachments: args[2..]
                .iter()
                .map(|p| material(p))
                .collect::<Result<_, _>>()?,
        };
        let worker = Runtime::new(
            Grok::from_env(std::env::var("XAI_WORKER_MODEL")?)?,
            Duration::from_secs(90),
        );
        let reviewer = Runtime::new(
            Grok::from_env(std::env::var("XAI_REVIEWER_MODEL")?)?,
            Duration::from_secs(90),
        );
        let report = review_equipment_adequacy(
            &input,
            &QualificationOptions::default(),
            &worker,
            &reviewer,
            &PublicHttp,
        )
        .await
        .map_err(|e| format!("Invalid input: {e:?}"))?;
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if args.is_empty() {
        let original = replay(
            include_str!("fixtures/qualification/original.md"),
            include_str!("fixtures/qualification/original-worker.json"),
            include_str!("fixtures/qualification/original-reviewer.json"),
        )
        .await;
        let revised = replay(
            include_str!("fixtures/qualification/revised.md"),
            include_str!("fixtures/qualification/revised-worker.json"),
            include_str!("fixtures/qualification/revised-reviewer.json"),
        )
        .await;
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"original":original,"revised":revised})
            )?
        );
    } else {
        return Err("Use no arguments for offline replay, or --live-public specification.md [attachments...]".into());
    }
    Ok(())
}
