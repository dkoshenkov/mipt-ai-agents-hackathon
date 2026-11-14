mod qualification_support;
use procurement_review::{
    core::{ErrorCategory, Visibility},
    qualification::{sources::SnapshotHttp, *},
    runtime::{
        ReviewRun, Runtime, StageKind, StageRequest, StepError,
        grok::Model,
        search::{SearchModel, SearchResult},
    },
};
use qualification_support::*;
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct SearchProbe {
    calls: Arc<AtomicUsize>,
    mode: &'static str,
}
impl Model for SearchProbe {
    async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
        Ok("[]".into())
    }
}
impl SearchModel for SearchProbe {
    async fn search(&self, _: &str, _: usize) -> Result<SearchResult, ErrorCategory> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            "slow" => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(SearchResult::default())
            }
            "limited" => Err(ErrorCategory::RateLimited),
            _ => Ok(SearchResult::default()),
        }
    }
}

#[tokio::test]
async fn search_shares_budget_and_rechecks_every_source_before_dispatch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::new(
        SearchProbe {
            calls: calls.clone(),
            mode: "ok",
        },
        Duration::from_secs(1),
    );
    let run = ReviewRun::new(1);
    for visibility in [Visibility::Private, Visibility::Unknown] {
        let sources = [Visibility::Public, visibility];
        let request = StageRequest::new(StageKind::Search, "trusted application instruction")
            .field("derived_query", &sources, "source text");
        assert_eq!(
            runtime.search(&run, &request, 2).await.err(),
            Some(StepError::RestrictedContext)
        );
        assert_eq!(run.attempts(), 0);
    }
    let request = StageRequest::new(StageKind::Search, "lookup").field(
        "query",
        &[Visibility::Public],
        "A manual",
    );
    let (a, b) = tokio::join!(
        runtime.search(&run, &request, 2),
        runtime.search(&run, &request, 2)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.attempts(), 1);
    assert_eq!(
        runtime.step::<Vec<u32>>(&run, &request).await.err(),
        Some(StepError::BudgetExhausted)
    );
    for (mode, expected) in [
        ("slow", ErrorCategory::TimedOut),
        ("limited", ErrorCategory::RateLimited),
    ] {
        let runtime = Runtime::new(
            SearchProbe {
                calls: calls.clone(),
                mode,
            },
            Duration::from_millis(2),
        );
        let run = ReviewRun::new(1);
        assert_eq!(
            runtime.search(&run, &request, 2).await.err(),
            Some(StepError::Model(expected))
        );
        assert_eq!(run.attempts(), 1);
    }
}

fn source_worker() -> Script {
    Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            return Ok(extracted(vec![numeric(
                &b,
                "Bandwidth >= 50 MHz",
                "bandwidth",
                "50",
                "MHz",
                Operator::AtLeast,
            )]));
        }
        if instruction.starts_with("Verify") {
            let b = blocks(data, "source_blocks");
            if b.iter().any(|b| b.material_id.starts_with("source-")) {
                assert_eq!(
                    data["source_origins"][0]["url"],
                    "https://maker.example/passport"
                );
                assert_eq!(data["source_origins"][0]["material_id"], b[0].material_id);
            }
            return Ok(json!(FactVerification {
                primary_source: true,
                reason: "Original passport, correct A column".into(),
                fact: Some(FactProposal {
                    model: quote(&b, "A"),
                    parameter: quote(&b, "Bandwidth"),
                    value: quote(&b, "50"),
                    unit: Some(quote(&b, "MHz")),
                    conditions: vec![]
                })
            }));
        }
        if data["confirmed_facts"].as_array().unwrap().is_empty() {
            Ok(json!(Decision::TechnicalFact {
                requirement_id: data["current_requirement"]["id"].as_str().unwrap().into(),
                target: FactTarget {
                    model: "A".into(),
                    parameter: "Bandwidth".into(),
                    numeric: true,
                    conditions: vec![]
                },
                query: "Manufacturer A bandwidth passport".into()
            }))
        } else {
            Ok(assessed(
                data,
                Assessment::Justified,
                "Task: measure 40 MHz signals.",
            ))
        }
    })
}

#[tokio::test]
async fn attached_passport_precedes_search_and_live_candidate_is_read_directly() {
    for format in [MaterialFormat::Html, MaterialFormat::Pdf] {
        let bytes = if format == MaterialFormat::Html {
            include_bytes!("fixtures/qualification/passport.html").as_slice()
        } else {
            include_bytes!("fixtures/qualification/passport.pdf").as_slice()
        };
        for attached in [true, false] {
            let mut input = input("Bandwidth >= 50 MHz\nTask: measure 40 MHz signals.");
            if attached {
                input.attachments.push(Material {
                    id: "passport".into(),
                    visibility: Visibility::Public,
                    format,
                    bytes: bytes.to_vec(),
                });
            }
            let mut http = SnapshotHttp::default();
            http.insert(
                "https://maker.example/passport",
                if format == MaterialFormat::Pdf {
                    "application/pdf"
                } else {
                    "text/html"
                },
                bytes.to_vec(),
            );
            // A source list without an inline annotation is still only a candidate.
            let worker = source_worker().with_search(SearchResult {
                sources: vec!["https://maker.example/passport".into()],
                text: "A has 100 MHz (unverified summary)".into(),
                ..Default::default()
            });
            let report = review_equipment_adequacy(
                &input,
                &QualificationOptions::default(),
                &runtime(worker.clone()),
                &reviewer(),
                &http,
            )
            .await
            .unwrap();
            let record = record(&report, "bandwidth");
            assert_eq!(record.facts.len(), 1);
            assert_eq!(record.facts[0].value, "50");
            assert!(matches!(
                record.outcome,
                RequirementOutcome::Assessed { .. }
            ));
            assert_eq!(worker.search_count(), usize::from(!attached));
            assert_eq!(http.calls().len(), usize::from(!attached));
            assert_eq!(report.http_requests, usize::from(!attached));
        }
    }
}

#[tokio::test]
async fn failed_verification_keeps_locally_read_source_and_prior_evidence() {
    let worker = Script::new(|instruction, data| {
        if instruction.starts_with("Extract") {
            return Ok(extracted(vec![numeric(
                &blocks(data, "packet"),
                "Bandwidth >= 50 MHz",
                "bandwidth",
                "50",
                "MHz",
                Operator::AtLeast,
            )]));
        }
        if instruction.starts_with("Verify") {
            return Err(ErrorCategory::RateLimited);
        }
        Ok(json!(Decision::TechnicalFact {
            requirement_id: data["current_requirement"]["id"].as_str().unwrap().into(),
            target: FactTarget {
                model: "A".into(),
                parameter: "Bandwidth".into(),
                numeric: true,
                conditions: vec![]
            },
            query: "A passport".into()
        }))
    })
    .with_search(SearchResult {
        text: "candidate".into(),
        sources: vec!["https://maker.example/passport".into()],
        ..Default::default()
    });
    let mut http = SnapshotHttp::default();
    http.insert(
        "https://maker.example/passport",
        "text/html",
        include_bytes!("fixtures/qualification/passport.html").to_vec(),
    );
    let report = review_equipment_adequacy(
        &input("Bandwidth >= 50 MHz"),
        &QualificationOptions::default(),
        &runtime(worker),
        &reviewer(),
        &http,
    )
    .await
    .unwrap();
    let record = record(&report, "bandwidth");
    assert!(!record.read_evidence.is_empty());
    assert_eq!(record.requirement.evidence.len(), 1);
    assert!(record.facts.is_empty());
    assert!(matches!(record.outcome, RequirementOutcome::Stopped { .. }));
    assert_eq!(report.questions().count(), 0);
    assert!(!report.coverage.complete());
}
