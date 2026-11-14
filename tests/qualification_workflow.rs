mod qualification_support;
use procurement_review::{
    core::{ErrorCategory, Visibility},
    qualification::{sources::SnapshotHttp, *},
};
use qualification_support::*;
use serde_json::json;

fn balance_worker(memory_failure: bool) -> Script {
    Script::new(move |instruction, data| {
        if instruction.starts_with("Extract") {
            let b = blocks(data, "packet");
            let mut requirements = vec![];
            if b.iter().any(|b| b.text.contains("Capacity >= 2 kg")) {
                requirements.push(numeric(
                    &b,
                    "Capacity >= 2 kg",
                    "capacity",
                    "2",
                    "kg",
                    Operator::AtLeast,
                ));
            }
            if b.iter().any(|b| b.text.contains("division <= 0,1 g")) {
                requirements.push(numeric(
                    &b,
                    "division <= 0,1 g",
                    "division",
                    "0,1",
                    "g",
                    Operator::AtMost,
                ));
            }
            if b.iter().any(|b| b.text.contains("Memory >= 1000 records")) {
                let mut r = numeric(
                    &b,
                    "Memory >= 1000 records",
                    "memory",
                    "1000",
                    "records",
                    Operator::AtLeast,
                );
                r.position = "15".into();
                requirements.push(r);
            }
            return Ok(extracted(requirements));
        }
        match data["current_requirement"]["property"].as_str().unwrap() {
            "capacity" => Ok(assessed(
                data,
                Assessment::Justified,
                "Task: weigh samples up to 1,5 kg, including the fixture.",
            )),
            "division" if data["answers"].as_array().unwrap().is_empty() => {
                Ok(ask(data, "minimum distinguishable mass difference"))
            }
            "division" => Ok(assessed(
                data,
                Assessment::Insufficient,
                "Task: weigh samples up to 1,5 kg, including the fixture.",
            )),
            "memory" if memory_failure => Err(ErrorCategory::RateLimited),
            "memory" => Ok(ask(
                data,
                "maximum records before export, including reserve",
            )),
            other => panic!("Unexpected property {other}"),
        }
    })
}

#[tokio::test]
async fn independent_conditions_keep_order_evidence_and_partial_failures() {
    let mut source = input(include_str!("fixtures/qualification/original.md"));
    source.attachments.push(Material::markdown(
        "uncited-passport",
        "Spare passport, not an obligation.",
        Visibility::Public,
    ));
    let worker = balance_worker(true);
    let review = Script::new(|instruction, data| {
        assert!(instruction.contains("ONLY final review"));
        assert_eq!(data["records"].as_array().unwrap().len(), 3);
        assert!(
            data["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["material_id"] == "uncited-passport")
        );
        Ok(json!(no_change_review(data)))
    });
    let report = review_equipment_adequacy(
        &source,
        &QualificationOptions::default(),
        &runtime(worker.clone()),
        &runtime(review.clone()),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert_eq!(review.count(), 1);
    assert_eq!(worker.count(), 4);
    assert_eq!(
        report
            .requirements
            .iter()
            .map(|r| r.requirement.property.as_str())
            .collect::<Vec<_>>(),
        ["capacity", "division", "memory"]
    );
    assert!(matches!(
        record(&report, "capacity").outcome,
        RequirementOutcome::Assessed {
            assessment: Assessment::Justified,
            ..
        }
    ));
    assert!(matches!(
        record(&report, "division").outcome,
        RequirementOutcome::Pending {
            question: Some(_),
            ..
        }
    ));
    assert!(matches!(
        record(&report, "memory").outcome,
        RequirementOutcome::Stopped {
            failure: Failure {
                reason: FailureReason::Model(ErrorCategory::RateLimited),
                ..
            }
        }
    ));
    assert!(!report.coverage.complete());
    assert_eq!(report.questions().count(), 1);
    assert_eq!(report.materials.len(), 2);
    assert_eq!(report.model_attempts, 5);
    for r in &report.requirements {
        for s in &r.requirement.evidence {
            assert_eq!(
                &String::from_utf8_lossy(&source.document.bytes)[s.start..s.end],
                s.quote
            );
        }
    }
    let restored: QualificationReport =
        serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
    assert_eq!(restored.materials, report.materials);
    assert_eq!(restored.requirements, report.requirements);
}

#[tokio::test]
async fn no_applicable_requirements_is_distinct_from_unreadable_or_skipped_input() {
    let worker = Script::new(|_, _| Ok(extracted(vec![])));
    let options = QualificationOptions::default();
    let complete = review_equipment_adequacy(
        &input("Delivery within 20 days."),
        &options,
        &runtime(worker.clone()),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(complete.requirements.is_empty());
    assert!(complete.coverage.complete());
    let mut broken = input("Delivery within 20 days.");
    broken.attachments.push(Material {
        id: "unreadable".into(),
        visibility: Visibility::Public,
        format: MaterialFormat::Pdf,
        bytes: b"not a PDF".to_vec(),
    });
    let partial = review_equipment_adequacy(
        &broken,
        &options,
        &runtime(worker.clone()),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(!partial.coverage.complete());
    assert!(
        partial
            .coverage
            .gaps
            .iter()
            .any(|g| g.material_id.as_deref() == Some("unreadable"))
    );
    let mut tiny = options.clone();
    tiny.max_context_bytes = 1;
    let calls = worker.count();
    let report = review_equipment_adequacy(
        &input("Capacity >= 2 kg"),
        &tiny,
        &runtime(worker.clone()),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(!report.coverage.complete());
    assert_eq!(worker.count(), calls);
    assert_eq!(report.model_attempts, 0);
}

#[tokio::test]
async fn extraction_packets_stay_bounded_when_the_document_exceeds_context() {
    let text = (0..100)
        .map(|i| format!("Position {i}: delivery within {} days.\n", i + 1))
        .collect::<String>();
    let worker = Script::new(|instruction, data| {
        assert!(instruction.starts_with("Extract"));
        assert!(serde_json::to_vec(data).unwrap().len() < 3_000);
        Ok(extracted(vec![]))
    });
    let options = QualificationOptions {
        max_context_bytes: 20_000,
        extraction_batch_bytes: 1_000,
        ..Default::default()
    };
    let report = review_equipment_adequacy(
        &input(&text),
        &options,
        &runtime(worker.clone()),
        &reviewer(),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert!(serde_json::to_vec(&report.blocks).unwrap().len() > options.max_context_bytes);
    assert!(worker.count() > 1);
    assert!(report.coverage.blocks.iter().all(|b| b.extracted));
    assert!(
        !report
            .coverage
            .gaps
            .iter()
            .any(|g| g.failure.stage == "extract")
    );
}

#[tokio::test]
async fn bounded_attachment_extraction_retains_references_from_other_packets() {
    let mut text = "Position 13: bandwidth is mandatory in passport.\n".to_string();
    for i in 0..100 {
        text.push_str(&format!("Position {i}: delivery within {} days.\n", i + 1));
    }
    let mut source = input(&text);
    source.attachments.push(Material::markdown(
        "passport",
        "Bandwidth >= 50 MHz",
        Visibility::Public,
    ));
    let worker = Script::new(|instruction, data| {
        if !instruction.starts_with("Extract") {
            return Ok(ask(data, "operating frequency"));
        }
        assert!(serde_json::to_vec(data).unwrap().len() < 20_000);
        let b = blocks(data, "packet");
        let specification = blocks(data, "specification");
        if b.iter().any(|b| b.material_id == "passport")
            && specification
                .iter()
                .any(|b| b.text.contains("bandwidth is mandatory in passport"))
        {
            assert!(
                !b.iter()
                    .any(|b| b.text.contains("bandwidth is mandatory in passport"))
            );
            let mut requirement = numeric(
                &b,
                "Bandwidth >= 50 MHz",
                "bandwidth",
                "50",
                "MHz",
                Operator::AtLeast,
            );
            requirement.reference =
                Some(quote(&specification, "bandwidth is mandatory in passport"));
            Ok(extracted(vec![requirement]))
        } else {
            Ok(extracted(vec![]))
        }
    });
    for extraction_batch_bytes in [1_000, 12_000] {
        let options = QualificationOptions {
            max_context_bytes: 20_000,
            extraction_batch_bytes,
            model_attempts: 200,
            ..Default::default()
        };
        let report = review_equipment_adequacy(
            &source,
            &options,
            &runtime(worker.clone()),
            &reviewer(),
            &SnapshotHttp::default(),
        )
        .await
        .unwrap();
        assert_eq!(report.requirements.len(), 1);
        let requirement = &report.requirements[0].requirement;
        assert_eq!(requirement.evidence[0].material_id, "passport");
        assert_eq!(requirement.reference.as_ref().unwrap().material_id, "tz");
        assert!(report.coverage.blocks.iter().all(|b| b.extracted));
        assert!(
            !report
                .coverage
                .gaps
                .iter()
                .any(|g| g.failure.stage == "extract")
        );
    }
}

#[tokio::test]
async fn restricted_context_and_budget_never_become_an_empty_success() {
    for visibility in [Visibility::Private, Visibility::Unknown] {
        let mut source = input("Capacity >= 2 kg");
        source.document.visibility = visibility;
        let worker = balance_worker(false);
        let review = Script::new(|_, _| panic!("restricted reviewer"));
        let result = review_equipment_adequacy(
            &source,
            &QualificationOptions::default(),
            &runtime(worker.clone()),
            &runtime(review.clone()),
            &SnapshotHttp::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.model_attempts, 0);
        assert_eq!(worker.count(), 0);
        assert_eq!(review.count(), 0);
        assert!(!result.coverage.complete());
    }
    for limit in [0, 1, 2] {
        let options = QualificationOptions {
            model_attempts: limit,
            ..Default::default()
        };
        let worker = balance_worker(false);
        let review = Script::new(|_, d| Ok(json!(no_change_review(d))));
        let result = review_equipment_adequacy(
            &input(include_str!("fixtures/qualification/original.md")),
            &options,
            &runtime(worker.clone()),
            &runtime(review.clone()),
            &SnapshotHttp::default(),
        )
        .await
        .unwrap();
        assert!(result.model_attempts <= limit);
        assert_eq!(review.count(), usize::from(limit > 0));
        assert!(!result.coverage.complete());
        if limit == 2 {
            assert_eq!(result.requirements.len(), 3);
            assert!(
                result
                    .requirements
                    .iter()
                    .all(|r| matches!(r.outcome, RequirementOutcome::Stopped { .. }))
            );
        }
    }
}

#[tokio::test]
async fn late_answer_changes_only_its_condition_and_rejects_changed_materials() {
    let mut source = input(include_str!("fixtures/qualification/original.md"));
    source.attachments.push(Material::markdown(
        "unused",
        "This original passport is retained.",
        Visibility::Public,
    ));
    let worker = balance_worker(false);
    let reviewer = reviewer();
    let options = QualificationOptions::default();
    let report = review_equipment_adequacy(
        &source,
        &options,
        &runtime(worker.clone()),
        &reviewer,
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    let (r, q) = report
        .questions()
        .find(|(r, _)| r.property == "division")
        .unwrap();
    let answer = Clarification {
        question_id: q.id.clone(),
        requirement_id: r.id.clone(),
        document_version: report.materials[0].version.clone(),
        text: "Need to distinguish 0,02 g".into(),
        visibility: Visibility::Public,
    };
    let original_capacity = record(&report, "capacity").clone();
    let original_memory = record(&report, "memory").clone();
    let restored = serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
    let ResumeOutcome::Updated { report: updated } = resume_equipment_adequacy(
        &source,
        restored,
        answer.clone(),
        &options,
        &runtime(worker.clone()),
        &SnapshotHttp::default(),
    )
    .await
    else {
        panic!("resume failed")
    };
    assert_eq!(record(&updated, "capacity"), &original_capacity);
    assert_eq!(record(&updated, "memory"), &original_memory);
    assert!(matches!(
        record(&updated, "division").outcome,
        RequirementOutcome::Assessed {
            assessment: Assessment::Insufficient,
            ..
        }
    ));
    assert_eq!(updated.coverage.review, report.coverage.review);
    assert_eq!(updated.questions().count(), 1);
    let before = worker.count();
    assert!(matches!(
        resume_equipment_adequacy(
            &source,
            updated,
            answer.clone(),
            &options,
            &runtime(worker.clone()),
            &SnapshotHttp::default()
        )
        .await,
        ResumeOutcome::Rejected {
            reason: FailureReason::InvalidQuestion,
            ..
        }
    ));
    for mutation in 0..5 {
        let mut input = source.clone();
        let mut answer = answer.clone();
        match mutation {
            0 => {
                input.attachments.clear();
            }
            1 => input.attachments[0].bytes.push(b' '),
            2 => input.attachments[0].visibility = Visibility::Private,
            3 => {
                input.document.bytes = include_bytes!("fixtures/qualification/revised.md").to_vec()
            }
            _ => answer.question_id = "other-question".into(),
        }
        let ResumeOutcome::Rejected { report: kept, .. } = resume_equipment_adequacy(
            &input,
            report.clone(),
            answer,
            &options,
            &runtime(worker.clone()),
            &SnapshotHttp::default(),
        )
        .await
        else {
            panic!("accepted changed context")
        };
        assert_eq!(kept.requirements, report.requirements);
    }
    assert_eq!(worker.count(), before);
    assert_eq!(worker.search_count(), 0);
}

#[tokio::test]
async fn reviewer_failure_keeps_prior_results_and_is_not_retried() {
    let review = Script::new(|_, _| Err(ErrorCategory::TimedOut));
    let result = review_equipment_adequacy(
        &input(include_str!("fixtures/qualification/original.md")),
        &QualificationOptions::default(),
        &runtime(balance_worker(false)),
        &runtime(review.clone()),
        &SnapshotHttp::default(),
    )
    .await
    .unwrap();
    assert_eq!(review.count(), 1);
    assert_eq!(result.requirements.len(), 3);
    assert_eq!(result.questions().count(), 2);
    assert!(matches!(
        result.coverage.review,
        ReviewStatus::Failed { .. }
    ));
    assert!(!result.coverage.complete());
}

#[tokio::test]
async fn ambiguous_attachment_scope_and_reference_only_passports_do_not_create_obligations() {
    for ambiguous in [true, false] {
        let text = if ambiguous {
            "Position 13: see table with models A and B in passport."
        } else {
            "Delivery within 20 days; passport is for reference only."
        };
        let mut source = input(text);
        source.attachments.push(Material::markdown(
            "passport",
            "| Parameter | A | B |\n| Bandwidth | 50 MHz | 100 MHz |",
            Visibility::Public,
        ));
        let worker = Script::new(move |_, data| {
            let unresolved = if ambiguous {
                vec![quote(
                    &blocks(data, "specification"),
                    "see table with models A and B in passport",
                )]
            } else {
                vec![]
            };
            Ok(json!(Extraction {
                requirements: vec![],
                unresolved
            }))
        });
        let report = review_equipment_adequacy(
            &source,
            &QualificationOptions::default(),
            &runtime(worker),
            &reviewer(),
            &SnapshotHttp::default(),
        )
        .await
        .unwrap();
        assert!(report.requirements.is_empty());
        assert_eq!(report.coverage.complete(), !ambiguous);
        assert!(report.blocks.iter().any(|b| b.text.contains("100 MHz")));
    }
}
