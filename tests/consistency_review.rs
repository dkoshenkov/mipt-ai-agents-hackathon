use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use procurement_review::{
    consistency::{ReviewOptions, investigation::Verdict, review_markdown},
    core::{Document, ErrorCategory, Visibility},
    runtime::{Runtime, grok::Model},
};
use serde_json::{Value, json};

struct Replay;

struct PriorityReplay;

struct RecordingReplay(Arc<Mutex<Vec<&'static str>>>);

impl Model for RecordingReplay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let request: Value = serde_json::from_str(prompt).unwrap();
        let fields = request["data"].as_array().unwrap();
        let has = |name: &str| fields.iter().any(|field| field["name"] == name);
        let stage = if has("mentions") {
            "priority"
        } else if has("candidate") {
            "critic"
        } else if has("mention") {
            "standard"
        } else if has("existing_claims") {
            "control"
        } else if has("control_pass") {
            "claims"
        } else {
            "comparison"
        };
        self.0.lock().unwrap().push(stage);
        match stage {
            "priority" => Ok(json!({"rules":[]}).to_string()),
            "standard" => Ok(
                json!({"role":"unknown","title":null,"statement":null,"method":null,
                "temperature":null,"caveats":[],"attributed_threshold":null,"official_excerpt":null,
                "outcome":"insufficient_evidence"})
                .to_string(),
            ),
            "comparison" => Ok(json!({"relations":[]}).to_string()),
            _ => {
                let mut response: Value =
                    serde_json::from_str(&Replay.complete(prompt).await?).unwrap();
                if let Some(packet) = fields.iter().find(|f| f["name"] == "packet") {
                    for block in packet["value"]["blocks"].as_array().unwrap() {
                        let text = block["text"][0].as_str().unwrap();
                        if text.contains("приоритет") {
                            response["priority_rules"] =
                                json!([{"block_id":block["id"],"quote":text}]);
                        }
                    }
                }
                Ok(response.to_string())
            }
        }
    }
}

#[tokio::test]
async fn markdown_routes_each_stage_and_shares_attempts() {
    let document = Document {
        markdown: "ведомость: Лифт А должен иметь массу ровно 300 кг.\n\nЛифт А должен иметь массу ровно 500 кг после проверки.\n\nДля Лифт А массу приоритет ведомость.\n\nБиблиография: ГОСТ 9013-59.".into(),
        visibility: Visibility::Public,
    };
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    let worker = Arc::new(Mutex::new(Vec::new()));
    let reviewer = Arc::new(Mutex::new(Vec::new()));
    let runtime = Runtime::new(RecordingReplay(worker.clone()), Duration::from_secs(1))
        .with_reviewer(RecordingReplay(reviewer.clone()));
    let report = review_markdown(&document, ReviewOptions::default(), &runtime, &catalog)
        .await
        .unwrap();
    assert_eq!(*worker.lock().unwrap(), ["claims"]);
    let reviewer_calls = reviewer.lock().unwrap().clone();
    for stage in ["control", "comparison", "standard", "critic", "priority"] {
        assert!(
            reviewer_calls.contains(&stage),
            "missing {stage}: {reviewer_calls:?}"
        );
    }
    assert_eq!(report.model_attempts, 1 + reviewer_calls.len());

    let calls = Arc::new(Mutex::new(Vec::new()));
    let single = Runtime::new(RecordingReplay(calls.clone()), Duration::from_secs(1));
    let report = review_markdown(&document, ReviewOptions::default(), &single, &catalog)
        .await
        .unwrap();
    let single_calls = calls.lock().unwrap();
    assert_eq!(single_calls.len(), report.model_attempts);
    assert_eq!(single_calls[0], "claims");
    for stage in ["control", "comparison", "standard", "critic", "priority"] {
        assert!(
            single_calls.contains(&stage),
            "missing {stage}: {single_calls:?}"
        );
    }
}

impl Model for PriorityReplay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let parsed: Value = serde_json::from_str(prompt).unwrap();
        let fields = parsed["data"].as_array().unwrap();
        if fields.iter().any(|f| f["name"] == "mentions") {
            return Ok(
                json!({"rules":[{"mention":0,"source":0,"source_reference":"ведомость",
                "object":"Лифт А","property":"массу","conditions":[],"effective_date":null,
                "previous_source":null,"transfer_entire_requirement":false}]})
                .to_string(),
            );
        }
        let mut response: Value = serde_json::from_str(&Replay.complete(prompt).await?).unwrap();
        if let Some(packet) = fields.iter().find(|f| f["name"] == "packet") {
            for block in packet["value"]["blocks"].as_array().unwrap() {
                let text = block["text"][0].as_str().unwrap();
                if text.contains("приоритет") {
                    response["priority_rules"] = json!([{"block_id":block["id"],"quote":text}]);
                }
            }
        }
        Ok(response.to_string())
    }
}

impl Model for Replay {
    async fn complete(&self, prompt: &str) -> Result<String, ErrorCategory> {
        let prompt: Value = serde_json::from_str(prompt).unwrap();
        let field = |name: &str| {
            prompt["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|field| field["name"] == name)
                .map(|f| &f["value"])
        };
        if let Some(packet) = field("packet") {
            let mut claims = Vec::new();
            for block in packet["blocks"].as_array().unwrap() {
                let text = block["text"][0].as_str().unwrap();
                let Some(value) = ["300", "500"].into_iter().find(|n| text.contains(n)) else {
                    continue;
                };
                if value == "500" && field("control_pass") == Some(&json!(false)) {
                    continue;
                }
                let quote = |text: &str| json!({"block_id":block["id"],"quote":text});
                claims.push(json!({"statement":quote(text),"object":{"key":"Лифт А","evidence":[quote("Лифт А")],"alternatives":[]},
                    "property":quote("массу"),"constraint":{"kind":"numeric","operator":quote("ровно"),"value":quote(value),"unit":quote("кг")},
                    "mandatory":quote("должен"),"stage":null,"conditions":[]}));
            }
            return Ok(json!({"claims":claims,"priority_rules":[], "standard_mentions":[], "refinements":[]}).to_string());
        }
        if let Some(candidate) = field("candidate") {
            return Ok(json!({"candidate_id":candidate["id"],"version":candidate["evidence"][0]["version"],
                "action":"confirmed","explanation":"Одна позиция и два обязательных точных значения.",
                "evidence": (0..candidate["evidence"].as_array().unwrap().len()).collect::<Vec<_>>()}).to_string());
        }
        Err(ErrorCategory::InvalidResponse)
    }
}

#[tokio::test]
async fn stale_response_blocks_automatic_patch() {
    use procurement_review::consistency::{
        patch::Verification,
        priority::{Selection, Target, UserResponse},
    };

    let document = Document {
        markdown: "ведомость: Лифт А должен иметь массу ровно 300 кг.\n\nЛифт А должен иметь массу ровно 500 кг.\n\nДля Лифт А массу приоритет ведомость.".into(),
        visibility: Visibility::Public,
    };
    let runtime = Runtime::new(PriorityReplay, Duration::from_secs(1));
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    let automatic = review_markdown(&document, ReviewOptions::default(), &runtime, &catalog)
        .await
        .unwrap();
    let Selection::Selected {
        target,
        change,
        source,
        ..
    } = &automatic.selections[0].1
    else {
        panic!("expected automatic selection: {:?}", automatic.selections);
    };
    assert!(!automatic.patches.is_empty());
    let answer = UserResponse {
        visibility: Visibility::Public,
        target: Target {
            version: "stale".into(),
            sides: target.sides.clone(),
        },
        change: change.clone(),
        use_requirement: source.statement.evidence.clone(),
        revise_priority: false,
        provenance: "решение заказчика".into(),
    };
    let report = review_markdown(
        &document,
        ReviewOptions {
            user_response: Some(&answer),
            ..Default::default()
        },
        &runtime,
        &catalog,
    )
    .await
    .unwrap();
    assert_eq!(report.response_target_matches, Some(0));
    assert!(matches!(report.selections[0].1, Selection::Selected { .. }));
    assert!(matches!(
        report.patch_verification,
        Some(Verification::Incomplete { .. })
    ));
    assert!(report.patches.is_empty());
    assert!(report.copy_version.is_none());
}

#[tokio::test]
async fn control_pass_restores_a_conflict_with_versioned_sources() {
    let document = Document {
        markdown:
            "Лифт А должен иметь массу ровно 300 кг.\n\nЛифт А должен иметь массу ровно 500 кг."
                .into(),
        visibility: Visibility::Public,
    };
    let runtime = Runtime::new(Replay, Duration::from_secs(1));
    let catalog = serde_json::from_str(include_str!("fixtures/consistency/catalog.json")).unwrap();
    let report = review_markdown(&document, ReviewOptions::default(), &runtime, &catalog)
        .await
        .unwrap();
    assert!(report.initial_candidates.is_empty());
    assert_eq!(report.investigations.len(), 1);
    let result = &report.investigations[0];
    assert!(matches!(result.outcome, Ok(Verdict::Confirmed { .. })));
    for value in ["300", "500"] {
        assert!(
            result.candidate.evidence.iter().any(
                |e| e.version == report.version && &document.markdown[e.range.clone()] == value
            )
        );
    }
    use procurement_review::consistency::{
        patch::Verification,
        priority::{Selection, UserResponse},
    };
    let Selection::AskUser { target, .. } = &report.selections[0].1 else {
        panic!("missing choice")
    };
    let mut answer = UserResponse {
        visibility: Visibility::Public,
        target: target.clone(),
        change: report.claims[1].statement.evidence.clone(),
        use_requirement: report.claims[0].statement.evidence.clone(),
        revise_priority: false,
        provenance: "решение заказчика".into(),
    };
    let options = ReviewOptions {
        user_response: Some(&answer),
        ..Default::default()
    };
    let revised = review_markdown(&document, options, &runtime, &catalog)
        .await
        .unwrap();
    assert!(
        matches!(revised.patch_verification, Some(Verification::Verified)),
        "{:?}",
        revised.patch_verification
    );
    assert_eq!(revised.patches[0].replacement, "300");
    assert_ne!(revised.copy_version.as_ref(), Some(&revised.version));
    assert_eq!(revised.model_attempts, 5);
    assert!(document.markdown.contains("500"));
    let limited = review_markdown(
        &document,
        ReviewOptions {
            model_attempts: 3,
            ..options
        },
        &runtime,
        &catalog,
    )
    .await
    .unwrap();
    assert!(matches!(
        limited.patch_verification,
        Some(Verification::Incomplete { .. })
    ));
    assert_eq!(limited.model_attempts, 3);
    answer.visibility = Visibility::Private;
    let restricted = review_markdown(
        &document,
        ReviewOptions {
            user_response: Some(&answer),
            ..Default::default()
        },
        &runtime,
        &catalog,
    )
    .await
    .unwrap();
    assert!(matches!(
        restricted.patch_verification,
        Some(Verification::Incomplete { .. })
    ));
    assert_eq!(restricted.model_attempts, 3);
    let private = Document {
        visibility: Visibility::Private,
        ..document
    };
    let denied = review_markdown(&private, ReviewOptions::default(), &runtime, &catalog)
        .await
        .unwrap();
    assert_eq!(denied.model_attempts, 0);
    assert!(
        denied
            .packets
            .iter()
            .all(|p| p.stop.as_deref() == Some("RestrictedContext"))
    );
}
